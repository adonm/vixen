//! CSS `<resolution>` parsing — pure logic called out by `docs/PLAN.md`
//! "Testing strategy" as a Rust-unit-test surface. Implements the unit
//! grammar and conversions of CSS Values 4 § 6.8 ("Resolution: the
//! `<resolution>` type"). The companion to [`crate::length`] / [`crate::angle`]
//! / [`crate::time`].
//!
//! What lives here:
//! - [`ResolutionUnit`] — every `<resolution>` unit the cascade resolves at
//!   v1.0 (`dpi`, `dpcm`, `dppx`, plus the historical `x` alias).
//! - [`Resolution`] — a `(f64, ResolutionUnit)` pair with parse + arithmetic
//!   and conversion to dots-per-pixel (the canonical internal form media
//!   queries and `image-resolution` consume).
//!
//! What does *not* live here:
//! - Tokenisation of full CSS (Stylo).
//! - Actual device pixel-ratio queries (Phase 6 host hooks).
//!
//! Conversions are per CSS Values 4: `1dppx = 96dpi = 37.795dpcm`. The `x`
//! unit is the historical (CSS Image 4 § 7.3) alias for `dppx`, retained
//! because the `image-resolution` and `min-resolution`/`max-resolution`
//! media-feature surfaces both accept it.
//!
//! Reference: <https://www.w3.org/TR/css-values-4/#resolution>.

#![forbid(unsafe_code)]

/// Every `<resolution>` unit Vixen resolves at v1.0 (CSS Values 4 § 6.8 plus
/// the `x` alias from CSS Images 4 § 7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolutionUnit {
    /// Dots per CSS inch (`1in = 96px`).
    Dpi,
    /// Dots per CSS centimetre.
    Dpcm,
    /// Dots per CSS pixel — the canonical internal unit.
    Dppx,
    /// Alias for `dppx` (CSS Images 4 § 7.3). Accepted on parse, normalised
    /// to `dppx` on output.
    X,
}

impl ResolutionUnit {
    /// Canonical lower-case suffix used by [`Resolution::parse`].
    pub fn suffix(self) -> &'static str {
        match self {
            ResolutionUnit::Dpi => "dpi",
            ResolutionUnit::Dpcm => "dpcm",
            ResolutionUnit::Dppx => "dppx",
            ResolutionUnit::X => "x",
        }
    }

    /// Normalise to the canonical unit (`dppx`). `x` is folded into `dppx`
    /// so downstream code only needs to handle the three spec units.
    pub fn canonical(self) -> ResolutionUnit {
        match self {
            ResolutionUnit::X => ResolutionUnit::Dppx,
            other => other,
        }
    }

    /// Factor to convert one unit → dots-per-pixel. `1dppx = 1dot/px`;
    /// `1dpi = 1dot/(96px)`; `1dpcm = 1dot/(96/2.54 px)`.
    pub fn to_dppx_factor(self) -> f64 {
        match self {
            ResolutionUnit::Dpi => 1.0 / 96.0,
            ResolutionUnit::Dpcm => 2.54 / 96.0,
            ResolutionUnit::Dppx | ResolutionUnit::X => 1.0,
        }
    }
}

/// A CSS `<resolution>` value (magnitude + unit).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolution {
    pub value: f64,
    pub unit: ResolutionUnit,
}

impl Resolution {
    pub const fn new(value: f64, unit: ResolutionUnit) -> Self {
        Self { value, unit }
    }

    pub const fn dppx(value: f64) -> Self {
        Self::new(value, ResolutionUnit::Dppx)
    }

    pub const fn dpi(value: f64) -> Self {
        Self::new(value, ResolutionUnit::Dpi)
    }

    /// Parse a single `<resolution>` token (e.g. `"96dpi"`, `"2dppx"`,
    /// `"2x"`, `"38dpcm"`). Surrounding whitespace is trimmed. Per CSS Values
    /// 4 § 6.8 a unit is always required for `<resolution>` (no unitless zero).
    pub fn parse(input: &str) -> Result<Self, ResolutionParseError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(ResolutionParseError::Empty);
        }
        // Split magnitude from unit at the first non-number byte.
        let bytes = s.as_bytes();
        let mut i = 0;
        if matches!(bytes.get(i), Some(b'+') | Some(b'-')) {
            i += 1;
        }
        let digits_start = i;
        let mut seen_digit = false;
        while let Some(&b) = bytes.get(i) {
            if b.is_ascii_digit() {
                seen_digit = true;
                i += 1;
            } else {
                break;
            }
        }
        if bytes.get(i) == Some(&b'.') {
            i += 1;
            while let Some(&b) = bytes.get(i) {
                if b.is_ascii_digit() {
                    seen_digit = true;
                    i += 1;
                } else {
                    break;
                }
            }
        }
        if matches!(bytes.get(i), Some(b'e') | Some(b'E')) {
            let mut j = i + 1;
            if matches!(bytes.get(j), Some(b'+') | Some(b'-')) {
                j += 1;
            }
            let exp_start = j;
            while let Some(&b) = bytes.get(j) {
                if b.is_ascii_digit() {
                    j += 1;
                } else {
                    break;
                }
            }
            if j > exp_start {
                i = j;
            }
        }
        if !seen_digit && digits_start == i {
            return Err(ResolutionParseError::InvalidNumber(s.to_owned()));
        }
        let (num_str, unit_str) = s.split_at(i);
        let value: f64 = num_str
            .parse()
            .map_err(|_| ResolutionParseError::InvalidNumber(num_str.to_owned()))?;
        if unit_str.is_empty() {
            return Err(ResolutionParseError::MissingUnit);
        }
        let unit = parse_unit(unit_str)?;
        Ok(Self { value, unit })
    }

    /// Resolve to dots-per-pixel (the canonical internal form). `1dppx = 1`;
    /// `96dpi = 1dppx`; `38dpcm ≈ 1dppx`.
    pub fn to_dppx(self) -> f64 {
        self.value * self.unit.to_dppx_factor()
    }

    /// Resolve to dots-per-inch.
    pub fn to_dpi(self) -> f64 {
        self.to_dppx() * 96.0
    }

    /// Scale the magnitude, keeping the unit. `2 * 96dpi == 192dpi`.
    pub fn scale(self, factor: f64) -> Self {
        Self {
            value: self.value * factor,
            unit: self.unit,
        }
    }
}

/// Parse error for [`Resolution::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolutionParseError {
    #[error("empty resolution")]
    Empty,
    #[error("invalid number: {0}")]
    InvalidNumber(String),
    #[error("resolution missing a unit")]
    MissingUnit,
    #[error("unknown resolution unit: {0:?}")]
    UnknownUnit(String),
}

/// Parse a unit suffix (case-sensitive). `DPI`, `Dpi`, `X` (uppercase) are
/// invalid; `dpi`, `dppx`, `x` valid.
fn parse_unit(s: &str) -> Result<ResolutionUnit, ResolutionParseError> {
    Ok(match s {
        "dpi" => ResolutionUnit::Dpi,
        "dpcm" => ResolutionUnit::Dpcm,
        "dppx" => ResolutionUnit::Dppx,
        "x" => ResolutionUnit::X,
        other => return Err(ResolutionParseError::UnknownUnit(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Parse ----------------------------------------------------------

    #[test]
    fn parse_units() {
        for (s, unit) in [
            ("96dpi", ResolutionUnit::Dpi),
            ("38dpcm", ResolutionUnit::Dpcm),
            ("2dppx", ResolutionUnit::Dppx),
            ("2x", ResolutionUnit::X),
        ] {
            let r = Resolution::parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}"));
            assert_eq!(r.unit, unit, "unit for {s}");
        }
    }

    #[test]
    fn parse_signed_and_exponent() {
        assert_eq!(
            Resolution::parse("-96dpi").unwrap(),
            Resolution::new(-96.0, ResolutionUnit::Dpi)
        );
        let r = Resolution::parse("1e1dppx").unwrap();
        assert!((r.value - 10.0).abs() < 1e-9);
        assert_eq!(r.unit, ResolutionUnit::Dppx);
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(
            Resolution::parse("  2dppx  ").unwrap(),
            Resolution::dppx(2.0)
        );
    }

    #[test]
    fn parse_unit_required() {
        // No unitless zero for <resolution>.
        assert_eq!(
            Resolution::parse("0").unwrap_err(),
            ResolutionParseError::MissingUnit
        );
        assert_eq!(Resolution::parse("0dppx").unwrap(), Resolution::dppx(0.0));
    }

    #[test]
    fn parse_errors() {
        assert_eq!(
            Resolution::parse("").unwrap_err(),
            ResolutionParseError::Empty
        );
        assert!(matches!(
            Resolution::parse("dpi").unwrap_err(),
            ResolutionParseError::InvalidNumber(_)
        ));
        assert!(matches!(
            Resolution::parse("5px").unwrap_err(),
            ResolutionParseError::UnknownUnit(_)
        ));
        assert!(matches!(
            Resolution::parse("5DPI").unwrap_err(), // case-sensitive
            ResolutionParseError::UnknownUnit(_)
        ));
        assert!(matches!(
            Resolution::parse("5X").unwrap_err(), // uppercase X rejected
            ResolutionParseError::UnknownUnit(_)
        ));
    }

    // --- Conversion -----------------------------------------------------

    #[test]
    fn to_dppx_conversion() {
        assert!((Resolution::dpi(96.0).to_dppx() - 1.0).abs() < 1e-9);
        assert!((Resolution::dppx(2.0).to_dppx() - 2.0).abs() < 1e-9);
        // 1dppx = 96/2.54 dpcm ≈ 37.795.
        let one_dppx_in_dpcm = 96.0 / 2.54;
        assert!(
            (Resolution::new(one_dppx_in_dpcm, ResolutionUnit::Dpcm).to_dppx() - 1.0).abs() < 1e-9
        );
        // `x` aliases `dppx`.
        assert!((Resolution::new(2.0, ResolutionUnit::X).to_dppx() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn to_dpi_conversion() {
        assert!((Resolution::dpi(96.0).to_dpi() - 96.0).abs() < 1e-9);
        assert!((Resolution::dppx(1.0).to_dpi() - 96.0).abs() < 1e-9);
    }

    #[test]
    fn scale_keeps_unit() {
        assert_eq!(Resolution::dpi(96.0).scale(2.0), Resolution::dpi(192.0));
        assert_eq!(
            Resolution::new(2.0, ResolutionUnit::X).scale(1.5),
            Resolution::new(3.0, ResolutionUnit::X)
        );
    }

    // --- Unit introspection --------------------------------------------

    #[test]
    fn unit_suffix_factor_and_canonical() {
        assert_eq!(ResolutionUnit::Dpi.suffix(), "dpi");
        assert_eq!(ResolutionUnit::Dpcm.suffix(), "dpcm");
        assert_eq!(ResolutionUnit::Dppx.suffix(), "dppx");
        assert_eq!(ResolutionUnit::X.suffix(), "x");
        // x canonicalises to dppx; the others are themselves.
        assert_eq!(ResolutionUnit::X.canonical(), ResolutionUnit::Dppx);
        assert_eq!(ResolutionUnit::Dpi.canonical(), ResolutionUnit::Dpi);
        // Every unit's full-circle conversion equals 1 dppx (when its base
        // value is multiplied by its factor).
        assert!((96.0 * ResolutionUnit::Dpi.to_dppx_factor() - 1.0).abs() < 1e-9);
        assert!((1.0 * ResolutionUnit::Dppx.to_dppx_factor() - 1.0).abs() < 1e-9);
        assert!((1.0 * ResolutionUnit::X.to_dppx_factor() - 1.0).abs() < 1e-9);
    }
}
