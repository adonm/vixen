//! High Resolution Time § 4 — `DOMHighResTimeStamp` + `performance.now()`
//! (pure logic). The monotonic-clock + time-origin model the
//! `Performance.now()` / `Performance.timeOrigin` / `Performance.timing` host
//! hooks and the animation-frame driver reduce to. Complements
//! [`crate::easing`] (the timing-function family the animation driver feeds
//! these timestamps into).
//!
//! What lives here:
//! - [`TimeOrigin`] — the per-global time origin (ms since Unix epoch) that
//!   `performance.now()` is relative to.
//! - [`MonotonicClock`] — a `now(clock_ms)` that is non-decreasing across
//!   calls (§ 4.4: "timestamps MUST be monotonic") + clamped to `≥ 0`.
//! - [`coarsen`] — § 4.4 the effective-time-value coarsening: floor to `100µs`
//!   (0.1 ms) unless the global is cross-origin isolated (then the finer
//!   resolution is preserved).
//! - [`relative_to_unix`] — convert a `performance.now()` value to an absolute
//!   Unix-epoch ms timestamp (`timeOrigin + now`).
//!
//! What does *not` live here:
//! - The actual wall-clock source (a Phase 6 host-hook detail — the spec lets
//!   UAs pick; Vixen uses `time::Instant` monotonic + a boot/origin anchor).
//! - `performance.memory`, `PerformanceObserver`, the User Timing (`mark`/
//!   `measure`) API (Phase 6 host hook).
//! - The § 4.5 cross-origin-isolated gating (the caller passes the flag; this
//!   module applies the coarsening given it).
//!
//! ## Units
//!
//! [`DOMHighResTimeStamp`] is `f64` milliseconds, matching the WebIDL type.
//! Negative `now()` values are impossible (the origin is the lower bound); the
//! clock clamps to `0.0` if the wall clock ever reports a pre-origin reading
//! (clock skew defence).
//!
//! Reference: <https://w3.org/TR/hr-time-3/>.

#![forbid(unsafe_code)]

/// A `DOMHighResTimeStamp` (WebIDL): `f64` milliseconds. The unit every
/// `Performance` API + the animation-frame driver operates in.
pub type DOMHighResTimeStamp = f64;

/// The § 4.4 coarsening quantum: non-cross-origin-isolated globals floor every
/// reported timestamp to a multiple of this (100 µs = 0.1 ms).
pub const COARSE_QUANTUM_MS: f64 = 0.1;

// ---------------------------------------------------------------------------
// TimeOrigin
// ---------------------------------------------------------------------------

/// The per-global time origin (§ 4.3): a Unix-epoch millisecond timestamp
/// that `performance.now()` is relative to (`now` = `clock − origin`). One
/// origin per global (the document's navigation start).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeOrigin {
    /// Milliseconds since the Unix epoch.
    origin_ms: f64,
}

impl TimeOrigin {
    /// Construct from a Unix-epoch millisecond timestamp (typically the
    /// document's navigation-start / first-paint anchor).
    pub const fn from_unix_ms(origin_ms: f64) -> Self {
        Self { origin_ms }
    }

    /// The Unix-epoch ms of the origin (what `performance.timeOrigin`
    /// returns).
    pub const fn unix_ms(&self) -> f64 {
        self.origin_ms
    }

    /// Convert a `performance.now()` reading (ms since origin) back to an
    /// absolute Unix-epoch ms timestamp (§ 4.3 `timeOrigin + now`). Used by
    /// the `PerformanceTiming` legacy surface + cross-context correlation.
    pub fn relative_to_unix(&self, performance_ms: DOMHighResTimeStamp) -> f64 {
        self.origin_ms + performance_ms
    }

    /// Read a `performance.now()` value for an absolute wall-clock reading
    /// (ms since Unix epoch), without monotonicity tracking. Most callers
    /// should use [`MonotonicClock`] instead (§ 4.4 requires monotonicity).
    pub fn now(&self, clock_unix_ms: f64) -> DOMHighResTimeStamp {
        (clock_unix_ms - self.origin_ms).max(0.0)
    }
}

// ---------------------------------------------------------------------------
// MonotonicClock — § 4.4 monotonicity
// ---------------------------------------------------------------------------

/// A `performance.now()` clock that enforces § 4.4's monotonicity requirement:
/// every [`MonotonicClock::now`] call returns a value `≥` the previous one
/// (clock-skew + NTP-adjustment defence). Also clamps to `≥ 0`.
#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock {
    origin: TimeOrigin,
    last: f64,
}

impl MonotonicClock {
    /// A fresh clock anchored at `origin`. The first `now()` reading is `0.0`
    /// until a wall-clock sample arrives.
    pub fn new(origin: TimeOrigin) -> Self {
        Self { origin, last: 0.0 }
    }

    /// Sample the clock at the given wall-clock Unix-epoch ms and return the
    /// `performance.now()` value (ms since origin), coarsened per
    /// [`coarsen`] only if the caller asks (passing the cross-origin-isolated
    /// flag). Monotonic: never less than the previous return.
    pub fn now(&mut self, clock_unix_ms: f64, cross_origin_isolated: bool) -> DOMHighResTimeStamp {
        let raw = self.origin.now(clock_unix_ms);
        // § 4.4 monotonicity: never go backwards.
        let monotonic = raw.max(self.last);
        self.last = monotonic;
        coarsen(monotonic, cross_origin_isolated)
    }

    /// The most recent raw (un-coarsened) `performance.now()` value reported,
    /// for tests + the `Performance.now()` fast path.
    pub const fn last_raw(&self) -> f64 {
        self.last
    }

    /// The clock's origin.
    pub const fn origin(&self) -> TimeOrigin {
        self.origin
    }
}

// ---------------------------------------------------------------------------
// Coarsening (§ 4.4)
// ---------------------------------------------------------------------------

/// § 4.4 effective-time-value coarsening: floor to the [`COARSE_QUANTUM_MS`]
/// (100 µs) quantum unless the global is cross-origin isolated (then the
/// finer resolution is preserved). Guards against timing attacks that use
/// sub-100µs resolution to build side channels.
///
/// ```
/// # use vixen_engine::high_res_time::coarsen;
/// // Not isolated → floored to 0.1ms.
/// assert_eq!(coarsen(12.3456789, false), 12.3);
/// // Isolated → preserved.
/// assert_eq!(coarsen(12.3456789, true), 12.3456789);
/// ```
pub fn coarsen(timestamp: DOMHighResTimeStamp, cross_origin_isolated: bool) -> DOMHighResTimeStamp {
    if cross_origin_isolated {
        return timestamp;
    }
    // Floor to the nearest 100µs quantum (negative timestamps shouldn't occur
    // — the clock clamps to 0 — but floor handles them defensively).
    (timestamp / COARSE_QUANTUM_MS).floor() * COARSE_QUANTUM_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- TimeOrigin -----------------------------------------------------

    #[test]
    fn origin_stores_unix_ms() {
        let o = TimeOrigin::from_unix_ms(1_700_000_000_000.0);
        assert_eq!(o.unix_ms(), 1_700_000_000_000.0);
    }

    #[test]
    fn now_is_clock_minus_origin() {
        let o = TimeOrigin::from_unix_ms(1000.0);
        assert_eq!(o.now(1500.0), 500.0);
    }

    #[test]
    fn now_clamps_pre_origin_to_zero() {
        // Clock skew: wall clock reports a pre-origin reading.
        let o = TimeOrigin::from_unix_ms(1000.0);
        assert_eq!(o.now(500.0), 0.0);
    }

    #[test]
    fn relative_to_unix_is_origin_plus_now() {
        let o = TimeOrigin::from_unix_ms(1000.0);
        assert_eq!(o.relative_to_unix(250.5), 1250.5);
    }

    // --- MonotonicClock -------------------------------------------------

    #[test]
    fn clock_starts_at_zero() {
        let mut c = MonotonicClock::new(TimeOrigin::from_unix_ms(1000.0));
        assert_eq!(c.last_raw(), 0.0);
        assert_eq!(c.now(1000.0, true), 0.0);
    }

    #[test]
    fn clock_is_monotonic() {
        let mut c = MonotonicClock::new(TimeOrigin::from_unix_ms(0.0));
        let a = c.now(100.0, true);
        let b = c.now(200.0, true);
        let cc = c.now(150.0, true); // clock went backwards
        assert!(b >= a);
        assert!(cc >= b, "monotonic violated: {cc} < {b}");
    }

    #[test]
    fn clock_never_returns_negative() {
        let mut c = MonotonicClock::new(TimeOrigin::from_unix_ms(1000.0));
        assert_eq!(c.now(500.0, true), 0.0); // pre-origin
    }

    #[test]
    fn clock_origin_round_trips() {
        let o = TimeOrigin::from_unix_ms(42.0);
        let c = MonotonicClock::new(o);
        assert_eq!(c.origin().unix_ms(), 42.0);
    }

    // --- Coarsening -----------------------------------------------------

    #[test]
    fn coarsen_floors_to_100us_when_not_isolated() {
        assert_eq!(coarsen(0.0, false), 0.0);
        assert_eq!(coarsen(12.3456789, false), 12.3);
        assert_eq!(coarsen(0.099, false), 0.0);
        assert_eq!(coarsen(0.1, false), 0.1);
        assert_eq!(coarsen(99.9999, false), 99.9);
    }

    #[test]
    fn coarsen_preserves_resolution_when_isolated() {
        assert_eq!(coarsen(12.3456789, true), 12.3456789);
        assert_eq!(coarsen(0.000001, true), 0.000001);
    }

    #[test]
    fn coarsen_applied_by_clock_when_not_isolated() {
        let mut c = MonotonicClock::new(TimeOrigin::from_unix_ms(0.0));
        // Raw 12.3456789 → coarsened to 12.3.
        assert_eq!(c.now(12.3456789, false), 12.3);
    }

    #[test]
    fn coarsen_skipped_when_isolated() {
        let mut c = MonotonicClock::new(TimeOrigin::from_unix_ms(0.0));
        assert_eq!(c.now(12.3456789, true), 12.3456789);
    }

    #[test]
    fn coarse_quantum_is_one_tenth_millisecond() {
        assert_eq!(COARSE_QUANTUM_MS, 0.1);
    }

    // --- end-to-end -----------------------------------------------------

    #[test]
    fn performance_now_round_trip_to_unix() {
        let origin = TimeOrigin::from_unix_ms(1_700_000_000_000.0);
        let mut clock = MonotonicClock::new(origin);
        let now = clock.now(1_700_000_001_500.0, true);
        assert_eq!(now, 1500.0);
        // And convert back to Unix epoch.
        assert_eq!(origin.relative_to_unix(now), 1_700_000_001_500.0);
    }
}
