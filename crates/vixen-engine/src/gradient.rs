//! CSS Images 4 linear-gradient color sampling — Phase 5 paint prep (pure
//! logic called out by `docs/PLAN.md` "Testing strategy" as a Rust-unit-test
//! surface). Implements the one-dimensional colour function the paint path
//! samples against: given a list of `ColorStop`s positioned along a gradient
//! line, [`LinearGradient::sample`] returns the colour at a fractional
//! position. The angle / direction → gradient-line geometry (CSS Images 4
//! § 4.1.1) is a paint-path concern; this module is the colour math it
//! reduces to.
//!
//! What lives here:
//! - [`ColorStop`] — a `(color, Option<position>)` pair. Positions are
//!   fractions of the gradient line in `[0, 1]`; `None` means "auto-
//!   distribute evenly between the surrounding positioned stops".
//! - [`LinearGradient`] — the stop list + the `repeating` flag.
//! - [`LinearGradient::sample`] — colour at `t ∈ ℝ`. Non-repeating gradients
//!   clamp outside `[0, 1]` (the ends are solid); repeating gradients wrap.
//! - [`resolve_stop_positions`] — the CSS Images 4 § 4.5 "Color stops"
//!   normalisation algorithm (auto-position fill-in, monotonicity fix-up,
//!   first/last defaults).
//!
//! What does *not* live here:
//! - Angle / direction → gradient-line mapping (CSS Images 4 § 4.1.1). The
//!   paint path computes the per-pixel `t` from the gradient line; we accept
//!   `t` directly.
//! - Radial / conic gradients (CSS Images 4 § 4.2 / § 4.4). They share the
//!   color-stop resolution algorithm but sample along a different axis;
//!   this module is the linear case.
//! - Interpolation in non-sRGB spaces (CSS Color 4 § 13 "interpolate color
//!   in xyz / oklab / …"). v1.0 stays in linear-sRGB (the
//!   [`crate::color::interpolate`] default); the post-v1.0 colour-4 work
//!   lifts this.
//!
//! ## CSS Images 4 § 4.5 normalisation
//!
//! Given a list of `(color, position)`:
//! 1. If the first stop has no position, set it to `0`.
//! 2. If the last stop has no position, set it to `1`.
//! 3. If any middle stop has no position, distribute evenly between the
//!    nearest positioned neighbours (e.g. positioned stops at 0 and 1 with
//!    two auto stops in between → `0`, `0.33`, `0.67`, `1`).
//! 4. Enforce monotonicity: if any stop's position is less than the
//!    previous, set it equal to the previous.
//!
//! Reference: <https://www.w3.org/TR/css-images-4/#color-stop-syntax>,
//! § 4.5 "Color Stop Syntax" + § 4.3 "Color Interpolation" (linear-sRGB).

#![forbid(unsafe_code)]

use crate::color::{Color, interpolate};

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// One colour stop along a linear gradient. `position` is a fraction of the
/// gradient line; `None` means "auto-distribute between the surrounding
/// positioned stops" per CSS Images 4 § 4.5.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorStop {
    pub color: Color,
    pub position: Option<f32>,
}

impl ColorStop {
    /// A stop with an explicit position in `[0, 1]`.
    pub const fn at(color: Color, position: f32) -> Self {
        Self {
            color,
            position: Some(position),
        }
    }

    /// A stop with no explicit position (auto-distributed).
    pub const fn auto(color: Color) -> Self {
        Self {
            color,
            position: None,
        }
    }
}

/// A linear gradient: a stop list + the `repeating` flag. Non-repeating
/// gradients clamp `t` to `[0, 1]` (the end stops are solid beyond their
/// positions); repeating gradients wrap `t` mod `1` so the colour function
/// tiles.
///
/// Construct freely; sampling is via [`LinearGradient::sample`].
#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    pub stops: Vec<ColorStop>,
    pub repeating: bool,
}

impl LinearGradient {
    /// Non-repeating gradient with the given stops.
    pub fn new(stops: Vec<ColorStop>) -> Self {
        Self {
            stops,
            repeating: false,
        }
    }

    /// Repeating variant (`repeating-linear-gradient()`).
    pub fn repeating(stops: Vec<ColorStop>) -> Self {
        Self {
            stops,
            repeating: true,
        }
    }

    /// Sample the gradient at position `t ∈ ℝ`. Returns the colour at that
    /// position. For non-repeating gradients, `t` outside `[0, 1]` clamps to
    /// the end-stop colour; for repeating gradients, `t` wraps modulo `1`.
    ///
    /// If the gradient has no stops, returns [`Color::TRANSPARENT`] (defensive
    /// — paint path treats degenerate gradients as no-op). A single stop is
    /// solid colour at every `t`.
    pub fn sample(&self, t: f32) -> Color {
        if self.stops.is_empty() {
            return Color::TRANSPARENT;
        }
        let positions = resolve_stop_positions(&self.stops);
        if positions.len() == 1 {
            return self.stops[0].color;
        }

        // For repeating: wrap t to [0, 1) so the colour function tiles.
        let t = if self.repeating {
            // f32-aware modulo. Negative t wraps around to the end.
            let wrapped = t.rem_euclid(1.0);
            // Guard against `rem_euclid` returning exactly 1.0 due to f32
            // rounding (the only way is when t == 0.0 going in; trivially
            // fine).
            if wrapped >= 1.0 { 0.0 } else { wrapped }
        } else {
            t
        };

        // Find the segment [t_i, t_{i+1}] containing t (or the end stops if
        // outside the range for the non-repeating case).
        sample_resolved(&positions, &self.stops, t, self.repeating)
    }
}

// ---------------------------------------------------------------------------
// Position resolution (CSS Images 4 § 4.5)
// ---------------------------------------------------------------------------

/// Normalise a stop list into a parallel vector of definite positions in
/// `[0, 1]`. Implements CSS Images 4 § 4.5:
///
/// 1. First stop defaults to position `0` if `None`.
/// 2. Last stop defaults to position `1` if `None`.
/// 3. Middle stops with `None` positions distribute evenly between the
///    nearest surrounding *positioned* stops.
/// 4. Monotonicity: any position less than its predecessor is raised to the
///    predecessor's value (so the list is non-decreasing).
///
/// If only one stop is supplied, the result has a single `0.0` entry (the
/// gradient is solid colour — callers handle that path separately).
pub fn resolve_stop_positions(stops: &[ColorStop]) -> Vec<f32> {
    let n = stops.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![stops[0].position.unwrap_or(0.0).clamp(0.0, 1.0)];
    }

    let mut out: Vec<Option<f32>> = stops.iter().map(|s| s.position).collect();

    // Step 1: first defaults to 0.
    if out[0].is_none() {
        out[0] = Some(0.0);
    }
    // Step 2: last defaults to 1.
    if out[n - 1].is_none() {
        out[n - 1] = Some(1.0);
    }

    // Step 3: middle None positions distribute evenly between surrounding
    // positioned stops. Walk forward; each contiguous run of None positions
    // is filled between the previous positioned stop and the next positioned
    // stop.
    let mut i = 1;
    while i < n - 1 {
        if out[i].is_some() {
            i += 1;
            continue;
        }
        // Find the end of this run of None positions.
        let run_start = i;
        let mut run_end = i;
        while run_end < n - 1 && out[run_end].is_none() {
            run_end += 1;
        }
        // out[run_end] is positioned (the next anchor). Distribute.
        let start_pos = out[run_start - 1].unwrap();
        let end_pos = out[run_end].unwrap();
        let count = (run_end - run_start + 1) as f32; // intervals, not items
        for (k, idx) in (run_start..run_end).enumerate() {
            let frac = (k + 1) as f32 / count;
            out[idx] = Some(start_pos + (end_pos - start_pos) * frac);
        }
        i = run_end + 1;
    }

    // Step 4: clamp to [0, 1] + enforce monotonicity.
    let mut resolved: Vec<f32> = out
        .iter()
        .map(|&p| p.unwrap_or(0.0).clamp(0.0, 1.0))
        .collect();
    for k in 1..n {
        if resolved[k] < resolved[k - 1] {
            resolved[k] = resolved[k - 1];
        }
    }
    resolved
}

// ---------------------------------------------------------------------------
// Sampling (private; works on already-resolved positions)
// ---------------------------------------------------------------------------

/// Sample a normalised gradient at `t`. `positions` and `stops` are parallel.
/// For non-repeating gradients, `t` outside `[0, 1]` clamps to the end stop.
///
/// Exposed `pub(crate)` so the radial-gradient + conic-gradient siblings can
/// reuse the segment-search + interpolation pipeline (they only differ in
/// how they project a pixel onto `t`).
pub(crate) fn sample_resolved(
    positions: &[f32],
    stops: &[ColorStop],
    t: f32,
    repeating: bool,
) -> Color {
    let n = positions.len();
    debug_assert_eq!(n, stops.len());
    debug_assert!(n >= 2);

    // Repeating gradients already wrapped `t` into [0, 1); we can treat the
    // range [positions[0], positions[n-1]] as one tile.
    let first = positions[0];
    let last = positions[n - 1];

    // Pre-first: clamp to first stop colour (non-repeating only — repeating
    // already wrapped, but a stop list like [(red, 0.0), (blue, 0.5)] with
    // repeating would have t in [0.5, 1) undefined; we treat the [0, first]
    // and [last, 1] regions as the first/last stop colours too).
    if t <= first {
        if repeating && last > first {
            // Tile: shift t into [first, last] via modulo.
            let span = last - first;
            if span > 0.0 {
                let local = (t - first).rem_euclid(span) + first;
                return sample_segment(positions, stops, local);
            }
        }
        return stops[0].color;
    }
    if t >= last {
        if repeating && last > first {
            let span = last - first;
            if span > 0.0 {
                let local = (t - first).rem_euclid(span) + first;
                return sample_segment(positions, stops, local);
            }
        }
        return stops[n - 1].color;
    }
    sample_segment(positions, stops, t)
}

/// Linear-search the segment containing `t` (gradient stop lists are short —
/// ~3–8 stops — so binary search is needless complexity). Interpolates in
/// linear-sRGB via [`crate::color::interpolate`].
fn sample_segment(positions: &[f32], stops: &[ColorStop], t: f32) -> Color {
    let n = positions.len();
    for i in 0..n - 1 {
        let t0 = positions[i];
        let t1 = positions[i + 1];
        if t0 <= t && t <= t1 {
            let span = t1 - t0;
            let local_t = if span <= 0.0 { 0.0 } else { (t - t0) / span };
            return interpolate(stops[i].color, stops[i + 1].color, local_t);
        }
    }
    // Shouldn't reach here (caller clamps), but be safe.
    stops[n - 1].color
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Color, b: Color) -> bool {
        // Allow 2 LSB of slack — interpolate does quantise to u8, but linear-
        // sRGB decoding then re-encoding can round differently than the
        // straight average the test wants.
        a.r.abs_diff(b.r) <= 2
            && a.g.abs_diff(b.g) <= 2
            && a.b.abs_diff(b.b) <= 2
            && a.a.abs_diff(b.a) <= 2
    }

    // --- resolve_stop_positions -----------------------------------------

    #[test]
    fn two_stops_default_to_0_and_1() {
        let stops = vec![ColorStop::auto(Color::BLACK), ColorStop::auto(Color::WHITE)];
        assert_eq!(resolve_stop_positions(&stops), vec![0.0, 1.0]);
    }

    #[test]
    fn explicit_positions_preserved() {
        let stops = vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ];
        assert_eq!(resolve_stop_positions(&stops), vec![0.0, 1.0]);
        let stops = vec![
            ColorStop::at(Color::BLACK, 0.25),
            ColorStop::at(Color::WHITE, 0.75),
        ];
        assert_eq!(resolve_stop_positions(&stops), vec![0.25, 0.75]);
    }

    #[test]
    fn auto_stops_between_positioned_anchors_distribute_evenly() {
        // red@0, [auto green], [auto blue], white@1 ⇒ 0, 1/3, 2/3, 1.
        let stops = vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::auto(Color::rgb(255, 0, 0)),
            ColorStop::auto(Color::rgb(0, 255, 0)),
            ColorStop::at(Color::WHITE, 1.0),
        ];
        let p = resolve_stop_positions(&stops);
        assert!((p[0] - 0.0).abs() < 1e-4);
        assert!((p[1] - 1.0 / 3.0).abs() < 1e-4);
        assert!((p[2] - 2.0 / 3.0).abs() < 1e-4);
        assert!((p[3] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn auto_stops_distribute_within_partial_range() {
        // Two anchors at 0.2 and 0.8 with two auto stops between → 0.2, 0.4,
        // 0.6, 0.8.
        let stops = vec![
            ColorStop::at(Color::BLACK, 0.2),
            ColorStop::auto(Color::rgb(1, 0, 0)),
            ColorStop::auto(Color::rgb(0, 1, 0)),
            ColorStop::at(Color::WHITE, 0.8),
        ];
        let p = resolve_stop_positions(&stops);
        assert!((p[0] - 0.2).abs() < 1e-4);
        assert!((p[1] - 0.4).abs() < 1e-4);
        assert!((p[2] - 0.6).abs() < 1e-4);
        assert!((p[3] - 0.8).abs() < 1e-4);
    }

    #[test]
    fn monotonicity_violation_fixed_to_previous() {
        // Stops with positions [0.5, 0.2, 0.8] ⇒ [0.5, 0.5, 0.8].
        let stops = vec![
            ColorStop::at(Color::BLACK, 0.5),
            ColorStop::at(Color::rgb(1, 0, 0), 0.2),
            ColorStop::at(Color::WHITE, 0.8),
        ];
        let p = resolve_stop_positions(&stops);
        assert!((p[0] - 0.5).abs() < 1e-4);
        assert!((p[1] - 0.5).abs() < 1e-4);
        assert!((p[2] - 0.8).abs() < 1e-4);
    }

    #[test]
    fn positions_clamped_to_unit_interval() {
        let stops = vec![
            ColorStop::at(Color::BLACK, -0.5),
            ColorStop::at(Color::WHITE, 1.5),
        ];
        let p = resolve_stop_positions(&stops);
        assert_eq!(p, vec![0.0, 1.0]);
    }

    #[test]
    fn single_stop_defaults_to_zero() {
        let stops = vec![ColorStop::auto(Color::BLACK)];
        assert_eq!(resolve_stop_positions(&stops), vec![0.0]);
    }

    #[test]
    fn empty_stops_returns_empty() {
        assert!(resolve_stop_positions(&[]).is_empty());
    }

    // --- sample: ends ---------------------------------------------------

    #[test]
    fn sample_at_start_returns_first_stop_color() {
        let g = LinearGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        assert_eq!(g.sample(0.0), Color::BLACK);
    }

    #[test]
    fn sample_at_end_returns_last_stop_color() {
        let g = LinearGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        assert_eq!(g.sample(1.0), Color::WHITE);
    }

    #[test]
    fn sample_below_start_clamps_to_first() {
        // Non-repeating: t < 0 → first colour.
        let g = LinearGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        assert_eq!(g.sample(-0.5), Color::BLACK);
    }

    #[test]
    fn sample_above_end_clamps_to_last() {
        let g = LinearGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        assert_eq!(g.sample(1.5), Color::WHITE);
    }

    // --- sample: midpoint -----------------------------------------------

    #[test]
    fn sample_at_half_interpolates_in_linear_srgb() {
        // 50% blend of black + white in linear sRGB ≈ rgb(188, 188, 188)
        // (NOT the sRGB-blend rgb(127,127,127); the linear-space midpoint is
        // brighter, which is why gradients look correct this way).
        let g = LinearGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        let c = g.sample(0.5);
        // Allow 2 LSB of slack for the round-trip quantisation.
        assert!((c.r as i16 - 188).abs() <= 2, "r={}", c.r);
        assert!((c.g as i16 - 188).abs() <= 2, "g={}", c.g);
        assert!((c.b as i16 - 188).abs() <= 2, "b={}", c.b);
        assert_eq!(c.a, 255);
    }

    #[test]
    fn sample_at_third_interpolates_correctly() {
        // 1/3 blend of red + blue in linear sRGB. Red linear ≈ (0.2126, ...);
        // blue linear ≈ (0.0722, ..., 0.7155). Mix at t=1/3 gives mostly red
        // with a little blue. Exact values verified by symmetry: the alpha
        // stays at 255 and r > b in the result.
        let g = LinearGradient::new(vec![
            ColorStop::at(Color::rgb(255, 0, 0), 0.0),
            ColorStop::at(Color::rgb(0, 0, 255), 1.0),
        ]);
        let c = g.sample(1.0 / 3.0);
        assert_eq!(c.a, 255);
        assert!(
            c.r > c.b,
            "at 1/3, red should dominate: r={} b={}",
            c.r,
            c.b
        );
    }

    // --- sample: degenerate ---------------------------------------------

    #[test]
    fn empty_stops_sample_transparent() {
        let g = LinearGradient::new(vec![]);
        assert_eq!(g.sample(0.5), Color::TRANSPARENT);
    }

    #[test]
    fn single_stop_is_solid() {
        let g = LinearGradient::new(vec![ColorStop::auto(Color::rgb(10, 20, 30))]);
        assert_eq!(g.sample(0.0), Color::rgb(10, 20, 30));
        assert_eq!(g.sample(0.5), Color::rgb(10, 20, 30));
        assert_eq!(g.sample(1.0), Color::rgb(10, 20, 30));
    }

    // --- sample: multi-stop ---------------------------------------------

    #[test]
    fn three_stops_with_explicit_positions() {
        // black@0, red@0.5, white@1. At t=0.25, halfway between black+red.
        let g = LinearGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::at(Color::rgb(255, 0, 0), 0.5),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        // At t=0.25, local_t within the [0, 0.5] segment is 0.5.
        let c = g.sample(0.25);
        let expected = interpolate(Color::BLACK, Color::rgb(255, 0, 0), 0.5);
        assert!(approx(c, expected));
    }

    #[test]
    fn auto_positions_distribute_then_sample() {
        // Two auto stops between black@0 and white@1 ⇒ 0, 1/3, 2/3, 1.
        // At t=1/3 we should be at the second stop exactly.
        let g = LinearGradient::new(vec![
            ColorStop::at(Color::BLACK, 0.0),
            ColorStop::auto(Color::rgb(255, 0, 0)),
            ColorStop::auto(Color::rgb(0, 255, 0)),
            ColorStop::at(Color::WHITE, 1.0),
        ]);
        let c = g.sample(1.0 / 3.0);
        assert_eq!(c, Color::rgb(255, 0, 0), "t=1/3 should be red");
    }

    // --- repeating ------------------------------------------------------

    #[test]
    fn repeating_wraps_around_at_one() {
        // repeating-linear-gradient(red 0, blue 1) at t=1.5 should equal the
        // colour at t=0.5 within the tile.
        let g = LinearGradient::repeating(vec![
            ColorStop::at(Color::rgb(255, 0, 0), 0.0),
            ColorStop::at(Color::rgb(0, 0, 255), 1.0),
        ]);
        let c1 = g.sample(0.5);
        let c2 = g.sample(1.5);
        assert!(approx(c1, c2), "{c1:?} vs {c2:?}");
    }

    #[test]
    fn repeating_handles_negative_t() {
        let g = LinearGradient::repeating(vec![
            ColorStop::at(Color::rgb(255, 0, 0), 0.0),
            ColorStop::at(Color::rgb(0, 0, 255), 1.0),
        ]);
        // -0.5 wraps to 0.5 (modulo arithmetic, positive direction).
        let c_neg = g.sample(-0.5);
        let c_half = g.sample(0.5);
        assert!(approx(c_neg, c_half), "{c_neg:?} vs {c_half:?}");
    }

    #[test]
    fn repeating_with_partial_range_tiles_correctly() {
        // repeating-linear-gradient(red 0.25, blue 0.75). The tile is [0.25,
        // 0.75]; outside that the colour is the same as the same offset
        // within the tile. So sample at 0.0 should match sample at 0.5 (both
        // at the start of the tile).
        let g = LinearGradient::repeating(vec![
            ColorStop::at(Color::rgb(255, 0, 0), 0.25),
            ColorStop::at(Color::rgb(0, 0, 255), 0.75),
        ]);
        let c_zero = g.sample(0.0);
        let c_half = g.sample(0.5);
        assert!(approx(c_zero, c_half), "{c_zero:?} vs {c_half:?}");
    }

    // --- ColorStop constructors ----------------------------------------

    #[test]
    fn color_stop_at_carries_position() {
        let s = ColorStop::at(Color::BLACK, 0.5);
        assert_eq!(s.color, Color::BLACK);
        assert_eq!(s.position, Some(0.5));
    }

    #[test]
    fn color_stop_auto_is_none() {
        let s = ColorStop::auto(Color::WHITE);
        assert_eq!(s.color, Color::WHITE);
        assert!(s.position.is_none());
    }
}
