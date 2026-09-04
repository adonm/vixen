//! CSS `<ratio>` parsing — pure logic called out by `docs/PLAN.md` "Testing
//! strategy" as a Rust-unit-test surface. Implements the grammar of CSS Values
//! 4 § 4.4 ("Ratio Variables: the `<ratio>` type"). The companion to
//! [`crate::length`] / [`crate::angle`] / [`crate::time`] / [`crate::resolution`].
//!
//! What lives here:
//! - [`Ratio`] — an `(numerator, denominator)` pair with parse + the
//!   quotient the `aspect-ratio` property and the `aspect-ratio` /
//!   `device-aspect-ratio` media features reduce to.
//!
//! What does *not* live here:
//! - Tokenisation of full CSS (Stylo owns that; this module is the
//!   `--computed-style` projection + the host-hook surface for `aspect-ratio`).
//! - Box-aspect-ratio application (Phase 4 layout).
//!
//! ## Grammar (CSS Values 4 § 4.4)
//!
//! ```text
//! <ratio> = <number [0,∞]> [ / <number [0,∞]> ]?
//! ```
//!
//! - A single number `N` means the ratio `N/1`.
//! - Both terms must be non-negative finite numbers.
//! - The denominator may be `0`: per § 4.4 that denotes an infinite ratio
//!   (used by `aspect-ratio` to suppress one dimension). The quotient
//!   [`Ratio::quotient`] returns `+∞` for that case; consumers that forbid a
//!   zero denominator (e.g. the `aspect-ratio` *property*) reject it at their
//!   own boundary.
//! - The legacy `width/height` *integer* form (CSS Media Queries 4 § 7.3
//!   `device-aspect-ratio`) is folded into this grammar: integers are numbers.
//!
//! Reference: <https://www.w3.org/TR/css-values-4/#ratios>.

#![forbid(unsafe_code)]

/// A CSS `<ratio>` value (CSS Values 4 § 4.4). Stored as numerator /
/// denominator so the original authoring form round-trips through
/// [`Ratio::serialize`]; the quotient the layout / media-feature surfaces
/// reduce against is [`Ratio::quotient`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ratio {
    numerator: f64,
    denominator: f64,
}

impl Ratio {
    /// Construct from already-validated terms. Both must be finite and
    /// non-negative; the caller (usually [`Ratio::parse`]) is responsible for
    /// enforcing that invariant.
    pub const fn new(numerator: f64, denominator: f64) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    /// The single-number shorthand: `Ratio::single(3.0)` is `3/1`.
    pub const fn single(n: f64) -> Self {
        Self::new(n, 1.0)
    }

    /// Parse a `<ratio>` token (e.g. `"16/9"`, `"16 / 9"`, `"2"`, `"1.5/1"`).
    /// Surrounding whitespace is trimmed; whitespace around the `/` separator
    /// is optional per CSS tokenisation (the `/` is a single token, but this
    /// module accepts the raw attribute / declaration value).
    pub fn parse(input: &str) -> Result<Self, RatioParseError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(RatioParseError::Empty);
        }
        // Split on the first `/`. Anything after is the denominator.
        let (num_str, den_str) = match s.split_once('/') {
            None => (s, None),
            Some((a, b)) => (a, Some(b)),
        };
        let numerator = parse_number(num_str)?;
        let denominator = match den_str {
            Some(d) => parse_number(d)?,
            None => 1.0,
        };
        if numerator.is_sign_negative() || numerator < 0.0 {
            return Err(RatioParseError::Negative(numerator));
        }
        if denominator.is_sign_negative() || denominator < 0.0 {
            return Err(RatioParseError::Negative(denominator));
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// The numerator term (`16` in `16/9`).
    pub fn numerator(self) -> f64 {
        self.numerator
    }

    /// The denominator term (`9` in `16/9`); `1` for the single-number form.
    pub fn denominator(self) -> f64 {
        self.denominator
    }

    /// The numeric quotient the `aspect-ratio` property and media features
    /// reduce to (`numerator / denominator`). A zero denominator yields
    /// `+∞`, which is § 4.4's "infinite ratio" encoding; consumers that don't
    /// allow it reject the value at their own boundary.
    pub fn quotient(self) -> f64 {
        self.numerator / self.denominator
    }

    /// Is the denominator zero? (§ 4.4 "infinite ratio".)
    pub fn is_infinite(self) -> bool {
        self.denominator == 0.0
    }

    /// Serialise back to the canonical CSS form. A unit-denominator ratio
    /// serialises as the bare numerator (`3/1` → `"3"`); every other ratio
    /// round-trips as `"numerator/denominator"`. Matches the form Stylo emits
    /// for `aspect-ratio`'s computed value.
    pub fn serialize(self) -> String {
        if self.denominator == 1.0 {
            format_number(self.numerator)
        } else {
            format!(
                "{}/{}",
                format_number(self.numerator),
                format_number(self.denominator)
            )
        }
    }
}

/// Parse a single non-negative `<number>` term, rejecting empty / negative /
/// non-finite inputs. An empty term (arising from a leading/trailing `/` such
/// as `"16/"` or `"/9"`) is reported as [`RatioParseError::InvalidNumber`];
/// the only [`RatioParseError::Empty`] case is the whole input being empty,
/// which [`Ratio::parse`] handles before delegating here.
fn parse_number(input: &str) -> Result<f64, RatioParseError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(RatioParseError::InvalidNumber(input.to_owned()));
    }
    let value: f64 = s
        .parse()
        .map_err(|_| RatioParseError::InvalidNumber(input.to_owned()))?;
    if !value.is_finite() {
        return Err(RatioParseError::NotFinite);
    }
    Ok(value)
}

/// Render an `f64` the way CSS serialises numbers: integers without a trailing
/// `.0`, everything else via Rust's default `Display` (shortest round-trip).
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// Parse error for [`Ratio::parse`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RatioParseError {
    #[error("empty ratio")]
    Empty,
    #[error("invalid number: {0:?}")]
    InvalidNumber(String),
    #[error("non-finite number")]
    NotFinite,
    #[error("ratio term must be non-negative, got {0}")]
    Negative(f64),
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Parse: two-term form ------------------------------------------

    #[test]
    fn parse_two_term_no_spaces() {
        let r = Ratio::parse("16/9").unwrap();
        assert!((r.numerator() - 16.0).abs() < 1e-9);
        assert!((r.denominator() - 9.0).abs() < 1e-9);
    }

    #[test]
    fn parse_two_term_with_spaces() {
        let r = Ratio::parse("16 / 9").unwrap();
        assert!((r.quotient() - 16.0 / 9.0).abs() < 1e-9);
    }

    #[test]
    fn parse_trims_outer_whitespace() {
        let r = Ratio::parse("  4/3  ").unwrap();
        assert!((r.quotient() - 4.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn parse_floating_terms() {
        let r = Ratio::parse("1.5/1").unwrap();
        assert!((r.quotient() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn parse_integer_form_from_media_queries() {
        // device-aspect-ratio historically used the width/height integer form;
        // it folds into the modern grammar unchanged.
        let r = Ratio::parse("16/9").unwrap();
        assert!((r.numerator() - 16.0).abs() < 1e-9);
        assert!((r.denominator() - 9.0).abs() < 1e-9);
    }

    // --- Parse: single-number form -------------------------------------

    #[test]
    fn parse_single_number_means_n_over_one() {
        let r = Ratio::parse("2").unwrap();
        assert!((r.numerator() - 2.0).abs() < 1e-9);
        assert!((r.denominator() - 1.0).abs() < 1e-9);
        assert!((r.quotient() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn parse_leading_plus() {
        let r = Ratio::parse("+3").unwrap();
        assert!((r.quotient() - 3.0).abs() < 1e-9);
    }

    // --- Parse: zero / infinite handling --------------------------------

    #[test]
    fn zero_numerator_is_zero_ratio() {
        let r = Ratio::parse("0").unwrap();
        assert!((r.quotient() - 0.0).abs() < 1e-9);
        assert!(!r.is_infinite());
    }

    #[test]
    fn zero_denominator_is_infinite() {
        // § 4.4: a 0 denominator denotes an infinite ratio.
        let r = Ratio::parse("16/0").unwrap();
        assert!(r.is_infinite());
        assert!(r.quotient().is_infinite());
    }

    // --- Parse: errors --------------------------------------------------

    #[test]
    fn parse_empty_errors() {
        assert_eq!(Ratio::parse("").unwrap_err(), RatioParseError::Empty);
        assert_eq!(Ratio::parse("   ").unwrap_err(), RatioParseError::Empty);
    }

    #[test]
    fn parse_invalid_number_errors() {
        for bad in ["abc", "/", "16/", "/9", "1//2", "1 2", "--3"] {
            assert!(
                matches!(Ratio::parse(bad), Err(RatioParseError::InvalidNumber(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn parse_negative_term_errors() {
        assert!(matches!(
            Ratio::parse("-1"),
            Err(RatioParseError::Negative(_))
        ));
        assert!(matches!(
            Ratio::parse("-16/9"),
            Err(RatioParseError::Negative(_))
        ));
        assert!(matches!(
            Ratio::parse("16/-9"),
            Err(RatioParseError::Negative(_))
        ));
    }

    // --- Serialize round-trip ------------------------------------------

    #[test]
    fn serialize_round_trips() {
        for s in ["16/9", "4/3", "2", "1.5/1", "1/1", "0"] {
            let r = Ratio::parse(s).unwrap();
            let out = r.serialize();
            let reparsed = Ratio::parse(&out).unwrap();
            assert!(
                (r.quotient() - reparsed.quotient()).abs() < 1e-9,
                "{s} -> {out} quotient differs"
            );
        }
    }

    #[test]
    fn serialize_single_form_drops_unit_denominator() {
        assert_eq!(Ratio::single(3.0).serialize(), "3");
        assert_eq!(Ratio::parse("3/1").unwrap().serialize(), "3");
    }

    #[test]
    fn serialize_keeps_fractional_numerator() {
        assert_eq!(Ratio::parse("1.5/1").unwrap().serialize(), "1.5");
    }

    // --- Quotient semantics --------------------------------------------

    #[test]
    fn quotient_matches_aspect_ratio_reduction() {
        // aspect-ratio: 16/9 → 1.777…
        assert!(((Ratio::parse("16/9").unwrap().quotient()) - 16.0 / 9.0).abs() < 1e-9);
        // aspect-ratio: 1 → 1.0
        assert!((Ratio::parse("1").unwrap().quotient() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn equality_is_structural() {
        assert_eq!(Ratio::new(16.0, 9.0), Ratio::new(16.0, 9.0));
        assert_ne!(Ratio::new(16.0, 9.0), Ratio::new(8.0, 9.0));
    }
}
