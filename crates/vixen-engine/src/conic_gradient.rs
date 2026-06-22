//! CSS Images 4 conic-gradient color sampling — Phase 5 paint prep (pure
//! logic called out by `docs/PLAN.md` "Testing strategy" as a Rust-unit-test
//! surface). Implements the conic-gradient colour function the paint path
//! samples against: given the gradient's start angle and a stop list,
//! [`ConicGradient::sample`] returns the colour at a fractional angle
//! around the centre. The CSS Images 4 § 4.3.3 angle → colour projection is
//! pure given a known centre + `from` angle; the paint path supplies the
//! per-pixel angle.
//!
//! What lives here:
//! - [`ConicGradient`] — the stop list + the `from` angle (radians) + the
//!   `repeating` flag. [`ConicGradient::sample`] takes the (already-computed)
//!   angle normalised to `[0, 1]` and returns the colour.
//! - [`project_angle_to_t`] — the per-pixel `(dx, dy)` → angle projection.
//!   Returns the angle in *turns* (one full revolution = `1.0`), starting
//!   from the 12-o'clock position and going clockwise (the CSS § 4.3.3
//!   convention). The `from` angle is added by the caller; this function is
//!   the raw projection.
//!
//! What does *not* live here:
//! - The `<angle>` grammar for `from <angle>`. The cascade-resolved angle is
//!   the input; [`crate::angle`] owns the parser.
//! - The `<position>` grammar (`at <position>`). The cascade-resolved centre
//!   is the input; [`crate::background_position`] owns the parser.
//! - The `<color-stop>` grammar parser. The stop list is shared with the
//!   linear-gradient surface via [`crate::gradient::ColorStop`]; the parser
//!   lives in the cascade and feeds both. The § 4.5 stop-position
//!   normalisation is shared via [`crate::gradient::resolve_stop_positions`].
//!
//! ## Sampling arithmetic
//!
//! For a conic gradient, the colour depends only on the angle from the
//! centre (not the distance). A pixel at offset `(dx, dy)` from the centre
//! lies at angle `θ = atan2(dx, -dy)` in standard math (positive dy points
//! down in CSS, so `-dy` flips to the math convention). The CSS § 4.3.3
//! "from 0" position is the 12-o'clock position (negative-y direction);
//! positive angles go clockwise.
//!
//! [`project_angle_to_t`] returns the angle in *turns* (one full revolution
//! = `1.0`), with `0.0` at 12 o'clock. Adding the `from` angle and reducing
//! modulo 1.0 gives the canonical `t ∈ [0, 1)` the colour-sampler samples.
//!
//! Reference: <https://www.w3.org/TR/css-images-4/#conic-gradients>,
//! § 4.3.3 "Conic Gradient Syntax".

#![forbid(unsafe_code)]

use crate::color::Color;
use crate::gradient::{ColorStop, resolve_stop_positions, sample_resolved};

// ---------------------------------------------------------------------------
// ConicGradient
// ---------------------------------------------------------------------------

/// A conic gradient: a stop list + the `from` angle (in radians) + the
/// `repeating` flag. The centre is caller-resolved (the paint path computes
/// per-pixel offsets from the centre).
///
/// Construct freely; per-pixel work is just [`project_angle_to_t`] +
/// [`ConicGradient::sample`].
#[derive(Debug, Clone, PartialEq)]
pub struct ConicGradient {
    pub stops: Vec<ColorStop>,
    /// The `from <angle>` start angle in radians. `0.0` = the 12-o'clock
    /// position; positive angles rotate clockwise (matching CSS).
    pub from_radians: f32,
    pub repeating: bool,
}

impl ConicGradient {
    /// Non-repeating conic gradient with `from = 0` (12-o'clock start).
    pub fn new(stops: Vec<ColorStop>) -> Self {
        Self {
            stops,
            from_radians: 0.0,
            repeating: false,
        }
    }

    /// Non-repeating conic gradient with a custom `from <angle>` start
    /// angle (in radians).
    pub fn with_from(stops: Vec<ColorStop>, from_radians: f32) -> Self {
        Self {
            stops,
            from_radians,
            repeating: false,
        }
    }

    /// Repeating variant (`repeating-conic-gradient()`).
    pub fn repeating(stops: Vec<ColorStop>) -> Self {
        Self {
            stops,
            from_radians: 0.0,
            repeating: true,
        }
    }

    /// Sample the gradient at the projected angle `t` (in *turns*: one full
    /// revolution = `1.0`). Returns the colour at that angle. For
    /// non-repeating gradients, `t` outside `[0, 1]` clamps to the end-stop
    /// colour; for repeating gradients, `t` wraps modulo the gradient's
    /// last-stop angle.
    ///
    /// Empty stop list ⇒ [`Color::TRANSPARENT`] (paint path treats degenerate
    /// gradients as no-op). A single stop is solid colour at every angle.
    pub fn sample(&self, t: f32) -> Color {
        if self.stops.is_empty() {
            return Color::TRANSPARENT;
        }
        let positions = resolve_stop_positions(&self.stops);
        if positions.len() == 1 {
            return self.stops[0].color;
        }
        sample_resolved(&positions, &self.stops, t, self.repeating)
    }
}

// ---------------------------------------------------------------------------
// Angle projection (pixel → t)
// ---------------------------------------------------------------------------

/// Project a pixel-offset `(dx, dy)` from the gradient centre onto the
/// conic-gradient parameter `t ∈ [0, 1)`, where `0` is the 12-o'clock
/// position and `1` is a full clockwise revolution back to 12 o'clock
/// (CSS Images 4 § 4.3.3).
///
/// The caller adds the `from <angle>` offset and reduces modulo 1.0 to get
/// the canonical `t`. The reduction ensures the projection itself is
/// independent of where the gradient's authored start angle is.
pub fn project_angle_to_t(dx: f32, dy: f32) -> f32 {
    // CSS conic gradients start at 12 o'clock (the -y direction in CSS,
    // where +y is down) and rotate clockwise. `atan2(dy, dx)` measures CCW
    // from the +x axis (3 o'clock), so adding 1/4 turn rotates the origin
    // to 12 o'clock and the direction is already clockwise (because +y is
    // down in CSS, atan2's CCW becomes clockwise in screen space).
    let base_turns = dy.atan2(dx) / (2.0 * core::f32::consts::PI);
    let turns = base_turns + 0.25;
    // Reduce to [0, 1).
    let wrapped = turns.rem_euclid(1.0);
    // Guard against `0.999...` rounding to exactly 1.0 from f32 imprecision.
    if wrapped >= 1.0 { 0.0 } else { wrapped }
}

/// Add the `from` angle (in radians) to a per-pixel `t` (in turns) and
/// reduce modulo 1.0. Convenience for the paint-path caller that already
/// has `t` from [`project_angle_to_t`] and the gradient's `from` angle.
pub fn add_from_angle(t: f32, from_radians: f32) -> f32 {
    let from_turns = from_radians / (2.0 * core::f32::consts::PI);
    let s = t + from_turns;
    // rem_euclid gives the [0, 1) range; a tiny epsilon guard prevents
    // exact-1.0 rounding.
    let wrapped = s.rem_euclid(1.0);
    if wrapped >= 1.0 { 0.0 } else { wrapped }
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

    // --- project_angle_to_t: cardinal directions -----------------------

    #[test]
    fn project_at_12_oclock_returns_zero() {
        // 12 o'clock = straight up = (dx=0, dy=-1) in CSS (y down).
        let t = project_angle_to_t(0.0, -1.0);
        assert!(approx(t, 0.0), "12 o'clock should be t=0; got {t}");
    }

    #[test]
    fn project_at_3_oclock_returns_quarter() {
        // 3 o'clock = right = (dx=1, dy=0). 90° clockwise from top = 0.25 turns.
        let t = project_angle_to_t(1.0, 0.0);
        assert!(approx(t, 0.25), "3 o'clock should be t=0.25; got {t}");
    }

    #[test]
    fn project_at_6_oclock_returns_half() {
        // 6 o'clock = straight down = (dx=0, dy=1). 180° = 0.5 turns.
        let t = project_angle_to_t(0.0, 1.0);
        assert!(approx(t, 0.5), "6 o'clock should be t=0.5; got {t}");
    }

    #[test]
    fn project_at_9_oclock_returns_three_quarters() {
        // 9 o'clock = left = (dx=-1, dy=0). 270° clockwise = 0.75 turns.
        let t = project_angle_to_t(-1.0, 0.0);
        assert!(approx(t, 0.75), "9 o'clock should be t=0.75; got {t}");
    }

    #[test]
    fn project_at_diagonal_45deg() {
        // Half-way between 12 and 3 o'clock = NE direction = (1, -1).
        // 45° clockwise from top = 0.125 turns.
        let t = project_angle_to_t(1.0, -1.0);
        assert!(approx(t, 0.125), "NE should be t=0.125; got {t}");
    }

    #[test]
    fn project_at_centre_is_defined() {
        // The centre pixel has no well-defined angle; atan2(0, 0) = 0 in
        // Rust, so the projection is the +0.25-turn origin offset (0.25).
        // The value is arbitrary; the test pins the documented behaviour.
        let t = project_angle_to_t(0.0, 0.0);
        assert!(approx(t, 0.25));
    }

    #[test]
    fn project_magnitude_invariant() {
        // The angle is independent of the pixel's distance from centre.
        let near = project_angle_to_t(0.5, 0.5);
        let far = project_angle_to_t(50.0, 50.0);
        assert!(approx(near, far));
    }

    // --- add_from_angle ------------------------------------------------

    #[test]
    fn add_from_angle_rotates_start() {
        // from = 90° (0.25 turns). The 12-o'clock position rotates to 0.25.
        let t = add_from_angle(0.0, std::f32::consts::FRAC_PI_2);
        assert!(approx(t, 0.25));
    }

    #[test]
    fn add_from_angle_wraps_around() {
        // from = 720° (2 full turns). t = 0.5 + 2 = 2.5, mod 1 = 0.5.
        let t = add_from_angle(0.5, 4.0 * std::f32::consts::PI);
        assert!(approx(t, 0.5));
    }

    // --- sample: colour at known angles --------------------------------

    #[test]
    fn sample_returns_transparent_for_empty_stops() {
        let g = ConicGradient::new(vec![]);
        assert_eq!(g.sample(0.5), Color::TRANSPARENT);
    }

    #[test]
    fn sample_single_stop_is_solid_colour() {
        let g = ConicGradient::new(vec![ColorStop::at(Color::rgb(255, 0, 0), 0.0)]);
        assert_eq!(g.sample(0.0), Color::rgb(255, 0, 0));
        assert_eq!(g.sample(0.5), Color::rgb(255, 0, 0));
        assert_eq!(g.sample(1.0), Color::rgb(255, 0, 0));
    }

    #[test]
    fn sample_two_stops_top_is_first_color() {
        let g = ConicGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        // t=0 → black.
        assert!(color_approx(g.sample(0.0), Color::BLACK));
        // t=1 → white (clamps to last stop).
        assert!(color_approx(g.sample(1.0), Color::WHITE));
    }

    #[test]
    fn sample_two_stops_half_turn_is_linear_blend() {
        // At t = 0.5 between BLACK and WHITE, linear-sRGB interpolation
        // produces a brighter mid-tone than naive sRGB averaging: encode of
        // linear 0.5 is ~0.744, so the channel value is ~190 (not 128).
        let g = ConicGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        let mid = g.sample(0.5);
        assert!(mid.r > 175 && mid.r < 205, "mid.r = {}", mid.r);
        assert_eq!(mid.r, mid.g);
        assert_eq!(mid.g, mid.b);
    }

    #[test]
    fn sample_outside_range_clamps() {
        let g = ConicGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        assert!(color_approx(g.sample(-0.5), Color::BLACK));
        assert!(color_approx(g.sample(1.5), Color::WHITE));
    }

    #[test]
    fn repeating_wraps_around() {
        let g = ConicGradient::repeating(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        let half = g.sample(0.5);
        let one_half = g.sample(1.5);
        assert!(
            color_approx(half, one_half),
            "repeating conic at t=1.5 should match t=0.5; got {half:?} vs {one_half:?}"
        );
    }

    // --- from-angle integration ----------------------------------------

    #[test]
    fn from_angle_zero_does_not_shift() {
        // Without a `from` angle, the 12-o'clock position is t=0.
        let dx = 0.0;
        let dy = -1.0;
        let t = project_angle_to_t(dx, dy);
        let final_t = add_from_angle(t, 0.0);
        assert!(approx(final_t, 0.0));
    }

    #[test]
    fn from_angle_90deg_shifts_12oclock_to_quarter() {
        // With `from: 90deg`, the colour that was at 12 o'clock now appears
        // at 9 o'clock (the start position rotated by 90° clockwise).
        let dx = -1.0; // 9 o'clock
        let dy = 0.0;
        let t = project_angle_to_t(dx, dy); // 0.75
        let final_t = add_from_angle(t, std::f32::consts::FRAC_PI_2); // +0.25
        // 0.75 + 0.25 = 1.0, wrapped to 0.0. The 9-o'clock pixel now sits at
        // the start of the gradient.
        assert!(approx(final_t, 0.0));
    }

    // --- end-to-end angle + sample -------------------------------------

    #[test]
    fn end_to_end_quartered_color_wheel() {
        // A four-stop gradient (red, yellow, green, blue) at quarter turns.
        let g = ConicGradient::new(vec![
            ColorStop::at(Color::rgb(255, 0, 0), 0.0),
            ColorStop::at(Color::rgb(0, 255, 0), 0.5),
            ColorStop::at(Color::rgb(0, 0, 255), 1.0),
        ]);
        // 12 o'clock = red.
        let t = project_angle_to_t(0.0, -1.0);
        assert!(color_approx(g.sample(t), Color::rgb(255, 0, 0)));
        // 3 o'clock = midpoint between red and green.
        let t = project_angle_to_t(1.0, 0.0);
        let c = g.sample(t);
        // Should be between red and green (yellow-ish in sRGB blending).
        assert!(c.r > 100 && c.g > 100);
        // 6 o'clock = green.
        let t = project_angle_to_t(0.0, 1.0);
        assert!(color_approx(g.sample(t), Color::rgb(0, 255, 0)));
    }
}
