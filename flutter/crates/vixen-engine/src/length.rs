//! CSS `<length>` parsing and arithmetic — pure logic called out by
//! `docs/PLAN.md` "Testing strategy" as a Rust-unit-test surface (not a WPT
//! fixture). Implements the unit grammar and absolute-length conversions of
//! CSS Values 4 § 6.2 ("Lengths: the `<length>` type") plus the relative
//! length resolution that the cascade/layout passes need.
//!
//! What lives here:
//! - [`Unit`] — every `<length>` unit the cascade resolves at v1.0.
//! - [`Length`] — a `(f64, Unit)` pair with parse + arithmetic.
//! - [`LengthContext`] — the font/viewport/percentage basis needed to turn a
//!   relative [`Length`] into device pixels.
//!
//! What does *not* live here:
//! - Tokenisation of full CSS (that is Stylo's job — `docs/PLAN.md` Phase 3).
//! - Cascade preferencing / `calc()` reduction (Stylo). This module is the
//!   arithmetic helper used by layout and by the headless `--computed-style`
//!   projection, where we sometimes re-resolve a stored length ourselves.
//!
//! Conversions are per CSS Values 4: `1in = 96px = 2.54cm`, hence `1pt =
//! 1in/72 = 4/3 px` and `1pc = 12pt = 16px`. `ex`/`ch` cannot be resolved
//! without a shaped font; per CSS Values 4 § 6.2 "the advance measure '0'
//! glyph ... when this would be impractical to determine, it must be assumed
//! to be `0.5em`" — Vixen uses that `0.5em` fallback for both `ex` and `ch`.
//! The default viewport units (`vw` / `vh` / `vi` / `vb`) resolve against the
//! caller's current viewport; the explicit small / large / dynamic viewport
//! families (`sv*` / `lv*` / `dv*`) resolve against their dedicated fields in
//! [`LengthContext`], defaulting to the current viewport when the host layer has
//! not reported browser-chrome deltas yet.
//!
//! Reference: <https://www.w3.org/TR/css-values-4/#lengths>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Unit + Length
// ---------------------------------------------------------------------------

/// Every `<length>` unit Vixen resolves at v1.0 (CSS Values 4 § 6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    // Absolute (CSS Values 4 § 6.2 "Absolute length units").
    Px,
    Cm,
    Mm,
    Q,
    In,
    Pc,
    Pt,
    // Relative — font (CSS Values 4 § 6.2 "font-relative lengths").
    Em,
    Rem,
    Ex,
    Ch,
    // Relative — viewport (CSS Values 4 § 6.2 "viewport-relative lengths").
    Vw,
    Vh,
    Vi,
    Vb,
    Vmin,
    Vmax,
    Svw,
    Svh,
    Svi,
    Svb,
    Svmin,
    Svmax,
    Lvw,
    Lvh,
    Lvi,
    Lvb,
    Lvmin,
    Lvmax,
    Dvw,
    Dvh,
    Dvi,
    Dvb,
    Dvmin,
    Dvmax,
    /// Percentages are not strictly `<length>` but the layout math treats them
    /// as length-valued; they resolve against a caller-provided basis
    /// ([`LengthContext::percent_basis`]).
    Percent,
}

impl Unit {
    /// True for units whose pixel value depends on the [`LengthContext`]
    /// (font-relative, viewport-relative, percentages). Absolute units are
    /// context-free.
    pub fn is_relative(self) -> bool {
        matches!(
            self,
            Unit::Em
                | Unit::Rem
                | Unit::Ex
                | Unit::Ch
                | Unit::Vw
                | Unit::Vh
                | Unit::Vi
                | Unit::Vb
                | Unit::Vmin
                | Unit::Vmax
                | Unit::Svw
                | Unit::Svh
                | Unit::Svi
                | Unit::Svb
                | Unit::Svmin
                | Unit::Svmax
                | Unit::Lvw
                | Unit::Lvh
                | Unit::Lvi
                | Unit::Lvb
                | Unit::Lvmin
                | Unit::Lvmax
                | Unit::Dvw
                | Unit::Dvh
                | Unit::Dvi
                | Unit::Dvb
                | Unit::Dvmin
                | Unit::Dvmax
                | Unit::Percent
        )
    }

    /// Canonical lower-case suffix used by [`Length::parse`].
    pub fn suffix(self) -> &'static str {
        match self {
            Unit::Px => "px",
            Unit::Cm => "cm",
            Unit::Mm => "mm",
            Unit::Q => "q",
            Unit::In => "in",
            Unit::Pc => "pc",
            Unit::Pt => "pt",
            Unit::Em => "em",
            Unit::Rem => "rem",
            Unit::Ex => "ex",
            Unit::Ch => "ch",
            Unit::Vw => "vw",
            Unit::Vh => "vh",
            Unit::Vi => "vi",
            Unit::Vb => "vb",
            Unit::Vmin => "vmin",
            Unit::Vmax => "vmax",
            Unit::Svw => "svw",
            Unit::Svh => "svh",
            Unit::Svi => "svi",
            Unit::Svb => "svb",
            Unit::Svmin => "svmin",
            Unit::Svmax => "svmax",
            Unit::Lvw => "lvw",
            Unit::Lvh => "lvh",
            Unit::Lvi => "lvi",
            Unit::Lvb => "lvb",
            Unit::Lvmin => "lvmin",
            Unit::Lvmax => "lvmax",
            Unit::Dvw => "dvw",
            Unit::Dvh => "dvh",
            Unit::Dvi => "dvi",
            Unit::Dvb => "dvb",
            Unit::Dvmin => "dvmin",
            Unit::Dvmax => "dvmax",
            Unit::Percent => "%",
        }
    }
}

/// A CSS `<length>` value (magnitude + unit).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Length {
    pub value: f64,
    pub unit: Unit,
}

impl Length {
    /// Convenience constructor.
    pub const fn new(value: f64, unit: Unit) -> Self {
        Self { value, unit }
    }

    /// `Length::px(3.0)` etc.
    pub const fn px(value: f64) -> Self {
        Self::new(value, Unit::Px)
    }

    /// Parse a single `<length>` token (e.g. `"16px"`, `"1.5rem"`, `"50%"`,
    /// `"0"`). Surrounding whitespace is trimmed. Per CSS Values 4 § 6.2 a
    /// unitless zero (`"0"`) is accepted for length-valued properties; any
    /// other unitless value is a parse error.
    pub fn parse(input: &str) -> Result<Self, LengthParseError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(LengthParseError::Empty);
        }
        // Split at the first non-number byte. A CSS number is an optional sign,
        // digits, optional '.', digits, optional exponent. We find the first
        // byte that cannot continue a number and treat the rest as the unit.
        let bytes = s.as_bytes();
        let mut i = 0;
        // optional leading sign
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
        // fractional part
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
        // exponent
        if matches!(bytes.get(i), Some(b'e') | Some(b'E')) {
            let mut j = i + 1;
            if matches!(bytes.get(j), Some(b'+') | Some(b'-')) {
                j += 1;
            }
            let exp_digits_start = j;
            while let Some(&b) = bytes.get(j) {
                if b.is_ascii_digit() {
                    j += 1;
                } else {
                    break;
                }
            }
            // Only consume the exponent if it actually has digits; otherwise
            // 'e' is part of a unit like 'em' and we stop before it.
            if j > exp_digits_start {
                i = j;
            }
        }
        if !seen_digit && digits_start == i {
            return Err(LengthParseError::InvalidNumber(s.to_owned()));
        }
        let (num_str, unit_str) = s.split_at(i);
        let value: f64 = num_str
            .parse()
            .map_err(|_| LengthParseError::InvalidNumber(num_str.to_owned()))?;

        let unit = if unit_str.is_empty() {
            // Unitless: only accepted for a zero value (CSS Values 4 § 6.2).
            if value == 0.0 {
                Unit::Px
            } else {
                return Err(LengthParseError::MissingUnit);
            }
        } else {
            parse_unit(unit_str)?
        };
        Ok(Self { value, unit })
    }

    /// Scale the magnitude, keeping the unit. `2 * 5px == 10px`.
    pub fn scale(self, factor: f64) -> Self {
        Self {
            value: self.value * factor,
            unit: self.unit,
        }
    }

    /// Add two lengths of the *same* unit. Mixed-unit addition requires px
    /// resolution (see [`LengthContext`]); attempting it here returns an error
    /// rather than silently coercing.
    pub fn add_same_unit(self, other: Self) -> Result<Self, LengthArithError> {
        if self.unit != other.unit {
            return Err(LengthArithError::UnitMismatch {
                a: self.unit,
                b: other.unit,
            });
        }
        Ok(Self {
            value: self.value + other.value,
            unit: self.unit,
        })
    }

    /// Resolve to CSS pixels (the unit layout consumes). Absolute units use
    /// the CSS Values 4 fixed conversions; relative units use `ctx`. `ex`/`ch`
    /// fall back to `0.5em` per CSS Values 4 § 6.2 (see module docs).
    pub fn to_px(self, ctx: &LengthContext) -> f64 {
        match self.unit {
            Unit::Px => self.value,
            Unit::In => self.value * 96.0,
            Unit::Cm => self.value * (96.0 / 2.54),
            Unit::Mm => self.value * (96.0 / 25.4),
            Unit::Q => self.value * (96.0 / 2.54 / 40.0),
            Unit::Pt => self.value * (96.0 / 72.0),
            Unit::Pc => self.value * (96.0 / 6.0),
            Unit::Em => self.value * ctx.font_px,
            Unit::Rem => self.value * ctx.root_font_px,
            Unit::Ex => self.value * 0.5 * ctx.font_px,
            Unit::Ch => self.value * 0.5 * ctx.font_px,
            Unit::Vw => self.value / 100.0 * ctx.viewport_w as f64,
            Unit::Vh => self.value / 100.0 * ctx.viewport_h as f64,
            // `vi`/`vb` map to inline/block of the *root* writing mode; for
            // horizontal-tb (the v1.0 scope per docs/ACCEPTANCE.md) these are
            // equivalent to vw/vh.
            Unit::Vi => self.value / 100.0 * ctx.viewport_w as f64,
            Unit::Vb => self.value / 100.0 * ctx.viewport_h as f64,
            Unit::Vmin => self.value / 100.0 * ctx.viewport_w.min(ctx.viewport_h) as f64,
            Unit::Vmax => self.value / 100.0 * ctx.viewport_w.max(ctx.viewport_h) as f64,
            Unit::Svw => self.value / 100.0 * ctx.small_viewport_w as f64,
            Unit::Svh => self.value / 100.0 * ctx.small_viewport_h as f64,
            Unit::Svi => self.value / 100.0 * ctx.small_viewport_w as f64,
            Unit::Svb => self.value / 100.0 * ctx.small_viewport_h as f64,
            Unit::Svmin => {
                self.value / 100.0 * ctx.small_viewport_w.min(ctx.small_viewport_h) as f64
            }
            Unit::Svmax => {
                self.value / 100.0 * ctx.small_viewport_w.max(ctx.small_viewport_h) as f64
            }
            Unit::Lvw => self.value / 100.0 * ctx.large_viewport_w as f64,
            Unit::Lvh => self.value / 100.0 * ctx.large_viewport_h as f64,
            Unit::Lvi => self.value / 100.0 * ctx.large_viewport_w as f64,
            Unit::Lvb => self.value / 100.0 * ctx.large_viewport_h as f64,
            Unit::Lvmin => {
                self.value / 100.0 * ctx.large_viewport_w.min(ctx.large_viewport_h) as f64
            }
            Unit::Lvmax => {
                self.value / 100.0 * ctx.large_viewport_w.max(ctx.large_viewport_h) as f64
            }
            Unit::Dvw => self.value / 100.0 * ctx.dynamic_viewport_w as f64,
            Unit::Dvh => self.value / 100.0 * ctx.dynamic_viewport_h as f64,
            Unit::Dvi => self.value / 100.0 * ctx.dynamic_viewport_w as f64,
            Unit::Dvb => self.value / 100.0 * ctx.dynamic_viewport_h as f64,
            Unit::Dvmin => {
                self.value / 100.0 * ctx.dynamic_viewport_w.min(ctx.dynamic_viewport_h) as f64
            }
            Unit::Dvmax => {
                self.value / 100.0 * ctx.dynamic_viewport_w.max(ctx.dynamic_viewport_h) as f64
            }
            Unit::Percent => self.value / 100.0 * ctx.percent_basis,
        }
    }
}

/// Parse error for [`Length::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LengthParseError {
    #[error("empty length")]
    Empty,
    #[error("invalid number: {0}")]
    InvalidNumber(String),
    #[error("non-zero length missing a unit")]
    MissingUnit,
    #[error("unknown length unit: {0:?}")]
    UnknownUnit(String),
}

/// Error for same-unit arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LengthArithError {
    #[error("unit mismatch: {a:?} vs {b:?}")]
    UnitMismatch { a: Unit, b: Unit },
}

/// Parse a unit suffix (case-sensitive on the first letter, matching CSS
/// tokenisation: `Px`, `PX`, `pX` are invalid; `px`, `rem`, `vh` valid).
fn parse_unit(s: &str) -> Result<Unit, LengthParseError> {
    Ok(match s {
        "px" => Unit::Px,
        "cm" => Unit::Cm,
        "mm" => Unit::Mm,
        "q" => Unit::Q,
        "in" => Unit::In,
        "pc" => Unit::Pc,
        "pt" => Unit::Pt,
        "em" => Unit::Em,
        "rem" => Unit::Rem,
        "ex" => Unit::Ex,
        "ch" => Unit::Ch,
        "vw" => Unit::Vw,
        "vh" => Unit::Vh,
        "vi" => Unit::Vi,
        "vb" => Unit::Vb,
        "vmin" => Unit::Vmin,
        "vmax" => Unit::Vmax,
        "svw" => Unit::Svw,
        "svh" => Unit::Svh,
        "svi" => Unit::Svi,
        "svb" => Unit::Svb,
        "svmin" => Unit::Svmin,
        "svmax" => Unit::Svmax,
        "lvw" => Unit::Lvw,
        "lvh" => Unit::Lvh,
        "lvi" => Unit::Lvi,
        "lvb" => Unit::Lvb,
        "lvmin" => Unit::Lvmin,
        "lvmax" => Unit::Lvmax,
        "dvw" => Unit::Dvw,
        "dvh" => Unit::Dvh,
        "dvi" => Unit::Dvi,
        "dvb" => Unit::Dvb,
        "dvmin" => Unit::Dvmin,
        "dvmax" => Unit::Dvmax,
        "%" => Unit::Percent,
        other => return Err(LengthParseError::UnknownUnit(other.to_owned())),
    })
}

// ---------------------------------------------------------------------------
// Resolution context
// ---------------------------------------------------------------------------

/// The context a relative [`Length`] resolves against. Absolute units ignore
/// every field; percentages resolve against `percent_basis` (which the caller
/// sets to the containing-block dimension, font size, etc. per property).
#[derive(Debug, Clone, Copy)]
pub struct LengthContext {
    /// Element font size in px (the `em` basis).
    pub font_px: f64,
    /// Root element font size in px (the `rem` basis).
    pub root_font_px: f64,
    /// Viewport width in px (`vw` / `vi` / horizontal `vmin`/`vmax`).
    pub viewport_w: u32,
    /// Viewport height in px (`vh` / `vb`).
    pub viewport_h: u32,
    /// Small viewport width in px (`svw` / `svi` / `svmin`/`svmax`).
    pub small_viewport_w: u32,
    /// Small viewport height in px (`svh` / `svb`).
    pub small_viewport_h: u32,
    /// Large viewport width in px (`lvw` / `lvi` / `lvmin`/`lvmax`).
    pub large_viewport_w: u32,
    /// Large viewport height in px (`lvh` / `lvb`).
    pub large_viewport_h: u32,
    /// Dynamic viewport width in px (`dvw` / `dvi` / `dvmin`/`dvmax`).
    pub dynamic_viewport_w: u32,
    /// Dynamic viewport height in px (`dvh` / `dvb`).
    pub dynamic_viewport_h: u32,
    /// Percentage basis in px — property-dependent (containing-block size,
    /// font size, ...). The caller picks the right one before resolving.
    pub percent_basis: f64,
}

impl LengthContext {
    /// Construct a context whose default, small, large, and dynamic viewport
    /// families all resolve against the same dimensions. Host integrations can
    /// override individual `small_*` / `large_*` / `dynamic_*` fields once the
    /// browser chrome state is known.
    pub fn for_viewport(width: u32, height: u32) -> Self {
        Self {
            viewport_w: width,
            viewport_h: height,
            small_viewport_w: width,
            small_viewport_h: height,
            large_viewport_w: width,
            large_viewport_h: height,
            dynamic_viewport_w: width,
            dynamic_viewport_h: height,
            ..Self::default()
        }
    }
}

impl Default for LengthContext {
    fn default() -> Self {
        // The browser default: 16px font, 800x600 viewport (matches
        // vixen-headless and the WPT harness default), zero percentage basis
        // so an uninitialised percentage resolves to 0 (fail-safe).
        Self {
            font_px: 16.0,
            root_font_px: 16.0,
            viewport_w: 800,
            viewport_h: 600,
            small_viewport_w: 800,
            small_viewport_h: 600,
            large_viewport_w: 800,
            large_viewport_h: 600,
            dynamic_viewport_w: 800,
            dynamic_viewport_h: 600,
            percent_basis: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> LengthContext {
        LengthContext {
            font_px: 20.0,
            root_font_px: 16.0,
            viewport_w: 1000,
            viewport_h: 500,
            small_viewport_w: 900,
            small_viewport_h: 450,
            large_viewport_w: 1200,
            large_viewport_h: 700,
            dynamic_viewport_w: 960,
            dynamic_viewport_h: 480,
            percent_basis: 400.0,
        }
    }

    // --- Parse: units --------------------------------------------------

    #[test]
    fn parse_all_units() {
        for (s, unit) in [
            ("16px", Unit::Px),
            ("2cm", Unit::Cm),
            ("3mm", Unit::Mm),
            ("10q", Unit::Q),
            ("1in", Unit::In),
            ("2pc", Unit::Pc),
            ("12pt", Unit::Pt),
            ("1.5em", Unit::Em),
            ("1.5rem", Unit::Rem),
            ("2ex", Unit::Ex),
            ("2ch", Unit::Ch),
            ("50vw", Unit::Vw),
            ("50vh", Unit::Vh),
            ("10vi", Unit::Vi),
            ("10vb", Unit::Vb),
            ("5vmin", Unit::Vmin),
            ("5vmax", Unit::Vmax),
            ("10svw", Unit::Svw),
            ("10svh", Unit::Svh),
            ("10svi", Unit::Svi),
            ("10svb", Unit::Svb),
            ("5svmin", Unit::Svmin),
            ("5svmax", Unit::Svmax),
            ("10lvw", Unit::Lvw),
            ("10lvh", Unit::Lvh),
            ("10lvi", Unit::Lvi),
            ("10lvb", Unit::Lvb),
            ("5lvmin", Unit::Lvmin),
            ("5lvmax", Unit::Lvmax),
            ("10dvw", Unit::Dvw),
            ("10dvh", Unit::Dvh),
            ("10dvi", Unit::Dvi),
            ("10dvb", Unit::Dvb),
            ("5dvmin", Unit::Dvmin),
            ("5dvmax", Unit::Dvmax),
            ("50%", Unit::Percent),
        ] {
            let l = Length::parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}"));
            assert_eq!(l.unit, unit, "unit for {s}");
        }
    }

    #[test]
    fn parse_signed_and_exponent() {
        assert_eq!(Length::parse("-3px").unwrap(), Length::new(-3.0, Unit::Px));
        assert_eq!(Length::parse("+2.5em").unwrap(), Length::new(2.5, Unit::Em));
        let e = Length::parse("1e2px").unwrap();
        assert!((e.value - 100.0).abs() < 1e-9);
        assert_eq!(e.unit, Unit::Px);
    }

    #[test]
    fn parse_unitless_zero_only() {
        // Unitless zero is accepted (treated as px) per CSS Values 4 § 6.2.
        assert_eq!(Length::parse("0").unwrap(), Length::new(0.0, Unit::Px));
        assert_eq!(Length::parse("0.0").unwrap(), Length::new(0.0, Unit::Px));
        // Unitless non-zero is rejected.
        assert_eq!(
            Length::parse("5").unwrap_err(),
            LengthParseError::MissingUnit
        );
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(
            Length::parse("  16px  ").unwrap(),
            Length::new(16.0, Unit::Px)
        );
    }

    #[test]
    fn parse_errors() {
        assert_eq!(Length::parse("").unwrap_err(), LengthParseError::Empty);
        assert!(matches!(
            Length::parse("px").unwrap_err(),
            LengthParseError::InvalidNumber(_)
        ));
        assert!(matches!(
            Length::parse("5pxx").unwrap_err(),
            LengthParseError::UnknownUnit(_)
        ));
        // Case-sensitive: PX is not a valid unit.
        assert!(matches!(
            Length::parse("5PX").unwrap_err(),
            LengthParseError::UnknownUnit(_)
        ));
    }

    // --- Absolute conversions -----------------------------------------

    #[test]
    fn absolute_conversions() {
        let c = LengthContext::default();
        // 1in = 96px
        assert!((Length::parse("1in").unwrap().to_px(&c) - 96.0).abs() < 1e-9);
        // 1pt = 4/3 px
        assert!((Length::parse("1pt").unwrap().to_px(&c) - (96.0 / 72.0)).abs() < 1e-9);
        // 1pc = 16px
        assert!((Length::parse("1pc").unwrap().to_px(&c) - 16.0).abs() < 1e-9);
        // 2.54cm = 96px
        assert!((Length::parse("2.54cm").unwrap().to_px(&c) - 96.0).abs() < 1e-9);
        // 25.4mm = 96px
        assert!((Length::parse("25.4mm").unwrap().to_px(&c) - 96.0).abs() < 1e-9);
        // 40Q = 1cm = 96/2.54 px
        assert!(
            (Length::parse("40q").unwrap().to_px(&c) - (96.0 / 2.54)).abs() < 1e-9,
            "1Q = 1cm/40"
        );
    }

    // --- Relative resolution ------------------------------------------

    #[test]
    fn font_relative_resolution() {
        let c = ctx(); // font_px=20, root_font_px=16
        assert!((Length::parse("2em").unwrap().to_px(&c) - 40.0).abs() < 1e-9);
        assert!((Length::parse("2rem").unwrap().to_px(&c) - 32.0).abs() < 1e-9);
        // ex/ch fall back to 0.5em per CSS Values 4 § 6.2.
        assert!((Length::parse("2ex").unwrap().to_px(&c) - 20.0).abs() < 1e-9);
        assert!((Length::parse("2ch").unwrap().to_px(&c) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn viewport_relative_resolution() {
        let c = ctx(); // 1000x500
        assert!((Length::parse("50vw").unwrap().to_px(&c) - 500.0).abs() < 1e-9);
        assert!((Length::parse("50vh").unwrap().to_px(&c) - 250.0).abs() < 1e-9);
        assert!((Length::parse("10vmin").unwrap().to_px(&c) - 50.0).abs() < 1e-9);
        assert!((Length::parse("10vmax").unwrap().to_px(&c) - 100.0).abs() < 1e-9);
        // horizontal-tb: vi→vw, vb→vh (docs/ACCEPTANCE.md scope).
        assert!((Length::parse("50vi").unwrap().to_px(&c) - 500.0).abs() < 1e-9);
        assert!((Length::parse("50vb").unwrap().to_px(&c) - 250.0).abs() < 1e-9);
    }

    #[test]
    fn viewport_variant_units_resolve_against_dedicated_contexts() {
        let c = ctx();
        // Small viewport: 900x450.
        assert!((Length::parse("10svw").unwrap().to_px(&c) - 90.0).abs() < 1e-9);
        assert!((Length::parse("10svh").unwrap().to_px(&c) - 45.0).abs() < 1e-9);
        assert!((Length::parse("10svi").unwrap().to_px(&c) - 90.0).abs() < 1e-9);
        assert!((Length::parse("10svb").unwrap().to_px(&c) - 45.0).abs() < 1e-9);
        assert!((Length::parse("10svmin").unwrap().to_px(&c) - 45.0).abs() < 1e-9);
        assert!((Length::parse("10svmax").unwrap().to_px(&c) - 90.0).abs() < 1e-9);

        // Large viewport: 1200x700.
        assert!((Length::parse("10lvw").unwrap().to_px(&c) - 120.0).abs() < 1e-9);
        assert!((Length::parse("10lvh").unwrap().to_px(&c) - 70.0).abs() < 1e-9);
        assert!((Length::parse("10lvmin").unwrap().to_px(&c) - 70.0).abs() < 1e-9);
        assert!((Length::parse("10lvmax").unwrap().to_px(&c) - 120.0).abs() < 1e-9);

        // Dynamic viewport: 960x480.
        assert!((Length::parse("10dvw").unwrap().to_px(&c) - 96.0).abs() < 1e-9);
        assert!((Length::parse("10dvh").unwrap().to_px(&c) - 48.0).abs() < 1e-9);
        assert!((Length::parse("10dvmin").unwrap().to_px(&c) - 48.0).abs() < 1e-9);
        assert!((Length::parse("10dvmax").unwrap().to_px(&c) - 96.0).abs() < 1e-9);
    }

    #[test]
    fn for_viewport_initialises_all_viewport_families() {
        let c = LengthContext::for_viewport(360, 640);
        assert!((Length::parse("100vw").unwrap().to_px(&c) - 360.0).abs() < 1e-9);
        assert!((Length::parse("100svw").unwrap().to_px(&c) - 360.0).abs() < 1e-9);
        assert!((Length::parse("100lvh").unwrap().to_px(&c) - 640.0).abs() < 1e-9);
        assert!((Length::parse("100dvb").unwrap().to_px(&c) - 640.0).abs() < 1e-9);
    }

    #[test]
    fn percentage_resolution_uses_basis() {
        let c = ctx(); // percent_basis = 400
        assert!((Length::parse("50%").unwrap().to_px(&c) - 200.0).abs() < 1e-9);
        assert!((Length::parse("120%").unwrap().to_px(&c) - 480.0).abs() < 1e-9);
        // Default context has a 0 basis → percentage resolves to 0 (fail-safe).
        let d = LengthContext::default();
        assert!((Length::parse("50%").unwrap().to_px(&d) - 0.0).abs() < 1e-9);
    }

    // --- Arithmetic ----------------------------------------------------

    #[test]
    fn scale_and_same_unit_add() {
        assert_eq!(Length::px(5.0).scale(2.0), Length::px(10.0));
        assert_eq!(
            Length::add_same_unit(Length::px(5.0), Length::px(3.0)).unwrap(),
            Length::px(8.0)
        );
        // Mixed units reject rather than coerce silently.
        assert_eq!(
            Length::add_same_unit(Length::px(5.0), Length::new(3.0, Unit::Em)),
            Err(LengthArithError::UnitMismatch {
                a: Unit::Px,
                b: Unit::Em
            })
        );
    }

    // --- Introspection -------------------------------------------------

    #[test]
    fn unit_is_relative_classification() {
        for u in [
            Unit::Em,
            Unit::Rem,
            Unit::Ex,
            Unit::Ch,
            Unit::Vw,
            Unit::Percent,
        ] {
            assert!(u.is_relative(), "{u:?} should be relative");
        }
        for u in [
            Unit::Px,
            Unit::Pt,
            Unit::In,
            Unit::Cm,
            Unit::Mm,
            Unit::Q,
            Unit::Pc,
        ] {
            assert!(!u.is_relative(), "{u:?} should be absolute");
        }
        assert_eq!(Unit::Vmin.suffix(), "vmin");
        assert_eq!(Unit::Percent.suffix(), "%");
    }
}
