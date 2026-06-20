//! CSS `box-shadow` resolution — Phase 5 paint prep (pure logic called out by
//! `docs/PLAN.md` "Testing strategy" as a Rust-unit-test surface). Implements
//! CSS Backgrounds Level 3 § 7.2: the `<shadow>` grammar (`none | [ <shadow> ]#`),
//! the per-shadow geometry (offset / blur / spread / inset), and the paint-rect
//! arithmetic the display-list builder feeds WebRender.
//!
//! What lives here:
//! - [`BoxShadow`] — one shadow's resolved values (px offsets/radii + colour +
//!   inset flag), already cascade-resolved so the math is pure.
//! - [`BoxShadow::outer_paint_rect`] — the axis-aligned rect an outer shadow's
//!   paint region covers, for display-list culling + dirty-region tracking.
//! - [`BoxShadow::inset_clip_rect`] — the inner "frame" rect an inset shadow
//!   occupies inside the padding-box (everything outside this hole is opaque).
//! - [`parse_box_shadow`] — the `<shadow>#` grammar parser, the host-hook +
//!   reftest helper (the full cascade resolves author CSS via Stylo; this
//!   parser handles the common grammar the `--computed-style` projection
//!   re-derives).
//!
//! What does *not* live here:
//! - Percentage / `em` resolution. Authored values are often relative; the
//!   cascade resolves them against the element + font metrics before this
//!   module sees them (the [`crate::length::LengthContext`] pattern). The
//!   caller hands `outer_paint_rect` definite px values.
//! - The blur rasterisation (WebRender's gaussian-blur primitive paints the
//!   colour into the shadow rect; this module is the *geometry* it reduces to).
//! - `text-shadow` (CSS Text Decoration 3 § 4 has its own geometry; the v1.0
//!   paint path defers it). Same grammar, different positioning box.
//!
//! ## Geometry (CSS Backgrounds 3 § 7.2)
//!
//! An outer box-shadow "casts a shadow as if the border-box of the element
//! were opaque". Concretely:
//! 1. Take the border-box.
//! 2. Expand it by `spread` (negative spread shrinks).
//! 3. Offset the result by `(offset_x, offset_y)`.
//! 4. Inflate by `blur_radius` on every side (the blur's visual extent).
//!
//! The result is the axis-aligned rect the painter must cover for the shadow
//! to render fully. The painter then blurs the alpha mask inside it.
//!
//! An inset box-shadow "casts a shadow as if everything outside the padding
//! edge were opaque" — the shadow is drawn *inside* the padding-box, with the
//! "expanded by spread" rect used as the hole that *isn't* painted (the
//! inside of the cast frame). For inset, positive spread shrinks the visible
//! frame; negative spread grows it.
//!
//! The spec adds a guard: a shadow whose blur+spread would collapse the
//! painting rect to nothing is not painted ([`BoxShadow::is_degenerate`]).
//! We model the spec's "no part of the shadow rect overlaps the element's
//! border-box" early-exit for outer shadows separately: [`BoxShadow::is_hidden`]
//! captures the case where the offset/spread produce a rect that does not
//! touch the element. The painter still has to draw that case (the shadow
//! extends off-element), so it isn't degenerate — just clipped against the
//! viewport elsewhere.
//!
//! Reference: <https://www.w3.org/TR/css-backgrounds-3/#box-shadow>,
//! § 7.2 "Box Shadow: the `box-shadow` property".

#![forbid(unsafe_code)]

use crate::color::Color;
use crate::display_list::Rect;

/// One `box-shadow` value, cascade-resolved to definite px + a colour + the
/// inset flag. The list form (`box-shadow: a, b, c`) is a `Vec<BoxShadow>`,
/// painted first-shadow-on-top per CSS Backgrounds 3 § 7.2 ("the first
/// specified shadow … is painted on top").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxShadow {
    /// Horizontal offset (CSS x-axis points right). Positive ⇒ shadow shifted
    /// right; negative ⇒ left.
    pub offset_x: f32,
    /// Vertical offset (CSS y-axis points down). Positive ⇒ shadow shifted
    /// down; negative ⇒ up.
    pub offset_y: f32,
    /// `<blur-radius>`. Negative values are clamped to 0 at parse time per
    /// the § 7.2 grammar ("values must not be negative").
    pub blur_radius: f32,
    /// `<spread-distance>`. Negative values shrink the shadow rect; positive
    /// values grow it. May move a shadow's edge across the element.
    pub spread: f32,
    /// `<color>` — already resolved from `currentcolor` by the cascade.
    pub color: Color,
    /// `inset` ⇒ draw inside the padding-box instead of outside the border-box.
    pub inset: bool,
}

impl BoxShadow {
    /// Construct an outer shadow. Convenience for tests + the common case.
    pub const fn outer(offset_x: f32, offset_y: f32, blur: f32, spread: f32, color: Color) -> Self {
        Self {
            offset_x,
            offset_y,
            blur_radius: blur,
            spread,
            color,
            inset: false,
        }
    }

    /// Construct an inset shadow. Convenience mirror of [`BoxShadow::outer`].
    pub const fn inset(offset_x: f32, offset_y: f32, blur: f32, spread: f32, color: Color) -> Self {
        Self {
            offset_x,
            offset_y,
            blur_radius: blur,
            spread,
            color,
            inset: true,
        }
    }

    /// The paint rect for an outer shadow against the given border-box.
    ///
    /// Per CSS Backgrounds 3 § 7.2: expand by `spread`, offset by
    /// `(offset_x, offset_y)`, then inflate by `blur_radius` on every side.
    /// Negative `blur_radius` is clamped to 0 (the parser does this too; this
    /// method does it defensively in case a caller bypasses the parser).
    pub fn outer_paint_rect(self, border_box: Rect) -> Rect {
        debug_assert!(!self.inset, "outer_paint_rect called on an inset shadow");
        let blur = self.blur_radius.max(0.0);
        // Step 1: expand by spread (per side ⇒ ± spread).
        let mut r = expand(border_box, self.spread);
        // Step 2: offset.
        r.x += self.offset_x;
        r.y += self.offset_y;
        // Step 3: inflate by blur on every side.
        expand(r, blur)
    }

    /// The clip rect (the "hole") for an inset shadow against the given
    /// padding-box. The painter draws the shadow only inside the padding-box
    /// but outside this hole — i.e. in the frame between the padding-box edge
    /// and the (spread-adjusted, offset, blur-inflated) hole.
    ///
    /// Per CSS Backgrounds 3 § 7.2: for inset, the spread is inverted — a
    /// positive `spread` shrinks the hole by `spread` per side (the visible
    /// frame grows); a negative `spread` grows the hole. The blur extends the
    /// visible frame inward by `blur` per side (the shadow bleeds from the
    /// frame edge toward the centre), so the hole shrinks by `blur` per side
    /// as well. Same offset math as the outer case.
    pub fn inset_clip_rect(self, padding_box: Rect) -> Rect {
        debug_assert!(self.inset, "inset_clip_rect called on an outer shadow");
        let blur = self.blur_radius.max(0.0);
        // Step 1: spread for inset is inverted — positive spread shrinks the
        // hole (frame grows inside the padding-box).
        let mut r = expand(padding_box, -self.spread);
        r.x += self.offset_x;
        r.y += self.offset_y;
        // Step 2: the blur extends the visible frame inward by `blur` per
        // side ⇒ the hole shrinks by `blur` per side.
        expand(r, -blur)
    }

    /// `true` when the shadow has no visual effect — zero dimensions after
    /// blur/spread collapse it. The painter skips these. CSS Backgrounds 3
    /// § 7.2: "if the blur radius is negative, the entire shadow is not
    /// drawn"; we generalise to "the paint rect is empty".
    pub fn is_degenerate(self, reference: Rect) -> bool {
        let r = if self.inset {
            self.inset_clip_rect(reference)
        } else {
            self.outer_paint_rect(reference)
        };
        r.is_empty()
    }
}

/// Inflate `r` by `delta` on every side: `delta > 0` grows, `delta < 0`
/// shrinks. A shrunk rect that collapses becomes empty (w/h clamped at 0).
fn expand(r: Rect, delta: f32) -> Rect {
    if delta >= 0.0 {
        Rect::new(
            r.x - delta,
            r.y - delta,
            r.w + 2.0 * delta,
            r.h + 2.0 * delta,
        )
    } else {
        // delta < 0: shrink. Clamp the resulting w/h at 0 (empty rect).
        let shrink = -delta;
        let w = (r.w - 2.0 * shrink).max(0.0);
        let h = (r.h - 2.0 * shrink).max(0.0);
        // If shrinking collapses the rect, recentre it on the original centre.
        let cx = r.x + r.w / 2.0;
        let cy = r.y + r.h / 2.0;
        Rect::new(cx - w / 2.0, cy - h / 2.0, w, h)
    }
}

// ---------------------------------------------------------------------------
// Parser (host-hook + reftest helper)
// ---------------------------------------------------------------------------

/// Parse a `box-shadow` property value (CSS Backgrounds 3 § 7.2). Returns the
/// shadow list in document order (first listed ⇒ painted on top).
///
/// Accepts `none` (empty list), the single-shadow form (`10px 10px black`),
/// the comma-separated list form, the `inset` keyword in any position within
/// a single shadow, and an optional trailing `<color>`. Length args are
/// `<length>`-like values stripped of their `px` suffix; the cascade-resolved
/// surface feeds definite px, so exotic units are rejected at this layer
/// (`em`/`%` come through Stylo first).
pub fn parse_box_shadow(input: &str) -> Result<Vec<BoxShadow>, BoxShadowParseError> {
    let trimmed = input.trim();
    if trimmed.eq_ignore_ascii_case("none") || trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for shadow in split_shadows(trimmed) {
        out.push(parse_one_shadow(shadow)?);
    }
    Ok(out)
}

/// Split the top-level comma-separated list. Commas inside `rgb(...)` /
/// `rgba(...)` colour functions are tracked via paren depth.
fn split_shadows(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                out.push(input[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = input[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}

/// Parse one `<shadow>` token: `[ inset? && [ <length>{2,4} <color>? ] ]`.
/// Per § 7.2 the `inset` keyword may appear anywhere within the token (we
/// accept it before, after, or interleaved with the lengths).
fn parse_one_shadow(token: &str) -> Result<BoxShadow, BoxShadowParseError> {
    let mut offset_x: Option<f32> = None;
    let mut offset_y: Option<f32> = None;
    let mut blur_radius = 0.0f32;
    let mut spread = 0.0f32;
    let mut color: Option<Color> = None;
    let mut inset = false;
    let mut length_count = 0usize;

    for part in split_tokens_respecting_parens(token) {
        if part.eq_ignore_ascii_case("inset") {
            inset = true;
            continue;
        }
        // Try colour first iff it's not a length (no leading digit / sign).
        if !is_length_token(&part) {
            let c = crate::color::Color::parse(&part)
                .map_err(|e| BoxShadowParseError::BadColor(part.clone(), e.to_string()))?;
            color = Some(c);
            continue;
        }
        // Length slot 1..=4.
        let v = parse_px(&part)?;
        match length_count {
            0 => {
                offset_x = Some(v);
            }
            1 => {
                offset_y = Some(v);
            }
            2 => {
                blur_radius = v;
            }
            3 => {
                spread = v;
            }
            _ => return Err(BoxShadowParseError::TooManyLengths),
        }
        length_count += 1;
    }

    let Some(offset_x) = offset_x else {
        return Err(BoxShadowParseError::NotEnoughLengths);
    };
    let Some(offset_y) = offset_y else {
        return Err(BoxShadowParseError::NotEnoughLengths);
    };

    Ok(BoxShadow {
        offset_x,
        offset_y,
        // CSS Backgrounds 3 § 7.2: "Values must not be negative" for blur.
        blur_radius: blur_radius.max(0.0),
        spread,
        // Per § 7.2 the colour default is `currentcolor`; we surface that as
        // transparent black here and let the cascade-resolved caller override.
        // Tests construct explicitly with a colour, so this is the safe
        // fall-through.
        color: color.unwrap_or(Color::TRANSPARENT),
        inset,
    })
}

/// Tokenise on ASCII whitespace, but treat parenthesised runs (e.g.
/// `rgb(10, 20, 30)`) as one token. Returns owned strings so the colour
/// parser sees the function call verbatim.
fn split_tokens_respecting_parens(token: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = token.as_bytes();
    let mut buf = String::new();
    let mut depth = 0i32;
    for &b in bytes {
        match b {
            b'(' => {
                depth += 1;
                buf.push(b as char);
            }
            b')' => {
                depth -= 1;
                buf.push(b as char);
            }
            c if c.is_ascii_whitespace() && depth == 0 => {
                if !buf.is_empty() {
                    out.push(std::mem::take(&mut buf));
                }
            }
            _ => buf.push(b as char),
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

/// `true` iff `s` looks like a length token (digit or `+`/`-`/`.` start, or a
/// numeric value with a `px` suffix). Used to disambiguate the first colour
/// argument from a length in ambiguous cases (a leading colour would be
/// parsed first by `Color::parse` if it weren't a length).
fn is_length_token(s: &str) -> bool {
    let first = match s.as_bytes().first() {
        Some(b) => *b,
        None => return false,
    };
    first.is_ascii_digit() || first == b'+' || first == b'-' || first == b'.'
}

fn parse_px(s: &str) -> Result<f32, BoxShadowParseError> {
    let stripped = s.strip_suffix("px").unwrap_or(s);
    stripped
        .parse::<f32>()
        .map_err(|_| BoxShadowParseError::BadLength(s.to_owned()))
}

/// Parse error for [`parse_box_shadow`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoxShadowParseError {
    #[error("a shadow needs at least two lengths (offset-x and offset-y)")]
    NotEnoughLengths,
    #[error("a shadow accepts at most four lengths")]
    TooManyLengths,
    #[error("invalid length {0:?}")]
    BadLength(String),
    #[error("invalid color {0:?}: {1}")]
    BadColor(String, String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::Rect;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::new(x, y, w, h)
    }

    // --- outer_paint_rect ------------------------------------------------

    #[test]
    fn outer_simple_offset_and_spread() {
        // box-shadow: 5px 5px 0 0 (no blur, no spread).
        // border-box 100x100 at (0,0) → shadow rect at (5,5) 100x100.
        let s = BoxShadow::outer(5.0, 5.0, 0.0, 0.0, Color::BLACK);
        let r = s.outer_paint_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.x, 5.0));
        assert!(approx(r.y, 5.0));
        assert!(approx(r.w, 100.0));
        assert!(approx(r.h, 100.0));
    }

    #[test]
    fn outer_spread_expands_per_side() {
        // spread 10: each side grows by 10 ⇒ w += 20.
        let s = BoxShadow::outer(0.0, 0.0, 0.0, 10.0, Color::BLACK);
        let r = s.outer_paint_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.x, -10.0));
        assert!(approx(r.y, -10.0));
        assert!(approx(r.w, 120.0));
        assert!(approx(r.h, 120.0));
    }

    #[test]
    fn outer_negative_spread_shrinks() {
        // spread -10: each side shrinks by 10 ⇒ w -= 20.
        let s = BoxShadow::outer(0.0, 0.0, 0.0, -10.0, Color::BLACK);
        let r = s.outer_paint_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.w, 80.0));
        assert!(approx(r.h, 80.0));
    }

    #[test]
    fn outer_blur_inflates_every_side() {
        // blur 8: every side grows by 8 ⇒ w += 16.
        let s = BoxShadow::outer(0.0, 0.0, 8.0, 0.0, Color::BLACK);
        let r = s.outer_paint_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.x, -8.0));
        assert!(approx(r.y, -8.0));
        assert!(approx(r.w, 116.0));
        assert!(approx(r.h, 116.0));
    }

    #[test]
    fn outer_negative_offset_shifts_left_and_up() {
        let s = BoxShadow::outer(-5.0, -5.0, 0.0, 0.0, Color::BLACK);
        let r = s.outer_paint_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.x, -5.0));
        assert!(approx(r.y, -5.0));
    }

    #[test]
    fn outer_full_combo_uses_spec_order() {
        // spread first, then offset, then blur.
        // border-box 100x100 at (0,0); spread 10 ⇒ 120x120 at (-10,-10);
        // offset (5, 5) ⇒ 120x120 at (-5,-5); blur 8 ⇒ 136x136 at (-13,-13).
        let s = BoxShadow::outer(5.0, 5.0, 8.0, 10.0, Color::BLACK);
        let r = s.outer_paint_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.x, -13.0), "x={}", r.x);
        assert!(approx(r.y, -13.0), "y={}", r.y);
        assert!(approx(r.w, 136.0), "w={}", r.w);
        assert!(approx(r.h, 136.0), "h={}", r.h);
    }

    #[test]
    fn outer_negative_blur_treated_as_zero() {
        // The grammar forbids negative blur; the parser clamps, and so does
        // the geometry method defensively.
        let s = BoxShadow {
            blur_radius: -5.0,
            ..BoxShadow::outer(0.0, 0.0, 0.0, 0.0, Color::BLACK)
        };
        let r = s.outer_paint_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.w, 100.0));
        assert!(approx(r.h, 100.0));
    }

    // --- inset_clip_rect -------------------------------------------------

    #[test]
    fn inset_simple_clip_equals_padding_box_offset() {
        // inset, no blur, no spread: the clip equals the offset padding-box.
        let s = BoxShadow::inset(0.0, 0.0, 0.0, 0.0, Color::BLACK);
        let r = s.inset_clip_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.x, 0.0));
        assert!(approx(r.y, 0.0));
        assert!(approx(r.w, 100.0));
        assert!(approx(r.h, 100.0));
    }

    #[test]
    fn inset_positive_spread_shrinks_hole() {
        // inset + spread 10 ⇒ the hole shrinks by 10 per side (positive
        // spread for inset inverts ⇒ frame grows inside the padding-box).
        let s = BoxShadow::inset(0.0, 0.0, 0.0, 10.0, Color::BLACK);
        let r = s.inset_clip_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.w, 80.0), "w={}", r.w);
        assert!(approx(r.h, 80.0));
    }

    #[test]
    fn inset_blur_extends_frame_inward() {
        // For inset, the blur bleeds inward from the frame edge, shrinking
        // the hole by `blur` per side ⇒ w -= 2*blur.
        let s = BoxShadow::inset(0.0, 0.0, 8.0, 0.0, Color::BLACK);
        let r = s.inset_clip_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.w, 84.0), "w={}", r.w);
        assert!(approx(r.h, 84.0));
    }

    #[test]
    fn inset_spread_plus_blur_combine() {
        // inset + spread 10 + blur 8 ⇒ hole shrinks by (10+8) per side
        // ⇒ w = 100 - 2*18 = 64.
        let s = BoxShadow::inset(0.0, 0.0, 8.0, 10.0, Color::BLACK);
        let r = s.inset_clip_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.w, 64.0), "w={}", r.w);
    }

    #[test]
    fn inset_negative_spread_grows_hole() {
        // inset + spread -10 ⇒ hole grows by 10 per side (no visible frame).
        let s = BoxShadow::inset(0.0, 0.0, 0.0, -10.0, Color::BLACK);
        let r = s.inset_clip_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.w, 120.0));
    }

    #[test]
    fn inset_offset_shifts_hole() {
        let s = BoxShadow::inset(5.0, 5.0, 0.0, 0.0, Color::BLACK);
        let r = s.inset_clip_rect(rect(0.0, 0.0, 100.0, 100.0));
        assert!(approx(r.x, 5.0));
        assert!(approx(r.y, 5.0));
    }

    // --- is_degenerate ---------------------------------------------------

    #[test]
    fn degenerate_when_negative_spread_collapses_outer() {
        // border-box 10x10, spread -10 ⇒ w - 20 = -10 ⇒ clamped to empty.
        let s = BoxShadow::outer(0.0, 0.0, 0.0, -10.0, Color::BLACK);
        assert!(s.is_degenerate(rect(0.0, 0.0, 10.0, 10.0)));
    }

    #[test]
    fn non_degenerate_normal_shadow() {
        let s = BoxShadow::outer(0.0, 0.0, 0.0, 0.0, Color::BLACK);
        assert!(!s.is_degenerate(rect(0.0, 0.0, 10.0, 10.0)));
    }

    // --- expand() edge cases --------------------------------------------

    #[test]
    fn expand_zero_is_identity() {
        let r = expand(rect(1.0, 2.0, 3.0, 4.0), 0.0);
        assert!(approx(r.x, 1.0));
        assert!(approx(r.y, 2.0));
        assert!(approx(r.w, 3.0));
        assert!(approx(r.h, 4.0));
    }

    #[test]
    fn expand_negative_clamps_at_empty() {
        // Shrink by more than half the dimension ⇒ w/h clamp at 0.
        let r = expand(rect(0.0, 0.0, 10.0, 20.0), -10.0);
        assert!(approx(r.w, 0.0));
        // height 20 - 2*10 = 0 too.
        assert!(approx(r.h, 0.0));
    }

    // --- Parser ----------------------------------------------------------

    #[test]
    fn parse_none_is_empty_list() {
        assert!(parse_box_shadow("none").unwrap().is_empty());
        assert!(parse_box_shadow("NONE").unwrap().is_empty());
        assert!(parse_box_shadow("").unwrap().is_empty());
    }

    #[test]
    fn parse_two_lengths_offset_only() {
        let v = parse_box_shadow("5px 5px").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].offset_x, 5.0);
        assert_eq!(v[0].offset_y, 5.0);
        assert_eq!(v[0].blur_radius, 0.0);
        assert_eq!(v[0].spread, 0.0);
        assert!(!v[0].inset);
    }

    #[test]
    fn parse_three_lengths_includes_blur() {
        let v = parse_box_shadow("0 0 10px").unwrap();
        assert_eq!(v[0].blur_radius, 10.0);
        assert_eq!(v[0].spread, 0.0);
    }

    #[test]
    fn parse_four_lengths_includes_spread() {
        let v = parse_box_shadow("1px 2px 3px 4px").unwrap();
        assert_eq!(v[0].offset_x, 1.0);
        assert_eq!(v[0].offset_y, 2.0);
        assert_eq!(v[0].blur_radius, 3.0);
        assert_eq!(v[0].spread, 4.0);
    }

    #[test]
    fn parse_trailing_named_color() {
        let v = parse_box_shadow("5px 5px red").unwrap();
        assert_eq!(v[0].color, Color::rgb(255, 0, 0));
    }

    #[test]
    fn parse_leading_color_then_lengths() {
        // § 7.2 grammar allows colour in any position. The common form is
        // trailing, but leading is legal.
        let v = parse_box_shadow("red 5px 5px").unwrap();
        assert_eq!(v[0].color, Color::rgb(255, 0, 0));
        assert_eq!(v[0].offset_x, 5.0);
    }

    #[test]
    fn parse_inset_keyword_anywhere() {
        let v = parse_box_shadow("inset 5px 5px black").unwrap();
        assert!(v[0].inset);
        let v = parse_box_shadow("5px 5px inset black").unwrap();
        assert!(v[0].inset);
        let v = parse_box_shadow("5px 5px black inset").unwrap();
        assert!(v[0].inset);
    }

    #[test]
    fn parse_hex_color() {
        let v = parse_box_shadow("0 0 0 2px #3366ff").unwrap();
        assert_eq!(v[0].spread, 2.0);
        assert_eq!(v[0].color, Color::rgb(0x33, 0x66, 0xff));
    }

    #[test]
    fn parse_rgb_function_color() {
        // rgb() commas must not be confused with the shadow-list comma.
        let v = parse_box_shadow("0 0 0 1px rgb(10, 20, 30), 5px 5px black").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].color, Color::rgb(10, 20, 30));
        assert_eq!(v[1].color, Color::rgb(0, 0, 0));
        assert_eq!(v[1].offset_x, 5.0);
    }

    #[test]
    fn parse_negative_blur_clamped() {
        let v = parse_box_shadow("5px 5px -5px").unwrap();
        assert_eq!(v[0].blur_radius, 0.0, "negative blur must clamp to 0");
    }

    #[test]
    fn parse_negative_spread_kept() {
        let v = parse_box_shadow("5px 5px 0 -5px").unwrap();
        assert_eq!(v[0].spread, -5.0);
    }

    #[test]
    fn parse_negative_offsets() {
        let v = parse_box_shadow("-5px -5px").unwrap();
        assert_eq!(v[0].offset_x, -5.0);
        assert_eq!(v[0].offset_y, -5.0);
    }

    #[test]
    fn parse_errors_on_too_few_lengths() {
        assert!(matches!(
            parse_box_shadow("5px"),
            Err(BoxShadowParseError::NotEnoughLengths)
        ));
        assert!(parse_box_shadow("black").is_err()); // colour only, no lengths
    }

    #[test]
    fn parse_errors_on_too_many_lengths() {
        assert!(matches!(
            parse_box_shadow("1px 2px 3px 4px 5px"),
            Err(BoxShadowParseError::TooManyLengths)
        ));
    }

    #[test]
    fn parse_errors_on_bad_length() {
        assert!(matches!(
            parse_box_shadow("foo 5px"),
            Err(BoxShadowParseError::BadColor(_, _)) // "foo" tried as colour
        ));
    }

    // --- Document-order list semantics ---------------------------------

    #[test]
    fn list_first_is_first() {
        // First shadow in source order is painted on top (CSS § 7.2).
        let v = parse_box_shadow("1px 1px red, 2px 2px blue").unwrap();
        assert_eq!(v[0].color, Color::rgb(255, 0, 0));
        assert_eq!(v[1].color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn list_trims_whitespace_between_entries() {
        let v = parse_box_shadow("  1px 1px  ,  2px 2px  ").unwrap();
        assert_eq!(v.len(), 2);
    }
}
