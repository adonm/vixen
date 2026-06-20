//! CSS Lists 3 § 6.1 — `<list-style-type>` marker-text resolution (pure logic).
//! The primitive `<li>` markers and `content: counter(...)` reduce to: given a
//! counter value (the cascade/layout layer resolves the counter scope first)
//! and a list-style-type, produce the marker string. Complements
//! [`crate::counter`] (counter scope + value resolution) as the rendering half
//! of the CSS counters/list-marker surface.
//!
//! What lives here:
//! - [`ListStyleType`] — the named counter styles v1 ships (`disc`/`circle`/
//!   `square` symbols, `decimal`/`decimal-leading-zero`, `lower-alpha`/
//!   `upper-alpha` (+ the `lower-latin`/`upper-latin` aliases), `lower-roman`/
//!   `upper-roman`, `lower-greek`, `none`).
//! - [`ListStyleType::parse`] — parse the `list-style-type` / counter-style
//!   name (case-insensitive per CSS, aliases normalise to the canonical name).
//! - [`ListStyleType::render`] — `value → marker text` per § 6.1's algorithm
//!   table (the `symbolic` bullet glyphs; the `numeric` decimal family; the
//!   `alphabetic` latin/greek bijective-base family; the `additive` roman
//!   numerals). Returns `None` for `none` (no marker).
//!
//! What does *not* live here:
//! - Counter scoping/reset/increment (that's [`crate::counter`]).
//! - The `::marker` box / layout placement (Phase 4 layout).
//! - `@counter-style` user-defined styles (CSS Lists 3 § 3 — deferred; v1
//!   ships the § 6.1.1 predefined set).
//! - CJK / `arabic-indic` / `devanagari` &c. (deferred to v1.1).
//!
//! ## Fallback rule
//!
//! CSS Lists 3 § 6.1: every counter style has a *fallback*. When the value is
//! outside the style's representable range, the fallback is used. The default
//! fallback for every predefined style is `decimal`, which can represent any
//! integer. So:
//! - `lower-roman`/`upper-roman` are additive and capped at `[1, 3999]`
//!   (CSS2 § 12.6.2); values outside fall back to decimal.
//! - `lower-alpha`/`upper-alpha`/`lower-greek` are alphabetic and require a
//!   value `≥ 1`; values `< 1` fall back to decimal.
//!
//! Reference: <https://www.w3.org/TR/css-lists-3/#marker-text>.
//! CSS2 § 12.6.2 for the predefined lists:
//! <https://www.w3.org/TR/CSS2/generate.html#lists>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// ListStyleType
// ---------------------------------------------------------------------------

/// A CSS `<list-style-type>` (CSS Lists 3 § 6.1.1, predefined set). The
/// canonical counter style; aliases (`lower-latin` ≡ `lower-alpha`,
/// `upper-latin` ≡ `upper-alpha`) normalise at [`ListStyleType::parse`] so the
/// round-trip is canonical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ListStyleType {
    /// `none` — no marker is generated (`::marker` content is empty).
    None,
    /// `disc` — `U+2022 BULLET` (•). The default per CSS2 § 12.6.1.
    #[default]
    Disc,
    /// `circle` — `U+25E6 WHITE BULLET` (◦).
    Circle,
    /// `square` — `U+25AA BLACK SMALL SQUARE` (▪).
    Square,
    /// `decimal` — plain decimal (`1`, `2`, `3`, …).
    Decimal,
    /// `decimal-leading-zero` — zero-padded to at least 2 digits
    /// (`01`, `02`, …, `10`, …). Negative values keep the sign: `-05`.
    DecimalLeadingZero,
    /// `lower-alpha` (alias `lower-latin`) — bijective base-26 over `a-z`
    /// (`a` … `z`, `aa`, `ab`, …).
    LowerAlpha,
    /// `upper-alpha` (alias `upper-latin`) — bijective base-26 over `A-Z`.
    UpperAlpha,
    /// `lower-roman` — additive Roman numerals, lowercase (`i`, `iv`, …).
    /// Range `[1, 3999]`; outside → decimal fallback.
    LowerRoman,
    /// `upper-roman` — additive Roman numerals, uppercase (`I`, `IV`, …).
    /// Range `[1, 3999]`; outside → decimal fallback.
    UpperRoman,
    /// `lower-greek` — bijective base-24 over the lower Greek alphabet
    /// (α β γ … ω, then αα …). Range `≥ 1`; outside → decimal fallback.
    LowerGreek,
}

impl ListStyleType {
    /// Parse a counter-style name (case-insensitive). Accepts the CSS
    /// aliases (`lower-latin`/`upper-latin`); unknown names return `None`
    /// (the host-hook layer then treats it as `decimal`, the § 6.1 fallback
    /// for unknown styles, or surfaces the parse error per its grammar).
    ///
    /// ```
    /// # use vixen_engine::list_marker::ListStyleType;
    /// assert_eq!(
    ///     ListStyleType::parse("Lower-Latin"),
    ///     Some(ListStyleType::LowerAlpha)
    /// );
    /// ```
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "disc" => Some(Self::Disc),
            "circle" => Some(Self::Circle),
            "square" => Some(Self::Square),
            "decimal" => Some(Self::Decimal),
            "decimal-leading-zero" => Some(Self::DecimalLeadingZero),
            "lower-alpha" | "lower-latin" => Some(Self::LowerAlpha),
            "upper-alpha" | "upper-latin" => Some(Self::UpperAlpha),
            "lower-roman" => Some(Self::LowerRoman),
            "upper-roman" => Some(Self::UpperRoman),
            "lower-greek" => Some(Self::LowerGreek),
            _ => None,
        }
    }

    /// The canonical CSS name (round-trips through [`ListStyleType::parse`];
    /// aliases serialise to their canonical form).
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Disc => "disc",
            Self::Circle => "circle",
            Self::Square => "square",
            Self::Decimal => "decimal",
            Self::DecimalLeadingZero => "decimal-leading-zero",
            Self::LowerAlpha => "lower-alpha",
            Self::UpperAlpha => "upper-alpha",
            Self::LowerRoman => "lower-roman",
            Self::UpperRoman => "upper-roman",
            Self::LowerGreek => "lower-greek",
        }
    }

    /// Render `value` as a marker string per the style's § 6.1 algorithm.
    /// Returns `None` for [`ListStyleType::None`] (no marker). Out-of-range
    /// values for additive/alphabetic styles fall back to decimal per the
    /// § 6.1 fallback rule (see module docs).
    ///
    /// ```
    /// # use vixen_engine::list_marker::ListStyleType;
    /// assert_eq!(ListStyleType::Decimal.render(42), Some("42".to_owned()));
    /// assert_eq!(ListStyleType::LowerAlpha.render(28), Some("ab".to_owned()));
    /// assert_eq!(ListStyleType::UpperRoman.render(14), Some("XIV".to_owned()));
    /// assert_eq!(ListStyleType::None.render(1), None);
    /// ```
    pub fn render(self, value: i64) -> Option<String> {
        match self {
            Self::None => None,
            // Bullet glyphs render for any value (CSS2 § 12.6.2: "the marker
            // is one of … regardless of the value").
            Self::Disc => Some("\u{2022}".to_owned()),
            Self::Circle => Some("\u{25E6}".to_owned()),
            Self::Square => Some("\u{25AA}".to_owned()),
            Self::Decimal => Some(render_decimal(value)),
            Self::DecimalLeadingZero => Some(render_decimal_leading_zero(value)),
            Self::LowerAlpha => {
                render_alphabetic(value, ALPHABET_LOWER).or_else(|| Some(render_decimal(value)))
            }
            Self::UpperAlpha => {
                render_alphabetic(value, ALPHABET_UPPER).or_else(|| Some(render_decimal(value)))
            }
            Self::LowerRoman => {
                render_roman(value, ROMAN_TABLE_LOWER).or_else(|| Some(render_decimal(value)))
            }
            Self::UpperRoman => {
                render_roman(value, ROMAN_TABLE_UPPER).or_else(|| Some(render_decimal(value)))
            }
            Self::LowerGreek => {
                render_alphabetic(value, ALPHABET_GREEK).or_else(|| Some(render_decimal(value)))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Numeric (decimal family)
// ---------------------------------------------------------------------------

/// Plain decimal rendering of any `i64` (handles the sign; no padding).
fn render_decimal(value: i64) -> String {
    value.to_string()
}

/// `decimal-leading-zero`: zero-pad the magnitude to ≥ 2 digits; preserve the
/// sign (CSS2 § 12.6.2 + CSS Lists 3 § 6.1.3 "fixed" sign rule).
fn render_decimal_leading_zero(value: i64) -> String {
    let negative = value < 0;
    let magnitude = value.unsigned_abs();
    if magnitude < 10 {
        if negative {
            format!("-0{magnitude}")
        } else {
            format!("0{magnitude}")
        }
    } else if negative {
        format!("-{magnitude}")
    } else {
        magnitude.to_string()
    }
}

// ---------------------------------------------------------------------------
// Alphabetic (bijective base-N)
// ---------------------------------------------------------------------------

/// The 26-letter lower/upper Latin alphabets. Used by `lower-alpha`/
/// `upper-alpha`. 1-indexed bijective base-26: 1→a, 26→z, 27→aa, 28→ab, …
const ALPHABET_LOWER: &str = "abcdefghijklmnopqrstuvwxyz";
const ALPHABET_UPPER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// The 24-letter lower Greek alphabet (final sigma ς excluded — it's a
/// contextual variant, not a letter for ordering). 1-indexed bijective base-24.
/// Used by `lower-greek`.
const ALPHABET_GREEK: &str = "αβγδεζηθικλμνξοπρστυφχψω";

/// Render `value` (`≥ 1`) as a bijective base-N string over the given alphabet
/// (CSS Lists 3 § 6.1.3 "alphabetic"). Returns `None` for `value < 1`
/// (the § 6.1 fallback to decimal).
fn render_alphabetic(value: i64, alphabet: &str) -> Option<String> {
    if value < 1 {
        return None;
    }
    let chars: Vec<char> = alphabet.chars().collect();
    let radix = chars.len() as u64;
    let mut n = value as u64;
    let mut out: Vec<char> = Vec::new();
    while n > 0 {
        n -= 1; // shift to 0-indexed for the modulo
        let idx = (n % radix) as usize;
        out.push(chars[idx]);
        n /= radix;
    }
    out.reverse();
    Some(out.into_iter().collect())
}

// ---------------------------------------------------------------------------
// Additive (Roman numerals)
// ---------------------------------------------------------------------------

/// `(value, symbol)` pairs for lower-roman, descending (the additive table,
/// CSS Lists 3 § 6.1.2). Used greedily left-to-right.
const ROMAN_TABLE_LOWER: &[(u32, &str)] = &[
    (1000, "m"),
    (900, "cm"),
    (500, "d"),
    (400, "cd"),
    (100, "c"),
    (90, "xc"),
    (50, "l"),
    (40, "xl"),
    (10, "x"),
    (9, "ix"),
    (5, "v"),
    (4, "iv"),
    (1, "i"),
];

/// `(value, symbol)` pairs for upper-roman, descending.
const ROMAN_TABLE_UPPER: &[(u32, &str)] = &[
    (1000, "M"),
    (900, "CM"),
    (500, "D"),
    (400, "CD"),
    (100, "C"),
    (90, "XC"),
    (50, "L"),
    (40, "XL"),
    (10, "X"),
    (9, "IX"),
    (5, "V"),
    (4, "IV"),
    (1, "I"),
];

/// The classic additive-Roman upper bound (CSS2 § 12.6.2). Values above this
/// have no compact additive representation and fall back to decimal.
const ROMAN_MAX: i64 = 3999;

/// Render `value` (`[1, 3999]`) as additive Roman numerals over the given
/// table (CSS Lists 3 § 6.1.2 "additive"). Returns `None` outside that range
/// (the § 6.1 fallback to decimal).
fn render_roman(value: i64, table: &[(u32, &str)]) -> Option<String> {
    if !(1..=ROMAN_MAX).contains(&value) {
        return None;
    }
    let mut out = String::new();
    let mut remaining = value as u32;
    for &(unit, sym) in table {
        while remaining >= unit {
            out.push_str(sym);
            remaining -= unit;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse + round-trip --------------------------------------------

    #[test]
    fn parse_canonical_names_case_insensitive() {
        assert_eq!(ListStyleType::parse("disc"), Some(ListStyleType::Disc));
        assert_eq!(ListStyleType::parse("DISC"), Some(ListStyleType::Disc));
        assert_eq!(ListStyleType::parse(" Disc "), Some(ListStyleType::Disc));
    }

    #[test]
    fn parse_latin_aliases_normalise() {
        assert_eq!(
            ListStyleType::parse("lower-latin"),
            Some(ListStyleType::LowerAlpha)
        );
        assert_eq!(
            ListStyleType::parse("upper-latin"),
            Some(ListStyleType::UpperAlpha)
        );
        // And serialise back to the canonical name (not the alias).
        assert_eq!(ListStyleType::LowerAlpha.name(), "lower-alpha");
    }

    #[test]
    fn parse_unknown_is_none() {
        assert_eq!(ListStyleType::parse("cjk-ideographic"), None);
        assert_eq!(ListStyleType::parse("klingon"), None);
        assert_eq!(ListStyleType::parse(""), None);
    }

    #[test]
    fn name_round_trips_through_parse() {
        for &style in &[
            ListStyleType::None,
            ListStyleType::Disc,
            ListStyleType::Circle,
            ListStyleType::Square,
            ListStyleType::Decimal,
            ListStyleType::DecimalLeadingZero,
            ListStyleType::LowerAlpha,
            ListStyleType::UpperAlpha,
            ListStyleType::LowerRoman,
            ListStyleType::UpperRoman,
            ListStyleType::LowerGreek,
        ] {
            assert_eq!(ListStyleType::parse(style.name()), Some(style));
        }
    }

    #[test]
    fn default_is_disc() {
        assert_eq!(ListStyleType::default(), ListStyleType::Disc);
    }

    // --- none -----------------------------------------------------------

    #[test]
    fn none_renders_nothing() {
        assert_eq!(ListStyleType::None.render(1), None);
        assert_eq!(ListStyleType::None.render(-5), None);
    }

    // --- bullets (value-independent) -----------------------------------

    #[test]
    fn bullet_glyphs_are_value_independent() {
        assert_eq!(ListStyleType::Disc.render(1), Some("\u{2022}".to_owned()));
        assert_eq!(ListStyleType::Disc.render(999), Some("\u{2022}".to_owned()));
        assert_eq!(ListStyleType::Circle.render(1), Some("\u{25E6}".to_owned()));
        assert_eq!(ListStyleType::Square.render(1), Some("\u{25AA}".to_owned()));
    }

    // --- decimal family -------------------------------------------------

    #[test]
    fn decimal_renders_signed_value() {
        assert_eq!(ListStyleType::Decimal.render(0), Some("0".to_owned()));
        assert_eq!(ListStyleType::Decimal.render(42), Some("42".to_owned()));
        assert_eq!(ListStyleType::Decimal.render(-7), Some("-7".to_owned()));
    }

    #[test]
    fn decimal_leading_zero_pads_single_digits() {
        assert_eq!(
            ListStyleType::DecimalLeadingZero.render(0),
            Some("00".to_owned())
        );
        assert_eq!(
            ListStyleType::DecimalLeadingZero.render(7),
            Some("07".to_owned())
        );
        // Two+ digits: no extra padding.
        assert_eq!(
            ListStyleType::DecimalLeadingZero.render(10),
            Some("10".to_owned())
        );
        assert_eq!(
            ListStyleType::DecimalLeadingZero.render(123),
            Some("123".to_owned())
        );
    }

    #[test]
    fn decimal_leading_zero_keeps_sign() {
        assert_eq!(
            ListStyleType::DecimalLeadingZero.render(-5),
            Some("-05".to_owned())
        );
        assert_eq!(
            ListStyleType::DecimalLeadingZero.render(-42),
            Some("-42".to_owned())
        );
    }

    // --- alphabetic (latin) --------------------------------------------

    #[test]
    fn lower_alpha_one_cycle() {
        assert_eq!(ListStyleType::LowerAlpha.render(1), Some("a".to_owned()));
        assert_eq!(ListStyleType::LowerAlpha.render(2), Some("b".to_owned()));
        assert_eq!(ListStyleType::LowerAlpha.render(26), Some("z".to_owned()));
        assert_eq!(ListStyleType::LowerAlpha.render(27), Some("aa".to_owned()));
        assert_eq!(ListStyleType::LowerAlpha.render(28), Some("ab".to_owned()));
        assert_eq!(ListStyleType::LowerAlpha.render(52), Some("az".to_owned()));
        assert_eq!(ListStyleType::LowerAlpha.render(53), Some("ba".to_owned()));
        assert_eq!(ListStyleType::LowerAlpha.render(702), Some("zz".to_owned()));
        assert_eq!(
            ListStyleType::LowerAlpha.render(703),
            Some("aaa".to_owned())
        );
    }

    #[test]
    fn upper_alpha_matches_lower() {
        assert_eq!(ListStyleType::UpperAlpha.render(1), Some("A".to_owned()));
        assert_eq!(ListStyleType::UpperAlpha.render(27), Some("AA".to_owned()));
        assert_eq!(
            ListStyleType::UpperAlpha.render(703),
            Some("AAA".to_owned())
        );
    }

    #[test]
    fn alpha_zero_and_negative_fall_back_to_decimal() {
        assert_eq!(ListStyleType::LowerAlpha.render(0), Some("0".to_owned()));
        assert_eq!(ListStyleType::LowerAlpha.render(-3), Some("-3".to_owned()));
        assert_eq!(ListStyleType::UpperAlpha.render(-3), Some("-3".to_owned()));
    }

    // --- greek ----------------------------------------------------------

    #[test]
    fn lower_greek_first_two_cycles() {
        assert_eq!(ListStyleType::LowerGreek.render(1), Some("α".to_owned()));
        assert_eq!(ListStyleType::LowerGreek.render(24), Some("ω".to_owned()));
        assert_eq!(ListStyleType::LowerGreek.render(25), Some("αα".to_owned()));
        assert_eq!(ListStyleType::LowerGreek.render(26), Some("αβ".to_owned()));
    }

    #[test]
    fn greek_zero_falls_back_to_decimal() {
        assert_eq!(ListStyleType::LowerGreek.render(0), Some("0".to_owned()));
    }

    // --- roman (additive) ----------------------------------------------

    #[test]
    fn lower_roman_classic_values() {
        assert_eq!(ListStyleType::LowerRoman.render(1), Some("i".to_owned()));
        assert_eq!(ListStyleType::LowerRoman.render(2), Some("ii".to_owned()));
        assert_eq!(ListStyleType::LowerRoman.render(4), Some("iv".to_owned()));
        assert_eq!(ListStyleType::LowerRoman.render(5), Some("v".to_owned()));
        assert_eq!(ListStyleType::LowerRoman.render(9), Some("ix".to_owned()));
        assert_eq!(ListStyleType::LowerRoman.render(40), Some("xl".to_owned()));
        assert_eq!(ListStyleType::LowerRoman.render(90), Some("xc".to_owned()));
        assert_eq!(ListStyleType::LowerRoman.render(400), Some("cd".to_owned()));
        assert_eq!(ListStyleType::LowerRoman.render(900), Some("cm".to_owned()));
        assert_eq!(
            ListStyleType::LowerRoman.render(49),
            Some("xlix".to_owned())
        );
        assert_eq!(
            ListStyleType::LowerRoman.render(3999),
            Some("mmmcmxcix".to_owned())
        );
    }

    #[test]
    fn upper_roman_matches_lower() {
        assert_eq!(ListStyleType::UpperRoman.render(14), Some("XIV".to_owned()));
        assert_eq!(
            ListStyleType::UpperRoman.render(3999),
            Some("MMMCMXCIX".to_owned())
        );
    }

    #[test]
    fn roman_out_of_range_falls_back_to_decimal() {
        assert_eq!(ListStyleType::LowerRoman.render(0), Some("0".to_owned()));
        assert_eq!(ListStyleType::LowerRoman.render(-1), Some("-1".to_owned()));
        assert_eq!(
            ListStyleType::LowerRoman.render(4000),
            Some("4000".to_owned())
        );
        assert_eq!(
            ListStyleType::UpperRoman.render(4000),
            Some("4000".to_owned())
        );
    }
}
