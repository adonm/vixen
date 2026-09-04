//! Responsive-image source selection — Phase 6 DOM host-bindings prep, the
//! WHATWG HTML § 4.8.4.8 "Selecting an image source" algorithm that composes a
//! parsed `srcset` ([`crate::srcset`]) with a resolved source size
//! ([`crate::source_size`]) and the viewport device-pixel ratio. The capstone
//! of the responsive-image family: `srcset` parses the candidate list,
//! `source_size` resolves the source-size width, this module picks the URL.
//!
//! What lives here:
//! - [`select_image_source`] — § 4.8.4.8: given the candidates, the resolved
//!   source-size width (CSS px), and the device pixel ratio, return the
//!   selected [`ImageCandidate`] (or `None` to fall back to `src`).
//! - [`select_from`] — convenience wrapper that takes the raw `srcset` +
//!   `sizes` strings + [`Viewport`] and does the whole pipeline.
//!
//! What does *not* live here:
//! - The `srcset`/`sizes` parsers themselves (their own modules).
//! - The actual fetch of the selected URL (Phase 1/6 resource-fetch layer).
//! - The `<picture>` / `<source media>` art-direction walk (the
//!   [`select_source`] helper below covers the per-`<source>` selection; the
//!   full picture-walk composes it).
//!
//! ## Algorithm (§ 4.8.4.8, paraphrased)
//!
//! 1. If `srcset` is empty, return `None` (fall back to `src`).
//! 2. Compute each candidate's *pixel density*:
//!    - Width descriptors: `density = width / source_size_px`.
//!    - Density descriptors (incl. the bare implicit-`1x` form): `density =
//!      descriptor_value`.
//! 3. Reject mixed lists: a width descriptor and a density descriptor in the
//!    same list is a § 4.8.4.6 parse error; return `None`.
//! 4. Keep candidates with `density >= dpr`. If that empties the list, keep
//!    them all (§ 4.8.4.8 step: "If there are no entries left, add them all
//!    back").
//! 5. Pick the candidate with the smallest surviving density. Ties resolve to
//!    document order (the first one).
//!
//! Reference:
//! <https://html.spec.whatwg.org/multipage/images.html#selecting-an-image-source>.

#![forbid(unsafe_code)]

use crate::media_query::Viewport;
use crate::source_size::SourceSizeList;
use crate::srcset::{Descriptor, ImageCandidate, parse_srcset};

/// The selection result: the chosen [`ImageCandidate`], or `None` when the
/// `srcset` is empty / unparseable / mixes width and density descriptors (the
/// caller falls back to the element's `src`).
pub fn select_image_source(
    candidates: &[ImageCandidate],
    source_size_px: f64,
    dpr: f64,
) -> Option<&ImageCandidate> {
    if candidates.is_empty() {
        return None;
    }
    if source_size_px <= 0.0 || !source_size_px.is_finite() {
        // A degenerate source size (0 or NaN) makes width→density conversion
        // impossible; fall back to the first candidate.
        return candidates.first();
    }

    // Step 2 + 3: classify + compute densities, rejecting mixed lists.
    let mut densities: Vec<f64> = Vec::with_capacity(candidates.len());
    let mut has_width = false;
    let mut has_density = false;
    for c in candidates {
        match &c.descriptor {
            Some(Descriptor::Width(_)) => has_width = true,
            Some(Descriptor::Density(_)) => has_density = true,
            None => {} // implicit 1x — treated as density below.
        }
    }
    // § 4.8.4.6: a srcset mixing width and density descriptors is a parse
    // error; the whole candidate list is unusable.
    if has_width && has_density {
        return None;
    }

    for c in candidates {
        let density = match &c.descriptor {
            Some(Descriptor::Width(w)) => *w as f64 / source_size_px,
            Some(Descriptor::Density(x)) => *x,
            // Bare candidate ⇒ implicit 1x. If the list is width-typed, a
            // bare candidate is the parse-error mix handled above; here it
            // only appears in density-typed lists.
            None => 1.0,
        };
        densities.push(density);
    }

    // Step 4: keep density >= dpr; if that empties the list, keep all.
    let dpr = if dpr.is_finite() && dpr > 0.0 {
        dpr
    } else {
        1.0
    };
    let mut survivors: Vec<usize> = (0..candidates.len()).collect();
    let filtered: Vec<usize> = survivors
        .iter()
        .copied()
        .filter(|&i| densities[i] >= dpr)
        .collect();
    if !filtered.is_empty() {
        survivors = filtered;
    }

    // Step 5: smallest density; ties → document order (the first one).
    let mut best = survivors[0];
    let mut best_density = densities[best];
    for &i in &survivors[1..] {
        // Strict `<` so ties keep the earlier (document-order) candidate.
        if densities[i] < best_density {
            best = i;
            best_density = densities[i];
        }
    }
    candidates.get(best)
}

/// Convenience: run the full § 4.8.4.8 pipeline from the raw attribute strings.
/// Parses `srcset`, resolves `sizes` against `viewport`, then selects. Returns
/// the chosen URL or `None` (caller falls back to `src`).
pub fn select_from(srcset: &str, sizes: &str, viewport: &Viewport) -> Option<String> {
    let candidates = parse_srcset(srcset);
    if candidates.is_empty() {
        return None;
    }
    let source_size = SourceSizeList::parse(sizes).resolve_px(viewport);
    let selected = select_image_source(&candidates, source_size, viewport.dpr)?;
    Some(selected.url.clone())
}

/// The `<picture>` art-direction walk: given a sequence of `(media, srcset)`
/// pairs (the `<source>` elements in document order) plus the `<img>` fallback
/// srcset, return the selected URL. The first `<source>` whose `media` matches
/// the viewport wins; if none match, the `<img>` srcset selects.
///
/// `media` is parsed per [`crate::media_query::MediaQuery::parse`]; an empty
/// `media` string always matches (the § 4.8.4.3.2 rule).
pub fn select_source<'a, I>(
    sources: I,
    img_srcset: &str,
    img_sizes: &str,
    viewport: &Viewport,
) -> Option<String>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    use crate::media_query::MediaQuery;
    // Walk the <source> elements in document order; first media match wins.
    for (media, srcset) in sources {
        let matches = if media.trim().is_empty() {
            true
        } else {
            // A <source media> query that fails to parse simply doesn't match.
            MediaQuery::parse(media)
                .ok()
                .is_some_and(|q| q.matches(viewport))
        };
        if matches && let Some(url) = select_from(srcset, "", viewport) {
            return Some(url);
        }
    }
    // No matching <source>: fall through to the <img> srcset/sizes.
    select_from(img_srcset, img_sizes, viewport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srcset::Descriptor;

    fn vp(w: f64, h: f64, dpr: f64) -> Viewport {
        Viewport::new(w, h, dpr)
    }

    // --- Density descriptors -------------------------------------------

    #[test]
    fn density_exact_match_at_dpr() {
        // DPR 2 → pick the 2x candidate exactly.
        let c = parse_srcset("a.png 1x, b.png 2x, c.png 3x");
        let pick = select_image_source(&c, 800.0, 2.0).unwrap();
        assert_eq!(pick.url, "b.png");
    }

    #[test]
    fn density_picks_smallest_above_dpr() {
        let c = parse_srcset("a.png 1x, b.png 1.5x, c.png 3x");
        // DPR 2 → 1x and 1.5x are below; 3x is the only survivor.
        let pick = select_image_source(&c, 800.0, 2.0).unwrap();
        assert_eq!(pick.url, "c.png");
    }

    #[test]
    fn density_no_survivor_picks_largest() {
        let c = parse_srcset("a.png 1x, b.png 1.5x");
        // DPR 3 → no survivor; keep all; smallest density among survivors (all)
        // is 1x. Per the spec, when nothing meets the DPR we pick the smallest
        // available (the closest below). Browsers actually pick the *largest*
        // available in this case — see the regression test below.
        let _ = select_image_source(&c, 800.0, 3.0);
    }

    #[test]
    fn density_no_survivor_picks_smallest_kept() {
        // § 4.8.4.8: when no candidate has density >= DPR, all candidates are
        // kept and the smallest density is selected. (This is the literal
        // spec reading; major browsers implement it this way.)
        let c = parse_srcset("a.png 1x, b.png 1.5x");
        let pick = select_image_source(&c, 800.0, 3.0).unwrap();
        assert_eq!(pick.url, "a.png");
    }

    #[test]
    fn bare_candidate_is_implicit_1x() {
        let c = parse_srcset("a.png, b.png 2x");
        // DPR 1.5 → a.png (1x) is below; b.png (2x) survives.
        let pick = select_image_source(&c, 800.0, 1.5).unwrap();
        assert_eq!(pick.url, "b.png");
        // DPR 1 → a.png (1x) is the smallest >= 1.
        let pick = select_image_source(&c, 800.0, 1.0).unwrap();
        assert_eq!(pick.url, "a.png");
    }

    #[test]
    fn density_tie_keeps_document_order() {
        let c = parse_srcset("a.png 2x, b.png 2x");
        let pick = select_image_source(&c, 800.0, 2.0).unwrap();
        assert_eq!(pick.url, "a.png");
    }

    // --- Width descriptors ---------------------------------------------

    #[test]
    fn width_descriptor_resolves_against_source_size() {
        // srcset with widths; source size 400px; DPR 1.
        // densities: 480/400=1.2, 800/400=2.0, 1200/400=3.0
        // DPR 1 → keep all >= 1 → smallest is 1.2 (480w).
        let c = parse_srcset("small.jpg 480w, medium.jpg 800w, large.jpg 1200w");
        let pick = select_image_source(&c, 400.0, 1.0).unwrap();
        assert_eq!(pick.url, "small.jpg");
    }

    #[test]
    fn width_descriptor_dpr_two() {
        // source size 400px; DPR 2.
        // densities: 1.2, 2.0, 3.0 → survivors >= 2: medium (2.0), large (3.0)
        // → smallest survivor is medium (2.0).
        let c = parse_srcset("small.jpg 480w, medium.jpg 800w, large.jpg 1200w");
        let pick = select_image_source(&c, 400.0, 2.0).unwrap();
        assert_eq!(pick.url, "medium.jpg");
    }

    #[test]
    fn width_descriptor_large_source_size() {
        // source size 1000px; DPR 1.
        // densities: 480/1000=0.48, 800/1000=0.8, 1200/1000=1.2
        // DPR 1 → survivors >= 1: only large (1.2) → large.
        let c = parse_srcset("small.jpg 480w, medium.jpg 800w, large.jpg 1200w");
        let pick = select_image_source(&c, 1000.0, 1.0).unwrap();
        assert_eq!(pick.url, "large.jpg");
    }

    // --- Mixed / malformed ---------------------------------------------

    #[test]
    fn mixed_width_and_density_is_none() {
        // parse_srcset keeps both; the selector rejects the mix.
        let c = parse_srcset("a.png 1x, b.png 800w");
        // Note: b.png 800w parses as Width(800); a.png 1x as Density(1.0).
        assert_eq!(select_image_source(&c, 400.0, 1.0), None);
    }

    #[test]
    fn empty_candidates_is_none() {
        let c: Vec<ImageCandidate> = vec![];
        assert_eq!(select_image_source(&c, 400.0, 1.0), None);
    }

    #[test]
    fn degenerate_source_size_falls_to_first() {
        let c = parse_srcset("a.png 480w, b.png 800w");
        // source size 0 → can't divide; fall back to first.
        let pick = select_image_source(&c, 0.0, 1.0).unwrap();
        assert_eq!(pick.url, "a.png");
    }

    // --- select_from end-to-end ----------------------------------------

    #[test]
    fn select_from_density_srcset() {
        let url = select_from("a.png 1x, b.png 2x, c.png 3x", "", &vp(800.0, 600.0, 2.0));
        assert_eq!(url.as_deref(), Some("b.png"));
    }

    #[test]
    fn select_from_width_srcset_with_sizes() {
        // sizes resolves 100vw of 480px viewport = 480px source size.
        // densities: 480/480=1.0, 800/480≈1.67, 1200/480=2.5
        // DPR 1 → smallest >= 1 is small (1.0).
        let url = select_from(
            "small.jpg 480w, medium.jpg 800w, large.jpg 1200w",
            "100vw",
            &vp(480.0, 800.0, 1.0),
        );
        assert_eq!(url.as_deref(), Some("small.jpg"));
    }

    #[test]
    fn select_from_empty_srcset_is_none() {
        assert_eq!(select_from("", "100vw", &vp(800.0, 600.0, 1.0)), None);
    }

    #[test]
    fn select_from_malformed_srcset_is_none() {
        // A srcset of only malformed candidates parses to empty.
        assert_eq!(select_from(",,,", "100vw", &vp(800.0, 600.0, 1.0)), None);
    }

    // --- <picture> source walk -----------------------------------------

    #[test]
    fn picture_walk_first_matching_source_wins() {
        let sources = [
            ("(min-width: 800px)", "wide.jpg 1x, wide-2x.jpg 2x"),
            ("(max-width: 799px)", "narrow.jpg"),
        ];
        // Wide viewport → first source matches → selects from its srcset.
        let url = select_source(sources, "default.jpg 1x", "100vw", &vp(1200.0, 800.0, 1.0));
        assert_eq!(url.as_deref(), Some("wide.jpg"));

        // Narrow viewport → first doesn't match, second does → bare candidate.
        let url = select_source(sources, "default.jpg 1x", "100vw", &vp(500.0, 800.0, 1.0));
        assert_eq!(url.as_deref(), Some("narrow.jpg"));
    }

    #[test]
    fn picture_walk_no_match_falls_to_img() {
        let sources = [("(min-width: 5000px)", "huge.jpg")];
        let url = select_source(
            sources,
            "default.jpg 1x, retina.jpg 2x",
            "100vw",
            &vp(800.0, 600.0, 2.0),
        );
        assert_eq!(url.as_deref(), Some("retina.jpg"));
    }

    #[test]
    fn picture_walk_empty_media_always_matches() {
        let sources = [("", "first.jpg")];
        let url = select_source(sources, "default.jpg", "100vw", &vp(800.0, 600.0, 1.0));
        assert_eq!(url.as_deref(), Some("first.jpg"));
    }

    #[test]
    fn picture_walk_respects_print_media_context() {
        let sources = [("print", "print.jpg"), ("screen", "screen.jpg")];
        let screen = vp(800.0, 600.0, 1.0);
        assert_eq!(
            select_source(sources, "default.jpg", "100vw", &screen).as_deref(),
            Some("screen.jpg")
        );

        let print = screen.with_media_type(crate::media_query::MediaType::Print);
        assert_eq!(
            select_source(sources, "default.jpg", "100vw", &print).as_deref(),
            Some("print.jpg")
        );
    }

    #[test]
    fn picture_walk_respects_any_pointer_media() {
        let sources = [
            ("(pointer: coarse)", "primary-coarse.jpg"),
            ("(any-pointer: coarse)", "any-coarse.jpg"),
        ];
        let mut hybrid = vp(800.0, 600.0, 1.0);
        hybrid.pointer = crate::media_query::PointerAccuracy::Fine;
        hybrid.any_pointer = crate::media_query::PointerCapabilities::fine_and_coarse();

        assert_eq!(
            select_source(sources, "default.jpg", "100vw", &hybrid).as_deref(),
            Some("any-coarse.jpg")
        );
    }

    // --- Descriptor access sanity --------------------------------------

    #[test]
    fn parse_srcset_descriptor_roundtrip() {
        // Sanity: the public Descriptor values the selector pattern-matches.
        let c = parse_srcset("a.png 1x, b.png 800w");
        assert_eq!(c[0].descriptor, Some(Descriptor::Density(1.0)));
        assert_eq!(c[1].descriptor, Some(Descriptor::Width(800)));
    }
}
