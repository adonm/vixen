//! CSS Filter Effects 1 § 5 — the `filter` / `filter-function-list` grammar
//! and the per-pixel colour-matrix family the paint path reduces to (pure
//! logic). Complements [`crate::blend`] (the compositing and blend modes) and
//! [`crate::color`] (the sRGB arithmetic); [`crate::box_shadow`] already
//! owns the `drop-shadow()` shadow geometry.
//!
//! What lives here:
//! - [`FilterFunction`] — the 10 § 5 functions (`blur`/`brightness`/
//!   `contrast`/`drop-shadow`/`grayscale`/`hue-rotate`/`invert`/`opacity`/
//!   `saturate`/`sepia`) with default-argument handling per § 5 Table.
//! - [`FilterList`] — a parsed `filter: blur(2px) sepia(0.5) …` chain, with
//!   [`FilterList::parse`] tolerant of the `<url>` sentinel (v1.0 fails
//!   closed on `<url>` references — SVG filter elements are post-v1.0).
//! - [`ColorMatrix`] — the SVG `feColorMatrix` 4×5 matrix the per-pixel
//!   family composes into; [`ColorMatrix::combine`] folds a chain into one
//!   matrix so the paint path runs a single multiply per pixel.
//! - [`FilterFunction::apply`] — per-pixel projection (the colour-matrix
//!   family); `blur`/`drop-shadow` keep their geometry here for the paint
//!   path to apply spatially.
//!
//! What does *not* live here:
//! - The Gaussian blur kernel (Filter Effects 1 § 8 — the paint path runs
//!   the box/triangle-pass approximation; [`FilterFunction::Blur`] carries
//!   the std-dev only).
//! - SVG `<filter>` element references + `feGaussianBlur`/`feOffset`/&c. —
//!   post-v1.0 (the CSS function-list surface covers v1.0 needs).
//! - The `filter-region` / `filter-margins` geometry (paint path).
//!
//! ## Working colour space
//!
//! CSS `filter()` colour-matrix ops run in normalised **sRGB** (non-linear)
//! space — matching the de-facto browser behaviour (the SVG
//! `color-interpolation-filters: linearRGB` switch does not apply to the CSS
//! `filter` shorthand). Channels are normalised to `[0,1]` as `c/255`, the
//! matrix is applied, and the output is re-quantised to `u8`. This agrees
//! with Firefox/Servo's CSS filter pipeline; the linear-space variant is a
//! documented future slice.
//!
//! Reference: <https://www.w3.org/TR/filter-effects-1/>.
//! SVG `feColorMatrix`: <https://www.w3.org/TR/SVG11/filters.html#feColorMatrixElement>.

#![forbid(unsafe_code)]

use crate::box_shadow;
use crate::color::Color;

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// One CSS Filter Effects 1 § 5 filter function. `blur`/`drop-shadow` are
/// spatial (the paint path applies them); the rest are per-pixel
/// colour-matrix ops [`FilterFunction::apply`] evaluates directly.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterFunction {
    /// `blur(radius?)` — Gaussian blur radius in CSS px (default `0`).
    Blur(f32),
    /// `brightness(amount?)` — per-channel scalar (default `1`; `0` = black,
    /// `>1` = brighter). Percentages map `100%` → `1`.
    Brightness(f32),
    /// `contrast(amount?)` — `(c - 0.5)·amount + 0.5` (default `1`).
    Contrast(f32),
    /// `drop-shadow(<offset>{2,3} <color>?)` — the paint path applies; the
    /// geometry + colour live in [`box_shadow`] (the `inset: false` case).
    DropShadow(box_shadow::BoxShadow),
    /// `grayscale(amount?)` — desaturate (default `0`; `1` = fully grey).
    Grayscale(f32),
    /// `hue-rotate(angle?)` — rotate the hue by `angle` degrees (default `0`).
    HueRotate(f32),
    /// `invert(amount?)` — `amount·(1-c) + (1-amount)·c` (default `0`; `1` =
    /// fully inverted).
    Invert(f32),
    /// `opacity(amount?)` — scale alpha by `amount` (default `1`).
    Opacity(f32),
    /// `saturate(amount?)` — colour-saturation matrix (default `1`).
    Saturate(f32),
    /// `sepia(amount?)` — sepia-tone matrix (default `0`; `1` = fully sepia).
    Sepia(f32),
}

/// Parse error for [`FilterFunction::parse`] / [`FilterList::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterParseError {
    /// Empty filter list (`filter: none` is handled by the caller, not the
    /// parser; an empty string here is an authoring error).
    #[error("empty filter list")]
    Empty,
    /// An unknown function name or malformed argument.
    #[error("invalid filter function: {0:?}")]
    InvalidFunction(String),
    /// `url()` references to SVG `<filter>` elements (post-v1.0).
    #[error("url() filter references are unsupported (SVG filter elements are post-v1.0)")]
    UnsupportedUrl,
}

impl FilterFunction {
    /// Parse a single § 5 filter function (e.g. `"blur(2px)"`,
    /// `"sepia(0.5)"`). The argument defaults are applied when the
    /// parentheses are empty (`"blur()"` ⇒ radius 0, `"brightness()"` ⇒ 1,
    /// &c.).
    ///
    /// ```
    /// # use vixen_engine::filter::FilterFunction;
    /// assert_eq!(FilterFunction::parse("blur()").unwrap(), FilterFunction::Blur(0.0));
    /// assert_eq!(FilterFunction::parse("brightness(1.5)").unwrap(), FilterFunction::Brightness(1.5));
    /// assert_eq!(FilterFunction::parse("brightness(150%)").unwrap(), FilterFunction::Brightness(1.5));
    /// ```
    pub fn parse(s: &str) -> Result<Self, FilterParseError> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        if let Some(rest) = strip_call(&lower, "blur") {
            return Ok(FilterFunction::Blur(parse_length_arg(rest)?));
        }
        if let Some(rest) = strip_call(&lower, "brightness") {
            return Ok(FilterFunction::Brightness(parse_amount_arg(rest, 1.0)?));
        }
        if let Some(rest) = strip_call(&lower, "contrast") {
            return Ok(FilterFunction::Contrast(parse_amount_arg(rest, 1.0)?));
        }
        if let Some(rest) = strip_call(&lower, "drop-shadow") {
            return Ok(FilterFunction::DropShadow(parse_drop_shadow(rest)?));
        }
        if let Some(rest) = strip_call(&lower, "grayscale") {
            return Ok(FilterFunction::Grayscale(parse_amount_arg(rest, 0.0)?));
        }
        if let Some(rest) = strip_call(&lower, "hue-rotate") {
            return Ok(FilterFunction::HueRotate(parse_angle_arg(rest)?));
        }
        if let Some(rest) = strip_call(&lower, "invert") {
            return Ok(FilterFunction::Invert(parse_amount_arg(rest, 0.0)?));
        }
        if let Some(rest) = strip_call(&lower, "opacity") {
            return Ok(FilterFunction::Opacity(parse_amount_arg(rest, 1.0)?));
        }
        if let Some(rest) = strip_call(&lower, "saturate") {
            return Ok(FilterFunction::Saturate(parse_amount_arg(rest, 1.0)?));
        }
        if let Some(rest) = strip_call(&lower, "sepia") {
            return Ok(FilterFunction::Sepia(parse_amount_arg(rest, 0.0)?));
        }
        // `url(...)` is the SVG-filter-element reference surface; v1.0 fails
        // closed so the host hook reports `unsupported` rather than silently
        // dropping the filter.
        if lower.starts_with("url(") {
            return Err(FilterParseError::UnsupportedUrl);
        }
        Err(FilterParseError::InvalidFunction(s.to_owned()))
    }
}

/// A parsed `filter` chain: one or more [`FilterFunction`]s in source order.
/// `filter: none` produces an empty list (the caller decides the empty
/// semantics — the paint path treats it as identity).
#[derive(Debug, Clone, PartialEq)]
pub struct FilterList {
    functions: Vec<FilterFunction>,
}

impl FilterList {
    /// Parse a `filter` declaration value (whitespace-separated function
    /// list). `none` / empty ⇒ the empty list. The functions apply in source
    /// order per § 5 (leftmost applied first against the source pixels).
    ///
    /// ```
    /// # use vixen_engine::filter::{FilterFunction, FilterList};
    /// let list = FilterList::parse("blur(2px) sepia(0.5)").unwrap();
    /// assert_eq!(list.functions(), &[
    ///     FilterFunction::Blur(2.0),
    ///     FilterFunction::Sepia(0.5),
    /// ]);
    /// ```
    pub fn parse(s: &str) -> Result<Self, FilterParseError> {
        let trimmed = s.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
            return Ok(Self { functions: vec![] });
        }
        let mut functions = Vec::new();
        for token in iter_functions(trimmed) {
            functions.push(FilterFunction::parse(token)?);
        }
        if functions.is_empty() {
            return Err(FilterParseError::Empty);
        }
        Ok(Self { functions })
    }

    /// The parsed functions in source order.
    pub fn functions(&self) -> &[FilterFunction] {
        &self.functions
    }

    /// Whether the chain is empty (`filter: none`).
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }
}

/// Iterate the whitespace-separated filter-function tokens, respecting the
/// parenthesised argument lists so a function argument containing a space
/// (`drop-shadow(2px 4px black)`) stays one token.
fn iter_functions(s: &str) -> impl Iterator<Item = &str> {
    // The filter grammar is name + balanced parens; whitespace outside
    // parens separates functions. We split on whitespace at paren depth 0.
    struct Fns<'a>(&'a str);
    impl<'a> Iterator for Fns<'a> {
        type Item = &'a str;
        fn next(&mut self) -> Option<&'a str> {
            let s = self.0.trim_start();
            if s.is_empty() {
                return None;
            }
            // Find the end of this function: the closing paren matching the
            // first `(`, or the next whitespace if there is no paren.
            let bytes = s.as_bytes();
            let mut depth = 0i32;
            let mut end = bytes.len();
            for (i, &b) in bytes.iter().enumerate() {
                match b {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                    _ if depth == 0 && b.is_ascii_whitespace() => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            let (head, tail) = s.split_at(end);
            self.0 = tail;
            Some(head)
        }
    }
    Fns(s)
}

/// Strip a `name(` prefix and matching `)` suffix, returning the inner
/// argument string (untrimmed). Returns `None` if the prefix doesn't match.
fn strip_call<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}(");
    let inner = s.strip_prefix(&prefix)?;
    let inner = inner.strip_suffix(')')?;
    Some(inner)
}

/// Parse a `<length>` argument (px assumed; the `px` suffix optional). Empty
/// ⇒ the supplied default. Negative values are a parse error per § 5.
fn parse_length_arg(inner: &str) -> Result<f32, FilterParseError> {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return Ok(0.0);
    }
    let v = trimmed
        .strip_suffix("px")
        .unwrap_or(trimmed)
        .trim()
        .parse::<f32>()
        .map_err(|_| FilterParseError::InvalidFunction(format!("blur({inner})")))?;
    if v < 0.0 {
        return Err(FilterParseError::InvalidFunction(format!("blur({inner})")));
    }
    Ok(v)
}

/// Parse an `<number-percentage>` argument. Empty ⇒ `default`. A percentage
/// maps `100%` → `1`. Negative amounts are allowed where the spec allows
/// (brightness/saturate can exceed 1; grayscale/invert/opacity/sepia clamp
/// to `[0,1]` at apply time). Returns the raw float; clamping happens in
/// [`FilterFunction::apply`] so the matrix composition stays faithful.
fn parse_amount_arg(inner: &str, default: f32) -> Result<f32, FilterParseError> {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return Ok(default);
    }
    let (n, scale) = if let Some(rest) = trimmed.strip_suffix('%') {
        (rest.trim(), 0.01)
    } else {
        (trimmed, 1.0)
    };
    n.parse::<f32>()
        .map(|v| v * scale)
        .map_err(|_| FilterParseError::InvalidFunction(format!("amount({inner})")))
}

/// Parse an `<angle>` argument (degrees assumed; `deg`/`rad`/`grad`/`turn`
/// units recognised). Empty ⇒ `0`.
fn parse_angle_arg(inner: &str) -> Result<f32, FilterParseError> {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return Ok(0.0);
    }
    let lower = trimmed.to_ascii_lowercase();
    let (n, scale) = if let Some(rest) = lower.strip_suffix("deg") {
        (rest, 1.0)
    } else if let Some(rest) = lower.strip_suffix("rad") {
        (rest, 180.0 / std::f32::consts::PI)
    } else if let Some(rest) = lower.strip_suffix("grad") {
        (rest, 0.9)
    } else if let Some(rest) = lower.strip_suffix("turn") {
        (rest, 360.0)
    } else {
        (lower.as_str(), 1.0)
    };
    n.trim()
        .parse::<f32>()
        .map(|v| v * scale)
        .map_err(|_| FilterParseError::InvalidFunction(format!("angle({inner})")))
}

/// Parse the `drop-shadow()` argument: `<offset-x> <offset-y>
/// [<blur>] [<color>]`. Delegates the colour + shadow geometry to
/// [`box_shadow`] (the single-shadow `inset: false` case is identical).
fn parse_drop_shadow(inner: &str) -> Result<box_shadow::BoxShadow, FilterParseError> {
    // box_shadow::parse_box_shadow handles the full `<box-shadow>#` grammar;
    // drop-shadow is exactly one shadow. Reject the multi-shadow form.
    let parsed = box_shadow::parse_box_shadow(inner)
        .map_err(|e| FilterParseError::InvalidFunction(format!("drop-shadow({inner}): {e}")))?;
    let mut iter = parsed.into_iter();
    let mut shadow = iter
        .next()
        .ok_or_else(|| FilterParseError::InvalidFunction(format!("drop-shadow({inner})")))?;
    if iter.next().is_some() {
        return Err(FilterParseError::InvalidFunction(
            "drop-shadow() takes a single shadow".to_owned(),
        ));
    }
    // drop-shadow is always a non-inset shadow.
    shadow.inset = false;
    Ok(shadow)
}

// ---------------------------------------------------------------------------
// ColorMatrix (SVG feColorMatrix shape, 4×5 row-major)
// ---------------------------------------------------------------------------

/// An SVG `feColorMatrix`-shaped colour matrix: 4 rows (r/g/b/a out) × 5
/// columns (r/g/b/a in + offset), row-major. Applied to a normalised sRGB
/// `[r, g, b, a, 1]` column vector:
///
/// ```text
/// r' = m[0]·r + m[1]·g + m[2]·b + m[3]·a + m[4]
/// g' = m[5]·r + m[6]·g + m[7]·b + m[8]·a + m[9]
/// b' = m[10]·r + m[11]·g + m[12]·b + m[13]·a + m[14]
/// a' = m[15]·r + m[16]·g + m[17]·b + m[18]·a + m[19]
/// ```
///
/// Two chained filters compose via matrix multiplication, so a `filter:
/// sepia(0.5) hue-rotate(40deg)` chain runs as one matrix multiply per
/// pixel — the optimisation the paint path uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorMatrix([f32; 20]);

impl ColorMatrix {
    /// The identity matrix (passes every pixel through unchanged).
    pub const IDENTITY: Self = Self([
        1.0, 0.0, 0.0, 0.0, 0.0, // r' = r
        0.0, 1.0, 0.0, 0.0, 0.0, // g' = g
        0.0, 0.0, 1.0, 0.0, 0.0, // b' = b
        0.0, 0.0, 0.0, 1.0, 0.0, // a' = a
    ]);

    /// Apply the matrix to a normalised `[r,g,b,a]` (all in `[0,1]`). The
    /// output is clamped back to `[0,1]` per pixel.
    pub fn apply(&self, rgba: [f32; 4]) -> [f32; 4] {
        let [r, g, b, a] = rgba;
        let m = &self.0;
        let clamp = |v: f32| v.clamp(0.0, 1.0);
        [
            clamp(m[0] * r + m[1] * g + m[2] * b + m[3] * a + m[4]),
            clamp(m[5] * r + m[6] * g + m[7] * b + m[8] * a + m[9]),
            clamp(m[10] * r + m[11] * g + m[12] * b + m[13] * a + m[14]),
            clamp(m[15] * r + m[16] * g + m[17] * b + m[18] * a + m[19]),
        ]
    }

    /// Compose two matrices: `self` applied after `other` (i.e. `other` runs
    /// first against the source pixels, then `self`). Matches the SVG filter
    /// chaining order (leftmost filter is applied first).
    pub fn combine(self, other: ColorMatrix) -> ColorMatrix {
        let a = &self.0;
        let b = &other.0;
        let mut out = [0.0f32; 20];
        for row in 0..4 {
            for col in 0..5 {
                let mut sum = 0.0;
                for k in 0..4 {
                    // a is row-major [row*5 + k], b is [k*5 + col].
                    sum += a[row * 5 + k] * b[k * 5 + col];
                }
                if col == 4 {
                    // The offset column: add a's own offset (b's offset
                    // column is the identity contribution `[0,0,0,0,1]`'s
                    // fifth element, which is 1 → a's offset carries
                    // through).
                    sum += a[row * 5 + 4];
                }
                out[row * 5 + col] = sum;
            }
        }
        ColorMatrix(out)
    }
}

impl Default for ColorMatrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Build the [`ColorMatrix`] for one per-pixel [`FilterFunction`]. Returns
/// `None` for `blur`/`drop-shadow` (those are spatial; the paint path runs
/// them around the composed matrix).
fn color_matrix_for(f: &FilterFunction) -> Option<ColorMatrix> {
    match *f {
        FilterFunction::Blur(_) | FilterFunction::DropShadow(_) => None,
        FilterFunction::Brightness(amt) => Some(brightness_matrix(amt)),
        FilterFunction::Contrast(amt) => Some(contrast_matrix(amt)),
        FilterFunction::Grayscale(amt) => Some(grayscale_matrix(amt.clamp(0.0, 1.0))),
        FilterFunction::HueRotate(deg) => Some(hue_rotate_matrix(deg)),
        FilterFunction::Invert(amt) => Some(invert_matrix(amt.clamp(0.0, 1.0))),
        FilterFunction::Opacity(amt) => Some(opacity_matrix(amt.clamp(0.0, 1.0))),
        FilterFunction::Saturate(amt) => Some(saturate_matrix(amt.max(0.0))),
        FilterFunction::Sepia(amt) => Some(sepia_matrix(amt.clamp(0.0, 1.0))),
    }
}

/// Fold a chain of per-pixel filters into a single [`ColorMatrix`]. Spatial
/// filters (`blur`/`drop-shadow`) are skipped (the caller applies them
/// around this matrix). Returns [`ColorMatrix::IDENTITY`] if the chain has
/// no per-pixel ops.
pub fn compose_color_matrix<'a, I>(filters: I) -> ColorMatrix
where
    I: IntoIterator<Item = &'a FilterFunction>,
{
    let mut acc = ColorMatrix::IDENTITY;
    for f in filters {
        if let Some(m) = color_matrix_for(f) {
            // Leftmost filter applied first: `f1` then `f2` ⇒ matrix is
            // `combine(f2, f1)` (f1 runs against source first). Combining
            // left-to-right with `acc = combine(m, acc)` keeps that order.
            acc = m.combine(acc);
        }
    }
    acc
}

impl FilterFunction {
    /// Apply a per-pixel filter to one [`Color`] (the colour-matrix family).
    /// `blur`/`drop-shadow` are spatial and return the input unchanged —
    /// the paint path applies them via a separate pass.
    pub fn apply(&self, c: Color) -> Color {
        match self {
            FilterFunction::Blur(_) | FilterFunction::DropShadow(_) => c,
            other => {
                let m = color_matrix_for(other).unwrap_or(ColorMatrix::IDENTITY);
                let rgba = [
                    c.r as f32 / 255.0,
                    c.g as f32 / 255.0,
                    c.b as f32 / 255.0,
                    c.a as f32 / 255.0,
                ];
                let [r, g, b, a] = m.apply(rgba);
                Color::rgba(
                    (r * 255.0).round().clamp(0.0, 255.0) as u8,
                    (g * 255.0).round().clamp(0.0, 255.0) as u8,
                    (b * 255.0).round().clamp(0.0, 255.0) as u8,
                    (a * 255.0).round().clamp(0.0, 255.0) as u8,
                )
            }
        }
    }
}

impl FilterList {
    /// Apply the full chain's per-pixel ops to one [`Color`]. Convenience
    /// for the colour-matrix family; the paint path uses
    /// [`compose_color_matrix`] for the optimised single-multiply path and
    /// runs `blur`/`drop-shadow` as separate passes.
    pub fn apply(&self, c: Color) -> Color {
        let m = compose_color_matrix(&self.functions);
        let rgba = [
            c.r as f32 / 255.0,
            c.g as f32 / 255.0,
            c.b as f32 / 255.0,
            c.a as f32 / 255.0,
        ];
        let [r, g, b, a] = m.apply(rgba);
        Color::rgba(
            (r * 255.0).round().clamp(0.0, 255.0) as u8,
            (g * 255.0).round().clamp(0.0, 255.0) as u8,
            (b * 255.0).round().clamp(0.0, 255.0) as u8,
            (a * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    }
}

// ---------------------------------------------------------------------------
// Per-function matrices (Filter Effects 1 § 5 + SVG filter primitive math)
// ---------------------------------------------------------------------------

/// `brightness(amount)`: per-channel scalar.
fn brightness_matrix(amount: f32) -> ColorMatrix {
    ColorMatrix([
        amount, 0.0, 0.0, 0.0, 0.0, 0.0, amount, 0.0, 0.0, 0.0, 0.0, 0.0, amount, 0.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
    ])
}

/// `contrast(amount)`: `(c - 0.5)·amount + 0.5`.
fn contrast_matrix(amount: f32) -> ColorMatrix {
    ColorMatrix([
        amount,
        0.0,
        0.0,
        0.0,
        0.5 * (1.0 - amount),
        0.0,
        amount,
        0.0,
        0.0,
        0.5 * (1.0 - amount),
        0.0,
        0.0,
        amount,
        0.0,
        0.5 * (1.0 - amount),
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
    ])
}

/// `invert(amount)`: `amount·(1-c) + (1-amount)·c` = `(1 - 2·amount)·c +
/// amount`. At `amount = 1` the coefficient is `-1` (full invert); at `0`
/// the coefficient is `1` (identity).
fn invert_matrix(amount: f32) -> ColorMatrix {
    let coeff = 1.0 - 2.0 * amount;
    ColorMatrix([
        coeff, 0.0, 0.0, 0.0, amount, 0.0, coeff, 0.0, 0.0, amount, 0.0, 0.0, coeff, 0.0, amount,
        0.0, 0.0, 0.0, 1.0, 0.0,
    ])
}

/// `opacity(amount)`: scale the alpha row.
fn opacity_matrix(amount: f32) -> ColorMatrix {
    ColorMatrix([
        1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        amount, 0.0,
    ])
}

/// `grayscale(amount)`: Rec.709-weighted desaturation matrix. `amount = 1`
/// is full grey; `0` is identity.
fn grayscale_matrix(amount: f32) -> ColorMatrix {
    let a = 1.0 - amount;
    ColorMatrix([
        // Row R.
        0.2126 + 0.7874 * a,
        0.7152 - 0.7152 * a,
        0.0722 - 0.0722 * a,
        0.0,
        0.0,
        // Row G.
        0.2126 - 0.2126 * a,
        0.7152 + 0.2848 * a,
        0.0722 - 0.0722 * a,
        0.0,
        0.0,
        // Row B.
        0.2126 - 0.2126 * a,
        0.7152 - 0.7152 * a,
        0.0722 + 0.9278 * a,
        0.0,
        0.0,
        // Row A.
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
    ])
}

/// `saturate(amount)`: colour-saturation matrix (amount = 0 is full grey, 1
/// identity, >1 oversaturated).
fn saturate_matrix(amount: f32) -> ColorMatrix {
    let s = amount;
    ColorMatrix([
        // Row R.
        0.213 + 0.787 * s,
        0.715 - 0.715 * s,
        0.072 - 0.072 * s,
        0.0,
        0.0,
        // Row G.
        0.213 - 0.213 * s,
        0.715 + 0.285 * s,
        0.072 - 0.072 * s,
        0.0,
        0.0,
        // Row B.
        0.213 - 0.213 * s,
        0.715 - 0.715 * s,
        0.072 + 0.928 * s,
        0.0,
        0.0,
        // Row A.
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
    ])
}

/// `sepia(amount)`: the sepia-tone matrix. `amount = 1` is full sepia, `0`
/// identity.
fn sepia_matrix(amount: f32) -> ColorMatrix {
    let a = 1.0 - amount;
    ColorMatrix([
        // Row R.
        0.393 + 0.607 * a,
        0.769 - 0.769 * a,
        0.189 - 0.189 * a,
        0.0,
        0.0,
        // Row G.
        0.349 - 0.349 * a,
        0.686 + 0.314 * a,
        0.168 - 0.168 * a,
        0.0,
        0.0,
        // Row B.
        0.272 - 0.272 * a,
        0.534 - 0.534 * a,
        0.131 + 0.869 * a,
        0.0,
        0.0,
        // Row A.
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
    ])
}

/// `hue-rotate(angle)`: rotate hue by `angle` degrees using the SVG filter
/// primitive's standard luminance-weighted rotation matrix.
fn hue_rotate_matrix(degrees: f32) -> ColorMatrix {
    let rad = degrees.to_radians();
    let (sin, cos) = rad.sin_cos();
    ColorMatrix([
        // Row R.
        0.213 + cos * 0.787 - sin * 0.213,
        0.715 - cos * 0.715 - sin * 0.715,
        0.072 - cos * 0.072 + sin * 0.928,
        0.0,
        0.0,
        // Row G.
        0.213 - cos * 0.213 + sin * 0.143,
        0.715 + cos * 0.285 + sin * 0.140,
        0.072 - cos * 0.072 - sin * 0.283,
        0.0,
        0.0,
        // Row B.
        0.213 - cos * 0.213 - sin * 0.787,
        0.715 - cos * 0.715 + sin * 0.715,
        0.072 + cos * 0.928 + sin * 0.072,
        0.0,
        0.0,
        // Row A.
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;

    fn approx_u8(a: u8, b: u8, tol: u8) -> bool {
        (a as i16 - b as i16).unsigned_abs() <= tol as u16
    }

    const TOL: u8 = 2;

    // --- Parse: single functions ---------------------------------------

    #[test]
    fn parse_blur_default_and_explicit() {
        assert_eq!(
            FilterFunction::parse("blur()").unwrap(),
            FilterFunction::Blur(0.0)
        );
        assert_eq!(
            FilterFunction::parse("blur(2px)").unwrap(),
            FilterFunction::Blur(2.0)
        );
        assert_eq!(
            FilterFunction::parse("blur(3.5)").unwrap(),
            FilterFunction::Blur(3.5)
        );
    }

    #[test]
    fn parse_blur_rejects_negative() {
        assert!(FilterFunction::parse("blur(-2px)").is_err());
    }

    #[test]
    fn parse_amount_default_and_percentage() {
        assert_eq!(
            FilterFunction::parse("brightness()").unwrap(),
            FilterFunction::Brightness(1.0)
        );
        assert_eq!(
            FilterFunction::parse("brightness(1.5)").unwrap(),
            FilterFunction::Brightness(1.5)
        );
        assert_eq!(
            FilterFunction::parse("brightness(150%)").unwrap(),
            FilterFunction::Brightness(1.5)
        );
        assert_eq!(
            FilterFunction::parse("grayscale()").unwrap(),
            FilterFunction::Grayscale(0.0)
        );
        assert_eq!(
            FilterFunction::parse("hue-rotate()").unwrap(),
            FilterFunction::HueRotate(0.0)
        );
    }

    #[test]
    fn parse_angle_units() {
        assert_eq!(
            FilterFunction::parse("hue-rotate(90deg)").unwrap(),
            FilterFunction::HueRotate(90.0)
        );
        assert_eq!(
            FilterFunction::parse("hue-rotate(0.25turn)").unwrap(),
            FilterFunction::HueRotate(90.0)
        );
        let rad = FilterFunction::parse("hue-rotate(1.5707963rad)").unwrap();
        match rad {
            FilterFunction::HueRotate(d) => assert!((d - 90.0).abs() < 0.1),
            _ => panic!(),
        }
    }

    #[test]
    fn parse_case_insensitive_names() {
        assert_eq!(
            FilterFunction::parse("BLUR(1px)").unwrap(),
            FilterFunction::Blur(1.0)
        );
        assert_eq!(
            FilterFunction::parse("  Sepia( 0.5 )  ").unwrap(),
            FilterFunction::Sepia(0.5)
        );
    }

    #[test]
    fn parse_url_fails_closed() {
        assert_eq!(
            FilterFunction::parse("url(#my-filter)").unwrap_err(),
            FilterParseError::UnsupportedUrl
        );
    }

    #[test]
    fn parse_unknown_function_fails_closed() {
        assert!(FilterFunction::parse("vivid(0.5)").is_err());
        assert!(FilterFunction::parse("blur").is_err());
    }

    #[test]
    fn parse_drop_shadow_basic() {
        let f = FilterFunction::parse("drop-shadow(2px 4px black)").unwrap();
        match f {
            FilterFunction::DropShadow(s) => {
                assert_eq!(s.offset_x, 2.0);
                assert_eq!(s.offset_y, 4.0);
                assert!(!s.inset);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parse_drop_shadow_rejects_multi() {
        // A comma introduces a second shadow; drop-shadow takes exactly one.
        assert!(FilterFunction::parse("drop-shadow(2px 4px red, 1px 1px blue)").is_err());
    }

    // --- Parse: list ---------------------------------------------------

    #[test]
    fn parse_list_none_and_empty() {
        assert!(FilterList::parse("none").unwrap().is_empty());
        assert!(FilterList::parse("").unwrap().is_empty());
        assert!(FilterList::parse("   ").unwrap().is_empty());
    }

    #[test]
    fn parse_list_multiple_functions_in_source_order() {
        let list = FilterList::parse("blur(2px) sepia(0.5) brightness(110%)").unwrap();
        assert_eq!(
            list.functions(),
            &[
                FilterFunction::Blur(2.0),
                FilterFunction::Sepia(0.5),
                FilterFunction::Brightness(1.1),
            ]
        );
    }

    #[test]
    fn parse_list_keeps_parenthesised_spaces_together() {
        // `drop-shadow(2px 4px black)` contains spaces but is one function.
        let list = FilterList::parse("drop-shadow(2px 4px black) blur(1px)").unwrap();
        assert_eq!(list.functions().len(), 2);
        assert!(matches!(list.functions()[0], FilterFunction::DropShadow(_)));
    }

    // --- Brightness / contrast / invert / opacity ----------------------

    #[test]
    fn brightness_zero_is_black() {
        let f = FilterFunction::Brightness(0.0);
        let out = f.apply(Color::rgb(255, 100, 50));
        assert_eq!(out, Color::rgb(0, 0, 0));
    }

    #[test]
    fn brightness_one_is_identity() {
        let f = FilterFunction::Brightness(1.0);
        assert_eq!(f.apply(Color::rgb(255, 100, 50)), Color::rgb(255, 100, 50));
    }

    #[test]
    fn brightness_two_doubles() {
        let f = FilterFunction::Brightness(2.0);
        let out = f.apply(Color::rgb(100, 50, 25));
        // 100*2 clamps to 255; 50*2 = 100; 25*2 = 50.
        assert_eq!(out, Color::rgb(200, 100, 50));
        let out2 = f.apply(Color::rgb(200, 0, 0));
        assert_eq!(out2, Color::rgb(255, 0, 0));
    }

    #[test]
    fn contrast_zero_is_mid_grey() {
        let f = FilterFunction::Contrast(0.0);
        let out = f.apply(Color::rgb(255, 0, 100));
        // (c - 0.5)*0 + 0.5 = 0.5 → 127/128.
        assert!(approx_u8(out.r, 128, TOL));
        assert!(approx_u8(out.g, 128, TOL));
        assert!(approx_u8(out.b, 128, TOL));
    }

    #[test]
    fn contrast_one_is_identity() {
        let f = FilterFunction::Contrast(1.0);
        assert_eq!(f.apply(Color::rgb(200, 100, 50)), Color::rgb(200, 100, 50));
    }

    #[test]
    fn invert_full_flips_channels() {
        let f = FilterFunction::Invert(1.0);
        let out = f.apply(Color::rgb(200, 100, 50));
        assert_eq!(out, Color::rgb(55, 155, 205));
    }

    #[test]
    fn invert_zero_is_identity() {
        let f = FilterFunction::Invert(0.0);
        assert_eq!(f.apply(Color::rgb(200, 100, 50)), Color::rgb(200, 100, 50));
    }

    #[test]
    fn opacity_scales_alpha() {
        let f = FilterFunction::Opacity(0.5);
        let out = f.apply(Color::rgba(200, 100, 50, 255));
        assert_eq!(out.a, 128);
    }

    // --- Grayscale / saturate / sepia ----------------------------------

    #[test]
    fn grayscale_full_desaturates() {
        let f = FilterFunction::Grayscale(1.0);
        let out = f.apply(Color::rgb(255, 0, 0));
        // Rec.709 luminance of red ≈ 0.2126 → ≈ 54 sRGB.
        assert!(approx_u8(out.r, 54, 3));
        assert!(approx_u8(out.g, 54, 3));
        assert!(approx_u8(out.b, 54, 3));
    }

    #[test]
    fn grayscale_zero_is_identity() {
        let f = FilterFunction::Grayscale(0.0);
        assert_eq!(f.apply(Color::rgb(255, 0, 0)), Color::rgb(255, 0, 0));
    }

    #[test]
    fn saturate_zero_matches_grayscale_one() {
        // saturate(0) and grayscale(1) both fully desaturate.
        let s = FilterFunction::Saturate(0.0).apply(Color::rgb(255, 100, 50));
        let g = FilterFunction::Grayscale(1.0).apply(Color::rgb(255, 100, 50));
        assert!(approx_u8(s.r, g.r, 2));
        assert!(approx_u8(s.g, g.g, 2));
        assert!(approx_u8(s.b, g.b, 2));
    }

    #[test]
    fn saturate_one_is_identity() {
        let f = FilterFunction::Saturate(1.0);
        let out = f.apply(Color::rgb(200, 100, 50));
        assert_eq!(out, Color::rgb(200, 100, 50));
    }

    #[test]
    fn sepia_zero_is_identity() {
        let f = FilterFunction::Sepia(0.0);
        assert_eq!(f.apply(Color::rgb(200, 100, 50)), Color::rgb(200, 100, 50));
    }

    #[test]
    fn sepia_full_warms_white() {
        let f = FilterFunction::Sepia(1.0);
        let out = f.apply(Color::WHITE);
        // Classic sepia of white ≈ (255, 250, 220)-ish (warm).
        assert!(out.r > out.b, "sepia(white) should be warm: {out:?}");
        assert!(out.g > out.b);
    }

    // --- hue-rotate ----------------------------------------------------

    #[test]
    fn hue_rotate_zero_is_identity() {
        let f = FilterFunction::HueRotate(0.0);
        let out = f.apply(Color::rgb(200, 100, 50));
        assert_eq!(out, Color::rgb(200, 100, 50));
    }

    #[test]
    fn hue_rotate_full_circle_is_identity() {
        // 360° ≈ 0° modulo rounding.
        let f = FilterFunction::HueRotate(360.0);
        let out = f.apply(Color::rgb(200, 100, 50));
        assert!(approx_u8(out.r, 200, 3));
        assert!(approx_u8(out.g, 100, 3));
        assert!(approx_u8(out.b, 50, 3));
    }

    #[test]
    fn hue_rotate_moves_primary_hue() {
        let f = FilterFunction::HueRotate(120.0);
        let out = f.apply(Color::rgb(255, 0, 0));
        // A 120° hue rotation of pure red moves the dominant energy off the
        // red channel — red should drop well below 255.
        assert!(out.r < 200, "hue-rotate(120deg) of red: {out:?}");
    }

    // --- blur / drop-shadow are spatial (identity in apply) ------------

    #[test]
    fn blur_is_identity_under_apply() {
        let f = FilterFunction::Blur(5.0);
        assert_eq!(f.apply(Color::rgb(10, 20, 30)), Color::rgb(10, 20, 30));
    }

    #[test]
    fn drop_shadow_is_identity_under_apply() {
        let f = FilterFunction::parse("drop-shadow(2px 4px black)").unwrap();
        assert_eq!(f.apply(Color::rgb(10, 20, 30)), Color::rgb(10, 20, 30));
    }

    // --- ColorMatrix composition ---------------------------------------

    #[test]
    fn matrix_identity_passes_through() {
        let out = ColorMatrix::IDENTITY.apply([0.8, 0.4, 0.2, 1.0]);
        assert_eq!(out, [0.8, 0.4, 0.2, 1.0]);
    }

    #[test]
    fn matrix_combine_leftmost_applied_first() {
        // brightness(2.0) then contrast(0.0): source 0.6 → brightness → 1.0
        // (clamped) → contrast(0) → 0.5. So the combined matrix maps 0.6 to
        // 0.5.
        let bm = brightness_matrix(2.0);
        let cm = contrast_matrix(0.0);
        // brightness first against source: combined = cm ∘ bm.
        let combined = cm.combine(bm);
        let out = combined.apply([0.6, 0.6, 0.6, 1.0]);
        assert!((out[0] - 0.5).abs() < 0.02, "got {}", out[0]);
    }

    #[test]
    fn compose_color_matrix_skips_spatial_filters() {
        // The spatial filters (blur, drop-shadow) are skipped; the per-pixel
        // ones compose.
        let list = FilterList::parse("blur(2px) brightness(0.0)").unwrap();
        let m = compose_color_matrix(list.functions());
        let out = m.apply([1.0, 1.0, 1.0, 1.0]);
        // brightness(0) zeroes colour channels.
        assert_eq!(out, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn list_apply_runs_full_chain() {
        let list = FilterList::parse("brightness(2.0) contrast(0.0)").unwrap();
        // 0.6 → ×2 → 1.0 (clamp) → contrast(0) → 0.5 → ≈ 128 sRGB.
        let out = list.apply(Color::rgb(153, 153, 153)); // 153/255 ≈ 0.6
        assert!(approx_u8(out.r, 128, 3), "got {}", out.r);
    }

    #[test]
    fn list_apply_empty_is_identity() {
        let list = FilterList::parse("none").unwrap();
        assert_eq!(list.apply(Color::rgb(10, 20, 30)), Color::rgb(10, 20, 30));
    }

    #[test]
    fn compose_color_matrix_empty_is_identity() {
        let m = compose_color_matrix(std::iter::empty());
        assert_eq!(m, ColorMatrix::IDENTITY);
    }
}
