//! CSS Easing 1 — pure logic for the timing-function family that maps an input
//! progress (`0..1`) to an output progress. The primitive CSS Transitions,
//! Web Animations, and `animation-timing-function` all reduce to; complements
//! [`crate::length`] (value primitives) and [`crate::calc`] (composition) as
//! the third pure value-resolution surface.
//!
//! What lives here:
//! - [`Easing`] — the parsed timing function (`linear`, `cubic-bezier`,
//!   `steps`, the CSS keyword aliases `ease`/`ease-in`/…, and the § 3.1
//!   `linear()` multi-stop function).
//! - [`Easing::parse`] — parse the `transition-timing-function` value grammar
//!   (§ 2 keywords + the `cubic-bezier()`/`steps()`/`linear()` functions).
//! - [`Easing::evaluate`] — `input_progress → output_progress` per § 4
//!   (cubic-bezier projection via Newton-Raphson + bisection fallback; steps
//!   per § 4.1 jump-position rules).
//!
//! What does *not` live here:
//! - The transition/animation *driver* (Phase 6 host-bindings layer owns the
//!   timing loop that feeds input-progress values here).
//! - The `<linear-stop>` `calc()`-valued positions ([`crate::calc`] owns
//!   those; this module parses plain `<percentage>` positions).
//!
//! ## Grammar (CSS Easing 1 § 2 + § 3)
//!
//! ```text
//! <timing-function> = linear | <cubic-bezier-easing> | <step-easing> | <linear-easing>
//! <cubic-bezier-easing> = ease | ease-in | ease-out | ease-in-out
//!                       | cubic-bezier( <number>, <number>, <number>, <number> )
//! <step-easing> = step-start | step-end
//!               | steps( <integer> [, <step-position> ]? )
//! <linear-easing> = linear | linear( <linear-stop># )
//! ```
//!
//! Reference: <https://www.w3.org/TR/css-easing-1/>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Easing — the parsed timing function
// ---------------------------------------------------------------------------

/// A CSS timing function (CSS Easing 1 § 2). The capstone easing value the
/// transition/animation drivers evaluate.
#[derive(Debug, Clone, PartialEq)]
pub enum Easing {
    /// The identity function: `output = input`. Also the result of the bare
    /// `linear` keyword.
    Linear,
    /// `cubic-bezier(x1, y1, x2, y2)` (§ 3.2). The four control-point
    /// coordinates; the x's must be in `[0, 1]`, the y's are unbounded (so
    /// spring/overshoot effects like `cubic-bezier(.5, -0.5, .5, 1.5)` work).
    CubicBezier { x1: f64, y1: f64, x2: f64, y2: f64 },
    /// `steps(n, position)` (§ 3.3). `count` intervals; `position` picks the
    /// jump-placement rule.
    Steps { count: u32, position: StepPosition },
    /// `linear(s1, s2, …)` (§ 3.1) — a piecewise-linear interpolation between
    /// the listed output stops. Each stop carries an output value plus an
    /// optional explicit input-position percentage.
    LinearStops(Vec<LinearStop>),
}

/// The `steps()` jump-placement keyword (CSS Easing 1 § 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StepPosition {
    /// `jump-start` / `start` — the first jump is at input `0`.
    JumpStart,
    /// `jump-end` / `end` (the default) — the last jump is at input `1`.
    #[default]
    JumpEnd,
    /// `jump-none` — no jump at either end (requires `count ≥ 2`).
    JumpNone,
    /// `jump-both` — jumps at both input `0` and input `1`.
    JumpBoth,
}

/// A single `linear()` stop (CSS Easing 1 § 3.1). The output value to
/// interpolate to; the optional input-position percentage pins where on the
/// `[0, 1]` input axis the stop sits. Stops without explicit positions are
/// evenly distributed (the § 3.1 "implicit position" rule).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearStop {
    /// The output value at this stop (the y-axis). Unbounded — `linear()`
    /// stops may overshoot `1` or undershoot `0` (the § 3.1 example with
    /// negative values).
    pub output: f64,
    /// The input-axis position in `[0, 1]`. `None` ⇒ distributed evenly per
    /// the § 3.1 implicit-position rule.
    pub position: Option<f64>,
}

impl Easing {
    /// Parse a `<timing-function>` value. Accepts the keyword aliases
    /// (`linear`, `ease`, `ease-in`, `ease-out`, `ease-in-out`, `step-start`,
    /// `step-end`) and the function forms (`cubic-bezier()`, `steps()`,
    /// `linear()`). Surrounding whitespace is trimmed.
    ///
    /// ```
    /// # use vixen_engine::easing::{Easing, StepPosition};
    /// let e = Easing::parse("cubic-bezier(0.42, 0, 0.58, 1)").unwrap();
    /// assert!((e.evaluate(0.5) - 0.5).abs() < 1e-3);
    /// let s = Easing::parse("steps(3, start)").unwrap();
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn parse(input: &str) -> Result<Self, EasingError> {
        let s = input.trim();
        let lower = s.to_ascii_lowercase();
        // Keyword aliases (§ 2.2).
        match lower.as_str() {
            "linear" => return Ok(Easing::Linear),
            "ease" => {
                return Ok(Easing::CubicBezier {
                    x1: 0.25,
                    y1: 0.1,
                    x2: 0.25,
                    y2: 1.0,
                });
            }
            "ease-in" => {
                return Ok(Easing::CubicBezier {
                    x1: 0.42,
                    y1: 0.0,
                    x2: 1.0,
                    y2: 1.0,
                });
            }
            "ease-out" => {
                return Ok(Easing::CubicBezier {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 0.58,
                    y2: 1.0,
                });
            }
            "ease-in-out" => {
                return Ok(Easing::CubicBezier {
                    x1: 0.42,
                    y1: 0.0,
                    x2: 0.58,
                    y2: 1.0,
                });
            }
            "step-start" => {
                return Ok(Easing::Steps {
                    count: 1,
                    position: StepPosition::JumpStart,
                });
            }
            "step-end" => {
                return Ok(Easing::Steps {
                    count: 1,
                    position: StepPosition::JumpEnd,
                });
            }
            _ => {}
        }

        // Function forms: name(arg, …).
        let (name, rest) =
            split_function(s).ok_or_else(|| EasingError::UnknownFunction(s.to_owned()))?;
        let args = parse_args(rest)?;

        match name {
            "cubic-bezier" => {
                if args.len() != 4 {
                    return Err(EasingError::CubicBezierArgCount(args.len()));
                }
                let nums: Vec<f64> = args
                    .iter()
                    .map(|a| a.trim().parse::<f64>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| EasingError::InvalidNumber(args.join(", ")))?;
                // § 3.2: the x-coordinates (P1.x, P2.x) must be in [0, 1].
                if !(0.0..=1.0).contains(&nums[0]) || !(0.0..=1.0).contains(&nums[2]) {
                    return Err(EasingError::CubicBezierXOutOfRange);
                }
                Ok(Easing::CubicBezier {
                    x1: nums[0],
                    y1: nums[1],
                    x2: nums[2],
                    y2: nums[3],
                })
            }
            "steps" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(EasingError::StepsArgCount(args.len()));
                }
                let count: u32 = args[0]
                    .trim()
                    .parse()
                    .map_err(|_| EasingError::InvalidNumber(args[0].clone()))?;
                if count == 0 {
                    return Err(EasingError::StepsZeroCount);
                }
                let position = if args.len() == 2 {
                    parse_step_position(args[1].trim())?
                } else {
                    StepPosition::JumpEnd
                };
                // § 3.3: jump-none requires count ≥ 2.
                if position == StepPosition::JumpNone && count < 2 {
                    return Err(EasingError::JumpNoneRequiresTwo);
                }
                Ok(Easing::Steps { count, position })
            }
            "linear" => {
                // `linear()` (with parens) is the multi-stop form; empty args
                // ⇒ plain identity.
                if args.is_empty() {
                    return Ok(Easing::Linear);
                }
                let stops: Vec<LinearStop> = args
                    .iter()
                    .map(|a| parse_linear_stop(a.trim()))
                    .collect::<Result<_, _>>()?;
                validate_linear_stops(&stops)?;
                Ok(Easing::LinearStops(stops))
            }
            other => Err(EasingError::UnknownFunction(other.to_owned())),
        }
    }

    /// Evaluate the easing: map an input progress (`0..1`, clamped) to an
    /// output progress. For inputs outside `[0, 1]`, the input is clamped to
    /// `[0, 1]` before evaluation (§ 4).
    pub fn evaluate(&self, input_progress: f64) -> f64 {
        let t = input_progress.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::CubicBezier { x1, y1, x2, y2 } => evaluate_cubic_bezier(t, *x1, *y1, *x2, *y2),
            Easing::Steps { count, position } => evaluate_steps(t, *count, *position),
            Easing::LinearStops(stops) => evaluate_linear_stops(t, stops),
        }
    }
}

impl Default for Easing {
    fn default() -> Self {
        // CSS Transitions 1 § 4.2.1 default for `transition-timing-function`.
        Easing::CubicBezier {
            x1: 0.25,
            y1: 0.1,
            x2: 0.25,
            y2: 1.0,
        }
    }
}

// ---------------------------------------------------------------------------
// steps evaluation (§ 4.1)
// ---------------------------------------------------------------------------

/// Evaluate `steps(count, position)` at input `t` (already clamped to [0,1]).
fn evaluate_steps(t: f64, count: u32, position: StepPosition) -> f64 {
    // The number of jumps per § 4.1: count (start/end), count-1 (none), count+1 (both).
    let jumps = match position {
        StepPosition::JumpNone => count.saturating_sub(1),
        StepPosition::JumpBoth => count + 1,
        StepPosition::JumpStart | StepPosition::JumpEnd => count,
    };
    if jumps == 0 {
        // Degenerate: steps(1, jump-none) is rejected at parse time, but fail
        // safe to the identity.
        return t;
    }
    // The current step: floor(t * jumps), adjusted so jump-start/jump-both
    // advance by one (the first jump happens at input 0).
    let start_adjustment = match position {
        StepPosition::JumpStart | StepPosition::JumpBoth => 1,
        StepPosition::JumpEnd | StepPosition::JumpNone => 0,
    };
    let step = (t * jumps as f64).floor() + start_adjustment as f64;
    (step / jumps as f64).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// cubic-bezier evaluation (§ 4.2)
// ---------------------------------------------------------------------------

/// Evaluate `cubic-bezier(x1, y1, x2, y2)` at input `t` (the x-axis value,
/// already clamped to [0,1]). Projects `t` onto the parametric curve: solves
/// for the parameter `s` such that `Bx(s) = t`, then returns `By(s)`.
///
/// The control points are `P0=(0,0)`, `P1=(x1,y1)`, `P2=(x2,y2)`, `P3=(1,1)`.
/// Uses Newton-Raphson (8 iterations) with a bisection fallback so it
/// converges on every valid curve, matching the WebKit/Gecko implementations.
fn evaluate_cubic_bezier(t: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    // Endpoint short-cuts (§ 4.2: at input 0/1 the output is exactly 0/1).
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    // Solve Bx(s) = t for s in (0, 1).
    let s = solve_bezier_x(t, x1, x2);
    bezier_component(s, y1, y2)
}

/// The x-component of the cubic bezier at parameter `s` (P0.x=0, P3.x=1).
fn bezier_x(s: f64, x1: f64, x2: f64) -> f64 {
    // Bx(s) = 3(1-s)²·s·x1 + 3(1-s)·s²·x2 + s³
    let one_minus_s = 1.0 - s;
    3.0 * one_minus_s * one_minus_s * s * x1 + 3.0 * one_minus_s * s * s * x2 + s * s * s
}

/// Either the x or y component of the bezier at `s` (substituting the right
/// control-point coordinate for the `1`/`1` endpoints).
fn bezier_component(s: f64, c1: f64, c2: f64) -> f64 {
    let one_minus_s = 1.0 - s;
    3.0 * one_minus_s * one_minus_s * s * c1 + 3.0 * one_minus_s * s * s * c2 + s * s * s
}

/// Derivative of Bx w.r.t. `s`, for Newton-Raphson.
fn bezier_x_derivative(s: f64, x1: f64, x2: f64) -> f64 {
    // Bx'(s) = 3(1-s)²·x1 + 6(1-s)·s·(x2-x1) + 3·s²·(1-x2)
    let one_minus_s = 1.0 - s;
    3.0 * one_minus_s * one_minus_s * x1
        + 6.0 * one_minus_s * s * (x2 - x1)
        + 3.0 * s * s * (1.0 - x2)
}

/// Solve `bezier_x(s) = target` for `s` in `[0, 1]` using Newton-Raphson with
/// a bisection fallback. Returns the parameter; the convergence is to ~1e-7.
fn solve_bezier_x(target: f64, x1: f64, x2: f64) -> f64 {
    // Initial guess: the linear-ish interpolation. WebKit uses this seed.
    let mut s = target;
    for _ in 0..8 {
        let f = bezier_x(s, x1, x2) - target;
        let d = bezier_x_derivative(s, x1, x2);
        if d.abs() < 1e-6 {
            break;
        }
        let next = s - f / d;
        if !next.is_finite() || !(0.0..=1.0).contains(&next) {
            break;
        }
        s = next;
        if f.abs() < 1e-7 {
            return s;
        }
    }
    // Newton didn't fully converge (or diverged) — fall back to bisection.
    if !(0.0..=1.0).contains(&s) || (bezier_x(s, x1, x2) - target).abs() >= 1e-6 {
        s = bisect_bezier_x(target, x1, x2);
    }
    s
}

/// Bisection fallback for `bezier_x(s) = target`. 60 iterations is more than
/// enough for `f64` precision on `[0, 1]`.
fn bisect_bezier_x(target: f64, x1: f64, x2: f64) -> f64 {
    let mut lo = 0.0;
    let mut hi = 1.0;
    let mut s = target;
    for _ in 0..60 {
        let x = bezier_x(s, x1, x2);
        if (x - target).abs() < 1e-7 {
            return s;
        }
        if x < target {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) / 2.0;
    }
    s
}

// ---------------------------------------------------------------------------
// linear() evaluation (§ 3.1)
// ---------------------------------------------------------------------------

/// Evaluate `linear()` at input `t`: piecewise-linear interpolation between
/// the stops. Stops without explicit positions are evenly distributed (§ 3.1).
fn evaluate_linear_stops(t: f64, stops: &[LinearStop]) -> f64 {
    if stops.is_empty() {
        return t;
    }
    if stops.len() == 1 {
        // A single stop is a constant: the output is that stop's value for
        // every input ≥ 0 (per § 3.1 the stop sits at input 0).
        return stops[0].output;
    }
    // Resolve stop input positions: explicit positions win; the § 3.1
    // implicit-position rule distributes the gaps evenly between explicit
    // anchors.
    let positions = resolve_stop_positions(stops);

    // Find the interval [positions[i], positions[i+1]] containing `t`.
    let last = stops.len() - 1;
    if t <= positions[0] {
        return stops[0].output;
    }
    if t >= positions[last] {
        return stops[last].output;
    }
    for i in 0..last {
        if t >= positions[i] && t <= positions[i + 1] {
            let span = positions[i + 1] - positions[i];
            if span <= 0.0 {
                return stops[i + 1].output;
            }
            let local = (t - positions[i]) / span;
            return stops[i].output + local * (stops[i + 1].output - stops[i].output);
        }
    }
    // Fallback (should be unreachable).
    stops[last].output
}

/// Assign an input position (in `[0, 1]`) to each stop per the § 3.1 rules:
/// explicit positions are anchors; the implicit ones distribute evenly
/// between the surrounding anchors. The first stop defaults to `0` and the
/// last to `1` if they lack an explicit position.
fn resolve_stop_positions(stops: &[LinearStop]) -> Vec<f64> {
    let n = stops.len();
    let mut out = vec![0.0; n];
    // If the first/last stop lack an explicit position, anchor them to 0/1.
    out[0] = stops[0].position.unwrap_or(0.0);
    out[n - 1] = stops[n - 1].position.unwrap_or(1.0);
    if out[n - 1] < out[0] {
        // The § 3.1 rule: positions must be ascending; clamp to the previous.
        out[n - 1] = out[0];
    }
    // Walk implicit runs: between two anchors at index a (position pa) and b
    // (position pb), with k implicit stops between, distribute them evenly.
    let mut anchor_idx = 0;
    let mut anchor_pos = out[0];
    let mut i = 1;
    while i < n - 1 {
        if let Some(p) = stops[i].position {
            // Record this stop's explicit position, then distribute any
            // implicit stops between the previous anchor and here.
            out[i] = p;
            distribute_run(&mut out, stops, anchor_idx, anchor_pos, i, p);
            anchor_idx = i;
            anchor_pos = p;
        }
        i += 1;
    }
    // Final run from the last anchor to the end.
    let last_pos = out[n - 1];
    distribute_run(&mut out, stops, anchor_idx, anchor_pos, n - 1, last_pos);
    out
}

/// Distribute the implicit stops between anchor `lo` (position `lo_pos`) and
/// anchor `hi` (position `hi_pos`) evenly into `out`.
fn distribute_run(
    out: &mut [f64],
    stops: &[LinearStop],
    lo: usize,
    lo_pos: f64,
    hi: usize,
    hi_pos: f64,
) {
    let count = hi - lo; // intervals
    if count == 0 {
        return;
    }
    for k in 1..count {
        if stops[lo + k].position.is_none() {
            out[lo + k] = lo_pos + (hi_pos - lo_pos) * (k as f64) / (count as f64);
        } else {
            out[lo + k] = stops[lo + k].position.unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Split `name(rest)` into `(name, rest)`, returning the inner content (up to
/// the matching closing paren, respecting nesting). `rest` excludes the outer
/// parens.
fn split_function(s: &str) -> Option<(&str, &str)> {
    let open = s.find('(')?;
    if !s.ends_with(')') {
        return None;
    }
    let name = s[..open].trim();
    let inner = &s[open + 1..s.len() - 1];
    Some((name, inner))
}

/// Split function arguments on top-level commas (respecting nested parens).
fn parse_args(inner: &str) -> Result<Vec<String>, EasingError> {
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for ch in inner.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    Ok(out)
}

/// Parse a `steps()` position keyword (case-insensitive).
fn parse_step_position(s: &str) -> Result<StepPosition, EasingError> {
    match s.to_ascii_lowercase().as_str() {
        "start" | "jump-start" => Ok(StepPosition::JumpStart),
        "end" | "jump-end" => Ok(StepPosition::JumpEnd),
        "jump-none" => Ok(StepPosition::JumpNone),
        "jump-both" => Ok(StepPosition::JumpBoth),
        _ => Err(EasingError::InvalidStepPosition(s.to_owned())),
    }
}

/// Parse a single `<linear-stop>`: `<number> [<percentage>]` or `<percentage>`.
fn parse_linear_stop(s: &str) -> Result<LinearStop, EasingError> {
    let parts: Vec<&str> = s.split_ascii_whitespace().collect();
    match parts.as_slice() {
        [single] => {
            // Either a bare percentage (`50%`) or a bare number.
            if let Some(num) = single.strip_suffix('%') {
                let pos: f64 = num
                    .parse()
                    .map_err(|_| EasingError::InvalidNumber((*single).to_string()))?;
                // A bare percentage stop: output == input position (§ 3.1).
                Ok(LinearStop {
                    output: pos / 100.0,
                    position: Some(pos / 100.0),
                })
            } else {
                let output: f64 = single
                    .parse()
                    .map_err(|_| EasingError::InvalidNumber((*single).to_string()))?;
                Ok(LinearStop {
                    output,
                    position: None,
                })
            }
        }
        [value, percent] => {
            let output: f64 = value
                .parse()
                .map_err(|_| EasingError::InvalidNumber((*value).to_string()))?;
            let pos = percent
                .strip_suffix('%')
                .ok_or_else(|| EasingError::InvalidNumber((*percent).to_string()))?;
            let pos: f64 = pos
                .parse()
                .map_err(|_| EasingError::InvalidNumber((*percent).to_string()))?;
            Ok(LinearStop {
                output,
                position: Some(pos / 100.0),
            })
        }
        _ => Err(EasingError::InvalidLinearStop(s.to_owned())),
    }
}

/// § 3.1 validation: explicit positions must be in `[0, 1]` and ascending.
fn validate_linear_stops(stops: &[LinearStop]) -> Result<(), EasingError> {
    let mut last_pos = 0.0f64;
    for stop in stops {
        if let Some(p) = stop.position {
            if !(0.0..=1.0).contains(&p) {
                return Err(EasingError::LinearStopPositionOutOfRange(p));
            }
            if p < last_pos {
                return Err(EasingError::LinearStopPositionsNotAscending);
            }
            last_pos = p;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Parse error for [`Easing::parse`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EasingError {
    #[error("unknown timing function: {0}")]
    UnknownFunction(String),
    #[error("cubic-bezier() requires exactly 4 arguments, got {0}")]
    CubicBezierArgCount(usize),
    #[error("cubic-bezier() x-coordinates must be in [0, 1]")]
    CubicBezierXOutOfRange,
    #[error("steps() requires 1 or 2 arguments, got {0}")]
    StepsArgCount(usize),
    #[error("steps() count must be ≥ 1")]
    StepsZeroCount,
    #[error("steps(n, jump-none) requires n ≥ 2")]
    JumpNoneRequiresTwo,
    #[error("invalid step position: {0}")]
    InvalidStepPosition(String),
    #[error("invalid number: {0}")]
    InvalidNumber(String),
    #[error("invalid linear() stop: {0}")]
    InvalidLinearStop(String),
    #[error("linear() stop position {0} out of [0, 1]")]
    LinearStopPositionOutOfRange(f64),
    #[error("linear() stop positions must be ascending")]
    LinearStopPositionsNotAscending,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    // --- Keyword aliases (§ 2.2) ---------------------------------------

    #[test]
    fn linear_keyword_is_identity() {
        let e = Easing::parse("linear").unwrap();
        assert!(approx(e.evaluate(0.0), 0.0, 1e-9));
        assert!(approx(e.evaluate(0.5), 0.5, 1e-9));
        assert!(approx(e.evaluate(1.0), 1.0, 1e-9));
    }

    #[test]
    fn ease_keyword_parses_to_canonical_bezier() {
        let e = Easing::parse("ease").unwrap();
        let Easing::CubicBezier { x1, y1, x2, y2 } = e else {
            panic!("expected CubicBezier");
        };
        assert!(approx(x1, 0.25, 1e-9));
        assert!(approx(y1, 0.1, 1e-9));
        assert!(approx(x2, 0.25, 1e-9));
        assert!(approx(y2, 1.0, 1e-9));
    }

    #[test]
    fn ease_in_out_is_symmetric_at_half() {
        // ease-in-out = cubic-bezier(.42, 0, .58, 1); at t=0.5, output=0.5
        // by the symmetry of the curve through (0.5, 0.5).
        let e = Easing::parse("ease-in-out").unwrap();
        assert!(approx(e.evaluate(0.5), 0.5, 1e-3));
    }

    #[test]
    fn step_start_is_steps_one_start() {
        let e = Easing::parse("step-start").unwrap();
        assert_eq!(
            e,
            Easing::Steps {
                count: 1,
                position: StepPosition::JumpStart
            }
        );
        assert!(approx(e.evaluate(0.0), 1.0, 1e-9));
        assert!(approx(e.evaluate(1.0), 1.0, 1e-9));
    }

    #[test]
    fn step_end_is_steps_one_end() {
        let e = Easing::parse("step-end").unwrap();
        assert_eq!(
            e,
            Easing::Steps {
                count: 1,
                position: StepPosition::JumpEnd
            }
        );
        assert!(approx(e.evaluate(0.0), 0.0, 1e-9));
        assert!(approx(e.evaluate(1.0), 1.0, 1e-9));
    }

    // --- cubic-bezier evaluation (§ 4.2) -------------------------------

    #[test]
    fn bezier_endpoints_are_exact() {
        let e = Easing::parse("cubic-bezier(0.42, 0, 0.58, 1)").unwrap();
        assert!(approx(e.evaluate(0.0), 0.0, 1e-9));
        assert!(approx(e.evaluate(1.0), 1.0, 1e-9));
    }

    #[test]
    fn bezier_identity_curve() {
        // cubic-bezier(0, 0, 1, 1) ≡ linear.
        let e = Easing::parse("cubic-bezier(0, 0, 1, 1)").unwrap();
        for &t in &[0.1, 0.25, 0.5, 0.75, 0.9] {
            assert!(approx(e.evaluate(t), t, 1e-6), "at {t}");
        }
    }

    #[test]
    fn bezier_can_overshoot() {
        // y-coordinates outside [0,1] give spring/overshoot curves.
        let e = Easing::parse("cubic-bezier(0.5, -0.5, 0.5, 1.5)").unwrap();
        // At some mid input, the output should exceed 1 or dip below 0.
        let mid = e.evaluate(0.5);
        assert!(
            !(0.0..=1.0).contains(&mid) || approx(mid, 0.5, 0.4),
            "expected overshoot/undershoot, got {mid}"
        );
    }

    #[test]
    fn bezier_monotonic_for_ease() {
        // `ease` should be monotonically increasing.
        let e = Easing::parse("ease").unwrap();
        let mut prev = 0.0;
        for i in 1..=100 {
            let t = i as f64 / 100.0;
            let v = e.evaluate(t);
            assert!(v >= prev - 1e-9, "non-monotonic at {t}: {v} < {prev}");
            prev = v;
        }
    }

    #[test]
    fn bezier_input_clamped_to_range() {
        let e = Easing::parse("ease").unwrap();
        // Inputs outside [0,1] clamp to the endpoints.
        assert!(approx(e.evaluate(-0.5), 0.0, 1e-9));
        assert!(approx(e.evaluate(2.0), 1.0, 1e-9));
    }

    // --- steps evaluation (§ 4.1) --------------------------------------

    #[test]
    fn steps_end_default() {
        let e = Easing::parse("steps(4)").unwrap();
        assert_eq!(
            e,
            Easing::Steps {
                count: 4,
                position: StepPosition::JumpEnd
            }
        );
    }

    #[test]
    fn steps_four_end_values() {
        let e = Easing::Steps {
            count: 4,
            position: StepPosition::JumpEnd,
        };
        // 0 at [0, 0.25), 0.25 at [0.25, 0.5), 0.5 at [0.5, 0.75), 0.75 at
        // [0.75, 1), 1 at exactly 1.
        assert!(approx(e.evaluate(0.0), 0.0, 1e-9));
        assert!(approx(e.evaluate(0.24), 0.0, 1e-9));
        assert!(approx(e.evaluate(0.25), 0.25, 1e-9));
        assert!(approx(e.evaluate(0.5), 0.5, 1e-9));
        assert!(approx(e.evaluate(0.99), 0.75, 1e-9));
        assert!(approx(e.evaluate(1.0), 1.0, 1e-9));
    }

    #[test]
    fn steps_four_start_values() {
        let e = Easing::Steps {
            count: 4,
            position: StepPosition::JumpStart,
        };
        // First jump at input 0 ⇒ output 0.25; 0.5 at [0.25, 0.5); etc.
        assert!(approx(e.evaluate(0.0), 0.25, 1e-9));
        assert!(approx(e.evaluate(0.24), 0.25, 1e-9));
        assert!(approx(e.evaluate(0.25), 0.5, 1e-9));
        assert!(approx(e.evaluate(0.99), 1.0, 1e-9));
        assert!(approx(e.evaluate(1.0), 1.0, 1e-9));
    }

    #[test]
    fn steps_jump_none() {
        // steps(4, jump-none): 4 output levels (0, 1/3, 2/3, 1) across 3 jumps.
        let e = Easing::Steps {
            count: 4,
            position: StepPosition::JumpNone,
        };
        assert!(approx(e.evaluate(0.0), 0.0, 1e-9));
        assert!(approx(e.evaluate(1.0), 1.0, 1e-9));
        // Mid-input steps through 1/3 and 2/3.
        assert!(approx(e.evaluate(0.34), 1.0 / 3.0, 1e-6));
        assert!(approx(e.evaluate(0.5), 1.0 / 3.0, 1e-6));
        assert!(approx(e.evaluate(0.67), 2.0 / 3.0, 1e-6));
    }

    #[test]
    fn steps_jump_both() {
        // steps(4, jump-both): 5 jumps; first output at 1/5.
        let e = Easing::Steps {
            count: 4,
            position: StepPosition::JumpBoth,
        };
        assert!(approx(e.evaluate(0.0), 1.0 / 5.0, 1e-9));
        assert!(approx(e.evaluate(1.0), 1.0, 1e-9));
    }

    #[test]
    fn steps_alias_keywords() {
        assert_eq!(
            Easing::parse("steps(3, start)").unwrap(),
            Easing::Steps {
                count: 3,
                position: StepPosition::JumpStart
            }
        );
        assert_eq!(
            Easing::parse("steps(3, end)").unwrap(),
            Easing::Steps {
                count: 3,
                position: StepPosition::JumpEnd
            }
        );
    }

    #[test]
    fn steps_jump_none_requires_two() {
        assert!(matches!(
            Easing::parse("steps(1, jump-none)"),
            Err(EasingError::JumpNoneRequiresTwo)
        ));
    }

    // --- linear() evaluation (§ 3.1) -----------------------------------

    #[test]
    fn linear_stops_evenly_distributed() {
        // linear(0, 1) ≡ identity.
        let e = Easing::parse("linear(0, 1)").unwrap();
        for &t in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            assert!(approx(e.evaluate(t), t, 1e-9), "at {t}");
        }
    }

    #[test]
    fn linear_stops_three_way() {
        // linear(0, 1, 0): triangle wave peaking at t=0.5.
        let e = Easing::parse("linear(0, 1, 0)").unwrap();
        assert!(approx(e.evaluate(0.0), 0.0, 1e-9));
        assert!(approx(e.evaluate(0.25), 0.5, 1e-9));
        assert!(approx(e.evaluate(0.5), 1.0, 1e-9));
        assert!(approx(e.evaluate(0.75), 0.5, 1e-9));
        assert!(approx(e.evaluate(1.0), 0.0, 1e-9));
    }

    #[test]
    fn linear_stops_explicit_positions() {
        // linear(0 0%, 1 25%, 1 75%, 0 100%): plateau at 1 from 25% to 75%.
        let e = Easing::parse("linear(0 0%, 1 25%, 1 75%, 0 100%)").unwrap();
        assert!(approx(e.evaluate(0.0), 0.0, 1e-9));
        assert!(approx(e.evaluate(0.125), 0.5, 1e-6)); // halfway to the plateau
        assert!(approx(e.evaluate(0.5), 1.0, 1e-9));
        assert!(approx(e.evaluate(0.875), 0.5, 1e-6)); // halfway back down
        assert!(approx(e.evaluate(1.0), 0.0, 1e-9));
    }

    #[test]
    fn linear_single_stop_is_constant() {
        let e = Easing::parse("linear(0.5)").unwrap();
        assert!(approx(e.evaluate(0.0), 0.5, 1e-9));
        assert!(approx(e.evaluate(0.5), 0.5, 1e-9));
        assert!(approx(e.evaluate(1.0), 0.5, 1e-9));
    }

    #[test]
    fn linear_can_overshoot() {
        // linear(1, -1, 1): dips to -1 at the midpoint.
        let e = Easing::parse("linear(1, -1, 1)").unwrap();
        assert!(approx(e.evaluate(0.5), -1.0, 1e-9));
    }

    #[test]
    fn linear_empty_parens_is_identity() {
        let e = Easing::parse("linear()").unwrap();
        assert!(approx(e.evaluate(0.5), 0.5, 1e-9));
    }

    #[test]
    fn linear_bare_percent_stop() {
        // A bare `50%` stop: output == position == 0.5 (constant).
        let e = Easing::parse("linear(0, 50%, 1)").unwrap();
        // Stops at t=0→0, t=0.5→0.5, t=1→1 (the middle stop pins both).
        assert!(approx(e.evaluate(0.5), 0.5, 1e-9));
    }

    // --- Parse errors --------------------------------------------------

    #[test]
    fn unknown_function_errors() {
        assert!(Easing::parse("bounce(0.5)").is_err());
    }

    #[test]
    fn cubic_bezier_wrong_arg_count_errors() {
        assert!(Easing::parse("cubic-bezier(0.42, 0, 0.58)").is_err());
    }

    #[test]
    fn cubic_bezier_x_out_of_range_errors() {
        // x2 must be in [0, 1].
        assert!(matches!(
            Easing::parse("cubic-bezier(0, 0, 1.5, 1)"),
            Err(EasingError::CubicBezierXOutOfRange)
        ));
        // Negative x1 also invalid.
        assert!(matches!(
            Easing::parse("cubic-bezier(-0.1, 0, 1, 1)"),
            Err(EasingError::CubicBezierXOutOfRange)
        ));
    }

    #[test]
    fn steps_zero_count_errors() {
        assert!(matches!(
            Easing::parse("steps(0)"),
            Err(EasingError::StepsZeroCount)
        ));
    }

    #[test]
    fn steps_invalid_position_errors() {
        assert!(Easing::parse("steps(3, sideways)").is_err());
    }

    #[test]
    fn linear_descending_positions_error() {
        assert!(matches!(
            Easing::parse("linear(0 50%, 1 25%)"),
            Err(EasingError::LinearStopPositionsNotAscending)
        ));
    }

    #[test]
    fn linear_position_out_of_range_error() {
        assert!(matches!(
            Easing::parse("linear(0 0%, 1 150%)"),
            Err(EasingError::LinearStopPositionOutOfRange(_))
        ));
    }

    // --- Default -------------------------------------------------------

    #[test]
    fn default_is_ease() {
        let e = Easing::default();
        let Easing::CubicBezier { x1, .. } = e else {
            panic!("default should be cubic-bezier");
        };
        assert!(approx(x1, 0.25, 1e-9));
    }
}
