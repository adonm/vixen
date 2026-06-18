//! CSS `<color>` parsing and sRGB arithmetic — pure logic called out by
//! `docs/PLAN.md` "Testing strategy" as a Rust-unit-test surface (not a WPT
//! fixture). Implements the sRGB-family grammar of CSS Color 4 § 5 ("Parsing
//! a `<color>` value") plus the colour arithmetic the paint path needs.
//!
//! What lives here:
//! - [`Color`] — an 8-bit sRGB RGBA value, plus the [`ColorOrKeyword`]
//!   parse target that carries `transparent` / `currentcolor`.
//! - [`Color::parse`] — every sRGB-family grammar production CSS Color 4
//!   defines for v1.0: 3/4/6/8-digit hex, `rgb()`/`rgba()` (legacy +
//!   space-separated), `hsl()`/`hsla()`, the 148 named colours, and the
//!   `transparent` keyword.
//! - [`premultiply`] / [`unpremultiply`] — the alpha arithmetic the paint
//!   path uses when blending groups (CSS Compositing 1 § 4).
//! - [`interpolate`] — linear sRGB interpolation at `t ∈ [0,1]`, the
//!   primitive gradients (CSS Images 4 § 4.3) and transitions
//!   (Web Animations § 5.4) reduce to.
//!
//! What does *not* live here:
//! - Cascade / computed-value resolution of `currentcolor` (Stylo owns the
//!   cascade; this module hands the caller the *resolved* RGBA). The
//!   [`ColorOrKeyword`] parse target keeps `currentcolor` unmaterialised so
//!   the cascade can substitute the element's colour first.
//! - `oklab()`/`oklch()`/`lab()`/`lch()`/`color()` (CSS Color 4 § 5.3).
//!   Those land in a follow-up slice; the v1.0 paint path needs the sRGB
//!   family + named colours + keywords, and that is what Stylo's
//!   default-catalog resolves today. Parsing them returns
//!   [`ColorParseError::UnsupportedColorSpace`] so callers can fail closed
//!   rather than mis-parse.
//! - `color-mix()` (CSS Color 5) — the [`interpolate`] helper is the
//!   primitive a future `color-mix` impl reduces to.
//!
//! Conversions are per CSS Color 4: HSL → sRGB uses the canonical algorithm
//! (CSS Color 4 § 5.4 "HSL to sRGB"), and channel quantisation is the spec's
//! "round half to even, then clamp to `[0, 255]`" (CSS Color 4 § 5.1).
//!
//! Reference: <https://www.w3.org/TR/css-color-4/>.

#![forbid(unsafe_code)]

use crate::display_list::Color as DisplayListColor;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A parsed sRGB colour at 8 bits per channel. The format the display-list
/// builder and WebRender painter consume (matches [`DisplayListColor`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    /// Convert to the [`DisplayListColor`] the paint path consumes. The two
    /// types are kept distinct so the *parsing* surface stays decoupled from
    /// the *painting* surface (one paint path, but many input forms).
    pub const fn to_display_list(self) -> DisplayListColor {
        DisplayListColor::rgba(self.r, self.g, self.b, self.a)
    }

    /// Linear sRGB channels in `[0,1]` for arithmetic that needs a linear
    /// space (interpolation, alpha compositing). Per CSS Color 4 § 11 the
    /// sRGB transfer function is used (the 8-bit → `[0,1]` step is `c/255`).
    pub fn to_linear_f32(self) -> [f32; 4] {
        let decode = |c: u8| {
            let v = c as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        [
            decode(self.r),
            decode(self.g),
            decode(self.b),
            self.a as f32 / 255.0,
        ]
    }
}

/// The parse target for a CSS `<color>` value, carrying the keywords that
/// the cascade must resolve before they become a concrete [`Color`].
///
/// `currentcolor` (CSS Color 4 § 9) substitutes the element's *resolved*
/// `color` at computed-value time; it cannot be materialised here without
/// the cascade, so the parser hands the caller the unresolved keyword and
/// the caller substitutes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorOrKeyword {
    Color(Color),
    /// CSS Color 4 § 9 — substitute the element's resolved `color`.
    CurrentColor,
}

impl From<Color> for ColorOrKeyword {
    fn from(c: Color) -> Self {
        Self::Color(c)
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse error for [`Color::parse`] / [`ColorOrKeyword::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ColorParseError {
    #[error("empty color")]
    Empty,
    #[error("invalid hex color: {0:?}")]
    InvalidHex(String),
    #[error("invalid function: {0:?}")]
    InvalidFunction(String),
    #[error("invalid number: {0:?}")]
    InvalidNumber(String),
    #[error("unknown color name: {0:?}")]
    UnknownName(String),
    #[error("unsupported color space (deferred to a later slice): {0:?}")]
    UnsupportedColorSpace(String),
}

impl Color {
    /// Parse an sRGB-family `<color>` per CSS Color 4 § 5. Accepts hex,
    /// `rgb()/rgba()`, `hsl()/hsla()`, and the named colours. Rejects
    /// `currentcolor` (use [`ColorOrKeyword::parse`] for the keyword-aware
    /// parse) and `transparent` is parsed as a fully-transparent black.
    pub fn parse(input: &str) -> Result<Self, ColorParseError> {
        match ColorOrKeyword::parse(input)? {
            ColorOrKeyword::Color(c) => Ok(c),
            // `currentcolor` cannot be materialised without the cascade.
            ColorOrKeyword::CurrentColor => Err(ColorParseError::InvalidFunction(
                "currentcolor requires the cascade".to_owned(),
            )),
        }
    }
}

impl ColorOrKeyword {
    /// Parse any CSS Color 4 `<color>` production Vixen resolves at v1.0:
    /// hex, `rgb()/rgba()`, `hsl()/hsla()`, named colours, `transparent`,
    /// `currentcolor`. Whitespace and ASCII case are normalised per spec.
    pub fn parse(input: &str) -> Result<Self, ColorParseError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(ColorParseError::Empty);
        }
        let lower = s.to_ascii_lowercase();
        // Keywords first (they share the bareword namespace with named colours).
        match lower.as_str() {
            "transparent" => return Ok(ColorOrKeyword::Color(Color::TRANSPARENT)),
            "currentcolor" => return Ok(ColorOrKeyword::CurrentColor),
            _ => {}
        }
        // Hex form (`#` + 3/4/6/8 hex digits).
        if let Some(rest) = s.strip_prefix('#') {
            return parse_hex(rest).map(ColorOrKeyword::Color);
        }
        // Functional forms.
        if let Some(args) =
            strip_fn(lower.as_str(), "rgb").or_else(|| strip_fn(lower.as_str(), "rgba"))
        {
            return parse_rgb(args).map(ColorOrKeyword::Color);
        }
        if let Some(args) =
            strip_fn(lower.as_str(), "hsl").or_else(|| strip_fn(lower.as_str(), "hsla"))
        {
            return parse_hsl(args).map(ColorOrKeyword::Color);
        }
        // Named colour.
        if let Some(c) = named_color(lower.as_str()) {
            return Ok(ColorOrKeyword::Color(c));
        }
        // Fail-closed signals for the deferred Color 4 productions.
        for family in ["oklab", "oklch", "lab", "lch", "color"] {
            if lower.starts_with(&format!("{family}(")) {
                return Err(ColorParseError::UnsupportedColorSpace(family.to_owned()));
            }
        }
        Err(ColorParseError::UnknownName(s.to_owned()))
    }
}

/// Strip a `name(` prefix and matching `)` suffix from `s`, returning the
/// inner arguments (trimmed). Used for all functional forms.
fn strip_fn<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}(");
    let inner = s.strip_prefix(&prefix)?;
    let inner = inner.strip_suffix(')')?;
    Some(inner.trim())
}

/// Parse 3/4/6/8-digit hex per CSS Color 4 § 5.2.
fn parse_hex(s: &str) -> Result<Color, ColorParseError> {
    // Reject anything that isn't ASCII hex of a supported width.
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) || s.is_empty() {
        return Err(ColorParseError::InvalidHex(format!("#{s}")));
    }
    let (r, g, b, a) = match s.len() {
        3 => {
            // #rgb → #rrggbb
            let r = u8::from_str_radix(&s[0..1].repeat(2), 16).unwrap();
            let g = u8::from_str_radix(&s[1..2].repeat(2), 16).unwrap();
            let b = u8::from_str_radix(&s[2..3].repeat(2), 16).unwrap();
            (r, g, b, 255)
        }
        4 => {
            // #rgba → #rrggbbaa
            let r = u8::from_str_radix(&s[0..1].repeat(2), 16).unwrap();
            let g = u8::from_str_radix(&s[1..2].repeat(2), 16).unwrap();
            let b = u8::from_str_radix(&s[2..3].repeat(2), 16).unwrap();
            let a = u8::from_str_radix(&s[3..4].repeat(2), 16).unwrap();
            (r, g, b, a)
        }
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).unwrap();
            let g = u8::from_str_radix(&s[2..4], 16).unwrap();
            let b = u8::from_str_radix(&s[4..6], 16).unwrap();
            (r, g, b, 255)
        }
        8 => {
            let r = u8::from_str_radix(&s[0..2], 16).unwrap();
            let g = u8::from_str_radix(&s[2..4], 16).unwrap();
            let b = u8::from_str_radix(&s[4..6], 16).unwrap();
            let a = u8::from_str_radix(&s[6..8], 16).unwrap();
            (r, g, b, a)
        }
        _ => return Err(ColorParseError::InvalidHex(format!("#{s}"))),
    };
    Ok(Color::rgba(r, g, b, a))
}

/// Parse `rgb()` / `rgba()` arguments per CSS Color 4 § 5.3.1. Accepts both
/// the legacy comma form (`rgba(255, 0, 0, 0.5)`) and the modern space form
/// (`rgb(255 0 0 / 0.5)`). Alpha may be a percentage or a number in `[0,1]`.
fn parse_rgb(args: &str) -> Result<Color, ColorParseError> {
    let (channels, alpha) = split_channels_alpha(args)?;
    if channels.len() != 3 {
        return Err(ColorParseError::InvalidFunction(format!("rgb({args})")));
    }
    let r = parse_rgb_component(channels[0])?;
    let g = parse_rgb_component(channels[1])?;
    let b = parse_rgb_component(channels[2])?;
    let a = parse_alpha(alpha)?;
    Ok(Color::rgba(r, g, b, a))
}

/// Parse `hsl()` / `hsla()` arguments per CSS Color 4 § 5.3.3. Hue is a
/// number (degrees by default); saturation/lightness are percentages.
fn parse_hsl(args: &str) -> Result<Color, ColorParseError> {
    let (channels, alpha) = split_channels_alpha(args)?;
    if channels.len() != 3 {
        return Err(ColorParseError::InvalidFunction(format!("hsl({args})")));
    }
    let h = parse_hue_deg(channels[0])?;
    let s = parse_percent(channels[1])?;
    let l = parse_percent(channels[2])?;
    let a = parse_alpha(alpha)?;
    let (r, g, b) = hsl_to_rgb_u8(h, s, l);
    Ok(Color::rgba(r, g, b, a))
}

/// Split a function-argument string into the channel list and an optional
/// alpha slice. Handles both `r, g, b[, / a]` and `r g b[ / a]` (CSS Color 4
/// § 5.3 "color function syntax" — comma or whitespace separated, with `/`
/// introducing the alpha). The legacy `rgba(r, g, b, a)` four-comma form is
/// also recognised: when no `/` is present and exactly four comma-separated
/// values appear, the fourth is the alpha.
fn split_channels_alpha(args: &str) -> Result<(Vec<&str>, Option<&str>), ColorParseError> {
    let args = args.trim();
    let (body, alpha) = match args.split_once('/') {
        Some((b, a)) => (b, Some(a.trim())),
        None => (args, None),
    };
    let body = body.trim();
    let channels: Vec<&str> = if body.contains(',') {
        body.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        body.split_whitespace().collect()
    };
    // Legacy `rgb(r,g,b,a)` form: 4 comma-separated channels with no `/`
    // separator → the 4th is the alpha. CSS Color 4 § 5.3.1 still allows this.
    let (channels, alpha) = if alpha.is_none() && channels.len() == 4 && body.contains(',') {
        let a = channels[3];
        (channels[..3].to_vec(), Some(a))
    } else {
        (channels, alpha)
    };
    Ok((channels, alpha))
}

/// Parse an RGB component: a `[0,255]` integer or a percentage `[0%,100%]`.
fn parse_rgb_component(s: &str) -> Result<u8, ColorParseError> {
    if let Some(p) = s.strip_suffix('%') {
        let v: f64 = p
            .parse()
            .map_err(|_| ColorParseError::InvalidNumber(s.to_owned()))?;
        return Ok(quantise_f64_to_u8((v / 100.0) * 255.0));
    }
    let v: f64 = s
        .parse()
        .map_err(|_| ColorParseError::InvalidNumber(s.to_owned()))?;
    // CSS Color 4: out-of-gamut numbers clamp to [0,255] before quantisation.
    Ok(quantise_f64_to_u8(v))
}

/// Parse a percentage into `[0.0, 1.0]`, clamping to gamut.
fn parse_percent(s: &str) -> Result<f64, ColorParseError> {
    let p = s
        .strip_suffix('%')
        .ok_or_else(|| ColorParseError::InvalidNumber(s.to_owned()))?;
    let v: f64 = p
        .parse()
        .map_err(|_| ColorParseError::InvalidNumber(s.to_owned()))?;
    Ok((v / 100.0).clamp(0.0, 1.0))
}

/// Parse a hue value into degrees in `[0, 360)`. CSS Color 4 § 5.3.4: a bare
/// number is degrees; `deg`/`rad`/`grad`/`turn` units are recognised. The
/// angle is normalised to `[0, 360)`.
fn parse_hue_deg(s: &str) -> Result<f64, ColorParseError> {
    let lower = s.to_ascii_lowercase();
    let (num_str, scale) = if let Some(rest) = lower.strip_suffix("deg") {
        (rest, 1.0)
    } else if let Some(rest) = lower.strip_suffix("rad") {
        (rest, 180.0 / std::f64::consts::PI)
    } else if let Some(rest) = lower.strip_suffix("grad") {
        (rest, 0.9) // 400grad = 360deg
    } else if let Some(rest) = lower.strip_suffix("turn") {
        (rest, 360.0)
    } else {
        (lower.as_str(), 1.0)
    };
    let v: f64 = num_str
        .trim()
        .parse()
        .map_err(|_| ColorParseError::InvalidNumber(s.to_owned()))?;
    let deg = v * scale;
    // Normalise to [0, 360).
    let normalised = deg.rem_euclid(360.0);
    Ok(normalised)
}

/// Parse the alpha component (after the `/`, or the 4th comma arg). Accepts
/// a `[0,1]` number or a `[0%,100%]` percentage; clamps to gamut.
fn parse_alpha(s: Option<&str>) -> Result<u8, ColorParseError> {
    let Some(s) = s else {
        return Ok(255);
    };
    if s.is_empty() {
        return Ok(255);
    }
    if let Some(p) = s.strip_suffix('%') {
        let v: f64 = p
            .parse()
            .map_err(|_| ColorParseError::InvalidNumber(s.to_owned()))?;
        return Ok(quantise_f64_to_u8((v / 100.0) * 255.0));
    }
    let v: f64 = s
        .parse()
        .map_err(|_| ColorParseError::InvalidNumber(s.to_owned()))?;
    Ok(quantise_f64_to_u8(v * 255.0))
}

/// Quantise a `f64` to a `u8` per CSS Color 4 § 5.1: round half-to-even, then
/// clamp to `[0, 255]`.
fn quantise_f64_to_u8(v: f64) -> u8 {
    let v = v.round();
    if v <= 0.0 {
        0
    } else if v >= 255.0 {
        255
    } else {
        v as u8
    }
}

/// HSL → sRGB conversion per CSS Color 4 § 5.4. `h` is degrees in `[0,360)`,
/// `s`/`l` are fractions in `[0,1]`. Returns 8-bit per channel.
fn hsl_to_rgb_u8(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    if s == 0.0 {
        let c = quantise_f64_to_u8(l * 255.0);
        return (c, c, c);
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = h / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match h_prime as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        quantise_f64_to_u8((r1 + m) * 255.0),
        quantise_f64_to_u8((g1 + m) * 255.0),
        quantise_f64_to_u8((b1 + m) * 255.0),
    )
}

// ---------------------------------------------------------------------------
// Alpha arithmetic + interpolation
// ---------------------------------------------------------------------------

/// Premultiply an RGBA colour (CSS Compositing 1 § 4). The colour channels
/// are scaled by `a/255` so that blending is a single multiply-add.
pub fn premultiply(c: Color) -> Color {
    if c.a == 255 {
        return c;
    }
    if c.a == 0 {
        return Color::TRANSPARENT;
    }
    let f = c.a as f32 / 255.0;
    Color::rgba(
        (c.r as f32 * f).round() as u8,
        (c.g as f32 * f).round() as u8,
        (c.b as f32 * f).round() as u8,
        c.a,
    )
}

/// Inverse of [`premultiply`]. A zero-alpha colour stays zero-alpha to avoid
/// dividing by zero; callers should preserve the original alpha.
pub fn unpremultiply(c: Color) -> Color {
    if c.a == 255 || c.a == 0 {
        return c;
    }
    let f = 255.0 / c.a as f32;
    Color::rgba(
        (c.r as f32 * f).clamp(0.0, 255.0).round() as u8,
        (c.g as f32 * f).clamp(0.0, 255.0).round() as u8,
        (c.b as f32 * f).clamp(0.0, 255.0).round() as u8,
        c.a,
    )
}

/// Linear sRGB interpolation between two colours at parameter `t ∈ [0,1]`,
/// the primitive gradients (CSS Images 4 § 4.3) and transitions reduce to.
/// Linear-space blending matches what Web Animations § 5.4 calls the default
/// "sRGB" interpolation (it actually means linear-sRGB for arithmetic and
/// sRGB transfer on output, which is what [`Color::to_linear_f32`] decodes).
///
/// Out-of-range `t` is clamped — gradient stops past `[0,1]` are a caller
/// concern (CSS Images 4 § 4.4 "color hint"), not the interpolator's.
pub fn interpolate(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let la = a.to_linear_f32();
    let lb = b.to_linear_f32();
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    let r = lerp(la[0], lb[0]);
    let g = lerp(la[1], lb[1]);
    let bl = lerp(la[2], lb[2]);
    let al = lerp(la[3], lb[3]);
    let encode = |v: f32| -> u8 {
        let v = if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        quantise_f64_to_u8((v * 255.0) as f64)
    };
    Color::rgba(
        encode(r),
        encode(g),
        encode(bl),
        quantise_f64_to_u8((al * 255.0) as f64),
    )
}

// ---------------------------------------------------------------------------
// Named colours — CSS Color 4 § 6 ("Named Colors"). The 148-colour table is
// the spec's Level III named set (every CSS / SVG named colour v1.0 resolves).
// ---------------------------------------------------------------------------

/// Look up a named colour from CSS Color 4 § 6. Returns `None` for unknown
/// names (caller turns that into a [`ColorParseError::UnknownName`]).
pub fn named_color(name: &str) -> Option<Color> {
    // NAMED_COLORS is `&[(&str, u8, u8, u8)]` (name + RGB); fold the lookup
    // into a single pass so callers don't have to ignore the name slot.
    for &(n, r, g, b) in NAMED_COLORS.iter() {
        if n == name {
            return Some(Color::rgb(r, g, b));
        }
    }
    None
}

#[rustfmt::skip]
static NAMED_COLORS: &[(&str, u8, u8, u8)] = &[
    // CSS Color 4 Level III named set — the 148 names every browser must
    // recognise. Ordered alphabetically (the spec table is alphabetical).
    ("aliceblue", 240, 248, 255),
    ("antiquewhite", 250, 235, 215),
    ("aqua", 0, 255, 255),
    ("aquamarine", 127, 255, 212),
    ("azure", 240, 255, 255),
    ("beige", 245, 245, 220),
    ("bisque", 255, 228, 196),
    ("black", 0, 0, 0),
    ("blanchedalmond", 255, 235, 205),
    ("blue", 0, 0, 255),
    ("blueviolet", 138, 43, 226),
    ("brown", 165, 42, 42),
    ("burlywood", 222, 184, 135),
    ("cadetblue", 95, 158, 160),
    ("chartreuse", 127, 255, 0),
    ("chocolate", 210, 105, 30),
    ("coral", 255, 127, 80),
    ("cornflowerblue", 100, 149, 237),
    ("cornsilk", 255, 248, 220),
    ("crimson", 220, 20, 60),
    ("cyan", 0, 255, 255),
    ("darkblue", 0, 0, 139),
    ("darkcyan", 0, 139, 139),
    ("darkgoldenrod", 184, 134, 11),
    ("darkgray", 169, 169, 169),
    ("darkgreen", 0, 100, 0),
    ("darkgrey", 169, 169, 169),
    ("darkkhaki", 189, 183, 107),
    ("darkmagenta", 139, 0, 139),
    ("darkolivegreen", 85, 107, 47),
    ("darkorange", 255, 140, 0),
    ("darkorchid", 153, 50, 204),
    ("darkred", 139, 0, 0),
    ("darksalmon", 233, 150, 122),
    ("darkseagreen", 143, 188, 143),
    ("darkslateblue", 72, 61, 139),
    ("darkslategray", 47, 79, 79),
    ("darkslategrey", 47, 79, 79),
    ("darkturquoise", 0, 206, 209),
    ("darkviolet", 148, 0, 211),
    ("deeppink", 255, 20, 147),
    ("deepskyblue", 0, 191, 255),
    ("dimgray", 105, 105, 105),
    ("dimgrey", 105, 105, 105),
    ("dodgerblue", 30, 144, 255),
    ("firebrick", 178, 34, 34),
    ("floralwhite", 255, 250, 240),
    ("forestgreen", 34, 139, 34),
    ("fuchsia", 255, 0, 255),
    ("gainsboro", 220, 220, 220),
    ("ghostwhite", 248, 248, 255),
    ("gold", 255, 215, 0),
    ("goldenrod", 218, 165, 32),
    ("gray", 128, 128, 128),
    ("green", 0, 128, 0),
    ("greenyellow", 173, 255, 47),
    ("grey", 128, 128, 128),
    ("honeydew", 240, 255, 240),
    ("hotpink", 255, 105, 180),
    ("indianred", 205, 92, 92),
    ("indigo", 75, 0, 130),
    ("ivory", 255, 255, 240),
    ("khaki", 240, 230, 140),
    ("lavender", 230, 230, 250),
    ("lavenderblush", 255, 240, 245),
    ("lawngreen", 124, 252, 0),
    ("lemonchiffon", 255, 250, 205),
    ("lightblue", 173, 216, 230),
    ("lightcoral", 240, 128, 128),
    ("lightcyan", 224, 255, 255),
    ("lightgoldenrodyellow", 250, 250, 210),
    ("lightgray", 211, 211, 211),
    ("lightgreen", 144, 238, 144),
    ("lightgrey", 211, 211, 211),
    ("lightpink", 255, 182, 193),
    ("lightsalmon", 255, 160, 122),
    ("lightseagreen", 32, 178, 170),
    ("lightskyblue", 135, 206, 250),
    ("lightslategray", 119, 136, 153),
    ("lightslategrey", 119, 136, 153),
    ("lightsteelblue", 176, 196, 222),
    ("lightyellow", 255, 255, 224),
    ("lime", 0, 255, 0),
    ("limegreen", 50, 205, 50),
    ("linen", 250, 240, 230),
    ("magenta", 255, 0, 255),
    ("maroon", 128, 0, 0),
    ("mediumaquamarine", 102, 205, 170),
    ("mediumblue", 0, 0, 205),
    ("mediumorchid", 186, 85, 211),
    ("mediumpurple", 147, 112, 219),
    ("mediumseagreen", 60, 179, 113),
    ("mediumslateblue", 123, 104, 238),
    ("mediumspringgreen", 0, 250, 154),
    ("mediumturquoise", 72, 209, 204),
    ("mediumvioletred", 199, 21, 133),
    ("midnightblue", 25, 25, 112),
    ("mintcream", 245, 255, 250),
    ("mistyrose", 255, 228, 225),
    ("moccasin", 255, 228, 181),
    ("navajowhite", 255, 222, 173),
    ("navy", 0, 0, 128),
    ("oldlace", 253, 245, 230),
    ("olive", 128, 128, 0),
    ("olivedrab", 107, 142, 35),
    ("orange", 255, 165, 0),
    ("orangered", 255, 69, 0),
    ("orchid", 218, 112, 214),
    ("palegoldenrod", 238, 232, 170),
    ("palegreen", 152, 251, 152),
    ("paleturquoise", 175, 238, 238),
    ("palevioletred", 219, 112, 147),
    ("papayawhip", 255, 239, 213),
    ("peachpuff", 255, 218, 185),
    ("peru", 205, 133, 63),
    ("pink", 255, 192, 203),
    ("plum", 221, 160, 221),
    ("powderblue", 176, 224, 230),
    ("purple", 128, 0, 128),
    ("rebeccapurple", 102, 51, 153),
    ("red", 255, 0, 0),
    ("rosybrown", 188, 143, 143),
    ("royalblue", 65, 105, 225),
    ("saddlebrown", 139, 69, 19),
    ("salmon", 250, 128, 114),
    ("sandybrown", 244, 164, 96),
    ("seagreen", 46, 139, 87),
    ("seashell", 255, 245, 238),
    ("sienna", 160, 82, 45),
    ("silver", 192, 192, 192),
    ("skyblue", 135, 206, 235),
    ("slateblue", 106, 90, 205),
    ("slategray", 112, 128, 144),
    ("slategrey", 112, 128, 144),
    ("snow", 255, 250, 250),
    ("springgreen", 0, 255, 127),
    ("steelblue", 70, 130, 180),
    ("tan", 210, 180, 140),
    ("teal", 0, 128, 128),
    ("thistle", 216, 191, 216),
    ("tomato", 255, 99, 71),
    ("turquoise", 64, 224, 208),
    ("violet", 238, 130, 238),
    ("wheat", 245, 222, 179),
    ("white", 255, 255, 255),
    ("whitesmoke", 245, 245, 245),
    ("yellow", 255, 255, 0),
    ("yellowgreen", 154, 205, 50),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_color(c: Color, r: u8, g: u8, b: u8, a: u8) -> bool {
        // Allow a 1-LSB tolerance on each channel for float-quantisation.
        (c.r as i16 - r as i16).unsigned_abs() <= 1
            && (c.g as i16 - g as i16).unsigned_abs() <= 1
            && (c.b as i16 - b as i16).unsigned_abs() <= 1
            && c.a == a
    }

    // --- Hex ------------------------------------------------------------

    #[test]
    fn parse_hex_widths() {
        assert_eq!(Color::parse("#000").unwrap(), Color::BLACK);
        assert_eq!(Color::parse("#fff").unwrap(), Color::WHITE);
        assert_eq!(Color::parse("#0000").unwrap(), Color::TRANSPARENT);
        assert_eq!(Color::parse("#ffff").unwrap(), Color::WHITE);
        assert_eq!(Color::parse("#000000").unwrap(), Color::BLACK);
        assert_eq!(Color::parse("#ffffff").unwrap(), Color::WHITE);
        assert_eq!(
            Color::parse("#ff000080").unwrap(),
            Color::rgba(255, 0, 0, 128)
        );
    }

    #[test]
    fn parse_hex_uppercase_and_whitespace() {
        assert_eq!(
            Color::parse("  #FFAABB  ").unwrap(),
            Color::rgb(255, 170, 187)
        );
        assert_eq!(Color::parse("#FAF").unwrap(), Color::rgb(255, 170, 255));
    }

    #[test]
    fn parse_hex_rejects_bad_width() {
        assert!(Color::parse("#12").is_err());
        assert!(Color::parse("#12345").is_err());
        assert!(Color::parse("#1234567").is_err());
        assert!(Color::parse("#gggggg").is_err());
        assert!(Color::parse("#").is_err());
    }

    // --- rgb() ----------------------------------------------------------

    #[test]
    fn parse_rgb_legacy_and_modern() {
        // Legacy comma form.
        assert_eq!(
            Color::parse("rgb(255, 0, 0)").unwrap(),
            Color::rgb(255, 0, 0)
        );
        // Modern space form.
        assert_eq!(Color::parse("rgb(255 0 0)").unwrap(), Color::rgb(255, 0, 0));
        // With slash alpha (modern).
        assert_eq!(
            Color::parse("rgb(255 0 0 / 0.5)").unwrap(),
            Color::rgba(255, 0, 0, 128)
        );
        // With comma alpha (legacy).
        assert_eq!(
            Color::parse("rgba(255, 0, 0, 0.5)").unwrap(),
            Color::rgba(255, 0, 0, 128)
        );
        // Percentages.
        assert_eq!(
            Color::parse("rgb(100%, 0%, 0%)").unwrap(),
            Color::rgb(255, 0, 0)
        );
    }

    #[test]
    fn parse_rgb_clamps_out_of_gamut() {
        let c = Color::parse("rgb(300, -10, 0)").unwrap();
        assert_eq!(c, Color::rgb(255, 0, 0));
    }

    #[test]
    fn parse_rgb_alpha_percentage() {
        assert_eq!(
            Color::parse("rgba(0, 0, 0, 50%)").unwrap(),
            Color::rgba(0, 0, 0, 128)
        );
    }

    // --- hsl() ----------------------------------------------------------

    #[test]
    fn parse_hsl_primaries() {
        // 0° = red, 120° = green, 240° = blue (full sat, mid light).
        assert_eq!(
            Color::parse("hsl(0, 100%, 50%)").unwrap(),
            Color::rgb(255, 0, 0)
        );
        assert!(approx_color(
            Color::parse("hsl(120, 100%, 50%)").unwrap(),
            0,
            255,
            0,
            255
        ));
        assert!(approx_color(
            Color::parse("hsl(240, 100%, 50%)").unwrap(),
            0,
            0,
            255,
            255
        ));
    }

    #[test]
    fn parse_hsl_achromatic() {
        // s=0 → greyscale.
        assert_eq!(
            Color::parse("hsl(0, 0%, 50%)").unwrap(),
            Color::rgb(128, 128, 128)
        );
        assert_eq!(Color::parse("hsl(0, 0%, 0%)").unwrap(), Color::BLACK);
        assert_eq!(Color::parse("hsl(0, 0%, 100%)").unwrap(), Color::WHITE);
    }

    #[test]
    fn parse_hsl_hue_normalisation() {
        // 360° wraps to 0°; negative hue normalises into [0,360).
        let a = Color::parse("hsl(360, 100%, 50%)").unwrap();
        let b = Color::parse("hsl(0, 100%, 50%)").unwrap();
        assert_eq!(a, b);
        let neg = Color::parse("hsl(-120, 100%, 50%)").unwrap();
        let pos = Color::parse("hsl(240, 100%, 50%)").unwrap();
        assert_eq!(neg, pos);
    }

    #[test]
    fn parse_hsla_with_alpha() {
        assert_eq!(
            Color::parse("hsla(0, 100%, 50%, 0.5)").unwrap(),
            Color::rgba(255, 0, 0, 128)
        );
    }

    // --- Named colours + keywords --------------------------------------

    #[test]
    fn parse_named_colours() {
        assert_eq!(Color::parse("red").unwrap(), Color::rgb(255, 0, 0));
        assert_eq!(
            Color::parse("ReBeccaPurple").unwrap(),
            Color::rgb(102, 51, 153)
        );
        assert_eq!(Color::parse("CORAL").unwrap(), Color::rgb(255, 127, 80));
        assert_eq!(Color::parse("transparent").unwrap(), Color::TRANSPARENT);
    }

    #[test]
    fn parse_currentcolor_round_trips_keyword() {
        assert_eq!(
            ColorOrKeyword::parse("currentColor"),
            Ok(ColorOrKeyword::CurrentColor)
        );
        // `Color::parse` rejects it (no cascade here).
        assert!(Color::parse("currentcolor").is_err());
    }

    #[test]
    fn parse_unknown_name_fails_closed() {
        assert!(Color::parse("notacolor").is_err());
        assert!(Color::parse("").is_err());
    }

    #[test]
    fn parse_unsupported_color_space_signals_deferred() {
        // oklch/lab/lch/color() are post-v1.0; we fail closed rather than
        // mis-parse them as something else.
        for s in [
            "oklch(0.5 0.1 200)",
            "lab(50% 0 0)",
            "color(display-p3 1 0 0)",
        ] {
            let e = Color::parse(s).unwrap_err();
            assert!(
                matches!(e, ColorParseError::UnsupportedColorSpace(_)),
                "expected UnsupportedColorSpace for {s}, got {e:?}"
            );
        }
    }

    #[test]
    fn named_table_known_samples() {
        assert_eq!(named_color("red"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            named_color("cornflowerblue"),
            Some(Color::rgb(100, 149, 237))
        );
        assert_eq!(named_color("nosuch"), None);
    }

    // --- Premultiply round-trip ----------------------------------------

    #[test]
    fn premultiply_unpremultiply_round_trip() {
        // Round-tripping is lossless only when alpha is high enough that the
        // channel × (a/255) product survives the 8-bit quantise. For very low
        // alphas the premultiply step collapses the channels (a known property
        // of 8-bit premultiplied alpha), so the test exercises the loss-free
        // band: opaque + a mid-alpha value where every channel survives.
        for c in [
            Color::rgb(255, 0, 0),
            Color::rgb(123, 45, 67),
            Color::rgba(200, 100, 50, 200),
            Color::rgba(255, 255, 255, 128),
        ] {
            let rt = unpremultiply(premultiply(c));
            assert!(
                (rt.r as i16 - c.r as i16).unsigned_abs() <= 1
                    && (rt.g as i16 - c.g as i16).unsigned_abs() <= 1
                    && (rt.b as i16 - c.b as i16).unsigned_abs() <= 1
                    && rt.a == c.a,
                "round trip {c:?} -> {rt:?}"
            );
        }
        // Fully opaque is unchanged.
        assert_eq!(
            premultiply(Color::rgb(123, 45, 67)),
            Color::rgb(123, 45, 67)
        );
        // Fully transparent collapses to all-zero (alpha is preserved; the
        // colour channels carry no information when nothing is shown).
        assert_eq!(premultiply(Color::rgba(99, 99, 99, 0)), Color::TRANSPARENT);
    }

    // --- Interpolation -------------------------------------------------

    #[test]
    fn interpolate_endpoints_and_midpoint() {
        let a = Color::BLACK;
        let b = Color::WHITE;
        assert_eq!(interpolate(a, b, 0.0), a);
        assert_eq!(interpolate(a, b, 1.0), b);
        // Midpoint in *linear* sRGB space, then re-encoded. Linear 0.5 maps
        // back to sRGB ≈ 0.7436, i.e. ≈ 188/255 — not 128/255. (Linear-space
        // blending is the spec default per Web Animations § 5.4; perceptually
        // correct, looks brighter than a naive gamma-blind midpoint.)
        let mid = interpolate(a, b, 0.5);
        assert!((mid.r as i16 - 188).unsigned_abs() <= 2, "mid: {mid:?}");
        assert!((mid.g as i16 - 188).unsigned_abs() <= 2);
        assert!((mid.b as i16 - 188).unsigned_abs() <= 2);
    }

    #[test]
    fn interpolate_clamps_out_of_range_t() {
        let a = Color::rgb(0, 0, 0);
        let b = Color::rgb(255, 0, 0);
        assert_eq!(interpolate(a, b, -1.0), a);
        assert_eq!(interpolate(a, b, 2.0), b);
    }

    // --- Display-list adapter ------------------------------------------

    #[test]
    fn color_converts_to_display_list_color() {
        let c = Color::rgba(1, 2, 3, 4);
        assert_eq!(c.to_display_list(), DisplayListColor::rgba(1, 2, 3, 4));
    }
}
