//! WHATWG HTML § 9.3 — `<meta name="viewport">` parsing (pure logic). The
//! mobile-layout + device-adaptation layer consults this one parser for the
//! `content` attribute every responsive layout resolves against. Complements
//! [`crate::media_query`] (the `@media` surface) and [`crate::length`]
//! (the CSS-px unit the resolved width reduces to).
//!
//! What lives here:
//! - [`ViewportMeta`] — the parsed declaration set: `width`/`height` (a
//!   device-keyword or a CSS-px number), `initial-scale`/`minimum-scale`/
//!   `maximum-scale` (clamped to `[0.1, 10]`), `user-scalable` (yes/no), and
//!   `viewport-fit` (auto/contain/cover).
//! - [`ViewportMeta::parse`] — the § 9.3.2 comma-separated `<name>=<value>`
//!   parser (ASCII-case-insensitive names, leading-numeric-prefix extraction,
//!   unknown properties ignored, the `device-width`/`device-height` keywords).
//!
//! What does *not` live here:
//! - Defaulting + the viewport-size computation (CSS Device Adaptation 1 § 10
//!   resolve — the layout layer applies the § 9.3.6 defaults: `width=980` when
//!   unauthored, the `initial-scale`-derived width, &c.).
//! - Visual viewport (`window.visualViewport`) — a separate API (Phase 6 host
//!   hook).
//! - The `@viewport` CSS at-rule (deferred — browsers ship the meta form).
//!
//! ## Grammar (WHATWG § 9.3.2)
//!
//! ```text
//! content  = decl ( "," decl )*
//! decl     = [ ws ] name [ ws ] [ "=" [ ws ] value ] [ ws ]
//! name     = <ascii-case-insensitive identifier>
//! value    = <leading-number> | "device-width" | "device-height"
//!           | "yes" | "no" | "auto" | "contain" | "cover"
//! ```
//!
//! The numeric extraction is deliberately lenient: the WHATWG algorithm reads
//! the leading numeric prefix and ignores trailing garbage (`"320px"` → `320`,
//! `"1.0,"` → `1.0`), matching the documented browser contract.
//!
//! Reference: <https://html.spec.whatwg.org/multipage/semantics.html#the-meta-element>
//! (§ 9.3 "Processing the `viewport` meta tag").
//! CSS Device Adaptation 1: <https://www.w3.org/TR/css-device-adapt-1/>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

/// A `width`/`height` value: either the `device-width`/`device-height` keyword
/// (resolve to the layout viewport's device dimension) or an explicit CSS-px
/// number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExtentValue {
    /// `device-width` / `device-height` — resolve to the device dimension.
    Device,
    /// An explicit CSS-pixel extent (e.g. `width=480`).
    Number(f64),
}

/// The `viewport-fit` value (CSS Round Display 1 § 4.1): how the web content
/// should be displayed into the display shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ViewportFit {
    /// `auto` — the web content covers the whole display, incl. any non-rect
    /// regions (the default).
    #[default]
    Auto,
    /// `contain` — the web content is displayed within the inscribed rectangle
    /// (safe area).
    Contain,
    /// `cover` — the web content covers the whole display, with content laid
    /// out into the shape (the `env()` insets define the safe area).
    Cover,
}

// ---------------------------------------------------------------------------
// ViewportMeta
// ---------------------------------------------------------------------------

/// The parsed `<meta name="viewport">` declaration set. Fields are `Option`
/// because the parser only captures authored values — the CSS Device
/// Adaptation 1 § 10 defaulting (width=980, initial-scale=fit, user-scalable=
/// yes) is the layout layer's job. `Default` (all-`None`) is the empty
/// declaration set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewportMeta {
    /// `width=…` (default `980` when unauthored; the layout layer applies it).
    pub width: Option<ExtentValue>,
    /// `height=…` (rarely authored; defaults to the aspect-derived value).
    pub height: Option<ExtentValue>,
    /// `initial-scale=…`, clamped to `[0.1, 10]`.
    pub initial_scale: Option<f64>,
    /// `minimum-scale=…`, clamped to `[0.1, 10]`.
    pub minimum_scale: Option<f64>,
    /// `maximum-scale=…`, clamped to `[0.1, 10]`.
    pub maximum_scale: Option<f64>,
    /// `user-scalable=yes|no|0|1`.
    pub user_scalable: Option<bool>,
    /// `viewport-fit=auto|contain|cover`.
    pub viewport_fit: Option<ViewportFit>,
}

impl ViewportMeta {
    /// Parse the `content` attribute of a `<meta name="viewport">` element
    /// per WHATWG § 9.3.2. Comma-separated `<name>=<value>` declarations;
    /// names are ASCII-case-insensitive; numeric values extract the leading
    /// numeric prefix; unknown properties are ignored; `initial-scale`/
    /// `minimum-scale`/`maximum-scale` are clamped to `[0.1, 10]`.
    ///
    /// ```
    /// # use vixen_engine::viewport_meta::{ExtentValue, ViewportMeta};
    /// let m = ViewportMeta::parse("width=device-width, initial-scale=1");
    /// assert_eq!(m.width, Some(ExtentValue::Device));
    /// assert_eq!(m.initial_scale, Some(1.0));
    /// ```
    pub fn parse(content: &str) -> Self {
        let mut meta = Self::default();
        for decl in content.split(',') {
            apply_declaration(&mut meta, decl);
        }
        meta
    }
}

/// Apply one `name=value` declaration to `meta`. Unknown names are ignored;
/// malformed values leave the field at its prior value (None after fresh
/// parse).
fn apply_declaration(meta: &mut ViewportMeta, decl: &str) {
    // Split on the first `=`. A declaration without `=` is a name with no
    // value — ignored (the WHATWG algorithm drops it).
    let Some((name, value)) = decl.split_once('=') else {
        return;
    };
    let name = name.trim().to_ascii_lowercase();
    let value = value.trim();
    if name.is_empty() {
        return;
    }
    match name.as_str() {
        "width" => meta.width = parse_extent(value),
        "height" => meta.height = parse_extent(value),
        "initial-scale" => meta.initial_scale = parse_scale(value),
        "minimum-scale" => meta.minimum_scale = parse_scale(value),
        "maximum-scale" => meta.maximum_scale = parse_scale(value),
        "user-scalable" => meta.user_scalable = parse_user_scalable(value),
        "viewport-fit" => meta.viewport_fit = parse_viewport_fit(value),
        _ => {} // unknown property — ignored per § 9.3.2.
    }
}

/// Parse a `width`/`height` value: `device-width`/`device-height` keyword
/// (case-insensitive) or a leading-number CSS-px extent. `None` for
/// unparseable values.
fn parse_extent(value: &str) -> Option<ExtentValue> {
    let lower = value.to_ascii_lowercase();
    if lower == "device-width" || lower == "device-height" {
        return Some(ExtentValue::Device);
    }
    parse_leading_number(value).map(ExtentValue::Number)
}

/// Parse a scale value: leading-number, clamped to `[0.1, 10]` per the spec's
/// documented browser clamp. `None` for unparseable values.
fn parse_scale(value: &str) -> Option<f64> {
    let n = parse_leading_number(value)?;
    Some(n.clamp(SCALE_MIN, SCALE_MAX))
}

/// Parse `user-scalable`: `yes`/`no` (case-insensitive) and the `1`/`0` aliases.
fn parse_user_scalable(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "1" => Some(true),
        "no" | "0" => Some(false),
        _ => None,
    }
}

/// Parse `viewport-fit`: `auto`/`contain`/`cover` (case-insensitive).
fn parse_viewport_fit(value: &str) -> Option<ViewportFit> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(ViewportFit::Auto),
        "contain" => Some(ViewportFit::Contain),
        "cover" => Some(ViewportFit::Cover),
        _ => None,
    }
}

/// The § 9.3.2 scale clamp bounds. Browsers clamp `initial-scale` &c. to this
/// range so a broken meta tag can't zoom a page out of usable range.
const SCALE_MIN: f64 = 0.1;
const SCALE_MAX: f64 = 10.0;

/// Extract the leading numeric prefix (sign + digits + optional `.` + digits)
/// from `s`, per the WHATWG lenient-number parse. Stops at the first non-
/// numeric byte; trailing garbage is ignored (`"320px"` → `320`, `"1.5e3"` →
/// `1.5`, the `e` ends the number). Returns `None` if there's no leading
/// digit/sign.
fn parse_leading_number(s: &str) -> Option<f64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    // Walk the leading numeric prefix: optional sign, integer digits, optional
    // `.` + fraction digits. Stop at the first non-numeric byte.
    let mut end = 0;
    if bytes[0] == b'+' || bytes[0] == b'-' {
        end = 1;
    }
    let mut saw_digit = false;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
        saw_digit = true;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
            saw_digit = true;
        }
    }
    if !saw_digit {
        return None;
    }
    s[..end].parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- width / height -------------------------------------------------

    #[test]
    fn device_width_keyword() {
        let m = ViewportMeta::parse("width=device-width");
        assert_eq!(m.width, Some(ExtentValue::Device));
    }

    #[test]
    fn explicit_pixel_width() {
        let m = ViewportMeta::parse("width=480");
        assert_eq!(m.width, Some(ExtentValue::Number(480.0)));
    }

    #[test]
    fn width_with_px_suffix_strips_garbage() {
        let m = ViewportMeta::parse("width=320px");
        assert_eq!(m.width, Some(ExtentValue::Number(320.0)));
    }

    #[test]
    fn case_insensitive_names_and_keywords() {
        let m = ViewportMeta::parse("WIDTH = Device-Width");
        assert_eq!(m.width, Some(ExtentValue::Device));
    }

    #[test]
    fn height_device_keyword() {
        let m = ViewportMeta::parse("height=device-height");
        assert_eq!(m.height, Some(ExtentValue::Device));
    }

    #[test]
    fn unparseable_width_is_none() {
        let m = ViewportMeta::parse("width=garbage");
        assert_eq!(m.width, None);
    }

    // --- scales ---------------------------------------------------------

    #[test]
    fn initial_scale_parses() {
        let m = ViewportMeta::parse("initial-scale=1.0");
        assert_eq!(m.initial_scale, Some(1.0));
    }

    #[test]
    fn scales_clamped_to_range() {
        let too_small = ViewportMeta::parse("initial-scale=0.0");
        assert_eq!(too_small.initial_scale, Some(0.1));
        let too_big = ViewportMeta::parse("maximum-scale=20");
        assert_eq!(too_big.maximum_scale, Some(10.0));
    }

    #[test]
    fn fractional_scale_with_trailing_garbage() {
        let m = ViewportMeta::parse("initial-scale=2.5x");
        assert_eq!(m.initial_scale, Some(2.5));
    }

    // --- user-scalable --------------------------------------------------

    #[test]
    fn user_scalable_yes_no_aliases() {
        assert_eq!(
            ViewportMeta::parse("user-scalable=yes").user_scalable,
            Some(true)
        );
        assert_eq!(
            ViewportMeta::parse("user-scalable=NO").user_scalable,
            Some(false)
        );
        assert_eq!(
            ViewportMeta::parse("user-scalable=1").user_scalable,
            Some(true)
        );
        assert_eq!(
            ViewportMeta::parse("user-scalable=0").user_scalable,
            Some(false)
        );
    }

    #[test]
    fn user_scalable_garbage_is_none() {
        assert_eq!(
            ViewportMeta::parse("user-scalable=maybe").user_scalable,
            None
        );
    }

    // --- viewport-fit ---------------------------------------------------

    #[test]
    fn viewport_fit_values() {
        assert_eq!(
            ViewportMeta::parse("viewport-fit=cover").viewport_fit,
            Some(ViewportFit::Cover)
        );
        assert_eq!(
            ViewportMeta::parse("viewport-fit=contain").viewport_fit,
            Some(ViewportFit::Contain)
        );
        assert_eq!(
            ViewportMeta::parse("viewport-fit=auto").viewport_fit,
            Some(ViewportFit::Auto)
        );
        assert_eq!(ViewportMeta::parse("viewport-fit=round").viewport_fit, None);
    }

    // --- full declarations ---------------------------------------------

    #[test]
    fn typical_mobile_declaration() {
        let m = ViewportMeta::parse("width=device-width, initial-scale=1, maximum-scale=1");
        assert_eq!(m.width, Some(ExtentValue::Device));
        assert_eq!(m.initial_scale, Some(1.0));
        assert_eq!(m.maximum_scale, Some(1.0));
    }

    #[test]
    fn unknown_property_ignored() {
        let m = ViewportMeta::parse("width=device-width, foo=bar, initial-scale=2");
        assert_eq!(m.width, Some(ExtentValue::Device));
        assert_eq!(m.initial_scale, Some(2.0));
    }

    #[test]
    fn name_without_value_ignored() {
        let m = ViewportMeta::parse("width=device-width, just-a-name");
        assert_eq!(m.width, Some(ExtentValue::Device));
    }

    #[test]
    fn empty_content_yields_default() {
        let m = ViewportMeta::parse("");
        assert_eq!(m, ViewportMeta::default());
    }

    #[test]
    fn whitespace_tolerant() {
        let m = ViewportMeta::parse("  width  =  device-width  , initial-scale = 1 ");
        assert_eq!(m.width, Some(ExtentValue::Device));
        assert_eq!(m.initial_scale, Some(1.0));
    }

    #[test]
    fn last_value_wins_on_duplicate() {
        // Duplicate width: the last declaration wins (matches browser
        // behaviour — the parser overwrites on each occurrence).
        let m = ViewportMeta::parse("width=320, width=device-width");
        assert_eq!(m.width, Some(ExtentValue::Device));
    }

    #[test]
    fn default_is_all_none() {
        let m = ViewportMeta::default();
        assert!(m.width.is_none());
        assert!(m.height.is_none());
        assert!(m.initial_scale.is_none());
        assert!(m.minimum_scale.is_none());
        assert!(m.maximum_scale.is_none());
        assert!(m.user_scalable.is_none());
        assert!(m.viewport_fit.is_none());
    }

    #[test]
    fn viewport_fit_default_is_auto() {
        assert_eq!(ViewportFit::default(), ViewportFit::Auto);
    }
}
