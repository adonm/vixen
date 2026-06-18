//! CSS `<time>` parsing — pure logic called out by `docs/PLAN.md` "Testing
//! strategy" as a Rust-unit-test surface. Implements the unit grammar of
//! CSS Values 4 § 6.7 ("Duration: the `<time>` type"). The companion to
//! [`crate::length`] and [`crate::angle`].
//!
//! What lives here:
//! - [`TimeUnit`] — every `<time>` unit the cascade resolves at v1.0
//!   (`s`, `ms`).
//! - [`Time`] — a `(f64, TimeUnit)` pair with parse + arithmetic +
//!   conversion to milliseconds (the canonical internal form transitions,
//!   animations, and scroll-timing consume).
//!
//! What does *not* live here:
//! - Tokenisation of full CSS (Stylo).
//! - Animation frame timing / `requestAnimationFrame` scheduling (Phase 6).
//!
//! Conversions are per CSS Values 4: `1s = 1000ms`. A unitless zero is
//! *not* accepted for `<time>` (CSS Values 4 § 6.7, unlike `<length>` and
//! `<angle>`).
//!
//! Reference: <https://www.w3.org/TR/css-values-4/#time>.

#![forbid(unsafe_code)]

/// Every `<time>` unit Vixen resolves at v1.0 (CSS Values 4 § 6.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeUnit {
    S,
    Ms,
}

impl TimeUnit {
    /// Canonical lower-case suffix used by [`Time::parse`].
    pub fn suffix(self) -> &'static str {
        match self {
            TimeUnit::S => "s",
            TimeUnit::Ms => "ms",
        }
    }

    /// Factor to convert one unit → milliseconds (the canonical internal form).
    /// `1s = 1000ms`.
    pub fn to_ms_factor(self) -> f64 {
        match self {
            TimeUnit::S => 1000.0,
            TimeUnit::Ms => 1.0,
        }
    }
}

/// A CSS `<time>` value (magnitude + unit).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Time {
    pub value: f64,
    pub unit: TimeUnit,
}

impl Time {
    pub const fn new(value: f64, unit: TimeUnit) -> Self {
        Self { value, unit }
    }

    pub const fn s(value: f64) -> Self {
        Self::new(value, TimeUnit::S)
    }

    pub const fn ms(value: f64) -> Self {
        Self::new(value, TimeUnit::Ms)
    }

    /// Parse a single `<time>` token (e.g. `"0.3s"`, `"250ms"`). Surrounding
    /// whitespace is trimmed. Unlike `<length>`/`<angle>`, a unitless `"0"`
    /// is **not** accepted for `<time>` per CSS Values 4 § 6.7 — a unit is
    /// always required.
    pub fn parse(input: &str) -> Result<Self, TimeParseError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(TimeParseError::Empty);
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
            return Err(TimeParseError::InvalidNumber(s.to_owned()));
        }
        let (num_str, unit_str) = s.split_at(i);
        let value: f64 = num_str
            .parse()
            .map_err(|_| TimeParseError::InvalidNumber(num_str.to_owned()))?;
        // Unit required (CSS Values 4 § 6.7). Negative times are syntactically
        // valid but a semantic error at the animation layer; we parse and let
        // the caller reject.
        if unit_str.is_empty() {
            return Err(TimeParseError::MissingUnit);
        }
        let unit = parse_unit(unit_str)?;
        Ok(Self { value, unit })
    }

    /// Resolve to milliseconds (the canonical internal form transitions and
    /// animations consume). `1s = 1000ms`.
    pub fn to_ms(self) -> f64 {
        self.value * self.unit.to_ms_factor()
    }

    /// Resolve to seconds.
    pub fn to_s(self) -> f64 {
        self.to_ms() / 1000.0
    }

    /// Scale the magnitude, keeping the unit. `2 * 250ms == 500ms`.
    pub fn scale(self, factor: f64) -> Self {
        Self {
            value: self.value * factor,
            unit: self.unit,
        }
    }
}

/// Parse error for [`Time::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeParseError {
    #[error("empty time")]
    Empty,
    #[error("invalid number: {0}")]
    InvalidNumber(String),
    #[error("time missing a unit")]
    MissingUnit,
    #[error("unknown time unit: {0:?}")]
    UnknownUnit(String),
}

/// Parse a unit suffix. `Ms`, `MS`, `mS` are invalid; `s`, `ms` valid.
/// Note: because `ms` starts with `m`, an `m` alone or `M` is rejected.
fn parse_unit(s: &str) -> Result<TimeUnit, TimeParseError> {
    Ok(match s {
        "s" => TimeUnit::S,
        "ms" => TimeUnit::Ms,
        other => return Err(TimeParseError::UnknownUnit(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Parse ----------------------------------------------------------

    #[test]
    fn parse_units() {
        for (s, unit, val) in [
            ("0.3s", TimeUnit::S, 0.3),
            ("250ms", TimeUnit::Ms, 250.0),
            ("2s", TimeUnit::S, 2.0),
            ("-1s", TimeUnit::S, -1.0), // syntactically valid
        ] {
            let t = Time::parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}"));
            assert_eq!(t.unit, unit, "unit for {s}");
            assert!((t.value - val).abs() < 1e-9, "value for {s}");
        }
    }

    #[test]
    fn parse_exponent() {
        let t = Time::parse("1e3ms").unwrap();
        assert!((t.value - 1000.0).abs() < 1e-9);
        assert_eq!(t.unit, TimeUnit::Ms);
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(Time::parse("  0.5s  ").unwrap(), Time::s(0.5));
    }

    #[test]
    fn parse_unit_required() {
        // Unlike length/angle, time requires a unit even for zero.
        assert_eq!(Time::parse("0").unwrap_err(), TimeParseError::MissingUnit);
        assert_eq!(Time::parse("0s").unwrap(), Time::s(0.0));
    }

    #[test]
    fn parse_errors() {
        assert_eq!(Time::parse("").unwrap_err(), TimeParseError::Empty);
        assert!(matches!(
            Time::parse("s").unwrap_err(),
            TimeParseError::InvalidNumber(_)
        ));
        assert!(matches!(
            Time::parse("5min").unwrap_err(),
            TimeParseError::UnknownUnit(_)
        ));
        assert!(matches!(
            Time::parse("5S").unwrap_err(), // case-sensitive
            TimeParseError::UnknownUnit(_)
        ));
        assert!(matches!(
            Time::parse("5MS").unwrap_err(),
            TimeParseError::UnknownUnit(_)
        ));
    }

    // --- Conversion -----------------------------------------------------

    #[test]
    fn to_ms_and_to_s() {
        assert!((Time::s(1.0).to_ms() - 1000.0).abs() < 1e-9);
        assert!((Time::ms(500.0).to_ms() - 500.0).abs() < 1e-9);
        assert!((Time::s(2.5).to_s() - 2.5).abs() < 1e-9);
        assert!((Time::ms(750.0).to_s() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn scale_keeps_unit() {
        assert_eq!(Time::ms(250.0).scale(2.0), Time::ms(500.0));
        assert_eq!(Time::s(1.0).scale(0.5), Time::s(0.5));
    }

    // --- Unit introspection --------------------------------------------

    #[test]
    fn unit_suffix_and_factor() {
        assert_eq!(TimeUnit::S.suffix(), "s");
        assert_eq!(TimeUnit::Ms.suffix(), "ms");
        assert!((TimeUnit::S.to_ms_factor() - 1000.0).abs() < 1e-9);
        assert!((TimeUnit::Ms.to_ms_factor() - 1.0).abs() < 1e-9);
    }
}
