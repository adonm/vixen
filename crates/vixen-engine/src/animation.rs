//! Web Animations § 5 — the timing model the CSS `transition` / `animation`
//! drivers + the `Animation` / `KeyframeEffect` host hooks reduce to (Phase 5
//! prep). Pure given the cascade-resolved timing + a monotonic local time;
//! the keyframe interpolation (per-property value sampling at the computed
//! progress) + the animation frame scheduling stay in the paint / event-loop
//! layer.
//!
//! What lives here:
//! - [`EffectTiming`] — the § 5.4 timing properties: `delay` / `end_delay`
//!   / `fill` / `iteration_start` / `iterations` / `duration` /
//!   `direction`. `duration` is in ms (the cascade resolves `auto` → `0`
//!   for animations without an intrinsic duration before constructing).
//! - [`Fill`] — § 5.4 `fill` (`none` / `forwards` / `backwards` / `both`).
//! - [`PlaybackDirection`] — § 5.4 `direction` (`normal` / `reverse` /
//!   `alternate` / `alternate-reverse`).
//! - [`EffectPhase`] — § 5.5 `before` / `active` / `after`.
//! - [`active_duration`] / [`end_time`] — the § 5.3 derived times.
//! - [`phase`] — the § 5.5 phase classification given a local time.
//! - [`simple_iteration_progress`] / [`current_iteration`] — the § 5.5
//!   iteration progress + the iteration index.
//! - [`directed_progress`] — the § 5.6 direction-aware progress.
//! - [`apply_easing`] — the § 5.7 transformed progress (consumes
//!   [`crate::easing::Easing`]).
//! - [`compute_timing`] — the top-level § 5.5–§ 5.7 pipeline producing a
//!   [`ComputedTiming`] (phase + current iteration + simple + transformed
//!   progress, with the fill-mode before/after resolution).
//!
//! What does *not* live here:
//! - The keyframe value interpolation (the per-property `progress → value`
//!   sampling) — the paint path's interpolation; this module produces the
//!   `progress` it samples at.
//! - The animation frame scheduling + the `Animation.currentTime` /
//!   `playState` / playback-rate surface — the event-loop layer; this
//!   module is the pure progress math given a local time.
//! - The `auto` duration resolution (the § 5.4 intrinsic-duration
//!   computation) — the caller resolves `auto` to a definite ms (CSS
//!   animations have no intrinsic duration ⇒ `0`; CSS transitions use the
//!   property's duration; the host hook resolves).
//! - Group effects (sequence / parallel) — the § 5.2 group timing; v1.0
//!   models a single effect.
//!
//! ## The § 5.5 pipeline
//!
//! Given a `local time` (ms, the animation's current time relative to its
//! start) + an [`EffectTiming`]:
//!
//! ```text
//! active duration = duration × iterations
//! end time        = max(delay + active duration + end delay, delay)
//!
//! phase:
//!   before  if local time < delay
//!   after   if active duration finite ∧ local time ≥ delay + active duration
//!   active  otherwise
//!
//! active phase:
//!   overall progress = (local time − delay) / duration + iteration start
//!   current iteration = ⌊overall progress⌋
//!   simple progress   = overall progress − ⌊overall progress⌋
//!
//! after phase:
//!   iterations = 0  ⇒ current iteration 0, simple progress 1
//!   else        ⇒ current iteration (⌈iterations + iteration start⌉ − 1),
//!                 simple progress 1 if iterations integral else fract
//!
//! directed progress (§ 5.6):
//!   normal            ⇒ simple
//!   reverse           ⇒ 1 − simple
//!   alternate         ⇒ even iteration: simple;   odd: 1 − simple
//!   alternate-reverse ⇒ even iteration: 1 − simple; odd: simple
//!
//! transformed progress (§ 5.7) = easing(directed)
//! ```
//!
//! The fill mode decides whether the before/after phase produces a progress
//! (`backwards`/`both` ⇒ the iteration-0 start in before; `forwards`/`both`
//! ⇒ the end state in after) or `None` (the effect has no effect).
//!
//! Reference: <https://www.w3.org/TR/web-animations-1/> (§ 5 the timing model).

#![forbid(unsafe_code)]

use crate::easing::Easing;

// ---------------------------------------------------------------------------
// Fill + PlaybackDirection + EffectTiming
// ---------------------------------------------------------------------------

/// CSS Web Animations § 5.4 `fill` — which phases the effect applies a value
/// in when not in the active phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Fill {
    /// `none` (the default) — no effect outside the active phase.
    #[default]
    None,
    /// `forwards` — the after phase applies the end value.
    Forwards,
    /// `backwards` — the before phase applies the iteration-0 start value.
    Backwards,
    /// `both` — both before + after apply.
    Both,
}

impl Fill {
    /// Parse the `fill` keyword (ASCII-case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "forwards" => Some(Self::Forwards),
            "backwards" => Some(Self::Backwards),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    /// `true` iff the before phase applies a value.
    pub fn applies_before(self) -> bool {
        matches!(self, Self::Backwards | Self::Both)
    }

    /// `true` iff the after phase applies a value.
    pub fn applies_after(self) -> bool {
        matches!(self, Self::Forwards | Self::Both)
    }
}

/// CSS Web Animations § 5.4 `direction` — the per-iteration playback
/// direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PlaybackDirection {
    /// `normal` (the default) — each iteration runs start → end.
    #[default]
    Normal,
    /// `reverse` — each iteration runs end → start.
    Reverse,
    /// `alternate` — iterations alternate start → end, end → start, …
    Alternate,
    /// `alternate-reverse` — iterations alternate end → start, start → end, …
    AlternateReverse,
}

impl PlaybackDirection {
    /// Parse the `direction` keyword (ASCII-case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(Self::Normal),
            "reverse" => Some(Self::Reverse),
            "alternate" => Some(Self::Alternate),
            "alternate-reverse" => Some(Self::AlternateReverse),
            _ => None,
        }
    }
}

/// The § 5.4 animation effect timing. All times in milliseconds. `duration`
/// is the per-iteration duration (the caller resolves `auto` → a definite ms
/// before constructing; `0` ⇒ the active phase is empty).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectTiming {
    /// `delay` (the § 5.4 start delay, ms). Default `0`.
    pub delay: f64,
    /// `end-delay` (ms). Default `0`.
    pub end_delay: f64,
    /// `fill`. Default [`Fill::None`].
    pub fill: Fill,
    /// `iteration-start` (the § 5.4 offset into the first iteration,
    /// `0..iterations`). Default `0`.
    pub iteration_start: f64,
    /// `iterations` (the § 5.4 iteration count; may be fractional, may be
    /// `f64::INFINITY`). Default `1`.
    pub iterations: f64,
    /// `duration` (the per-iteration duration in ms; must be `≥ 0`).
    pub duration: f64,
    /// `direction`. Default [`PlaybackDirection::Normal`].
    pub direction: PlaybackDirection,
}

impl Default for EffectTiming {
    fn default() -> Self {
        Self {
            delay: 0.0,
            end_delay: 0.0,
            fill: Fill::None,
            iteration_start: 0.0,
            iterations: 1.0,
            duration: 0.0,
            direction: PlaybackDirection::Normal,
        }
    }
}

impl EffectTiming {
    /// Construct a timing with just a duration (the common CSS
    /// `transition: <duration>` shape).
    pub const fn for_duration(duration: f64) -> Self {
        Self {
            delay: 0.0,
            end_delay: 0.0,
            fill: Fill::None,
            iteration_start: 0.0,
            iterations: 1.0,
            duration,
            direction: PlaybackDirection::Normal,
        }
    }
}

// ---------------------------------------------------------------------------
// EffectPhase + the derived times
// ---------------------------------------------------------------------------

/// The § 5.5 phase an effect is in at a given local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectPhase {
    /// Before the active interval (`local time < delay`).
    Before,
    /// Within the active interval.
    Active,
    /// After the active interval.
    After,
}

/// The § 5.3 `active duration` = `duration × iterations`. `Infinity` if
/// either factor is `Infinity`.
pub fn active_duration(timing: &EffectTiming) -> f64 {
    if timing.duration.is_infinite() || timing.iterations.is_infinite() {
        f64::INFINITY
    } else {
        timing.duration * timing.iterations
    }
}

/// The § 5.3 `end time` = `max(delay + active_duration + end_delay, delay)`.
pub fn end_time(timing: &EffectTiming) -> f64 {
    let ad = active_duration(timing);
    let candidate = timing.delay + ad + timing.end_delay;
    candidate.max(timing.delay)
}

/// The § 5.5 phase classification given a `local_time` (ms). The active
/// interval is `[delay, delay + active_duration)`; the after phase begins at
/// `delay + active_duration` (when finite).
pub fn phase(timing: &EffectTiming, local_time: f64) -> EffectPhase {
    let ad = active_duration(timing);
    if local_time < timing.delay {
        EffectPhase::Before
    } else if ad.is_finite() && local_time >= timing.delay + ad {
        EffectPhase::After
    } else {
        EffectPhase::Active
    }
}

// ---------------------------------------------------------------------------
// iteration progress + current iteration
// ---------------------------------------------------------------------------

/// The § 5.5 current iteration index at a `local_time` in the active/after
/// phase, or `None` in the before phase. Capped at `u64::MAX` to guard an
/// unbounded `Infinity`-iteration active phase.
pub fn current_iteration(timing: &EffectTiming, local_time: f64) -> Option<u64> {
    let phase = phase(timing, local_time);
    match phase {
        EffectPhase::Before => None,
        EffectPhase::Active => {
            let overall = overall_progress(timing, local_time);
            Some(floor_to_u64(overall))
        }
        EffectPhase::After => {
            if timing.iterations == 0.0 {
                Some(0)
            } else {
                let overall = timing.iterations + timing.iteration_start;
                Some(floor_to_u64((overall.ceil() - 1.0).max(0.0)))
            }
        }
    }
}

/// The § 5.5 simple iteration progress (`0..1`) at a `local_time`, or `None`
/// in the before phase (or any phase the effect has no progress).
pub fn simple_iteration_progress(timing: &EffectTiming, local_time: f64) -> Option<f64> {
    let phase = phase(timing, local_time);
    match phase {
        EffectPhase::Before => None,
        EffectPhase::Active => {
            let overall = overall_progress(timing, local_time);
            let sp = overall - overall.floor();
            Some(sp)
        }
        EffectPhase::After => {
            if timing.iterations == 0.0 || timing.iterations.fract() == 0.0 {
                Some(1.0)
            } else {
                Some(timing.iterations.fract())
            }
        }
    }
}

/// The § 5.5 "overall progress" in the active phase:
/// `(local_time − delay) / duration + iteration_start`. Guards a zero
/// duration (returns `iteration_start`; the active phase is empty when
/// `duration = 0` so this is only hit via a caller that forced it).
fn overall_progress(timing: &EffectTiming, local_time: f64) -> f64 {
    let per = if timing.duration > 0.0 {
        (local_time - timing.delay) / timing.duration
    } else {
        0.0
    };
    per + timing.iteration_start
}

/// The § 5.6 directed progress — the `direction`-aware progress. `simple` is
/// the simple iteration progress; `iteration` is the current iteration index.
pub fn directed_progress(direction: PlaybackDirection, simple: f64, iteration: u64) -> f64 {
    let odd = iteration % 2 == 1;
    match direction {
        PlaybackDirection::Normal => simple,
        PlaybackDirection::Reverse => 1.0 - simple,
        PlaybackDirection::Alternate => {
            if odd {
                1.0 - simple
            } else {
                simple
            }
        }
        PlaybackDirection::AlternateReverse => {
            if odd {
                simple
            } else {
                1.0 - simple
            }
        }
    }
}

/// The § 5.7 transformed progress — the easing applied to the directed
/// progress. `easing = None` ⇒ the identity (the directed progress as-is).
pub fn apply_easing(directed: f64, easing: Option<&Easing>) -> f64 {
    match easing {
        Some(e) => e.evaluate(directed),
        None => directed,
    }
}

/// `⌊v⌋` as a `u64`, capped at `u64::MAX` for non-finite / huge values.
fn floor_to_u64(v: f64) -> u64 {
    if !v.is_finite() || v >= u64::MAX as f64 {
        u64::MAX
    } else if v <= 0.0 {
        0
    } else {
        v.floor() as u64
    }
}

// ---------------------------------------------------------------------------
// ComputedTiming + the top-level pipeline
// ---------------------------------------------------------------------------

/// The § 5.5–§ 5.7 computed timing for one effect at one local time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComputedTiming {
    /// The phase.
    pub phase: EffectPhase,
    /// The current iteration index (`None` in the before phase).
    pub current_iteration: Option<u64>,
    /// The simple iteration progress (`0..1`; `None` in the before phase).
    pub simple_progress: Option<f64>,
    /// The transformed (directed + eased) progress the keyframe interpolation
    /// samples at. `None` when the effect has no effect (the before/after
    /// phase with a `fill` that doesn't apply).
    pub progress: Option<f64>,
    /// The § 5.3 active duration.
    pub active_duration: f64,
    /// The § 5.3 end time.
    pub end_time: f64,
}

/// The top-level § 5.5–§ 5.7 pipeline: classify the phase, compute the
/// simple + directed + transformed progress, and resolve the fill mode for
/// the before/after phases. `easing = None` ⇒ the identity timing function.
pub fn compute_timing(
    timing: &EffectTiming,
    local_time: f64,
    easing: Option<&Easing>,
) -> ComputedTiming {
    let active_dur = active_duration(timing);
    let end = end_time(timing);
    let ph = phase(timing, local_time);
    let ci = current_iteration(timing, local_time);
    let simple = simple_iteration_progress(timing, local_time);
    // The transformed progress, honouring the fill mode.
    let progress = match ph {
        EffectPhase::Before => {
            if timing.fill.applies_before() {
                // The iteration-0 start value.
                let d = directed_progress(timing.direction, 0.0, 0);
                Some(apply_easing(d, easing))
            } else {
                None
            }
        }
        EffectPhase::Active => {
            let d = directed_progress(timing.direction, simple.unwrap_or(0.0), ci.unwrap_or(0));
            Some(apply_easing(d, easing))
        }
        EffectPhase::After => {
            if timing.fill.applies_after() {
                let d = directed_progress(timing.direction, simple.unwrap_or(1.0), ci.unwrap_or(0));
                Some(apply_easing(d, easing))
            } else {
                None
            }
        }
    };
    ComputedTiming {
        phase: ph,
        current_iteration: ci,
        simple_progress: simple,
        progress,
        active_duration: active_dur,
        end_time: end,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn t(duration: f64) -> EffectTiming {
        EffectTiming::for_duration(duration)
    }

    // --- parse -------------------------------------------------------

    #[test]
    fn parse_fill_and_direction() {
        assert_eq!(Fill::parse("forwards"), Some(Fill::Forwards));
        assert_eq!(Fill::parse("BOTH"), Some(Fill::Both));
        assert_eq!(Fill::parse("auto"), None);
        assert_eq!(
            PlaybackDirection::parse("alternate"),
            Some(PlaybackDirection::Alternate)
        );
        assert_eq!(
            PlaybackDirection::parse("alternate-reverse"),
            Some(PlaybackDirection::AlternateReverse)
        );
        assert_eq!(PlaybackDirection::parse("sideways"), None);
    }

    #[test]
    fn fill_applies_predicates() {
        assert!(Fill::Backwards.applies_before());
        assert!(Fill::Both.applies_after());
        assert!(!Fill::None.applies_before());
        assert!(!Fill::Forwards.applies_before());
    }

    // --- derived times -----------------------------------------------

    #[test]
    fn active_duration_is_duration_times_iterations() {
        let timing = EffectTiming {
            iterations: 3.0,
            ..t(100.0)
        };
        assert_eq!(active_duration(&timing), 300.0);
    }

    #[test]
    fn active_duration_infinity_for_infinite_iterations() {
        let timing = EffectTiming {
            iterations: f64::INFINITY,
            ..t(100.0)
        };
        assert!(active_duration(&timing).is_infinite());
    }

    #[test]
    fn end_time_is_max_of_delay_plus_active_plus_end_delay() {
        let timing = EffectTiming {
            iterations: 2.0,
            delay: 50.0,
            end_delay: 30.0,
            ..t(100.0)
        };
        // max(50 + 200 + 30, 50) = 280.
        assert_eq!(end_time(&timing), 280.0);
    }

    // --- phase -------------------------------------------------------

    #[test]
    fn phase_before_active_after() {
        let timing = EffectTiming {
            delay: 50.0,
            ..t(100.0)
        };
        assert_eq!(phase(&timing, 0.0), EffectPhase::Before);
        assert_eq!(phase(&timing, 49.0), EffectPhase::Before);
        assert_eq!(phase(&timing, 50.0), EffectPhase::Active);
        assert_eq!(phase(&timing, 100.0), EffectPhase::Active);
        assert_eq!(phase(&timing, 150.0), EffectPhase::After);
        // Active duration = 100; after begins at delay + 100 = 150.
    }

    #[test]
    fn phase_never_after_for_infinite_iterations() {
        let timing = EffectTiming {
            iterations: f64::INFINITY,
            ..t(100.0)
        };
        assert_eq!(phase(&timing, 1_000_000.0), EffectPhase::Active);
    }

    #[test]
    fn phase_jumps_before_to_after_for_zero_duration() {
        let timing = EffectTiming {
            delay: 50.0,
            ..t(0.0)
        };
        assert_eq!(phase(&timing, 49.0), EffectPhase::Before);
        assert_eq!(
            phase(&timing, 50.0),
            EffectPhase::After,
            "zero-duration ⇒ no active"
        );
    }

    // --- simple progress + current iteration -------------------------

    #[test]
    fn active_simple_progress_halfway() {
        let timing = t(100.0);
        // local 50, duration 100 ⇒ overall 0.5 ⇒ simple 0.5.
        assert_eq!(simple_iteration_progress(&timing, 50.0), Some(0.5));
        assert_eq!(current_iteration(&timing, 50.0), Some(0));
    }

    #[test]
    fn active_simple_progress_wraps_across_iterations() {
        let timing = EffectTiming {
            iterations: 3.0,
            ..t(100.0)
        };
        // local 250 ⇒ overall 2.5 ⇒ simple 0.5, iteration 2.
        assert_eq!(simple_iteration_progress(&timing, 250.0), Some(0.5));
        assert_eq!(current_iteration(&timing, 250.0), Some(2));
    }

    #[test]
    fn before_phase_has_no_progress() {
        let timing = EffectTiming {
            delay: 50.0,
            ..t(100.0)
        };
        assert_eq!(simple_iteration_progress(&timing, 25.0), None);
        assert_eq!(current_iteration(&timing, 25.0), None);
    }

    #[test]
    fn after_phase_integer_iterations_reports_progress_one() {
        let timing = EffectTiming {
            iterations: 2.0,
            ..t(100.0)
        };
        // local 250 (after active=200, delay 0): iterations integral ⇒ simple 1.
        assert_eq!(simple_iteration_progress(&timing, 250.0), Some(1.0));
        // current iteration = ceil(2) - 1 = 1 (the second iteration, 0-indexed).
        assert_eq!(current_iteration(&timing, 250.0), Some(1));
    }

    #[test]
    fn after_phase_fractional_iterations_reports_remainder() {
        let timing = EffectTiming {
            iterations: 2.5,
            ..t(100.0)
        };
        // iterations fract = 0.5 ⇒ simple 0.5.
        assert_eq!(simple_iteration_progress(&timing, 1_000.0), Some(0.5));
    }

    #[test]
    fn after_phase_zero_iterations_reports_iteration_zero_progress_one() {
        let timing = EffectTiming {
            iterations: 0.0,
            ..t(100.0)
        };
        assert_eq!(current_iteration(&timing, 0.0), Some(0));
        assert_eq!(simple_iteration_progress(&timing, 0.0), Some(1.0));
    }

    // --- directed progress -------------------------------------------

    #[test]
    fn directed_normal_is_simple() {
        assert_eq!(directed_progress(PlaybackDirection::Normal, 0.3, 0), 0.3);
    }

    #[test]
    fn directed_reverse_inverts() {
        assert_eq!(directed_progress(PlaybackDirection::Reverse, 0.3, 0), 0.7);
    }

    #[test]
    fn directed_alternate_flips_on_odd_iterations() {
        assert_eq!(directed_progress(PlaybackDirection::Alternate, 0.3, 0), 0.3);
        assert_eq!(directed_progress(PlaybackDirection::Alternate, 0.3, 1), 0.7);
        assert_eq!(directed_progress(PlaybackDirection::Alternate, 0.3, 2), 0.3);
    }

    #[test]
    fn directed_alternate_reverse_starts_inverted() {
        assert_eq!(
            directed_progress(PlaybackDirection::AlternateReverse, 0.3, 0),
            0.7
        );
        assert_eq!(
            directed_progress(PlaybackDirection::AlternateReverse, 0.3, 1),
            0.3
        );
    }

    // --- apply_easing ------------------------------------------------

    #[test]
    fn apply_easing_none_is_identity() {
        assert_eq!(apply_easing(0.4, None), 0.4);
    }

    #[test]
    fn apply_easing_linear_is_identity() {
        let e = Easing::parse("linear").unwrap();
        assert!((apply_easing(0.4, Some(&e)) - 0.4).abs() < 1e-9);
    }

    #[test]
    fn apply_easing_ease_in_scales() {
        let e = Easing::parse("ease-in").unwrap();
        let out = apply_easing(0.5, Some(&e));
        // ease-in at 0.5 is < 0.5 (accelerating).
        assert!(out < 0.5, "ease-in(0.5) = {out} should be < 0.5");
        assert!(out > 0.0);
    }

    // --- compute_timing (the pipeline) -------------------------------

    #[test]
    fn compute_timing_active_with_linear_easing() {
        let timing = t(100.0);
        let ct = compute_timing(&timing, 50.0, None);
        assert_eq!(ct.phase, EffectPhase::Active);
        assert_eq!(ct.current_iteration, Some(0));
        assert_eq!(ct.simple_progress, Some(0.5));
        assert_eq!(ct.progress, Some(0.5));
        assert_eq!(ct.active_duration, 100.0);
    }

    #[test]
    fn compute_timing_before_with_fill_backwards_uses_start() {
        let timing = EffectTiming {
            delay: 50.0,
            fill: Fill::Backwards,
            ..t(100.0)
        };
        let ct = compute_timing(&timing, 25.0, None);
        assert_eq!(ct.phase, EffectPhase::Before);
        assert_eq!(
            ct.progress,
            Some(0.0),
            "backwards fill applies the iteration-0 start"
        );
    }

    #[test]
    fn compute_timing_before_without_fill_has_no_effect() {
        let timing = EffectTiming {
            delay: 50.0,
            ..t(100.0)
        };
        let ct = compute_timing(&timing, 25.0, None);
        assert_eq!(ct.phase, EffectPhase::Before);
        assert_eq!(ct.progress, None);
    }

    #[test]
    fn compute_timing_after_with_fill_forwards_uses_end() {
        let timing = EffectTiming {
            iterations: 2.0,
            fill: Fill::Forwards,
            ..t(100.0)
        };
        let ct = compute_timing(&timing, 250.0, None);
        assert_eq!(ct.phase, EffectPhase::After);
        // simple progress 1.0, normal direction ⇒ progress 1.0.
        assert_eq!(ct.progress, Some(1.0));
    }

    #[test]
    fn compute_timing_after_without_fill_has_no_effect() {
        let timing = EffectTiming {
            iterations: 2.0,
            ..t(100.0)
        };
        let ct = compute_timing(&timing, 250.0, None);
        assert_eq!(ct.phase, EffectPhase::After);
        assert_eq!(ct.progress, None);
    }

    #[test]
    fn compute_timing_alternate_direction_in_after_uses_last_iteration() {
        let timing = EffectTiming {
            iterations: 2.0,
            direction: PlaybackDirection::Alternate,
            fill: Fill::Forwards,
            ..t(100.0)
        };
        let ct = compute_timing(&timing, 250.0, None);
        // Last iteration index 1 (odd) ⇒ alternate ⇒ 1 - simple(1.0) = 0.0.
        assert_eq!(ct.progress, Some(0.0));
    }

    #[test]
    fn compute_timing_easing_applied_to_directed_progress() {
        let timing = t(100.0);
        let e = Easing::parse("ease-in").unwrap();
        let ct = compute_timing(&timing, 50.0, Some(&e));
        // directed 0.5, ease-in(0.5) < 0.5.
        assert!(ct.progress.unwrap() < 0.5);
        assert!(ct.progress.unwrap() > 0.0);
    }

    #[test]
    fn compute_timing_iteration_start_offsets_first_iteration() {
        let timing = EffectTiming {
            iteration_start: 0.25,
            ..t(100.0)
        };
        // local 0 ⇒ overall = 0 + 0.25 = 0.25 ⇒ simple 0.25.
        let ct = compute_timing(&timing, 0.0, None);
        assert_eq!(ct.simple_progress, Some(0.25));
        assert_eq!(ct.current_iteration, Some(0));
    }
}
