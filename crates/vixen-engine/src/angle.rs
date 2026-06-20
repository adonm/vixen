//! CSS `<angle>` parsing and arithmetic — pure logic called out by
//! `docs/PLAN.md` "Testing strategy" as a Rust-unit-test surface. Implements
//! the unit grammar and conversions of CSS Values 4 § 6.1 ("Angle: the
//! `<angle>` type"). The companion to [`crate::length`].
//!
//! What lives here:
//! - [`AngleUnit`] — every `<angle>` unit the cascade resolves at v1.0
//!   (`deg`/`rad`/`grad`/`turn`).
//! - [`Angle`] — a `(f64, AngleUnit)` pair with parse + arithmetic + the
//!   canonical normalisations paint/layout need.
//!
//! What does *not* live here:
//! - Tokenisation of full CSS (Stylo's job — `docs/PLAN.md` Phase 3).
//! - Reduced-motion / zero-angle short-circuits for transitions (caller).
//!
//! Conversions are per CSS Values 4: `360deg = 2π rad = 400grad = 1turn`.
//! `0` is an accepted unitless zero (CSS Values 4 § 6.1: "the unit may be
//! omitted for zero values").
//!
//! Reference: <https://www.w3.org/TR/css-values-4/#angles>.

#![forbid(unsafe_code)]

/// Every `<angle>` unit Vixen resolves at v1.0 (CSS Values 4 § 6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AngleUnit {
    Deg,
    Rad,
    Grad,
    Turn,
}

impl AngleUnit {
    /// Canonical lower-case suffix used by [`Angle::parse`].
    pub fn suffix(self) -> &'static str {
        match self {
            AngleUnit::Deg => "deg",
            AngleUnit::Rad => "rad",
            AngleUnit::Grad => "grad",
            AngleUnit::Turn => "turn",
        }
    }

    /// Factor to convert one unit → degrees (the canonical internal form).
    /// `360deg = 2π rad = 400grad = 1turn`.
    pub fn to_deg_factor(self) -> f64 {
        match self {
            AngleUnit::Deg => 1.0,
            AngleUnit::Rad => 180.0 / std::f64::consts::PI,
            AngleUnit::Grad => 0.9, // 400grad = 360deg
            AngleUnit::Turn => 360.0,
        }
    }
}

/// A CSS `<angle>` value (magnitude + unit).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Angle {
    pub value: f64,
    pub unit: AngleUnit,
}

impl Angle {
    pub const fn new(value: f64, unit: AngleUnit) -> Self {
        Self { value, unit }
    }

    pub const fn deg(value: f64) -> Self {
        Self::new(value, AngleUnit::Deg)
    }

    pub const fn rad(value: f64) -> Self {
        Self::new(value, AngleUnit::Rad)
    }

    pub const fn turn(value: f64) -> Self {
        Self::new(value, AngleUnit::Turn)
    }

    /// Parse a single `<angle>` token (e.g. `"45deg"`, `"1.5708rad"`,
    /// `"100grad"`, `"0.25turn"`, `"0"`). Surrounding whitespace is trimmed.
    /// Per CSS Values 4 § 6.1 a unitless zero (`"0"`) is accepted; any other
    /// unitless value is a parse error.
    pub fn parse(input: &str) -> Result<Self, AngleParseError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(AngleParseError::Empty);
        }
        // Split magnitude from unit at the first non-number byte. Mirrors
        // `length::Length::parse` so the parsing surface is consistent.
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
            return Err(AngleParseError::InvalidNumber(s.to_owned()));
        }
        let (num_str, unit_str) = s.split_at(i);
        let value: f64 = num_str
            .parse()
            .map_err(|_| AngleParseError::InvalidNumber(num_str.to_owned()))?;
        let unit = if unit_str.is_empty() {
            if value == 0.0 {
                AngleUnit::Deg
            } else {
                return Err(AngleParseError::MissingUnit);
            }
        } else {
            parse_unit(unit_str)?
        };
        Ok(Self { value, unit })
    }

    /// Resolve to degrees (the canonical internal form paint/layout consumes).
    /// The conversion factors come from [`AngleUnit::to_deg_factor`].
    pub fn to_deg(self) -> f64 {
        self.value * self.unit.to_deg_factor()
    }

    /// Resolve to radians (used by `cos`/`sin` for transforms).
    pub fn to_rad(self) -> f64 {
        self.to_deg() * std::f64::consts::PI / 180.0
    }

    /// Normalise into `[0, 360)` degrees. Angles outside this range are
    /// equivalent per CSS Values 4 § 6.1; gradients and `rotate()` reduce
    /// through this normalisation.
    pub fn normalised(self) -> Self {
        let deg = self.to_deg();
        Self {
            value: deg.rem_euclid(360.0),
            unit: AngleUnit::Deg,
        }
    }

    /// Scale the magnitude, keeping the unit. `2 * 45deg == 90deg`.
    pub fn scale(self, factor: f64) -> Self {
        Self {
            value: self.value * factor,
            unit: self.unit,
        }
    }

    /// `(cos, sin)` of the angle. The two projections transforms and
    /// conic-gradient paint need (CSS Transforms 1 § 13, CSS Images 4 § 4.5).
    pub fn cos_sin(self) -> (f64, f64) {
        let r = self.to_rad();
        (r.cos(), r.sin())
    }
}

/// Parse error for [`Angle::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AngleParseError {
    #[error("empty angle")]
    Empty,
    #[error("invalid number: {0}")]
    InvalidNumber(String),
    #[error("non-zero angle missing a unit")]
    MissingUnit,
    #[error("unknown angle unit: {0:?}")]
    UnknownUnit(String),
}

/// Parse a unit suffix (case-sensitive on the first letter, matching CSS
/// tokenisation: `Deg`, `DEG`, `dEg` are invalid; `deg`, `rad` valid).
fn parse_unit(s: &str) -> Result<AngleUnit, AngleParseError> {
    Ok(match s {
        "deg" => AngleUnit::Deg,
        "rad" => AngleUnit::Rad,
        "grad" => AngleUnit::Grad,
        "turn" => AngleUnit::Turn,
        other => return Err(AngleParseError::UnknownUnit(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PI: f64 = std::f64::consts::PI;

    // --- Parse ----------------------------------------------------------

    #[test]
    fn parse_all_units() {
        for (s, unit) in [
            ("45deg", AngleUnit::Deg),
            ("1.5708rad", AngleUnit::Rad),
            ("100grad", AngleUnit::Grad),
            ("0.25turn", AngleUnit::Turn),
        ] {
            let a = Angle::parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}"));
            assert_eq!(a.unit, unit, "unit for {s}");
        }
    }

    #[test]
    fn parse_signed_and_exponent() {
        assert_eq!(
            Angle::parse("-45deg").unwrap(),
            Angle::new(-45.0, AngleUnit::Deg)
        );
        assert_eq!(
            Angle::parse("+1e2deg").unwrap(),
            Angle::new(100.0, AngleUnit::Deg)
        );
    }

    #[test]
    fn parse_unitless_zero_only() {
        assert_eq!(Angle::parse("0").unwrap(), Angle::new(0.0, AngleUnit::Deg));
        // Unitless non-zero is rejected.
        assert_eq!(
            Angle::parse("45").unwrap_err(),
            AngleParseError::MissingUnit
        );
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(
            Angle::parse("  45deg  ").unwrap(),
            Angle::new(45.0, AngleUnit::Deg)
        );
    }

    #[test]
    fn parse_errors() {
        assert_eq!(Angle::parse("").unwrap_err(), AngleParseError::Empty);
        assert!(matches!(
            Angle::parse("deg").unwrap_err(),
            AngleParseError::InvalidNumber(_)
        ));
        assert!(matches!(
            Angle::parse("45radx").unwrap_err(),
            AngleParseError::UnknownUnit(_)
        ));
        assert!(matches!(
            Angle::parse("45DEG").unwrap_err(),
            AngleParseError::UnknownUnit(_)
        ));
    }

    // --- Conversion + normalisation ------------------------------------

    #[test]
    fn to_deg_conversion() {
        assert!((Angle::deg(180.0).to_deg() - 180.0).abs() < 1e-9);
        assert!((Angle::rad(PI).to_deg() - 180.0).abs() < 1e-9);
        assert!((Angle::new(200.0, AngleUnit::Grad).to_deg() - 180.0).abs() < 1e-9);
        assert!((Angle::turn(0.5).to_deg() - 180.0).abs() < 1e-9);
    }

    #[test]
    fn to_rad_conversion() {
        assert!((Angle::deg(180.0).to_rad() - PI).abs() < 1e-9);
        assert!((Angle::rad(1.0).to_rad() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn normalisation_wraps_into_0_360() {
        assert!((Angle::deg(360.0).normalised().value - 0.0).abs() < 1e-9);
        assert!((Angle::deg(720.0).normalised().value - 0.0).abs() < 1e-9);
        assert!((Angle::deg(-90.0).normalised().value - 270.0).abs() < 1e-9);
        assert!((Angle::deg(450.0).normalised().value - 90.0).abs() < 1e-9);
    }

    #[test]
    fn scale_keeps_unit() {
        assert_eq!(Angle::deg(45.0).scale(2.0), Angle::deg(90.0));
        assert_eq!(Angle::turn(0.25).scale(4.0), Angle::turn(1.0));
    }

    // --- Trigonometry ---------------------------------------------------

    #[test]
    fn cos_sin_of_known_angles() {
        let (c, s) = Angle::deg(0.0).cos_sin();
        assert!((c - 1.0).abs() < 1e-9 && (s - 0.0).abs() < 1e-9);
        let (c, s) = Angle::deg(90.0).cos_sin();
        assert!((c - 0.0).abs() < 1e-9 && (s - 1.0).abs() < 1e-9);
        let (c, s) = Angle::deg(180.0).cos_sin();
        assert!((c - -1.0).abs() < 1e-9 && (s - 0.0).abs() < 1e-9);
        let (c, s) = Angle::turn(0.25).cos_sin();
        assert!((c - 0.0).abs() < 1e-9 && (s - 1.0).abs() < 1e-9);
    }

    // --- Unit introspection --------------------------------------------

    #[test]
    fn unit_suffix_and_factor() {
        assert_eq!(AngleUnit::Deg.suffix(), "deg");
        assert_eq!(AngleUnit::Rad.suffix(), "rad");
        assert_eq!(AngleUnit::Grad.suffix(), "grad");
        assert_eq!(AngleUnit::Turn.suffix(), "turn");
        // Every unit's full circle equals 360 deg.
        assert!((AngleUnit::Deg.to_deg_factor() * 360.0 - 360.0).abs() < 1e-9);
        assert!((AngleUnit::Rad.to_deg_factor() * 2.0 * PI - 360.0).abs() < 1e-9);
        assert!((AngleUnit::Grad.to_deg_factor() * 400.0 - 360.0).abs() < 1e-9);
        assert!((AngleUnit::Turn.to_deg_factor() * 1.0 - 360.0).abs() < 1e-9);
    }
}
