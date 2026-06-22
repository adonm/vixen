//! CSS Images 4 radial-gradient color sampling — Phase 5 paint prep (pure
//! logic called out by `docs/PLAN.md` "Testing strategy" as a Rust-unit-test
//! surface). Implements the radial-gradient colour function the paint path
//! samples against: given the gradient shape, size keyword, and a stop list,
//! [`RadialGradient::sample`] returns the colour at a fractional distance from
//! the centre. The CSS Images 4 § 4.2.3 shape + § 4.2.4 size-keyword →
//! concrete radius computation is pure given a known reference box; the
//! pixel-distance arithmetic the paint path runs is the only call into here.
//!
//! What lives here:
//! - [`RadialShape`] — `circle` or `ellipse` (§ 4.2.3). The two shapes share
//!   the stop list + interpolation; they differ in how a pixel projects onto
//!   the gradient axis.
//! - [`RadialSize`] — the § 4.2.4 size keyword family (`closest-side` /
//!   `farthest-side` / `closest-corner` / `farthest-corner`) for the keyword
//!   form, plus the explicit `Length` / `LengthPair` forms.
//! - [`compute_radius`] — the § 4.2.4 radius computation for one of the four
//!   keyword forms, against a known `(width, height)` reference box centred
//!   at `(cx, cy)`. Returns `(rx, ry)` so a circle (`rx == ry`) and an
//!   ellipse share the call site.
//! - [`RadialGradient`] — the shape + size + stop list + `repeating` flag.
//!   [`RadialGradient::sample`] takes the (already-computed) pixel-distance
//!   normalised to `[0, 1]` and returns the colour the paint path writes.
//!
//! What does *not* live here:
//! - The `<position>` grammar (`at <position>`) — the cascade-resolved centre
//!   is the input; [`crate::background_position`] owns the parser.
//! - The reference-box computation (`<geometry-box>` / `border-box` &c.). The
//!   layout layer resolves the box dimensions; this module receives `(w, h)`.
//! - The `<color-stop>` grammar parser. The stop list is shared with the
//!   linear-gradient surface via [`crate::gradient::ColorStop`]; the parser
//!   lives in the cascade and feeds both. The § 4.5 stop-position
//!   normalisation is shared via [`crate::gradient::resolve_stop_positions`].
//!
//! ## Sampling arithmetic
//!
//! For a **circle** of radius `r` centred at `(cx, cy)`, a pixel at
//! `(px, py)` projects to `t = √((px−cx)² + (py−cy)²) / r`. Outside `r`, the
//! colour clamps to the last stop (or wraps for `repeating-radial-gradient`).
//!
//! For an **ellipse** with semi-axes `(rx, ry)`, the same pixel projects to
//! `t = √((px−cx)² / rx² + (py−cy)² / ry²)`. This is the standard
//! ellipse parametrisation; the angle is irrelevant because the colour
//! function only depends on which concentric ellipse the pixel lies on.
//!
//! Reference: <https://www.w3.org/TR/css-images-4/#radial-gradients>,
//! § 4.2.3 "Radial Gradient Syntax" + § 4.2.4 "Color Stop Positions".

#![forbid(unsafe_code)]

use crate::color::Color;
use crate::gradient::{ColorStop, resolve_stop_positions, sample_resolved};

// ---------------------------------------------------------------------------
// Shape + size
// ---------------------------------------------------------------------------

/// The radial-gradient shape (CSS Images 4 § 4.2.3). `Circle` is the
/// degenerate `ellipse` with `rx == ry`; the two share the colour-sampling
/// pipeline and differ only in the distance projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RadialShape {
    /// `circle` — one radius; the pixel-distance is the Euclidean norm.
    #[default]
    Circle,
    /// `ellipse` — two semi-axes; the pixel-distance is the ellipse norm.
    Ellipse,
}

/// The radial-gradient size (CSS Images 4 § 4.2.4). The four keywords compute
/// the radius from the reference box; the two explicit forms carry authored
/// lengths (`<length>` for circles, `<length-percentage>{2}` for ellipses).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum RadialSize {
    /// `closest-side` (§ 4.2.4). Circle: distance to the closest edge.
    /// Ellipse: the `(horizontal distance to closest side, vertical distance
    /// to closest side)` pair.
    ClosestSide,
    /// `farthest-side` (§ 4.2.4). Symmetric to `ClosestSide` against the
    /// far edges.
    FarthestSide,
    /// `closest-corner` (§ 4.2.4). Circle: distance to the closest corner.
    /// Ellipse: same ratio as `FarthestSide` but scaled so the ellipse
    /// passes through the closest corner.
    ClosestCorner,
    /// `farthest-corner` (§ 4.2.4) — the **default** when no size keyword or
    /// explicit length is supplied (CSS Images 4 § 4.2.3 "if not specified,
    /// `farthest-corner`").
    #[default]
    FarthestCorner,
    /// `circle <length>` — explicit radius. Carries the resolved pixel
    /// value (the cascade converts percentages + `em` first).
    Length(f32),
    /// `ellipse <length-percentage>{2}` — explicit semi-axes `(rx, ry)`.
    LengthPair(f32, f32),
}

// ---------------------------------------------------------------------------
// Radius computation (§ 4.2.4)
// ---------------------------------------------------------------------------

/// Compute the `(rx, ry)` semi-axes for a radial gradient against a known
/// reference box, per CSS Images 4 § 4.2.4. `width` / `height` are the box
/// dimensions in CSS px; `(cx, cy)` is the centre in box-relative
/// coordinates. For a `circle`, the returned pair has `rx == ry`.
///
/// The four keyword rules:
/// - **closest-side** (circle): the distance from the centre to the closest
///   box edge. (ellipse): the horizontal distance to the closest vertical
///   edge + the vertical distance to the closest horizontal edge.
/// - **farthest-side**: symmetric to `closest-side` against the far edges.
/// - **closest-corner**: circle = Euclidean distance to the closest corner;
///   ellipse = same `rx/ry` ratio as `closest-side` but scaled so the ellipse
///   passes through the closest corner.
/// - **farthest-corner**: symmetric to `closest-corner` against the far
///   corner. (This is the default per § 4.2.3.)
///
/// For the explicit `Length(r)` / `LengthPair(rx, ry)` forms, the values are
/// returned unchanged.
pub fn compute_radius(
    size: &RadialSize,
    shape: RadialShape,
    width: f32,
    height: f32,
    cx: f32,
    cy: f32,
) -> (f32, f32) {
    // Distances from the centre to each of the four sides.
    let left = cx;
    let right = width - cx;
    let top = cy;
    let bottom = height - cy;

    match size {
        RadialSize::Length(r) => (*r, *r),
        RadialSize::LengthPair(rx, ry) => (*rx, *ry),
        RadialSize::ClosestSide => match shape {
            RadialShape::Circle => {
                let d = left.min(right).min(top).min(bottom);
                (d, d)
            }
            RadialShape::Ellipse => {
                let rx = left.min(right);
                let ry = top.min(bottom);
                (rx, ry)
            }
        },
        RadialSize::FarthestSide => match shape {
            RadialShape::Circle => {
                let d = left.max(right).max(top).max(bottom);
                (d, d)
            }
            RadialShape::Ellipse => {
                let rx = left.max(right);
                let ry = top.max(bottom);
                (rx, ry)
            }
        },
        RadialSize::ClosestCorner => {
            // The corners and their squared distances from the centre.
            let corner_dists = [
                (left * left + top * top, left, top),
                (right * right + top * top, right, top),
                (left * left + bottom * bottom, left, bottom),
                (right * right + bottom * bottom, right, bottom),
            ];
            let (_, dx, dy) = corner_dists
                .iter()
                .copied()
                .reduce(|a, b| if a.0 < b.0 { a } else { b })
                .unwrap_or((0.0, 0.0, 0.0));
            match shape {
                RadialShape::Circle => {
                    let r = (dx * dx + dy * dy).sqrt();
                    (r, r)
                }
                RadialShape::Ellipse => {
                    // The ellipse keeps the same rx/ry ratio as `closest-side`
                    // but scales to pass through the closest corner. The
                    // spec's § 4.2.4 "closest-corner" step: compute the
                    // closest-side rx/ry, then scale by the corner-distance
                    // ratio so the ellipse touches the corner.
                    let (cs_rx, cs_ry) = compute_radius(
                        &RadialSize::ClosestSide,
                        RadialShape::Ellipse,
                        width,
                        height,
                        cx,
                        cy,
                    );
                    scale_ellipse_to_corner(cs_rx, cs_ry, dx, dy)
                }
            }
        }
        RadialSize::FarthestCorner => {
            let corner_dists = [
                (left * left + top * top, left, top),
                (right * right + top * top, right, top),
                (left * left + bottom * bottom, left, bottom),
                (right * right + bottom * bottom, right, bottom),
            ];
            let (_, dx, dy) = corner_dists
                .iter()
                .copied()
                .reduce(|a, b| if a.0 > b.0 { a } else { b })
                .unwrap_or((0.0, 0.0, 0.0));
            match shape {
                RadialShape::Circle => {
                    let r = (dx * dx + dy * dy).sqrt();
                    (r, r)
                }
                RadialShape::Ellipse => {
                    // Same ratio as `farthest-side`, scaled to pass through
                    // the farthest corner (§ 4.2.4).
                    let (fs_rx, fs_ry) = compute_radius(
                        &RadialSize::FarthestSide,
                        RadialShape::Ellipse,
                        width,
                        height,
                        cx,
                        cy,
                    );
                    scale_ellipse_to_corner(fs_rx, fs_ry, dx, dy)
                }
            }
        }
    }
}

/// Scale an ellipse with semi-axes `(rx, ry)` so that it passes through the
/// corner at offset `(dx, dy)` while keeping the `rx/ry` ratio (CSS Images 4
/// § 4.2.4 corner-scaling rule). The scale factor `s` is chosen so
/// `(dx / (s·rx))² + (dy / (s·ry))² = 1`.
fn scale_ellipse_to_corner(rx: f32, ry: f32, dx: f32, dy: f32) -> (f32, f32) {
    if rx <= 0.0 || ry <= 0.0 {
        return (rx, ry);
    }
    // Solve for s²: (dx/(s·rx))² + (dy/(s·ry))² = 1 ⇒ s² = (dx/rx)² + (dy/ry)².
    let sx = dx / rx;
    let sy = dy / ry;
    let s2 = sx * sx + sy * sy;
    if s2 <= 0.0 {
        return (rx, ry);
    }
    let s = s2.sqrt();
    (rx * s, ry * s)
}

// ---------------------------------------------------------------------------
// Distance projection (pixel → t)
// ---------------------------------------------------------------------------

/// Project a pixel-relative-to-centre `(dx, dy)` onto the gradient axis `t`
/// given the resolved semi-axes `(rx, ry)`. For a circle (`rx == ry`):
/// `t = √(dx² + dy²) / r`. For an ellipse: `t = √((dx/rx)² + (dy/ry)²)`.
///
/// The returned `t` is the un-clamped, un-wrapped distance; callers feed it
/// into [`RadialGradient::sample`] which applies the clamp/wrap rule.
pub fn project_to_t(dx: f32, dy: f32, rx: f32, ry: f32) -> f32 {
    if rx == ry {
        // Circle (avoid the ellipse division when the two are equal).
        (dx * dx + dy * dy).sqrt() / rx.max(0.0)
    } else {
        let nx = dx / rx.max(1e-9);
        let ny = dy / ry.max(1e-9);
        (nx * nx + ny * ny).sqrt()
    }
}

// ---------------------------------------------------------------------------
// RadialGradient
// ---------------------------------------------------------------------------

/// A radial gradient: a stop list + the `repeating` flag. The shape, size,
/// and centre are caller-resolved at every sample. The paint path computes
/// the per-pixel `(dx, dy)`, the per-gradient `t`, then calls
/// [`RadialGradient::sample`].
///
/// Construct freely; the shape/size selection happens once per gradient
/// (via [`compute_radius`]) and the per-pixel work is just [`project_to_t`]
/// + [`RadialGradient::sample`].
#[derive(Debug, Clone, PartialEq)]
pub struct RadialGradient {
    pub stops: Vec<ColorStop>,
    pub repeating: bool,
}

impl RadialGradient {
    /// Non-repeating radial gradient with the given stops.
    pub fn new(stops: Vec<ColorStop>) -> Self {
        Self {
            stops,
            repeating: false,
        }
    }

    /// Repeating variant (`repeating-radial-gradient()`).
    pub fn repeating(stops: Vec<ColorStop>) -> Self {
        Self {
            stops,
            repeating: true,
        }
    }

    /// Sample the gradient at the projected distance `t`. Returns the colour
    /// at that distance. For non-repeating gradients, `t` outside `[0, 1]`
    /// clamps to the end-stop colour; for repeating gradients, `t` wraps
    /// modulo the gradient's last-stop distance.
    ///
    /// Empty stop list ⇒ [`Color::TRANSPARENT`] (paint path treats degenerate
    /// gradients as no-op). A single stop is solid colour at every `t`.
    pub fn sample(&self, t: f32) -> Color {
        if self.stops.is_empty() {
            return Color::TRANSPARENT;
        }
        let positions = resolve_stop_positions(&self.stops);
        if positions.len() == 1 {
            return self.stops[0].color;
        }
        // The repeating wrap is handled inside `sample_resolved` (it tiles
        // across [first, last]); we just pass the raw `t`.
        sample_resolved(&positions, &self.stops, t, self.repeating)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    fn color_approx(a: Color, b: Color) -> bool {
        a.r.abs_diff(b.r) <= 2
            && a.g.abs_diff(b.g) <= 2
            && a.b.abs_diff(b.b) <= 2
            && a.a.abs_diff(b.a) <= 2
    }

    // --- compute_radius: closest/farthest-side -------------------------

    #[test]
    fn closest_side_circle_at_center() {
        // 100×100 box, centre at (50, 50). Closest edge is 50 away.
        let (rx, ry) = compute_radius(
            &RadialSize::ClosestSide,
            RadialShape::Circle,
            100.0,
            100.0,
            50.0,
            50.0,
        );
        assert!(approx(rx, 50.0) && approx(ry, 50.0));
    }

    #[test]
    fn closest_side_circle_off_center() {
        // 100×100 box, centre at (25, 75). Closest edge is min(25, 75, 75, 25) = 25.
        let (rx, ry) = compute_radius(
            &RadialSize::ClosestSide,
            RadialShape::Circle,
            100.0,
            100.0,
            25.0,
            75.0,
        );
        assert!(approx(rx, 25.0) && approx(ry, 25.0));
    }

    #[test]
    fn farthest_side_circle_at_center() {
        let (rx, ry) = compute_radius(
            &RadialSize::FarthestSide,
            RadialShape::Circle,
            100.0,
            100.0,
            50.0,
            50.0,
        );
        assert!(approx(rx, 50.0) && approx(ry, 50.0));
    }

    #[test]
    fn farthest_side_circle_off_center() {
        // 100×100 box, centre at (25, 75). Farthest edge is max(25, 75, 75, 25) = 75.
        let (rx, ry) = compute_radius(
            &RadialSize::FarthestSide,
            RadialShape::Circle,
            100.0,
            100.0,
            25.0,
            75.0,
        );
        assert!(approx(rx, 75.0) && approx(ry, 75.0));
    }

    #[test]
    fn closest_side_ellipse_at_center() {
        let (rx, ry) = compute_radius(
            &RadialSize::ClosestSide,
            RadialShape::Ellipse,
            100.0,
            50.0,
            50.0,
            25.0,
        );
        // Horizontal closest: min(50, 50) = 50. Vertical closest: min(25, 25) = 25.
        assert!(approx(rx, 50.0) && approx(ry, 25.0));
    }

    // --- compute_radius: closest/farthest-corner -----------------------

    #[test]
    fn closest_corner_circle_at_center() {
        // 100×100 box, centre at (50, 50). Closest corner is at distance √(50²+50²).
        let (rx, ry) = compute_radius(
            &RadialSize::ClosestCorner,
            RadialShape::Circle,
            100.0,
            100.0,
            50.0,
            50.0,
        );
        let expected = (50.0_f32 * 50.0 + 50.0 * 50.0).sqrt();
        assert!(approx(rx, expected) && approx(ry, expected));
    }

    #[test]
    fn farthest_corner_circle_off_center() {
        // 100×100 box, centre at (25, 25). Farthest corner is (100, 100).
        // Distance √(75² + 75²) ≈ 106.07.
        let (rx, ry) = compute_radius(
            &RadialSize::FarthestCorner,
            RadialShape::Circle,
            100.0,
            100.0,
            25.0,
            25.0,
        );
        let expected = (75.0_f32 * 75.0 + 75.0 * 75.0).sqrt();
        assert!(approx(rx, expected) && approx(ry, expected));
    }

    #[test]
    fn farthest_corner_ellipse_keeps_side_ratio() {
        // 200×100 box, centre at (100, 50). Farthest-side gives (100, 50).
        // The farthest corner is (200, 100), offset (100, 50).
        // Scale factor: √((100/100)² + (50/50)²) = √2.
        let (rx, ry) = compute_radius(
            &RadialSize::FarthestCorner,
            RadialShape::Ellipse,
            200.0,
            100.0,
            100.0,
            50.0,
        );
        let s = 2.0_f32.sqrt();
        assert!(approx(rx, 100.0 * s));
        assert!(approx(ry, 50.0 * s));
    }

    #[test]
    fn explicit_length_passes_through() {
        let (rx, ry) = compute_radius(
            &RadialSize::Length(42.0),
            RadialShape::Circle,
            100.0,
            100.0,
            50.0,
            50.0,
        );
        assert!(approx(rx, 42.0) && approx(ry, 42.0));
        let (rx, ry) = compute_radius(
            &RadialSize::LengthPair(30.0, 60.0),
            RadialShape::Ellipse,
            100.0,
            100.0,
            50.0,
            50.0,
        );
        assert!(approx(rx, 30.0) && approx(ry, 60.0));
    }

    // --- project_to_t --------------------------------------------------

    #[test]
    fn project_to_t_circle_center_is_zero() {
        let t = project_to_t(0.0, 0.0, 50.0, 50.0);
        assert!(approx(t, 0.0));
    }

    #[test]
    fn project_to_t_circle_at_radius_is_one() {
        // (50, 0) from centre, radius 50 → t = 1.0.
        let t = project_to_t(50.0, 0.0, 50.0, 50.0);
        assert!(approx(t, 1.0));
    }

    #[test]
    fn project_to_t_circle_diagonal() {
        // (30, 40) → distance 50; radius 50 → t = 1.0.
        let t = project_to_t(30.0, 40.0, 50.0, 50.0);
        assert!(approx(t, 1.0));
    }

    #[test]
    fn project_to_t_ellipse_at_semi_axis_is_one() {
        // Ellipse rx=100, ry=50. Point (100, 0) → t = 1.
        let t = project_to_t(100.0, 0.0, 100.0, 50.0);
        assert!(approx(t, 1.0));
        // Point (0, 50) → t = 1.
        let t = project_to_t(0.0, 50.0, 100.0, 50.0);
        assert!(approx(t, 1.0));
    }

    #[test]
    fn project_to_t_circle_at_double_radius_is_two() {
        let t = project_to_t(100.0, 0.0, 50.0, 50.0);
        assert!(approx(t, 2.0));
    }

    // --- sample: colour at known positions -----------------------------

    #[test]
    fn sample_returns_transparent_for_empty_stops() {
        let g = RadialGradient::new(vec![]);
        assert_eq!(g.sample(0.5), Color::TRANSPARENT);
    }

    #[test]
    fn sample_single_stop_is_solid_colour() {
        let g = RadialGradient::new(vec![ColorStop::at(Color::rgb(255, 0, 0), 0.0)]);
        assert_eq!(g.sample(0.0), Color::rgb(255, 0, 0));
        assert_eq!(g.sample(0.5), Color::rgb(255, 0, 0));
        assert_eq!(g.sample(1.0), Color::rgb(255, 0, 0));
    }

    #[test]
    fn sample_two_stops_center_is_first_color() {
        let g = RadialGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        // t=0 → black.
        assert!(color_approx(g.sample(0.0), Color::BLACK));
        // t=1 → white.
        assert!(color_approx(g.sample(1.0), Color::WHITE));
    }

    #[test]
    fn sample_two_stops_midpoint_is_linear_blend() {
        // Linear-sRGB interpolation of BLACK → WHITE at t=0.5 produces ~190
        // (brighter than the naive sRGB average of 128).
        let g = RadialGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        let mid = g.sample(0.5);
        assert!(mid.r > 175 && mid.r < 205, "mid.r = {}", mid.r);
        assert_eq!(mid.r, mid.g);
        assert_eq!(mid.g, mid.b);
    }

    #[test]
    fn sample_outside_radius_clamps_to_last_stop() {
        let g = RadialGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        // t > 1: clamps to white.
        assert!(color_approx(g.sample(2.0), Color::WHITE));
        // t < 0: clamps to black.
        assert!(color_approx(g.sample(-1.0), Color::BLACK));
    }

    #[test]
    fn repeating_wraps_around() {
        let g = RadialGradient::repeating(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        // For a repeating gradient, t=1.5 should wrap to the position
        // equivalent to t=0.5 (since first=0, last=1, span=1).
        let half = g.sample(0.5);
        let one_half = g.sample(1.5);
        assert!(
            color_approx(half, one_half),
            "repeating gradient at t=1.5 should match t=0.5; got {half:?} vs {one_half:?}"
        );
    }

    // --- end-to-end radius + projection + sample -----------------------

    #[test]
    fn end_to_end_circle_gradient() {
        // 100×100 box, centre, closest-side (radius 50). Two-stop black→white.
        let (rx, ry) = compute_radius(
            &RadialSize::ClosestSide,
            RadialShape::Circle,
            100.0,
            100.0,
            50.0,
            50.0,
        );
        let g = RadialGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        // Pixel at centre: distance 0 → t = 0 → black.
        let t = project_to_t(0.0, 0.0, rx, ry);
        assert!(color_approx(g.sample(t), Color::BLACK));
        // Pixel at edge (50, 0): distance 50 → t = 1 → white.
        let t = project_to_t(50.0, 0.0, rx, ry);
        assert!(color_approx(g.sample(t), Color::WHITE));
        // Pixel at half-way (25, 0): distance 25 → t = 0.5 → linear-sRGB
        // midpoint (~190, brighter than the sRGB-average 128).
        let t = project_to_t(25.0, 0.0, rx, ry);
        let c = g.sample(t);
        assert!(c.r > 175 && c.r < 205, "c.r = {}", c.r);
    }
}
