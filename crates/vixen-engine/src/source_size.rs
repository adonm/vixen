//! HTML `<img sizes>` / `<source sizes>` attribute parsing — Phase 6 DOM
//! host-bindings prep, the companion to [`crate::srcset`]. Implements WHATWG
//! HTML § 4.8.4.7 "Parsing a sizes attribute": the `<source-size-list>` the
//! responsive-image selection algorithm (§ 4.8.4.8) reduces the width
//! descriptors in `srcset` against.
//!
//! What lives here:
//! - [`SourceSizeList`] — the parsed `<source-size-list>`: a list of
//!   `(media-condition, length)` entries plus the trailing default length.
//! - [`SourceSizeList::parse`] — the § 4.8.4.7 splitter + per-entry validator.
//! - [`SourceSizeList::resolve_px`] — evaluate the list against a [`Viewport`],
//!   returning the source-size width in CSS px the selection algorithm feeds
//!   `Nw` descriptors with.
//!
//! What does *not` live here:
//! - The selection algorithm itself ([`crate::responsive_select`] owns it —
//!   it composes a parsed `srcset` with a resolved source size + viewport DPR).
//! - The `<img>` host-hook reflection (Phase 6 host-binding layer reads the
//!   attribute and hands it here).
//!
//! ## Grammar (§ 4.8.4.7, informal)
//!
//! ```text
//! <source-size-list> = <source-size># "," <default-source-size>
//! <source-size>       = <media-condition> <length>
//! <default-source-size> = <media-condition>? <length>
//! ```
//!
//! The last comma-separated entry is the *default* and may omit its
//! `<media-condition>`; every earlier entry must have one. A non-last entry
//! without a media-condition, or a list whose last entry has no length, is a
//! parse error and the *whole* list falls back to `100vw` (the § 4.8.4.7
//! default-source-size).
//!
//! Reference:
//! <https://html.spec.whatwg.org/multipage/images.html#parsing-a-sizes-attribute>.

#![forbid(unsafe_code)]

use crate::length::{Length, LengthContext};
use crate::media_query::{MediaCondition, MediaQuery, Viewport};

/// The parsed `<source-size-list>` (WHATWG § 4.8.4.7). Resolution iterates the
/// entries in order and returns the length of the first whose media-condition
/// matches the viewport; if none match, the trailing default length applies.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSizeList {
    /// `(condition, length)` entries, in document order. The final entry's
    /// `condition` is `None` for the § 4.8.4.7 "default source size".
    entries: Vec<(Option<MediaCondition>, Length)>,
}

/// The default source-size value the spec mandates when an attribute is empty
/// or unparseable: `100vw` (§ 4.8.4.7 step "default source size").
pub const DEFAULT_SOURCE_SIZE: Length = Length {
    value: 100.0,
    unit: crate::length::Unit::Vw,
};

impl SourceSizeList {
    /// WHATWG § 4.8.4.7. Returns a list whose [`SourceSizeList::resolve_px`]
    /// matches the viewport. An empty / whitespace-only / unparseable input
    /// yields the `100vw` default (no parse error surfaced — the algorithm is
    /// total and always produces a usable list).
    pub fn parse(input: &str) -> Self {
        // Split on commas. Trim each segment; drop empties.
        let mut segments: Vec<&str> = input
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if segments.is_empty() {
            return Self::default_100vw();
        }

        // The last segment is the default: it may have no media-condition.
        // All earlier segments must have a media-condition (the tokens before
        // the final length token).
        let mut entries = Vec::with_capacity(segments.len());
        let last_idx = segments.len() - 1;
        for (i, seg) in segments.drain(..).enumerate() {
            let is_last = i == last_idx;
            match parse_source_size(seg, is_last) {
                Some((cond, len)) => entries.push((cond, len)),
                None => return Self::default_100vw(),
            }
        }

        // The default (last) entry must exist and have a length; the per-entry
        // parser guarantees the length. Confirm the last entry has either a
        // condition (explicit) or is the implicit default — both are fine.
        // If a non-last entry parsed without a condition (shouldn't, since
        // parse_source_size returns None for that case), bail to default.
        if entries
            .iter()
            .enumerate()
            .any(|(i, (c, _))| i != last_idx && c.is_none())
        {
            return Self::default_100vw();
        }

        SourceSizeList { entries }
    }

    /// The trivial `100vw` list (the § 4.8.4.7 default).
    pub fn default_100vw() -> Self {
        SourceSizeList {
            entries: vec![(None, DEFAULT_SOURCE_SIZE)],
        }
    }

    /// Resolve the source-size width against a [`Viewport`] (the § 4.8.4.8
    /// "compute the source size" step). Returns the width in CSS pixels. Walks
    /// the entries in document order; the first whose media-condition matches
    /// wins. The final entry is the unconditional fallback — if reached, its
    /// length applies regardless of any condition (a single-entry list always
    /// returns that entry's value). This is the WHATWG § 4.8.4.8 "source size"
    /// rule.
    pub fn resolve_px(&self, vp: &Viewport) -> f64 {
        let last = self.entries.len().saturating_sub(1);
        for (i, (cond, len)) in self.entries.iter().enumerate() {
            let is_last = i == last;
            let matches = match cond {
                Some(c) => c.matches(vp),
                // No condition ⇒ always matches.
                None => true,
            };
            if matches || is_last {
                return len.to_px(&self.length_context(vp));
            }
        }
        // Unreachable: the loop always returns at the last entry. Fail safe
        // to 100vw of the viewport.
        DEFAULT_SOURCE_SIZE.to_px(&self.length_context(vp))
    }

    /// The number of entries (useful for the WPT fixture count checks).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Is the entry list empty? (Always false in practice — [`parse`]
    /// guarantees at least the `100vw` default entry.)
    ///
    /// [`parse`]: SourceSizeList::parse
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Is this the trivial `100vw` default?
    pub fn is_default(&self) -> bool {
        self.entries.len() == 1
            && self.entries[0].0.is_none()
            && self.entries[0].1.value == 100.0
            && self.entries[0].1.unit == crate::length::Unit::Vw
    }

    fn length_context(&self, vp: &Viewport) -> LengthContext {
        // Source-size lengths resolve viewport-relative units against the
        // actual viewport; percentages are not valid in `sizes` but resolve
        // against the viewport width if they somehow appear.
        // Source-size lengths are viewport-relative: a bare `vw` resolves to
        // viewport width; `em`/`rem` use the default font (16px). Percentages
        // are not valid in `sizes`, but if one sneaks through, resolve it
        // against the viewport width as the least-surprising fallback.
        let mut ctx = LengthContext::for_viewport(
            vp.width_px.round().max(0.0) as u32,
            vp.height_px.round().max(0.0) as u32,
        );
        ctx.percent_basis = vp.width_px;
        ctx
    }
}

impl Default for SourceSizeList {
    fn default() -> Self {
        Self::default_100vw()
    }
}

/// Parse a single `<source-size>` string segment. Returns `None` on any
/// malformation (the caller drops the whole list to the default on a None).
///
/// `is_last` controls whether a bare length (no media-condition) is accepted:
/// the trailing default may omit the condition; every earlier entry must have
/// one.
fn parse_source_size(segment: &str, is_last: bool) -> Option<(Option<MediaCondition>, Length)> {
    // The length is the final whitespace-separated token; everything before
    // it (if any) is the `<media-condition>`.
    let tokens: Vec<&str> = segment.split_ascii_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let (cond_tokens, length_token) = tokens.split_at(tokens.len() - 1);
    let length = Length::parse(length_token[0]).ok()?;

    if cond_tokens.is_empty() {
        // No media-condition: only valid for the trailing default entry.
        if is_last {
            return Some((None, length));
        }
        return None;
    }

    // Re-join the condition tokens and parse as a media-condition. The
    // condition is the § 3 `<media-condition>` (no media-type allowed in
    // `sizes`).
    let cond_str = cond_tokens.join(" ");
    let query = MediaQuery::parse(&cond_str).ok()?;
    // A `sizes` condition must not carry a media-type prefix (§ 4.8.4.7
    // grammar: `<media-condition>` only). If the parser latched a type, reject.
    if query.media_type != crate::media_query::MediaType::All
        || query.negate && query.condition.is_none()
    {
        // `not screen` etc. is not a bare condition; reject.
        return None;
    }
    let condition = query.condition?;
    Some((Some(condition), length))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(w: f64, h: f64) -> Viewport {
        Viewport::new(w, h, 1.0)
    }

    // --- Default + empty ------------------------------------------------

    #[test]
    fn empty_input_is_100vw_default() {
        let list = SourceSizeList::parse("");
        assert!(list.is_default());
        // 100vw of an 800px viewport = 800px.
        assert!((list.resolve_px(&vp(800.0, 600.0)) - 800.0).abs() < 1e-9);
    }

    #[test]
    fn whitespace_only_is_default() {
        let list = SourceSizeList::parse("   \t  ");
        assert!(list.is_default());
    }

    #[test]
    fn bare_length_last_entry_is_default() {
        // A single bare length is a valid (default) list.
        let list = SourceSizeList::parse("500px");
        assert_eq!(list.len(), 1);
        assert!((list.resolve_px(&vp(800.0, 600.0)) - 500.0).abs() < 1e-9);
    }

    // --- Conditional entries -------------------------------------------

    #[test]
    fn single_conditional_picks_when_matching() {
        let list = SourceSizeList::parse("(min-width: 600px) 50vw");
        // 50vw of 800px = 400px.
        assert!((list.resolve_px(&vp(800.0, 600.0)) - 400.0).abs() < 1e-9);
    }

    #[test]
    fn single_conditional_last_entry_is_fallback() {
        // A single-entry list: the entry is the last (default slot) ⇒ its
        // length applies even when its condition doesn't match (§ 4.8.4.8:
        // the last entry is the unconditional fallback). 500px viewport,
        // 50vw = 250px.
        let list = SourceSizeList::parse("(min-width: 1000px) 50vw");
        assert!((list.resolve_px(&vp(500.0, 400.0)) - 250.0).abs() < 1e-9);
    }

    #[test]
    fn multi_entry_picks_first_matching() {
        let list =
            SourceSizeList::parse("(max-width: 400px) 100vw, (max-width: 800px) 50vw, 1000px");
        // 300px viewport: first entry (<=400) matches ⇒ 100vw of 300 = 300.
        assert!((list.resolve_px(&vp(300.0, 400.0)) - 300.0).abs() < 1e-9);
        // 600px: first fails, second (<=800) matches ⇒ 50vw of 600 = 300.
        assert!((list.resolve_px(&vp(600.0, 400.0)) - 300.0).abs() < 1e-9);
        // 1200px: both fail, default 1000px.
        assert!((list.resolve_px(&vp(1200.0, 400.0)) - 1000.0).abs() < 1e-9);
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn default_entry_may_carry_a_condition() {
        // The last (default) entry can still carry a condition. Resolution
        // walks in order: entry 0 fails (300 < 1000), entry 1 matches
        // (300 >= 200) ⇒ 75vw of 300 = 225.
        let list = SourceSizeList::parse("(min-width: 1000px) 50vw, (min-width: 200px) 75vw");
        assert!((list.resolve_px(&vp(300.0, 400.0)) - 225.0).abs() < 1e-9);
    }

    // --- vw / em resolution --------------------------------------------

    #[test]
    fn vw_resolves_against_viewport_width() {
        let list = SourceSizeList::parse("33vw");
        // 33vw of 1000px = 330px.
        assert!((list.resolve_px(&vp(1000.0, 600.0)) - 330.0).abs() < 1e-9);
    }

    #[test]
    fn em_resolves_against_default_font() {
        // 16px default font ⇒ 20em = 320px.
        let list = SourceSizeList::parse("20em");
        assert!((list.resolve_px(&vp(800.0, 600.0)) - 320.0).abs() < 1e-9);
    }

    // --- Malformed ⇒ default -------------------------------------------

    #[test]
    fn non_last_entry_without_condition_is_default() {
        // A leading bare length (no condition) is invalid for a non-last
        // entry ⇒ whole list drops to the 100vw default.
        let list = SourceSizeList::parse("500px, (min-width: 100px) 200px");
        assert!(list.is_default());
    }

    #[test]
    fn trailing_comma_is_tolerated() {
        let list = SourceSizeList::parse("(min-width: 100px) 50vw,");
        assert!((list.resolve_px(&vp(200.0, 400.0)) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn invalid_length_is_default() {
        let list = SourceSizeList::parse("notalength");
        assert!(list.is_default());
    }

    #[test]
    fn empty_segments_are_dropped() {
        let list = SourceSizeList::parse(",, (min-width: 100px) 50vw ,");
        assert!((list.resolve_px(&vp(200.0, 400.0)) - 100.0).abs() < 1e-9);
    }

    // --- Round-trip vs the canonical responsive pattern ----------------

    #[test]
    fn canonical_responsive_pattern() {
        // The textbook mobile-first sizes attribute.
        let list = SourceSizeList::parse("(max-width: 600px) 100vw, 50vw");
        // Mobile (400px): 100vw = 400.
        assert!((list.resolve_px(&vp(400.0, 800.0)) - 400.0).abs() < 1e-9);
        // Desktop (1200px): default 50vw = 600.
        assert!((list.resolve_px(&vp(1200.0, 800.0)) - 600.0).abs() < 1e-9);
    }

    #[test]
    fn calc_free_length_works() {
        // Plain px length in the default slot.
        let list = SourceSizeList::parse("(min-width: 1px) 100vw, 320px");
        assert!((list.resolve_px(&vp(800.0, 600.0)) - 800.0).abs() < 1e-9);
    }
}
