//! CSS Backgrounds 3 § 6 — `border-image` nine-slice model (pure logic). The
//! paint geometry the `border-image-*` longhands reduce to. Complements
//! [`crate::border_radius`] (the corner-shaping the rounded-border path
//! uses) and [`crate::background_position`] (the `background-*` family).
//!
//! What lives here:
//! - [`BorderImageSlice`] / [`BorderImageWidths`] / [`BorderImageOutset`] /
//!   [`BorderImageRepeat`] — the four § 6 longhands with full 1–4 value
//!   TRBL expansion + parse.
//! - [`Insets`] — absolute-pixel slice insets resolved against the image
//!   dimensions ([`BorderImageSlice::resolve`]).
//! - [`NineRegions`] — the 3×3 source/destination grid the nine-slice
//!   algorithm carves out ([`source_regions`] / [`destination_regions`]).
//! - [`tile_edge`] — the `border-image-repeat` tiling primitive
//!   (`stretch`/`repeat`/`round`/`space`) the paint path runs over each
//!   edge.
//!
//! What does *not* live here:
//! - The actual raster draw (paint path; WebRender's nine-patch image
//!   segment). This module hands it the 9 source → destination rects + the
//!   per-edge tile plan.
//! - `border-image-source` `<image>` resolution (the `<url>`/`<gradient>`
//!   family lives with the resource loader + [`crate::gradient`]).
//! - The cascade-time resolution of `border-image-width: auto` to the
//!   matching slice dimension (handled in [`BorderImageWidthValue::resolve`]
//!   given the resolved source-inset dimensions).
//!
//! ## TRBL expansion
//!
//! Every 1–4 value longhand uses the CSS TRBL (top/right/bottom/left)
//! expansion: 1 value sets all four; 2 values are `vertical horizontal`; 3
//! are `top horizontal bottom`; 4 are `top right bottom left` verbatim. The
//! § 6.1 grammar for `border-image-slice` &c. inherits this unchanged.
//!
//! Reference: <https://www.w3.org/TR/css-backgrounds-3/#border-images>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Shared geometry
// ---------------------------------------------------------------------------

/// An axis-aligned rectangle in CSS pixels. Local to this module (the
/// nine-slice concern); the paint path adapts these into its own coordinate
/// space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    /// Right edge x-coordinate.
    pub const fn right(&self) -> f32 {
        self.x + self.w
    }
    /// Bottom edge y-coordinate.
    pub const fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// The four absolute-pixel insets (top/right/bottom/left) the slice values
/// resolve to against the image dimensions. Used for both the source-image
/// grid and the destination grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Insets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

/// A 3×3 grid of [`Rect`]s carved out of a source image or a destination
/// box. The eight perimeter regions are the corners + edges; `center` is
/// the § 6.6 `fill` region (only painted when `border-image-slice` has the
/// `fill` keyword).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NineRegions {
    pub top_left: Rect,
    pub top: Rect,
    pub top_right: Rect,
    pub right: Rect,
    pub bottom_right: Rect,
    pub bottom: Rect,
    pub bottom_left: Rect,
    pub left: Rect,
    pub center: Rect,
}

/// Build the 9 regions of a 3×3 grid given the outer rect, the resolved
/// insets, and whether to clip the center to nothing (when `fill` is false
/// the center is dropped at paint time — the rect is still computed so the
/// caller can reason about geometry; `fill` is the caller's gate). The
/// horizontal grid lines are `left` / `outer.w - right`; vertical are `top` /
/// `outer.h - bottom`.
fn carve(outer: Rect, ins: Insets) -> NineRegions {
    let x1 = outer.x + ins.left;
    let x2 = outer.right() - ins.right;
    let y1 = outer.y + ins.top;
    let y2 = outer.bottom() - ins.bottom;
    // Clamp degenerate slices (ins overlapping) to a 0-width seam so the
    // centre collapses rather than going negative.
    let x2 = x2.max(x1);
    let y2 = y2.max(y1);
    NineRegions {
        top_left: Rect::new(outer.x, outer.y, ins.left, ins.top),
        top: Rect::new(x1, outer.y, x2 - x1, ins.top),
        top_right: Rect::new(x2, outer.y, outer.right() - x2, ins.top),
        right: Rect::new(x2, y1, outer.right() - x2, y2 - y1),
        bottom_right: Rect::new(x2, y2, outer.right() - x2, outer.bottom() - y2),
        bottom: Rect::new(x1, y2, x2 - x1, outer.bottom() - y2),
        bottom_left: Rect::new(outer.x, y2, ins.left, outer.bottom() - y2),
        left: Rect::new(outer.x, y1, ins.left, y2 - y1),
        center: Rect::new(x1, y1, x2 - x1, y2 - y1),
    }
}

// ---------------------------------------------------------------------------
// border-image-slice (§ 6.4)
// ---------------------------------------------------------------------------

/// One `border-image-slice` value: either a number (image pixels for raster,
/// user units for SVG) or a percentage of the image dimension.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SliceInset {
    /// A bare number — image pixels (raster) / user units (SVG).
    Number(f32),
    /// A percentage of the image's width (for left/right) or height (for
    /// top/bottom), stored as a fraction `0..=1`.
    Percentage(f32),
}

impl SliceInset {
    /// Resolve to absolute pixels against a dimension. Numbers pass through;
    /// percentages multiply the dimension.
    pub fn resolve(self, dimension: f32) -> f32 {
        match self {
            SliceInset::Number(n) => n,
            SliceInset::Percentage(p) => p * dimension,
        }
    }
}

/// `border-image-slice` — the four insets that carve the image into the 3×3
/// grid, plus the optional `fill` keyword (§ 6.1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderImageSlice {
    pub top: SliceInset,
    pub right: SliceInset,
    pub bottom: SliceInset,
    pub left: SliceInset,
    /// Whether the center region is preserved (`fill`). Default `false`.
    pub fill: bool,
}

impl Default for BorderImageSlice {
    /// § 6.1: the initial value is `100%` (one region, no slicing).
    fn default() -> Self {
        Self {
            top: SliceInset::Percentage(1.0),
            right: SliceInset::Percentage(1.0),
            bottom: SliceInset::Percentage(1.0),
            left: SliceInset::Percentage(1.0),
            fill: false,
        }
    }
}

impl BorderImageSlice {
    /// Parse `border-image-slice: <number|percentage>{1,4} fill?`. The
    /// `fill` keyword may appear anywhere in the list (§ 6.1 grammar).
    ///
    /// ```
    /// # use vixen_engine::border_image::{BorderImageSlice, SliceInset};
    /// let s = BorderImageSlice::parse("25% 30% fill").unwrap();
    /// assert_eq!(s.top, SliceInset::Percentage(0.25));
    /// assert_eq!(s.right, SliceInset::Percentage(0.30));
    /// assert!(s.fill);
    /// ```
    pub fn parse(s: &str) -> Result<Self, BorderImageParseError> {
        let mut values: Vec<SliceInset> = Vec::new();
        let mut fill = false;
        for tok in s.split_whitespace() {
            if tok.eq_ignore_ascii_case("fill") {
                fill = true;
                continue;
            }
            values.push(parse_slice_value(tok)?);
        }
        let [t, r, b, l] = expand_trbl(&values)?;
        Ok(Self {
            top: t,
            right: r,
            bottom: b,
            left: l,
            fill,
        })
    }

    /// Resolve the four insets against the image dimensions (top/bottom
    /// resolve against `image_h`; left/right against `image_w`).
    pub fn resolve(&self, image_w: f32, image_h: f32) -> Insets {
        Insets {
            top: self.top.resolve(image_h),
            right: self.right.resolve(image_w),
            bottom: self.bottom.resolve(image_h),
            left: self.left.resolve(image_w),
        }
    }
}

fn parse_slice_value(tok: &str) -> Result<SliceInset, BorderImageParseError> {
    if let Some(rest) = tok.strip_suffix('%') {
        let v: f32 = rest.trim().parse()?;
        if v < 0.0 {
            return Err(BorderImageParseError::NegativeValue(tok.to_owned()));
        }
        return Ok(SliceInset::Percentage(v / 100.0));
    }
    let v: f32 = tok.parse()?;
    if v < 0.0 {
        return Err(BorderImageParseError::NegativeValue(tok.to_owned()));
    }
    Ok(SliceInset::Number(v))
}

// ---------------------------------------------------------------------------
// border-image-width (§ 6.3)
// ---------------------------------------------------------------------------

/// One `border-image-width` value: a length, a percentage of the border-box,
/// a multiple of the computed border width, or `auto`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BorderImageWidthValue {
    /// A CSS-px length.
    Length(f32),
    /// A percentage of the border-box dimension (width for the L/R sides,
    /// height for T/B).
    Percentage(f32),
    /// A multiple of the computed `border-width` for that side.
    Number(f32),
    /// `auto` — the intrinsic size of the corresponding source slice (the
    /// matched inset's dimension).
    Auto,
}

impl BorderImageWidthValue {
    /// Resolve to an absolute pixel width. `border_dim` is the border-box
    /// dimension perpendicular to the side (width for L/R, height for T/B);
    /// `border_width` is the side's computed `border-width`; `slice_dim` is
    /// the corresponding source-slice dimension (used for `auto`).
    pub fn resolve(&self, border_dim: f32, border_width: f32, slice_dim: f32) -> f32 {
        match self {
            BorderImageWidthValue::Length(l) => *l,
            BorderImageWidthValue::Percentage(p) => p * border_dim,
            BorderImageWidthValue::Number(n) => n * border_width,
            BorderImageWidthValue::Auto => slice_dim,
        }
        .max(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BorderImageWidths {
    pub top: BorderImageWidthValue,
    pub right: BorderImageWidthValue,
    pub bottom: BorderImageWidthValue,
    pub left: BorderImageWidthValue,
}

impl Default for BorderImageWidthValue {
    /// § 6.1 initial value is `1` (a number: 1× the border width).
    fn default() -> Self {
        BorderImageWidthValue::Number(1.0)
    }
}

impl BorderImageWidths {
    /// Parse `border-image-width: [ <length> | <percentage> | <number> | auto ]{1,4}`.
    pub fn parse(s: &str) -> Result<Self, BorderImageParseError> {
        let values: Vec<BorderImageWidthValue> = s
            .split_whitespace()
            .map(parse_width_value)
            .collect::<Result<_, _>>()?;
        let [t, r, b, l] = expand_trbl(&values)?;
        Ok(Self {
            top: t,
            right: r,
            bottom: b,
            left: l,
        })
    }
}

fn parse_width_value(tok: &str) -> Result<BorderImageWidthValue, BorderImageParseError> {
    if tok.eq_ignore_ascii_case("auto") {
        return Ok(BorderImageWidthValue::Auto);
    }
    if let Some(rest) = tok.strip_suffix('%') {
        let v: f32 = rest.trim().parse()?;
        if v < 0.0 {
            return Err(BorderImageParseError::NegativeValue(tok.to_owned()));
        }
        return Ok(BorderImageWidthValue::Percentage(v / 100.0));
    }
    // Distinguish number (unitless) from length (px). CSS-px only here; the
    // other units reduce via the cascade.
    if let Some(rest) = tok.strip_suffix("px") {
        let v: f32 = rest.trim().parse()?;
        if v < 0.0 {
            return Err(BorderImageParseError::NegativeValue(tok.to_owned()));
        }
        return Ok(BorderImageWidthValue::Length(v));
    }
    let v: f32 = tok.parse()?;
    if v < 0.0 {
        return Err(BorderImageParseError::NegativeValue(tok.to_owned()));
    }
    Ok(BorderImageWidthValue::Number(v))
}

// ---------------------------------------------------------------------------
// border-image-outset (§ 6.4)
// ---------------------------------------------------------------------------

/// One `border-image-outset` value: a CSS-px length or a multiple of the
/// border width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutsetValue {
    Length(f32),
    Number(f32),
}

impl OutsetValue {
    pub fn resolve(&self, border_width: f32) -> f32 {
        match self {
            OutsetValue::Length(l) => *l,
            OutsetValue::Number(n) => n * border_width,
        }
        .max(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BorderImageOutset {
    pub top: OutsetValue,
    pub right: OutsetValue,
    pub bottom: OutsetValue,
    pub left: OutsetValue,
}

impl Default for OutsetValue {
    fn default() -> Self {
        OutsetValue::Length(0.0)
    }
}

impl BorderImageOutset {
    /// Parse `border-image-outset: [ <length> | <number> ]{1,4}`.
    pub fn parse(s: &str) -> Result<Self, BorderImageParseError> {
        let values: Vec<OutsetValue> = s
            .split_whitespace()
            .map(parse_outset_value)
            .collect::<Result<_, _>>()?;
        let [t, r, b, l] = expand_trbl(&values)?;
        Ok(Self {
            top: t,
            right: r,
            bottom: b,
            left: l,
        })
    }
}

fn parse_outset_value(tok: &str) -> Result<OutsetValue, BorderImageParseError> {
    if let Some(rest) = tok.strip_suffix("px") {
        let v: f32 = rest.trim().parse()?;
        if v < 0.0 {
            return Err(BorderImageParseError::NegativeValue(tok.to_owned()));
        }
        return Ok(OutsetValue::Length(v));
    }
    let v: f32 = tok.parse()?;
    if v < 0.0 {
        return Err(BorderImageParseError::NegativeValue(tok.to_owned()));
    }
    Ok(OutsetValue::Number(v))
}

// ---------------------------------------------------------------------------
// border-image-repeat (§ 6.5)
// ---------------------------------------------------------------------------

/// One `border-image-repeat` keyword: how an edge region is tiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BorderImageRepeat {
    /// `stretch` — the source edge is stretched to fill the destination
    /// (the default; one tile, scaled).
    #[default]
    Stretch,
    /// `repeat` — the source edge is tiled at its natural size; partial tiles
    /// at the ends are clipped (no rescaling).
    Repeat,
    /// `round` — the source edge is tiled N times where N is the nearest
    /// integer to `dest_size / source_size`; the tiles are rescaled so N
    /// exactly fit.
    Round,
    /// `space` — the source edge is tiled at natural size; if the last tile
    /// overflows, the overflow is distributed as even half-gaps between
    /// tiles.
    Space,
}

impl BorderImageRepeat {
    pub fn parse(tok: &str) -> Option<Self> {
        match tok.trim().to_ascii_lowercase().as_str() {
            "stretch" => Some(BorderImageRepeat::Stretch),
            "repeat" => Some(BorderImageRepeat::Repeat),
            "round" => Some(BorderImageRepeat::Round),
            "space" => Some(BorderImageRepeat::Space),
            _ => None,
        }
    }
}

/// The horizontal + vertical `border-image-repeat` pair. One value sets
/// both axes; two values are `[horizontal] [vertical]` per § 6.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BorderImageRepeatPair {
    pub horizontal: BorderImageRepeat,
    pub vertical: BorderImageRepeat,
}

impl BorderImageRepeatPair {
    /// Parse `border-image-repeat: [ stretch | repeat | round | space ]{1,2}`.
    pub fn parse(s: &str) -> Result<Self, BorderImageParseError> {
        let toks: Vec<&str> = s.split_whitespace().collect();
        let pair = match toks.as_slice() {
            [h] => Self {
                horizontal: BorderImageRepeat::parse(h)
                    .ok_or_else(|| BorderImageParseError::UnknownKeyword(h.to_string()))?,
                vertical: BorderImageRepeat::parse(h).unwrap(),
            },
            [h, v] => Self {
                horizontal: BorderImageRepeat::parse(h)
                    .ok_or_else(|| BorderImageParseError::UnknownKeyword(h.to_string()))?,
                vertical: BorderImageRepeat::parse(v)
                    .ok_or_else(|| BorderImageParseError::UnknownKeyword(v.to_string()))?,
            },
            _ => return Err(BorderImageParseError::TooManyValues),
        };
        Ok(pair)
    }
}

// ---------------------------------------------------------------------------
// TRBL expansion (shared) + parse error
// ---------------------------------------------------------------------------

/// Parse error for the four `border-image-*` longhands.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BorderImageParseError {
    #[error("expected 1–4 values")]
    TooManyValues,
    #[error("negative value: {0:?}")]
    NegativeValue(String),
    #[error("unknown keyword: {0:?}")]
    UnknownKeyword(String),
    #[error("invalid number: {0}")]
    InvalidNumber(#[from] std::num::ParseFloatError),
}

/// Expand a 1–4 value slice into `[top, right, bottom, left]` per the CSS
/// TRBL convention. Returns an error on 0 or > 4 values.
fn expand_trbl<T: Copy>(values: &[T]) -> Result<[T; 4], BorderImageParseError> {
    if values.is_empty() {
        return Err(BorderImageParseError::TooManyValues);
    }
    let (t, r, b, l) = match values {
        [v] => (*v, *v, *v, *v),
        [v, h] => (*v, *h, *v, *h),
        [t, h, b] => (*t, *h, *b, *h),
        [t, r, b, l] => (*t, *r, *b, *l),
        _ => return Err(BorderImageParseError::TooManyValues),
    };
    Ok([t, r, b, l])
}

// ---------------------------------------------------------------------------
// Nine-region geometry (§ 6.6 "Drawing the Border Image")
// ---------------------------------------------------------------------------

/// Carve the source image into its 9 regions using the resolved slice insets.
/// `image_w`/`image_h` are the image's intrinsic size in CSS px.
pub fn source_regions(image_w: f32, image_h: f32, slice: &BorderImageSlice) -> NineRegions {
    let ins = slice.resolve(image_w, image_h);
    carve(Rect::new(0.0, 0.0, image_w, image_h), ins)
}

/// Carve the destination (the border box, optionally outset-extended) into
/// its 9 regions using the resolved widths. `border_box` is the box the
/// border image is painted around; `widths_resolved` is the four border-
/// image widths in absolute px (from [`BorderImageWidths`] resolved against
/// the border-box + border widths + source-slice dims); `outset_resolved`
/// is the four outset values in absolute px.
pub fn destination_regions(
    border_box: Rect,
    widths_resolved: Insets,
    outset_resolved: Insets,
) -> NineRegions {
    // The outset extends the border box outward on each side.
    let outer = Rect::new(
        border_box.x - outset_resolved.left,
        border_box.y - outset_resolved.top,
        border_box.w + outset_resolved.left + outset_resolved.right,
        border_box.h + outset_resolved.top + outset_resolved.bottom,
    );
    carve(outer, widths_resolved)
}

// ---------------------------------------------------------------------------
// Edge tiling (border-image-repeat)
// ---------------------------------------------------------------------------

/// The tiling plan for one edge: a list of (source-x-offset, dest-x,
/// dest-w) triples — the paint path draws `source[edge_w]` starting at each
/// `source-x-offset`, scaled to `dest-w`, at the destination x. For the
/// non-tiling `stretch` mode there's a single tile that spans the whole
/// edge.
#[derive(Debug, Clone, PartialEq)]
pub struct TilePlan {
    /// The (source offset, destination x, destination width) triples. The
    /// source offset is relative to the edge's source region x; the paint
    /// path adds the region's source x.
    pub tiles: Vec<(f32, f32, f32)>,
}

/// Plan the tiling of one edge along its main axis. `source_size` is the
/// source edge's dimension (the natural tile size); `dest_size` is the
/// destination edge's dimension to fill; `mode` is the repeat keyword. The
/// returned [`TilePlan`]'s destination x-coordinates are relative to the
/// edge start (the caller adds the edge's destination x).
///
/// - `stretch` — one tile scaled to `dest_size`.
/// - `repeat` — N = floor(dest/source) full tiles + a clipped final partial;
///   tiles keep their natural size.
/// - `round` — N = round(dest/source); each tile scaled to `dest/N` so the
///   integer count fits exactly.
/// - `space` — N = floor(dest/source) full tiles; the leftover is split into
///   even half-gaps distributed around each tile.
pub fn tile_edge(source_size: f32, dest_size: f32, mode: BorderImageRepeat) -> TilePlan {
    if source_size <= 0.0 || dest_size <= 0.0 {
        return TilePlan { tiles: vec![] };
    }
    match mode {
        BorderImageRepeat::Stretch => TilePlan {
            tiles: vec![(0.0, 0.0, dest_size)],
        },
        BorderImageRepeat::Repeat => {
            // Full natural-size tiles; the last one is clipped by the paint
            // path when its dest-x + dest-w > dest_size. We emit enough tiles
            // to cover the destination.
            let n = (dest_size / source_size).ceil() as usize;
            let mut tiles = Vec::with_capacity(n);
            let mut x = 0.0;
            while x < dest_size {
                tiles.push((0.0, x, source_size));
                x += source_size;
            }
            TilePlan { tiles }
        }
        BorderImageRepeat::Round => {
            // Round the natural count, then rescale so N tiles exactly fit.
            let n = (dest_size / source_size).round().max(1.0) as usize;
            let scaled = dest_size / n as f32;
            let mut tiles = Vec::with_capacity(n);
            for i in 0..n {
                tiles.push((0.0, i as f32 * scaled, scaled));
            }
            TilePlan { tiles }
        }
        BorderImageRepeat::Space => {
            // Floor the natural count; distribute the leftover as even gaps.
            let n = (dest_size / source_size).floor() as usize;
            if n == 0 {
                // Source larger than dest: one centred tile (no rescale per §).
                let offset = (dest_size - source_size) / 2.0;
                return TilePlan {
                    tiles: vec![(0.0, offset, source_size)],
                };
            }
            let used = n as f32 * source_size;
            let gap = (dest_size - used) / (n + 1) as f32;
            let mut tiles = Vec::with_capacity(n);
            for i in 0..n {
                let x = gap + i as f32 * (source_size + gap);
                tiles.push((0.0, x, source_size));
            }
            TilePlan { tiles }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-4;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    // --- TRBL expansion + slice parse ----------------------------------

    #[test]
    fn slice_default_is_100_percent() {
        let s = BorderImageSlice::default();
        assert_eq!(s.top, SliceInset::Percentage(1.0));
        assert_eq!(s.left, SliceInset::Percentage(1.0));
        assert!(!s.fill);
    }

    #[test]
    fn slice_parse_one_value() {
        let s = BorderImageSlice::parse("25%").unwrap();
        for (v, name) in [
            (s.top, "top"),
            (s.right, "right"),
            (s.bottom, "bottom"),
            (s.left, "left"),
        ] {
            assert_eq!(v, SliceInset::Percentage(0.25), "{name}");
        }
    }

    #[test]
    fn slice_parse_two_three_four_values() {
        let two = BorderImageSlice::parse("10 20").unwrap();
        assert_eq!(two.top, SliceInset::Number(10.0));
        assert_eq!(two.right, SliceInset::Number(20.0));
        assert_eq!(two.bottom, SliceInset::Number(10.0));
        assert_eq!(two.left, SliceInset::Number(20.0));

        let three = BorderImageSlice::parse("10 20 30").unwrap();
        assert_eq!(three.top, SliceInset::Number(10.0));
        assert_eq!(three.right, SliceInset::Number(20.0));
        assert_eq!(three.bottom, SliceInset::Number(30.0));
        assert_eq!(three.left, SliceInset::Number(20.0));

        let four = BorderImageSlice::parse("1 2 3 4").unwrap();
        assert_eq!(four.top, SliceInset::Number(1.0));
        assert_eq!(four.right, SliceInset::Number(2.0));
        assert_eq!(four.bottom, SliceInset::Number(3.0));
        assert_eq!(four.left, SliceInset::Number(4.0));
    }

    #[test]
    fn slice_parse_fill_keyword_anywhere() {
        let s = BorderImageSlice::parse("fill 25% 25%").unwrap();
        assert!(s.fill);
        let s2 = BorderImageSlice::parse("25% fill 25%").unwrap();
        assert!(s2.fill);
        let s3 = BorderImageSlice::parse("25% 25%").unwrap();
        assert!(!s3.fill);
    }

    #[test]
    fn slice_parse_rejects_negative() {
        assert!(BorderImageSlice::parse("-10%").is_err());
        assert!(BorderImageSlice::parse("25% -10").is_err());
    }

    #[test]
    fn slice_parse_rejects_too_many_values() {
        assert!(BorderImageSlice::parse("1 2 3 4 5").is_err());
    }

    #[test]
    fn slice_resolve_against_dimensions() {
        let s = BorderImageSlice::parse("25% 50%").unwrap();
        let ins = s.resolve(100.0, 200.0);
        // top/bottom = 25% of height(200) = 50; left/right = 50% of width(100) = 50.
        assert!(approx(ins.top, 50.0));
        assert!(approx(ins.bottom, 50.0));
        assert!(approx(ins.left, 50.0));
        assert!(approx(ins.right, 50.0));
    }

    // --- width parse + resolve -----------------------------------------

    #[test]
    fn width_default_is_number_one() {
        let w = BorderImageWidths::default();
        assert_eq!(w.top, BorderImageWidthValue::Number(1.0));
    }

    #[test]
    fn width_parse_mixed_forms() {
        let w = BorderImageWidths::parse("10px 20% 3 auto").unwrap();
        assert_eq!(w.top, BorderImageWidthValue::Length(10.0));
        assert_eq!(w.right, BorderImageWidthValue::Percentage(0.20));
        assert_eq!(w.bottom, BorderImageWidthValue::Number(3.0));
        assert_eq!(w.left, BorderImageWidthValue::Auto);
    }

    #[test]
    fn width_resolve_each_form() {
        let length = BorderImageWidthValue::Length(15.0);
        assert!(approx(length.resolve(0.0, 0.0, 0.0), 15.0));
        let pct = BorderImageWidthValue::Percentage(0.5);
        assert!(approx(pct.resolve(100.0, 0.0, 0.0), 50.0));
        let num = BorderImageWidthValue::Number(2.0);
        assert!(approx(num.resolve(0.0, 3.0, 0.0), 6.0));
        let auto = BorderImageWidthValue::Auto;
        assert!(approx(auto.resolve(0.0, 0.0, 42.0), 42.0));
    }

    #[test]
    fn width_resolve_clamps_negative_to_zero() {
        let w = BorderImageWidthValue::Length(-5.0);
        assert_eq!(w.resolve(0.0, 0.0, 0.0), 0.0);
    }

    // --- outset parse --------------------------------------------------

    #[test]
    fn outset_default_is_zero_length() {
        let o = BorderImageOutset::default();
        assert_eq!(o.top, OutsetValue::Length(0.0));
    }

    #[test]
    fn outset_parse_length_and_number() {
        let o = BorderImageOutset::parse("5px 2").unwrap();
        assert_eq!(o.top, OutsetValue::Length(5.0));
        assert_eq!(o.right, OutsetValue::Number(2.0));
        assert_eq!(o.bottom, OutsetValue::Length(5.0));
        assert_eq!(o.left, OutsetValue::Number(2.0));
    }

    #[test]
    fn outset_resolve_number_against_border_width() {
        let o = BorderImageOutset {
            top: OutsetValue::Number(2.0),
            right: OutsetValue::Number(2.0),
            bottom: OutsetValue::Number(2.0),
            left: OutsetValue::Number(2.0),
        };
        // 2 × a 3px border-width = 6px.
        assert!(approx(o.top.resolve(3.0), 6.0));
    }

    // --- repeat parse --------------------------------------------------

    #[test]
    fn repeat_default_is_stretch_both() {
        let r = BorderImageRepeatPair::default();
        assert_eq!(r.horizontal, BorderImageRepeat::Stretch);
        assert_eq!(r.vertical, BorderImageRepeat::Stretch);
    }

    #[test]
    fn repeat_one_value_applies_both_axes() {
        let r = BorderImageRepeatPair::parse("round").unwrap();
        assert_eq!(r.horizontal, BorderImageRepeat::Round);
        assert_eq!(r.vertical, BorderImageRepeat::Round);
    }

    #[test]
    fn repeat_two_values_are_h_then_v() {
        let r = BorderImageRepeatPair::parse("round space").unwrap();
        assert_eq!(r.horizontal, BorderImageRepeat::Round);
        assert_eq!(r.vertical, BorderImageRepeat::Space);
    }

    #[test]
    fn repeat_unknown_keyword_fails_closed() {
        assert!(BorderImageRepeatPair::parse("mirror").is_err());
        assert!(BorderImageRepeatPair::parse("stretch repeat extra").is_err());
    }

    // --- nine-region carving -------------------------------------------

    #[test]
    fn source_regions_3x3_grid() {
        // 100×100 image, slices 25 all sides → four 25×25 corners, four
        // 50×25 edges, 50×50 centre.
        let slice = BorderImageSlice::parse("25 25 25 25").unwrap();
        let g = source_regions(100.0, 100.0, &slice);
        assert_eq!(g.top_left, Rect::new(0.0, 0.0, 25.0, 25.0));
        assert_eq!(g.top, Rect::new(25.0, 0.0, 50.0, 25.0));
        assert_eq!(g.top_right, Rect::new(75.0, 0.0, 25.0, 25.0));
        assert_eq!(g.right, Rect::new(75.0, 25.0, 25.0, 50.0));
        assert_eq!(g.bottom_right, Rect::new(75.0, 75.0, 25.0, 25.0));
        assert_eq!(g.bottom, Rect::new(25.0, 75.0, 50.0, 25.0));
        assert_eq!(g.bottom_left, Rect::new(0.0, 75.0, 25.0, 25.0));
        assert_eq!(g.left, Rect::new(0.0, 25.0, 25.0, 50.0));
        assert_eq!(g.center, Rect::new(25.0, 25.0, 50.0, 50.0));
    }

    #[test]
    fn source_regions_asymmetric_slices() {
        // Slices 10/20/30/40 → grid lines at x=40, x=100-20=80; y=10, y=100-30=70.
        let slice = BorderImageSlice::parse("10 20 30 40").unwrap();
        let g = source_regions(100.0, 100.0, &slice);
        assert_eq!(g.top, Rect::new(40.0, 0.0, 40.0, 10.0));
        assert_eq!(g.center, Rect::new(40.0, 10.0, 40.0, 60.0));
        assert_eq!(g.right, Rect::new(80.0, 10.0, 20.0, 60.0));
    }

    #[test]
    fn source_regions_oversized_slice_clamps() {
        // Slices larger than the image collapse the centre to zero width
        // rather than going negative.
        let slice = BorderImageSlice::parse("80 80").unwrap();
        let g = source_regions(100.0, 100.0, &slice);
        assert!(g.center.w >= 0.0);
        assert!(g.center.h >= 0.0);
    }

    #[test]
    fn destination_regions_with_outset_extends_outward() {
        // Border box at (0,0) 80×60; widths 10 each side; outset 5 each side.
        let widths = Insets {
            top: 10.0,
            right: 10.0,
            bottom: 10.0,
            left: 10.0,
        };
        let outset = Insets {
            top: 5.0,
            right: 5.0,
            bottom: 5.0,
            left: 5.0,
        };
        let g = destination_regions(Rect::new(0.0, 0.0, 80.0, 60.0), widths, outset);
        // Outer rect is (-5, -5, 90, 70). Top-left corner is (-5, -5, 10, 10).
        assert_eq!(g.top_left, Rect::new(-5.0, -5.0, 10.0, 10.0));
        // Centre is (5, 5, 70, 50).
        assert_eq!(g.center, Rect::new(5.0, 5.0, 70.0, 50.0));
    }

    // --- edge tiling ---------------------------------------------------

    #[test]
    fn stretch_single_scaled_tile() {
        let plan = tile_edge(30.0, 100.0, BorderImageRepeat::Stretch);
        assert_eq!(plan.tiles, vec![(0.0, 0.0, 100.0)]);
    }

    #[test]
    fn repeat_natural_size_clipped_ends() {
        // dest 100, source 30 → 4 tiles at x=0,30,60,90 (the last clipped).
        let plan = tile_edge(30.0, 100.0, BorderImageRepeat::Repeat);
        assert_eq!(plan.tiles.len(), 4);
        assert_eq!(plan.tiles[0], (0.0, 0.0, 30.0));
        assert_eq!(plan.tiles[3], (0.0, 90.0, 30.0));
    }

    #[test]
    fn round_rescales_to_integer_count() {
        // dest 100, source 30 → round(100/30)=3 → each tile 100/3 ≈ 33.33.
        let plan = tile_edge(30.0, 100.0, BorderImageRepeat::Round);
        assert_eq!(plan.tiles.len(), 3);
        for (i, tile) in plan.tiles.iter().enumerate() {
            assert!(approx(tile.1, i as f32 * (100.0 / 3.0)));
            assert!(approx(tile.2, 100.0 / 3.0));
        }
    }

    #[test]
    fn round_source_larger_than_dest_clamps_to_one() {
        // dest 20, source 30 → round(0.66) = 1 → one tile scaled to 20.
        let plan = tile_edge(30.0, 20.0, BorderImageRepeat::Round);
        assert_eq!(plan.tiles, vec![(0.0, 0.0, 20.0)]);
    }

    #[test]
    fn space_distributes_gaps_evenly() {
        // dest 100, source 30 → floor(3.33) = 3 tiles, used 90, gap 10/4 = 2.5.
        let plan = tile_edge(30.0, 100.0, BorderImageRepeat::Space);
        assert_eq!(plan.tiles.len(), 3);
        let gap = (100.0 - 3.0 * 30.0) / 4.0;
        for (i, tile) in plan.tiles.iter().enumerate() {
            let expected_x = gap + i as f32 * (30.0 + gap);
            assert!(approx(tile.1, expected_x), "tile {i}: got {}", tile.1);
            assert!(approx(tile.2, 30.0));
        }
    }

    #[test]
    fn space_source_larger_than_dest_centres_one_tile() {
        // dest 20, source 30 → one tile centred at x=(20-30)/2 = -5.
        let plan = tile_edge(30.0, 20.0, BorderImageRepeat::Space);
        assert_eq!(plan.tiles, vec![(0.0, -5.0, 30.0)]);
    }

    #[test]
    fn tiling_zero_dimensions_yields_empty_plan() {
        assert!(
            tile_edge(0.0, 100.0, BorderImageRepeat::Stretch)
                .tiles
                .is_empty()
        );
        assert!(
            tile_edge(30.0, 0.0, BorderImageRepeat::Stretch)
                .tiles
                .is_empty()
        );
    }

    // --- Rect helpers --------------------------------------------------

    #[test]
    fn rect_right_and_bottom() {
        let r = Rect::new(10.0, 20.0, 30.0, 40.0);
        assert!(approx(r.right(), 40.0));
        assert!(approx(r.bottom(), 60.0));
    }
}
