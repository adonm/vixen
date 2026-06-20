//! CSS `border-radius` resolution — Phase 5 paint prep (pure logic called
//! out by `docs/PLAN.md` "Testing strategy" as a Rust-unit-test surface).
//! Turns the eight authored corner-radius values (a horizontal + vertical
//! radius per corner) into the four shaped corners the painter clips against,
//! applying the CSS Backgrounds 3 § 5.5 "corner shaping" scaling so a box
//! never has a corner that overflows its dimensions.
//!
//! What lives here:
//! - [`CornerRadius`] — one corner's `(h, v)` radii.
//! - [`BorderRadius`] — the four corners.
//! - [`BorderRadius::resolve`] — apply § 5.5: when adjacent radii on a side
//!   sum to more than that side's dimension, *both* are scaled down by the
//!   same factor (preserving their ratio), and clamp every radius to `≥ 0`.
//! - [`BorderRadius::circle_insquare`] — the common `border-radius: 50%`
//!   shortcut turned into exact radii for a known box.
//!
//! What does *not* live here:
//! - Percentage resolution. Authored values are often percentages (`50%`),
//!   but the cascade resolves those against the box dimensions before this
//!   module sees them — exactly the [`crate::length::LengthContext`] pattern.
//!   The caller hands `resolve` definite px radii + definite px sizes.
//! - Elliptical arc rasterisation (WebRender's job).
//!
//! CSS Backgrounds 3 § 5.5 scaling rule, applied per side:
//!
//! ```text
//! f_top    = min(1.0, border_box.width  / (top_left.h + top_right.h))
//! f_bottom = min(1.0, border_box.width  / (bottom_left.h + bottom_right.h))
//! f_left   = min(1.0, border_box.height / (top_left.v + bottom_left.v))
//! f_right  = min(1.0, border_box.height / (top_right.v + bottom_right.v))
//! ```
//!
//! Each corner's `h` is scaled by the smaller of its two adjacent side
//! factors; each corner's `v` likewise. (Backgrounds 3 § 5.5: "if the sum of
//! the two radii exceeds the side length, the UA must proportionally reduce
//! the radii …".) A radius of `0` produces a square corner.
//!
//! Reference: <https://www.w3.org/TR/css-backgrounds-3/#corner-shaping>,
//! § 5.2 `<border-radius>` syntax (<https://www.w3.org/TR/css-backgrounds-3/#border-radius>).

#![forbid(unsafe_code)]

/// A single corner radius: `(h, v)` are the horizontal and vertical radii of
/// the quarter-ellipse. Equal values give a circular arc.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerRadius {
    pub h: f32,
    pub v: f32,
}

impl CornerRadius {
    /// A circular corner of the given radius (`h == v`).
    pub const fn circle(r: f32) -> Self {
        Self { h: r, v: r }
    }

    /// A square corner (no rounding).
    pub const ZERO: CornerRadius = CornerRadius { h: 0.0, v: 0.0 };
}

/// The four corner radii of a box, in clock order from top-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderRadius {
    pub top_left: CornerRadius,
    pub top_right: CornerRadius,
    pub bottom_right: CornerRadius,
    pub bottom_left: CornerRadius,
}

impl BorderRadius {
    /// All four corners square (no rounding) — the initial value.
    pub const ZERO: BorderRadius = BorderRadius {
        top_left: CornerRadius::ZERO,
        top_right: CornerRadius::ZERO,
        bottom_right: CornerRadius::ZERO,
        bottom_left: CornerRadius::ZERO,
    };

    /// One radius on all four corners (the common `border-radius: 8px` form).
    pub const fn uniform(r: f32) -> Self {
        let c = CornerRadius::circle(r);
        Self {
            top_left: c,
            top_right: c,
            bottom_right: c,
            bottom_left: c,
        }
    }

    /// `border-radius: 50%` resolved against a known box → a perfect ellipse
    /// (a circle when the box is square). CSS Backgrounds 3 § 5.3.
    pub fn circle_insquare(w: f32, h: f32) -> Self {
        Self {
            top_left: CornerRadius {
                h: w / 2.0,
                v: h / 2.0,
            },
            top_right: CornerRadius {
                h: w / 2.0,
                v: h / 2.0,
            },
            bottom_right: CornerRadius {
                h: w / 2.0,
                v: h / 2.0,
            },
            bottom_left: CornerRadius {
                h: w / 2.0,
                v: h / 2.0,
            },
        }
    }

    /// Apply CSS Backgrounds 3 § 5.5 corner-shaping for a box of the given
    /// border-box dimensions. Returns radii clamped to `≥ 0` and scaled down
    /// proportionally where adjacent radii would overflow the side.
    pub fn resolve(self, w: f32, h: f32) -> BorderRadius {
        // Clamp negatives to 0 first (defensive — the cascade shouldn't emit
        // them, but malformed author CSS can).
        let tl = clamp0(self.top_left);
        let tr = clamp0(self.top_right);
        let br = clamp0(self.bottom_right);
        let bl = clamp0(self.bottom_left);

        let w = w.max(0.0);
        let h = h.max(0.0);

        // Per-side scale factors (§ 5.5). min(1, side_len / sum_of_adjacent).
        // Horizontal radii pair across the top/bottom edges; vertical radii
        // pair across the left/right edges.
        let f_top = scale_factor(w, tl.h + tr.h);
        let f_bottom = scale_factor(w, bl.h + br.h);
        let f_left = scale_factor(h, tl.v + bl.v);
        let f_right = scale_factor(h, tr.v + br.v);

        // Each corner's h uses min of the two horizontal-edge factors it
        // touches; each v uses min of the two vertical-edge factors. This is
        // the spec's "the reduction must be proportional" applied corner-wise.
        let tl_h = tl.h * f_top.min(f_left);
        let tl_v = tl.v * f_top.min(f_left);
        let tr_h = tr.h * f_top.min(f_right);
        let tr_v = tr.v * f_top.min(f_right);
        let br_h = br.h * f_bottom.min(f_right);
        let br_v = br.v * f_bottom.min(f_right);
        let bl_h = bl.h * f_bottom.min(f_left);
        let bl_v = bl.v * f_bottom.min(f_left);

        BorderRadius {
            top_left: CornerRadius { h: tl_h, v: tl_v },
            top_right: CornerRadius { h: tr_h, v: tr_v },
            bottom_right: CornerRadius { h: br_h, v: br_v },
            bottom_left: CornerRadius { h: bl_h, v: bl_v },
        }
    }
}

/// `min(1.0, dimension / sum)` — the § 5.5 per-side scale factor. Guards
/// against a zero dimension (returns 1.0, leaving radii untouched, so an empty
/// box doesn't divide by zero).
fn scale_factor(dimension: f32, sum: f32) -> f32 {
    if sum <= 0.0 || dimension <= 0.0 {
        return 1.0;
    }
    (dimension / sum).min(1.0)
}

fn clamp0(c: CornerRadius) -> CornerRadius {
    CornerRadius {
        h: c.h.max(0.0),
        v: c.v.max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn uniform_radius_passes_through_for_fitting_box() {
        // 10px radius on a 100x100 box fits (sum 20 < 100) → unchanged.
        let r = BorderRadius::uniform(10.0).resolve(100.0, 100.0);
        assert!(approx(r.top_left.h, 10.0));
        assert!(approx(r.top_left.v, 10.0));
        assert!(approx(r.bottom_right.h, 10.0));
    }

    #[test]
    fn adjacent_radii_scaled_proportionally_when_overflowing() {
        // top-left.h = 60, top-right.h = 60, width = 100 → sum 120 > 100.
        // f_top = 100/120 ≈ 0.8333 → both become 50. Vertical radii pair on
        // the left/right edges (height 100), which fit, so v is unchanged.
        let r = BorderRadius {
            top_left: CornerRadius { h: 60.0, v: 30.0 },
            top_right: CornerRadius { h: 60.0, v: 30.0 },
            bottom_right: CornerRadius::ZERO,
            bottom_left: CornerRadius::ZERO,
        }
        .resolve(100.0, 100.0);
        assert!(approx(r.top_left.h, 50.0), "tl.h {}", r.top_left.h);
        assert!(approx(r.top_right.h, 50.0), "tr.h {}", r.top_right.h);
        // The ratio 60:60 is preserved (still 1:1, both 50).
        assert!(approx(r.top_left.h, r.top_right.h));
        // Vertical: tl.v=30, bl.v=0 → sum 30 < 100, fits, but tl.v also
        // participates in the top edge (horizontal factor min'd in). The top
        // factor f_top≈0.833 dominates → tl.v scaled to 25.
        assert!(approx(r.top_left.v, 25.0), "tl.v {}", r.top_left.v);
    }

    #[test]
    fn ratio_preserved_when_one_radius_is_zero() {
        // top-left.h=100 on a 100-wide box with top-right.h=0. Sum=100 →
        // factor 1.0, no scaling (exactly fits).
        let r = BorderRadius {
            top_left: CornerRadius { h: 100.0, v: 0.0 },
            top_right: CornerRadius::ZERO,
            bottom_right: CornerRadius::ZERO,
            bottom_left: CornerRadius::ZERO,
        }
        .resolve(100.0, 100.0);
        assert!(approx(r.top_left.h, 100.0));
    }

    #[test]
    fn fifty_percent_makes_an_ellipse() {
        // border-radius: 50% on 200x100 → each corner h=100, v=50.
        let r = BorderRadius::circle_insquare(200.0, 100.0).resolve(200.0, 100.0);
        for c in [r.top_left, r.top_right, r.bottom_right, r.bottom_left] {
            assert!(approx(c.h, 100.0), "h {}", c.h);
            assert!(approx(c.v, 50.0), "v {}", c.v);
        }
    }

    #[test]
    fn fifty_percent_on_square_is_circle() {
        let r = BorderRadius::circle_insquare(100.0, 100.0).resolve(100.0, 100.0);
        for c in [r.top_left, r.top_right, r.bottom_right, r.bottom_left] {
            assert!(approx(c.h, 50.0));
            assert!(approx(c.v, 50.0));
        }
    }

    #[test]
    fn negative_radii_clamped_to_zero() {
        // The cascade shouldn't emit these, but author CSS / parsing bugs can.
        let r = BorderRadius::uniform(-5.0).resolve(100.0, 100.0);
        assert_eq!(r.top_left.h, 0.0);
        assert_eq!(r.top_left.v, 0.0);
    }

    #[test]
    fn zero_box_does_not_panic() {
        // Degenerate box: no division by zero, radii untouched (factor 1).
        let r = BorderRadius::uniform(5.0).resolve(0.0, 0.0);
        assert!(approx(r.top_left.h, 5.0));
    }

    #[test]
    fn zero_radius_is_square_corner() {
        assert_eq!(CornerRadius::ZERO.h, 0.0);
        assert_eq!(BorderRadius::ZERO.top_right.v, 0.0);
        // uniform(0) == ZERO.
        assert_eq!(BorderRadius::uniform(0.0).top_left, CornerRadius::ZERO);
    }

    #[test]
    fn all_four_sides_overflow_symmetrically() {
        // uniform(60) on 80x80: every side sums to 120 > 80 → factor 80/120.
        // Every corner becomes 60 * (80/120) = 40.
        let r = BorderRadius::uniform(60.0).resolve(80.0, 80.0);
        for c in [r.top_left, r.top_right, r.bottom_right, r.bottom_left] {
            assert!(approx(c.h, 40.0), "h {}", c.h);
            assert!(approx(c.v, 40.0), "v {}", c.v);
        }
    }

    #[test]
    fn elliptical_corner_kept_when_it_fits() {
        // h=30, v=10 on a 100x100 box fits everywhere → elliptical arc kept.
        let r = BorderRadius {
            top_left: CornerRadius { h: 30.0, v: 10.0 },
            top_right: CornerRadius::ZERO,
            bottom_right: CornerRadius::ZERO,
            bottom_left: CornerRadius::ZERO,
        }
        .resolve(100.0, 100.0);
        assert!(approx(r.top_left.h, 30.0));
        assert!(approx(r.top_left.v, 10.0));
    }
}
