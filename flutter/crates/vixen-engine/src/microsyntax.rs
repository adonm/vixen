//! WHATWG HTML § 2.4 "Common parser idioms" — the attribute-value
//! microsyntax parsers that recur across the HTML spec (`width="100"`,
//! `colspan="3"`, `tabindex="-1"`, `<input value="1.5">`, `<area coords>`,
//! …). Phase 6 DOM host-bindings prep, pure logic called out by `docs/PLAN.md`
//! "Testing strategy" as a Rust-unit-test surface.
//!
//! The HTML attribute-value parsers are deliberately *lenient*: they skip
//! leading ASCII whitespace, accept a leading `+`/`-`, extract the leading
//! numeric prefix, and ignore trailing content (`"100px"` → `100`,
//! `"3.14abc"` → `3.14`). This is the historical behaviour every browser
//! implements; matching it exactly is the whole point of the module. The
//! stricter value-sanitisation (e.g. `<input type=number>` rejecting
//! `"3.14abc"`) layers a trailing-garbage check *on top* of these primitives.
//!
//! What lives here:
//! - [`parse_signed_integer`] — § 2.4.4 "rules for parsing integers".
//! - [`parse_non_negative_integer`] — § 2.4.3 (signed parse + reject negative).
//! - [`parse_float`] — § 2.4.5 "rules for parsing floating-point number values"
//!   (the lenient prefix extractor; returns `None` on no digits).
//! - [`parse_dimension_value`] — § 2.4.6 "rules for parsing dimension values"
//!   (the legacy `<td width>` / `<img width>` / `<table height>` surface,
//!   producing either a pixel length or a percentage).
//! - [`parse_list_of_integers`] — the comma-separated list parser
//!   (`<area coords>`, `<input>` list surfaces).
//!
//! What does *not* live here:
//! - The reflection glue (the host-hook layer reads the attribute string and
//!   hands it here; this module is the pure value it reduces to).
//! - CSS length parsing ([`crate::length`] owns the unit-bearing CSS grammar;
//!   this module's dimension parser is the *legacy* `%`-or-nothing surface).
//!
//! Reference: <https://html.spec.whatwg.org/multipage/common-microsyntaxes.html>.

#![forbid(unsafe_code)]

/// ASCII whitespace per WHATWG § 1.2 ("ASCII whitespace" = space, tab, LF, CR,
/// FF — *not* the Unicode White_Space set).
const ASCII_WHITESPACE: &[char] = &[' ', '\t', '\n', '\r', '\x0c'];

// ---------------------------------------------------------------------------
// § 2.4.4 Rules for parsing integers (signed)
// ---------------------------------------------------------------------------

/// WHATWG § 2.4.4 "rules for parsing integers". Skips leading ASCII
/// whitespace, accepts a single leading `+`/`-`, collects the leading run of
/// ASCII digits, and returns the signed `i64`. Trailing content is ignored
/// (`"-12abc"` → `-12`). Returns `None` if no digit was found.
///
/// The integer is parsed as `i64`; out-of-`i64`-range values saturate to
/// `i64::MIN`/`i64::MAX` (Rust's `i64::from_str_radix` would reject them; the
/// WHATWG algorithm leaves overflow "implementation-defined", and saturating
/// matches the `value sanitization` behaviour browsers settle on).
pub fn parse_signed_integer(input: &str) -> Option<i64> {
    let bytes = skip_ascii_whitespace(input);
    parse_signed_integer_bytes(bytes)
}

/// WHATWG § 2.4.3 "rules for parsing non-negative integers". Runs the signed
/// integer parse and rejects a negative result. Note `-0` round-trips to `0`
/// (not negative), so it parses as `Some(0)` — the non-negative surface does
/// not reject the leading sign of an otherwise-zero value, matching the spec's
/// "parse signed, then reject if < 0" reduction.
pub fn parse_non_negative_integer(input: &str) -> Option<u64> {
    let v = parse_signed_integer(input)?;
    if v < 0 {
        return None;
    }
    Some(v as u64)
}

fn parse_signed_integer_bytes(bytes: &[u8]) -> Option<i64> {
    let mut i = 0;
    let mut sign: i64 = 1;
    if matches!(bytes.first(), Some(b'-')) {
        sign = -1;
        i += 1;
    } else if matches!(bytes.first(), Some(b'+')) {
        i += 1;
    }
    let digits_start = i;
    while let Some(&b) = bytes.get(i) {
        if b.is_ascii_digit() {
            i += 1;
        } else {
            break;
        }
    }
    if i == digits_start {
        return None;
    }
    // Saturating accumulation in u64 so out-of-range magnitudes clamp instead
    // of rejecting (the WHATWG algorithm leaves overflow "implementation-
    // defined"; saturating matches the value-sanitization behaviour browsers
    // settle on and never panics on huge inputs).
    let mut magnitude: u64 = 0;
    for &b in &bytes[digits_start..i] {
        let d = (b - b'0') as u64;
        match magnitude.checked_mul(10).and_then(|m| m.checked_add(d)) {
            Some(m) => magnitude = m,
            // Overflowed u64: clamp the magnitude so the sign branch below
            // saturates to i64::MIN/i64::MAX. The remaining digits are
            // irrelevant under saturation.
            None => {
                magnitude = u64::MAX;
                break;
            }
        }
    }
    if sign < 0 {
        // |i64::MIN| = 2^63, which is > i64::MAX, so any magnitude ≥ 2^63
        // saturates to i64::MIN; 2^63 exactly *is* i64::MIN.
        if magnitude as u128 >= 1u128 << 63 {
            Some(i64::MIN)
        } else {
            Some(-(magnitude as i64))
        }
    } else if magnitude > i64::MAX as u64 {
        Some(i64::MAX)
    } else {
        Some(magnitude as i64)
    }
}

// ---------------------------------------------------------------------------
// § 2.4.5 Rules for parsing floating-point number values
// ---------------------------------------------------------------------------

/// WHATWG § 2.4.5 "rules for parsing floating-point number values". The
/// lenient prefix extractor: skip leading ASCII whitespace, optional `+`/`-`,
/// integer digits, optional `.` + fraction digits, optional (`e`|`E`) +
/// optional sign + exponent digits. Parsing stops at the first non-numeric
/// code point and trailing content is ignored (`"100px"` → `100.0`,
/// `"3.14abc"` → `3.14`). Returns `None` if no digit was found.
///
/// Implemented as the spec's `value`/`divisor`/`exponent` running
/// accumulation so the documented invariants (e.g. `"-0"` → `-0.0`,
/// `"1.5e3"` → `1500.0`) hold; the final result is re-derived from the
/// collected substrings via Rust's `f64::from_str` to keep full IEEE-754
/// precision, then re-signed.
pub fn parse_float(input: &str) -> Option<f64> {
    let bytes = skip_ascii_whitespace(input);

    let mut i = 0;
    let mut sign = 1.0;

    if matches!(bytes.first(), Some(b'-')) {
        sign = -1.0;
        i += 1;
    } else if matches!(bytes.first(), Some(b'+')) {
        i += 1;
    }

    // Integer part digits.
    let int_start = i;
    while let Some(&b) = bytes.get(i) {
        if b.is_ascii_digit() {
            i += 1;
        } else {
            break;
        }
    }
    let int_digits = &bytes[int_start..i];

    // Fraction part digits (only if a leading '.').
    let mut frac_digits: &[u8] = &[];
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        let frac_start = i;
        while let Some(&b) = bytes.get(i) {
            if b.is_ascii_digit() {
                i += 1;
            } else {
                break;
            }
        }
        frac_digits = &bytes[frac_start..i];
    }

    if int_digits.is_empty() && frac_digits.is_empty() {
        return None;
    }

    // Exponent (e/E + optional sign + digits). Only honoured if at least one
    // exponent digit follows; otherwise parsing stops and the e/E is left as
    // trailing (matching the spec's "no exponent collected" branch).
    let mut exponent: i64 = 0;
    if matches!(bytes.get(i), Some(b'e') | Some(b'E')) {
        let mut j = i + 1;
        let mut esign: i64 = 1;
        if matches!(bytes.get(j), Some(b'-')) {
            esign = -1;
            j += 1;
        } else if matches!(bytes.get(j), Some(b'+')) {
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
            let exp_str = std::str::from_utf8(&bytes[exp_start..j]).ok()?;
            let exp_mag: u64 = exp_str.parse().ok()?;
            // Cap the exponent to avoid overflowing f64 (parsing "1e999"
            // should yield Infinity, not panic).
            exponent = esign.saturating_mul(exp_mag.min(i64::MAX as u64) as i64);
            i = j;
        }
    }

    // Reconstruct the canonical float string and let Rust parse it. This
    // preserves full precision and the spec's infinity handling uniformly.
    let mut canonical = String::with_capacity(i + 4);
    if sign < 0.0 {
        canonical.push('-');
    }
    if int_digits.is_empty() {
        canonical.push('0');
    } else {
        canonical.push_str(std::str::from_utf8(int_digits).ok()?);
    }
    if !frac_digits.is_empty() {
        canonical.push('.');
        canonical.push_str(std::str::from_utf8(frac_digits).ok()?);
    }
    if exponent != 0 {
        canonical.push('e');
        canonical.push_str(&exponent.to_string());
    }
    let _ = i; // position past the consumed prefix; trailing content ignored.
    canonical.parse::<f64>().ok()
}

// ---------------------------------------------------------------------------
// § 2.4.6 Rules for parsing dimension values
// ---------------------------------------------------------------------------

/// The kind of dimension value the legacy attribute parser produced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DimensionKind {
    /// A pixel length (`width="100"` → `100px`). The value is the raw float;
    /// the host-hook layer treats it as CSS pixels.
    Length,
    /// A percentage (`width="50%"`). The value is the percentage numerator.
    Percentage,
}

/// A WHATWG § 2.4.6 dimension value — the legacy `<td width>` / `<img width>`
/// / `<table height>` / `<hr width>` surface. Either a pixel length or a
/// percentage; never both (the grammar is `number | number%`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DimensionValue {
    pub value: f64,
    pub kind: DimensionKind,
}

impl DimensionValue {
    pub const fn length(value: f64) -> Self {
        Self {
            value,
            kind: DimensionKind::Length,
        }
    }
    pub const fn percentage(value: f64) -> Self {
        Self {
            value,
            kind: DimensionKind::Percentage,
        }
    }
}

/// WHATWG § 2.4.6 "rules for parsing dimension values". Collects the leading
/// run of (digit | `.` | `+` | `-`) characters, parses it as a float, then
/// (per the spec's step 8) skips ASCII whitespace and accepts an optional `%`
/// (which must be followed only by trailing ASCII whitespace). Unlike
/// [`parse_float`], any other trailing content is an error — that is the
/// spec's contract for the dimension surface.
pub fn parse_dimension_value(input: &str) -> Option<DimensionValue> {
    let bytes = skip_ascii_whitespace(input);
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b.is_ascii_digit() || b == b'.' || b == b'+' || b == b'-' {
            i += 1;
        } else {
            break;
        }
    }
    if i == 0 {
        return None;
    }
    let number_str = std::str::from_utf8(&bytes[..i]).ok()?;
    let value = parse_float(number_str)?;
    if value < 0.0 {
        return None;
    }
    // § 2.4.6 step 8: skip ASCII whitespace after the number.
    let mut j = i;
    while matches!(
        bytes.get(j),
        Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') | Some(b'\x0c')
    ) {
        j += 1;
    }
    // Step 9: end ⇒ length.
    if j == bytes.len() {
        return Some(DimensionValue::length(value));
    }
    // Step 10: '%' ⇒ percentage, then only trailing ASCII whitespace allowed.
    if bytes[j] == b'%' {
        let mut k = j + 1;
        while matches!(
            bytes.get(k),
            Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') | Some(b'\x0c')
        ) {
            k += 1;
        }
        if k == bytes.len() {
            return Some(DimensionValue::percentage(value));
        }
        return None; // trailing garbage after %
    }
    None // trailing garbage (non-%, non-whitespace) after the number
}

// ---------------------------------------------------------------------------
// Lists of integers (WHATWG § 2.4.9-ish; used by <area coords> etc.)
// ---------------------------------------------------------------------------

/// Parse a WHATWG "list of integers": comma- and/or whitespace-separated runs,
/// each run parsed with [`parse_signed_integer`]. Empty runs contribute
/// nothing. Used by `<area coords>`, `<input type=email list>`-style surfaces,
/// and the `cols`/`rows` list forms. Returns the collected integers in order;
/// an unparseable run is skipped (the spec's tolerant behaviour — the whole
/// list is not rejected for one bad entry).
pub fn parse_list_of_integers(input: &str) -> Vec<i64> {
    let mut out = Vec::new();
    for segment in input.split([',', '\n', '\r']) {
        // Within a comma-segment, the integer parser skips leading whitespace;
        // but multiple integers inside one comma-segment (space-separated)
        // should each be picked up. Walk the segment for integer prefixes.
        let bytes = segment.as_bytes();
        let mut i = 0;
        // Skip leading ASCII whitespace within the segment.
        while matches!(bytes.get(i), Some(b' ') | Some(b'\t') | Some(b'\x0c')) {
            i += 1;
        }
        while i < bytes.len() {
            let rest = &bytes[i..];
            // Skip separators / whitespace between integers in this segment.
            if matches!(rest.first(), Some(b' ') | Some(b'\t') | Some(b'\x0c')) {
                i += 1;
                continue;
            }
            let slice = std::str::from_utf8(rest).unwrap_or("");
            match parse_signed_integer(slice) {
                Some(v) => {
                    // Advance past the consumed integer prefix.
                    let consumed = consumed_integer_prefix(slice);
                    out.push(v);
                    i += consumed;
                }
                None => {
                    // No integer here; skip one code point and continue.
                    i += 1;
                }
            }
        }
    }
    out
}

/// How many bytes [`parse_signed_integer`] (effectively) consumes from the
/// front of `s`: leading sign (optional) + the digit run.
fn consumed_integer_prefix(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    if matches!(bytes.first(), Some(b'-') | Some(b'+')) {
        i += 1;
    }
    while let Some(&b) = bytes.get(i) {
        if b.is_ascii_digit() {
            i += 1;
        } else {
            break;
        }
    }
    i
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Skip ASCII whitespace from the front of `input`, returning the remaining
/// byte slice. WHATWG § 2.4.x parsers all begin with "skip ASCII whitespace".
fn skip_ascii_whitespace(input: &str) -> &[u8] {
    let bytes = input.as_bytes();
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'\x0c' {
            i += 1;
        } else {
            break;
        }
    }
    &bytes[i..]
}

// Keep the named constant referenced; it documents the WHATWG "ASCII
// whitespace" set for readers grepping for it. Re-exported as a private item
// so the module is self-documenting even where individual parsers inline the
// byte checks for speed.
#[allow(dead_code)]
const _: &[char] = ASCII_WHITESPACE;

#[cfg(test)]
mod tests {
    use super::*;

    // --- § 2.4.4 signed integers ---------------------------------------

    #[test]
    fn signed_integer_basic() {
        assert_eq!(parse_signed_integer("123"), Some(123));
        assert_eq!(parse_signed_integer("-123"), Some(-123));
        assert_eq!(parse_signed_integer("+123"), Some(123));
    }

    #[test]
    fn signed_integer_skips_leading_whitespace() {
        assert_eq!(parse_signed_integer("  \t 42"), Some(42));
        assert_eq!(parse_signed_integer("\n-7"), Some(-7));
    }

    #[test]
    fn signed_integer_ignores_trailing() {
        // Lenient: trailing content is dropped (the HTML contract).
        assert_eq!(parse_signed_integer("12abc"), Some(12));
        assert_eq!(parse_signed_integer("12 "), Some(12));
    }

    #[test]
    fn signed_integer_min_max_edges() {
        assert_eq!(parse_signed_integer("0"), Some(0));
        assert_eq!(parse_signed_integer("-0"), Some(0));
        // Saturation on overflow rather than rejection.
        assert_eq!(parse_signed_integer(&"9".repeat(40)), Some(i64::MAX));
        assert_eq!(
            parse_signed_integer(&format!("-{}", "9".repeat(40))),
            Some(i64::MIN)
        );
    }

    #[test]
    fn signed_integer_rejects_no_digits() {
        assert_eq!(parse_signed_integer(""), None);
        assert_eq!(parse_signed_integer("   "), None);
        assert_eq!(parse_signed_integer("abc"), None);
        assert_eq!(parse_signed_integer("-"), None);
        assert_eq!(parse_signed_integer("+"), None);
        // A second sign is not consumed as a digit ⇒ no digits ⇒ None.
        assert_eq!(parse_signed_integer("--5"), None);
        assert_eq!(parse_signed_integer("++5"), None);
    }

    // --- § 2.4.3 non-negative integers ---------------------------------

    #[test]
    fn non_negative_basic() {
        assert_eq!(parse_non_negative_integer("3"), Some(3));
        assert_eq!(parse_non_negative_integer("0"), Some(0));
        assert_eq!(parse_non_negative_integer("+5"), Some(5));
        assert_eq!(parse_non_negative_integer("  100  "), Some(100));
    }

    #[test]
    fn non_negative_rejects_negative() {
        // A leading '-' followed by digits is negative (not zero), so it's
        // rejected; "-0" parses as 0 (the sign of an otherwise-zero value
        // doesn't make it negative).
        assert_eq!(parse_non_negative_integer("-1"), None);
        assert_eq!(parse_non_negative_integer("-0"), Some(0));
    }

    #[test]
    fn non_negative_rejects_garbage() {
        assert_eq!(parse_non_negative_integer("abc"), None);
        assert_eq!(parse_non_negative_integer(""), None);
    }

    // colspan="3" sanity check — the canonical use.
    #[test]
    fn non_negative_colspan_form() {
        assert_eq!(parse_non_negative_integer("3"), Some(3));
    }

    // --- § 2.4.5 floating point ----------------------------------------

    #[test]
    fn float_integer_form() {
        assert_eq!(parse_float("100"), Some(100.0));
        assert_eq!(parse_float("-3"), Some(-3.0));
        assert_eq!(parse_float("+3"), Some(3.0));
    }

    #[test]
    fn float_fraction_form() {
        assert!((parse_float("2.5").unwrap() - 2.5).abs() < 1e-9);
        assert!((parse_float(".5").unwrap() - 0.5).abs() < 1e-9);
        assert!((parse_float("-0.5").unwrap() - (-0.5)).abs() < 1e-9);
        assert!((parse_float("3.").unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn float_exponent_form() {
        assert!((parse_float("1e3").unwrap() - 1000.0).abs() < 1e-9);
        assert!((parse_float("1.5e3").unwrap() - 1500.0).abs() < 1e-9);
        assert!((parse_float("1E-2").unwrap() - 0.01).abs() < 1e-9);
        assert!((parse_float("2e+2").unwrap() - 200.0).abs() < 1e-9);
    }

    #[test]
    fn float_exponent_without_digits_is_not_an_exponent() {
        // "3e" — no exponent digits ⇒ parsing stops at '3', 'e' is trailing.
        assert!((parse_float("3e").unwrap() - 3.0).abs() < 1e-9);
        assert!((parse_float("3e+").unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn float_lenient_trailing() {
        // The HTML contract: trailing content ignored.
        assert!((parse_float("100px").unwrap() - 100.0).abs() < 1e-9);
        assert!((parse_float("2.5abc").unwrap() - 2.5).abs() < 1e-9);
        assert!((parse_float("1.5.5").unwrap() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn float_skips_leading_whitespace() {
        assert!((parse_float("  \t1.5").unwrap() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn float_rejects_no_digits() {
        assert_eq!(parse_float(""), None);
        assert_eq!(parse_float("   "), None);
        assert_eq!(parse_float("abc"), None);
        assert_eq!(parse_float("-"), None);
        assert_eq!(parse_float("+"), None);
        assert_eq!(parse_float("."), None); // no digits at all
        assert_eq!(parse_float("-.e1"), None);
    }

    #[test]
    fn float_huge_exponent_is_infinity() {
        // "1e999" → +∞ (no panic).
        assert!(parse_float("1e999").unwrap().is_infinite());
        assert!(parse_float("1e999").unwrap().is_sign_positive());
    }

    #[test]
    fn float_negative_zero() {
        let z = parse_float("-0").unwrap();
        assert!(z == 0.0);
        assert!(z.is_sign_negative());
    }

    // --- § 2.4.6 dimension values --------------------------------------

    #[test]
    fn dimension_length() {
        assert_eq!(
            parse_dimension_value("100"),
            Some(DimensionValue::length(100.0))
        );
        // § 2.4.6 step 8: trailing ASCII whitespace is tolerated.
        assert_eq!(
            parse_dimension_value("  42  "),
            Some(DimensionValue::length(42.0))
        );
    }

    #[test]
    fn dimension_percentage() {
        assert_eq!(
            parse_dimension_value("50%"),
            Some(DimensionValue::percentage(50.0))
        );
        assert_eq!(
            parse_dimension_value("100.5%"),
            Some(DimensionValue::percentage(100.5))
        );
        // The spec skips ASCII whitespace before and after the %.
        assert_eq!(
            parse_dimension_value("50 % "),
            Some(DimensionValue::percentage(50.0))
        );
    }

    #[test]
    fn dimension_rejects_trailing_garbage() {
        // Unlike parse_float, the dimension surface rejects non-% trailing.
        assert_eq!(parse_dimension_value("100px"), None);
        assert_eq!(parse_dimension_value("50%x"), None);
        assert_eq!(parse_dimension_value("100 % x"), None);
        assert_eq!(parse_dimension_value("50% 50"), None);
    }

    #[test]
    fn dimension_rejects_negative_and_empty() {
        assert_eq!(parse_dimension_value("-5"), None);
        assert_eq!(parse_dimension_value(""), None);
        assert_eq!(parse_dimension_value("%"), None);
    }

    // --- lists of integers ---------------------------------------------

    #[test]
    fn list_of_integers_comma_separated() {
        // <area coords="0,0,100,50">
        assert_eq!(parse_list_of_integers("0,0,100,50"), vec![0, 0, 100, 50]);
    }

    #[test]
    fn list_of_integers_whitespace_runs() {
        assert_eq!(parse_list_of_integers("1 2 3"), vec![1, 2, 3]);
        assert_eq!(parse_list_of_integers("  1   2 3  "), vec![1, 2, 3]);
    }

    #[test]
    fn list_of_integers_signed_and_mixed_separators() {
        assert_eq!(parse_list_of_integers("1, -2, 3"), vec![1, -2, 3]);
        assert_eq!(parse_list_of_integers("1,2 3,4"), vec![1, 2, 3, 4]);
    }

    #[test]
    fn list_of_integers_tolerates_bad_entries() {
        // The whole list isn't rejected for one bad entry.
        assert_eq!(parse_list_of_integers("1, abc, 3"), vec![1, 3]);
        assert_eq!(parse_list_of_integers(",,,"), Vec::<i64>::new());
        assert_eq!(parse_list_of_integers(""), Vec::<i64>::new());
    }
}
