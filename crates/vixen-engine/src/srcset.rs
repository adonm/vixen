//! HTML `srcset` attribute parsing — Phase 6 DOM host-bindings prep (pure
//! logic called out by `docs/PLAN.md` "Testing strategy"). Implements WHATWG
//! HTML § 4.8.4.6 "Parsing a srcset attribute", the comma-separated image
//! candidate string grammar `<img srcset>` and `<source srcset>` reduce to
//! before the responsive-image selection algorithm picks a source.
//!
//! What lives here:
//! - [`parse_srcset`] — the § 4.8.4.6 splitter + per-candidate descriptor
//!   validator, returning the list of [`ImageCandidate`]s that survived
//!   validation.
//! - [`Descriptor`] — a validated width (`640w`) or pixel-density (`2x`)
//!   descriptor.
//!
//! What does *not* live here:
//! - The responsive-image source-selection algorithm (§ 4.8.4.8) — that
//!   composes the parsed candidates with the viewport DPR + `<img sizes>`
//!   and picks one URL. It runs in the resource-fetch layer (Phase 1/6).
//! - The `sizes` attribute parser (a separate microsyntax; lands alongside
//!   selection).
//!
//! ## Grammar + validation (§ 4.8.4.6 + § 4.8.4.7)
//!
//! A srcset is a comma-separated list of *image candidate strings*. Each
//! candidate is a URL optionally followed by one descriptor:
//!
//! ```text
//! srcset     = candidate ( "," candidate )*
//! candidate  = <url> [ <descriptor> ]
//! descriptor = <ascii-digits>+ "w"               ; width, in pixels
//!             | <number> "x"                      ; pixel density
//! ```
//!
//! Per-candidate validation the spec mandates (and browsers enforce):
//! - A candidate with ≥ 3 whitespace-separated tokens is malformed and the
//!   *whole* candidate is dropped (a URL can't carry two descriptors).
//! - A descriptor that is neither a `Nw` width nor a `Nx` density form is a
//!   parse error and drops its candidate.
//! - An empty URL drops the candidate.
//! - Surviving candidates keep document order (selection prefers the first
//!   match on ties).
//!
//! Reference:
//! <https://html.spec.whatwg.org/multipage/images.html#parsing-a-srcset-attribute>.

#![forbid(unsafe_code)]

/// A validated srcset descriptor (§ 4.8.4.7). Either a width in CSS pixels
/// (`640w`) or a pixel density multiplier (`2x`).
#[derive(Debug, Clone, PartialEq)]
pub enum Descriptor {
    /// A width descriptor: `<digits>w`. The integer width in CSS pixels. The
    /// selection algorithm compares this against the effective source size
    /// from `sizes`.
    Width(u64),
    /// A pixel-density descriptor: `<number>x`. Multiplier against the device
    /// pixel ratio. `1x` is the implicit default when a candidate carries no
    /// descriptor.
    Density(f64),
}

impl Descriptor {
    /// Validate a raw descriptor token per § 4.8.4.7. Returns `None` for any
    /// token that isn't a `Nw` width or `Nx` density (the parse-error case
    /// that drops the candidate).
    pub fn parse(raw: &str) -> Option<Self> {
        if let Some(digits) = raw.strip_suffix('w') {
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                let w: u64 = digits.parse().ok()?;
                return Some(Descriptor::Width(w));
            }
            return None;
        }
        if let Some(number) = raw.strip_suffix('x') {
            if number.is_empty() {
                return None;
            }
            // Density is a `<number>`: optional sign, integer digits, optional
            // single '.' + fraction digits. Reject anything looser ("2xy",
            // "1.5.5x", ".x", "x").
            let bytes = number.as_bytes();
            let mut i = 0;
            if matches!(bytes.first(), Some(b'+') | Some(b'-')) {
                i += 1;
            }
            let int_start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let int_len = i - int_start;
            let mut frac_len = 0;
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                let frac_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                frac_len = i - frac_start;
            }
            if i != bytes.len() {
                return None; // trailing junk
            }
            if int_len + frac_len == 0 {
                return None; // no digits at all ("+x", "-x")
            }
            let value: f64 = number.parse().ok()?;
            if !value.is_finite() || value < 0.0 {
                return None;
            }
            return Some(Descriptor::Density(value));
        }
        None
    }
}

/// A single srcset image candidate: a URL + an optional [`Descriptor`]. A
/// candidate with `descriptor == None` defaults to `1x` density at selection
/// time (§ 4.8.4.8).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageCandidate {
    pub url: String,
    pub descriptor: Option<Descriptor>,
}

impl ImageCandidate {
    /// Construct a candidate with no descriptor (the implicit `1x` form).
    pub fn bare(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            descriptor: None,
        }
    }
}

/// WHATWG § 4.8.4.6 "Parsing a srcset attribute". Returns the list of
/// [`ImageCandidate`]s that survived validation, in document order. Empty
/// input ⇒ empty list.
///
/// ```
/// # use vixen_engine::srcset::{parse_srcset, Descriptor, ImageCandidate};
/// let c = parse_srcset("a.png 1x, b.png 2x");
/// assert_eq!(c.len(), 2);
/// assert_eq!(c[0].url, "a.png");
/// assert_eq!(c[0].descriptor, Some(Descriptor::Density(1.0)));
/// ```
pub fn parse_srcset(input: &str) -> Vec<ImageCandidate> {
    let mut candidates = Vec::new();
    for segment in input.split(',') {
        // § 4.8.4.6 step 3: collect a run that's not whitespace — i.e. split
        // the comma-segment into whitespace-separated tokens.
        let tokens: Vec<&str> = segment.split_ascii_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        // A URL plus at most one descriptor. 0 tokens can't happen (filtered
        // above); 1 = bare URL; 2 = URL + descriptor; 3+ = malformed candidate
        // (a URL can't carry two descriptors) ⇒ drop the whole candidate.
        if tokens.len() >= 3 {
            continue;
        }
        let url = tokens[0];
        if url.is_empty() {
            continue;
        }
        let descriptor = match tokens.get(1) {
            None => None,
            Some(raw) => match Descriptor::parse(raw) {
                Some(d) => Some(d),
                None => continue, // invalid descriptor ⇒ drop this candidate
            },
        };
        candidates.push(ImageCandidate {
            url: url.to_owned(),
            descriptor,
        });
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Descriptor::parse ---------------------------------------------

    #[test]
    fn descriptor_width() {
        assert_eq!(Descriptor::parse("640w"), Some(Descriptor::Width(640)));
        assert_eq!(Descriptor::parse("1w"), Some(Descriptor::Width(1)));
    }

    #[test]
    fn descriptor_density() {
        assert_eq!(Descriptor::parse("2x"), Some(Descriptor::Density(2.0)));
        assert_eq!(Descriptor::parse("1.5x"), Some(Descriptor::Density(1.5)));
        assert_eq!(Descriptor::parse("1x"), Some(Descriptor::Density(1.0)));
        assert_eq!(Descriptor::parse("0.5x"), Some(Descriptor::Density(0.5)));
    }

    #[test]
    fn descriptor_rejects_garbage() {
        assert_eq!(Descriptor::parse(""), None);
        assert_eq!(Descriptor::parse("640"), None); // no unit
        assert_eq!(Descriptor::parse("640px"), None);
        assert_eq!(Descriptor::parse("w"), None); // no digits
        assert_eq!(Descriptor::parse("x"), None);
        assert_eq!(Descriptor::parse("6.5w"), None); // width must be integer
        assert_eq!(Descriptor::parse("-2x"), None); // negative density invalid
        assert_eq!(Descriptor::parse("abc"), None);
        assert_eq!(Descriptor::parse("2xy"), None); // trailing junk
    }

    // --- parse_srcset: basics ------------------------------------------

    #[test]
    fn empty_input() {
        assert!(parse_srcset("").is_empty());
        assert!(parse_srcset("   ").is_empty());
        assert!(parse_srcset(",,,,").is_empty());
    }

    #[test]
    fn single_bare_url() {
        let c = parse_srcset("a.png");
        assert_eq!(c, vec![ImageCandidate::bare("a.png")]);
    }

    #[test]
    fn single_with_density() {
        let c = parse_srcset("a.png 2x");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].url, "a.png");
        assert_eq!(c[0].descriptor, Some(Descriptor::Density(2.0)));
    }

    #[test]
    fn multiple_candidates() {
        let c = parse_srcset("a.png 1x, b.png 2x, c.png 3x");
        assert_eq!(c.len(), 3);
        assert_eq!(c[2].url, "c.png");
        assert_eq!(c[2].descriptor, Some(Descriptor::Density(3.0)));
    }

    // --- whitespace tolerance ------------------------------------------

    #[test]
    fn tolerant_whitespace_between_url_and_descriptor() {
        let c = parse_srcset("a.png    2x");
        assert_eq!(c[0].descriptor, Some(Descriptor::Density(2.0)));
    }

    #[test]
    fn tolerant_whitespace_around_commas() {
        let c = parse_srcset("  a.png 1x  ,  b.png 2x  ");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].url, "a.png");
        assert_eq!(c[1].url, "b.png");
    }

    #[test]
    fn leading_trailing_commas_tolerated() {
        let c = parse_srcset(",a.png 1x,");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].url, "a.png");
    }

    // --- validation drops bad candidates -------------------------------

    #[test]
    fn invalid_descriptor_drops_candidate() {
        // The middle candidate has a bad descriptor ("hi") ⇒ dropped; the
        // others survive.
        let c = parse_srcset("a.png 1x, b.png hi, c.png 3x");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].url, "a.png");
        assert_eq!(c[1].url, "c.png");
    }

    #[test]
    fn three_token_candidate_is_dropped() {
        // A URL can't carry two descriptors ⇒ malformed candidate dropped.
        let c = parse_srcset("a.png 1x 2x");
        assert!(c.is_empty());
    }

    #[test]
    fn mixed_width_and_density_descriptors() {
        // Real-world srcset mixes widths and densities.
        let c = parse_srcset("small.jpg 480w, medium.jpg 800w, large.jpg 1200w, retina.jpg 2x");
        assert_eq!(c.len(), 4);
        assert_eq!(c[0].descriptor, Some(Descriptor::Width(480)));
        assert_eq!(c[3].descriptor, Some(Descriptor::Density(2.0)));
    }

    #[test]
    fn bare_url_among_descripted() {
        // A candidate with no descriptor (implicit 1x) is valid.
        let c = parse_srcset("a.png, b.png 2x");
        assert_eq!(c.len(), 2);
        assert!(c[0].descriptor.is_none());
        assert_eq!(c[1].descriptor, Some(Descriptor::Density(2.0)));
    }

    // --- document order preserved --------------------------------------

    #[test]
    fn document_order_preserved_on_ties() {
        // Selection prefers the first match; the parse layer must keep order.
        let c = parse_srcset("a.png 1x, b.png 1x");
        assert_eq!(c[0].url, "a.png");
        assert_eq!(c[1].url, "b.png");
    }
}
