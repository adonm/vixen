//! DOM § 8.1 — `AbortController` / `AbortSignal` (pure logic). The state
//! model + composition primitives the `fetch()` / `XMLHttpRequest` / streaming
//! host hooks and `AbortSignal.any()` / `.timeout()` reduce to. Complements
//! [`crate::headers`] and the fetch host-hook data model.
//!
//! What lives here:
//! - [`AbortSignal`] — the `aborted` + `reason` value model (the default
//!   reason is the `"AbortError"` `DOMException`; a custom reason is an opaque
//!   `String` here — the JS value lives behind the host hook).
//! - [`AbortController`] — owns a signal + [`AbortController::abort`].
//! - [`abort_any`] — DOM § 8.1.3.2 `AbortSignal.any(signals)` snapshot: a new
//!   signal aborted iff any input is (taking the first-aborted input's
//!   reason); the reactive propagation (re-evaluate on a later input abort)
//!   is the host-hook event-loop layer's job.
//! - [`TimeoutSignal`] — DOM § 8.1.3.2 `AbortSignal.timeout(ms)` request
//!   record (the delay + the "already-finished" snapshot); the actual timer
//!   arming lives in the host hook.
//!
//! What does *not* live here:
//! - The event-loop integration that fires `abort` events and re-evaluates
//!   [`abort_any`] composites (Phase 6 host hook).
//! - The JS `reason` value (carried as an opaque `String` here; the host hook
//!   boxes the real JS `any` value).
//! - `addEventListener("abort", …)` plumbing (Phase 6 host hook).
//!
//! ## Default reason
//!
//! DOM § 8.1.3: `controller.abort()` with no argument sets the signal's reason
//! to a `DOMException` with name `"AbortError"`. Modelled here as
//! [`AbortReason::AbortError`]; a caller-supplied reason maps to
//! [`AbortReason::Custom`].
//!
//! Reference: <https://dom.spec.whatwg.org/#interface-abortcontroller>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// AbortReason
// ---------------------------------------------------------------------------

/// The reason an [`AbortSignal`] aborted. The DOM default (abort() with no
/// argument) is the `"AbortError"` `DOMException`; a caller can pass any JS
/// value, modelled here as an opaque [`String`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortReason {
    /// `controller.abort()` with no argument (DOM § 8.1.3 → `DOMException`
    /// named `"AbortError"`).
    AbortError,
    /// `controller.abort(reason)` with a caller-supplied reason. Carried as
    /// an opaque string; the host hook boxes the real JS value.
    Custom(String),
}

impl AbortReason {
    /// The DOM `DOMException.name` the host hook reports for this reason
    /// (`"AbortError"` for the default; the custom string otherwise —
    /// `reason.toString()` when it's a string, `reason.name` when it's an
    /// `Error`-like).
    pub fn name(&self) -> &str {
        match self {
            AbortReason::AbortError => "AbortError",
            AbortReason::Custom(s) => s,
        }
    }
}

impl std::fmt::Display for AbortReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

// ---------------------------------------------------------------------------
// AbortSignal
// ---------------------------------------------------------------------------

/// DOM § 8.1.3 `AbortSignal` — the `aborted` flag + reason. Cheap to clone;
/// the host hook keeps the authoritative copy and notifies listeners when the
/// flag flips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortSignal {
    aborted: bool,
    reason: Option<AbortReason>,
}

impl AbortSignal {
    /// A fresh, unsignaled signal.
    pub const fn new() -> Self {
        Self {
            aborted: false,
            reason: None,
        }
    }

    /// A signal already aborted with `reason` (used by [`abort_any`] /
    /// [`TimeoutSignal::expired`] / the host hook when an upstream abort
    /// fires synchronously).
    pub fn aborted_with(reason: AbortReason) -> Self {
        Self {
            aborted: true,
            reason: Some(reason),
        }
    }

    /// Whether the signal has aborted.
    pub const fn aborted(&self) -> bool {
        self.aborted
    }

    /// The abort reason, or `None` while unsignaled.
    pub fn reason(&self) -> Option<&AbortReason> {
        self.reason.as_ref()
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AbortController
// ---------------------------------------------------------------------------

/// DOM § 8.1.2 `AbortController` — owns a signal and the sole right to abort
/// it. The host hook hands the signal to async work and keeps the controller
/// for the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortController {
    signal: AbortSignal,
}

impl AbortController {
    /// A fresh controller + its unsignaled signal.
    pub const fn new() -> Self {
        Self {
            signal: AbortSignal::new(),
        }
    }

    /// The signal this controller owns.
    pub fn signal(&self) -> &AbortSignal {
        &self.signal
    }

    /// Abort the signal with `reason` (default [`AbortReason::AbortError`]).
    /// Idempotent: a second call is a no-op (the first reason wins, per DOM
    /// § 8.1.3 — `Set signal's abort reason` is a no-op once set).
    pub fn abort(&mut self, reason: Option<AbortReason>) {
        if self.signal.aborted {
            return;
        }
        self.signal = AbortSignal::aborted_with(reason.unwrap_or(AbortReason::AbortError));
    }
}

impl Default for AbortController {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AbortSignal.any() (DOM § 8.1.3.2)
// ---------------------------------------------------------------------------

/// DOM § 8.1.3.2 `AbortSignal.any(signals)` — snapshot composition: returns a
/// new signal that is aborted iff any input is already aborted, taking the
/// **first** aborted input's reason (document order). Returns an unsignaled
/// signal if no input has aborted yet.
///
/// The reactive half (abort the composite when an input aborts *later*) is the
/// host-hook event-loop layer's job: it re-runs this snapshot each time an
/// input's abort fires, or wires the dependency at construction time.
///
/// ```
/// # use vixen_engine::abort::{abort_any, AbortController, AbortReason};
/// let mut a = AbortController::new();
/// let mut b = AbortController::new();
/// b.abort(None);
/// let composite = abort_any(&[a.signal(), b.signal()]);
/// assert!(composite.aborted());
/// assert_eq!(composite.reason(), Some(&AbortReason::AbortError));
/// ```
pub fn abort_any(signals: &[&AbortSignal]) -> AbortSignal {
    for s in signals {
        if s.aborted {
            // Clone the first-aborted input's reason (§ 8.1.3.2: the
            // composite takes the reason of the dependent signal that
            // aborted first).
            return AbortSignal::aborted_with(s.reason.clone().unwrap_or(AbortReason::AbortError));
        }
    }
    AbortSignal::new()
}

// ---------------------------------------------------------------------------
// AbortSignal.timeout() (DOM § 8.1.3.2)
// ---------------------------------------------------------------------------

/// DOM § 8.1.3.2 `AbortSignal.timeout(ms)` — the request record the host hook
/// arms a timer against. Carries the delay (milliseconds, saturating) and the
/// "already-expired" snapshot (a zero/negative delay aborts synchronously, per
/// the spec's "if ms ≤ 0, abort the signal immediately").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeoutSignal {
    /// The delay in milliseconds, saturating at `i64::MAX` (the spec uses a
    /// `DOMHighResTimeStamp` = `f64` milliseconds; the host hook converts).
    pub delay_ms: u64,
    signal: AbortSignal,
}

impl TimeoutSignal {
    /// A timeout request for `delay_ms`. If `delay_ms == 0` the signal is
    /// already aborted (DOM § 8.1.3.2: a zero/negative delay aborts
    /// synchronously).
    pub fn new(delay_ms: u64) -> Self {
        let signal = if delay_ms == 0 {
            AbortSignal::aborted_with(AbortReason::Custom("timedout".to_owned()))
        } else {
            AbortSignal::new()
        };
        Self { delay_ms, signal }
    }

    /// The signal that will abort when the timer fires. The host hook arms the
    /// timer and calls [`TimeoutSignal::expire`] on elapse.
    pub fn signal(&self) -> &AbortSignal {
        &self.signal
    }

    /// Mark the timeout elapsed. The host hook calls this when the armed timer
    /// fires (or immediately if never armed). Idempotent.
    pub fn expire(&mut self) {
        if self.signal.aborted {
            return;
        }
        self.signal = AbortSignal::aborted_with(AbortReason::Custom("timedout".to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- AbortSignal basics --------------------------------------------

    #[test]
    fn fresh_signal_is_unsignaled() {
        let s = AbortSignal::new();
        assert!(!s.aborted());
        assert!(s.reason().is_none());
    }

    #[test]
    fn aborted_with_carries_reason() {
        let s = AbortSignal::aborted_with(AbortReason::AbortError);
        assert!(s.aborted());
        assert_eq!(s.reason(), Some(&AbortReason::AbortError));
    }

    #[test]
    fn reason_name_round_trips() {
        assert_eq!(AbortReason::AbortError.name(), "AbortError");
        assert_eq!(
            AbortReason::Custom("user-cancelled".into()).name(),
            "user-cancelled"
        );
        assert_eq!(format!("{}", AbortReason::AbortError), "AbortError");
    }

    // --- AbortController ------------------------------------------------

    #[test]
    fn controller_starts_unsignaled() {
        let c = AbortController::new();
        assert!(!c.signal().aborted());
    }

    #[test]
    fn abort_without_reason_defaults_to_abort_error() {
        let mut c = AbortController::new();
        c.abort(None);
        assert!(c.signal().aborted());
        assert_eq!(c.signal().reason(), Some(&AbortReason::AbortError));
    }

    #[test]
    fn abort_with_custom_reason() {
        let mut c = AbortController::new();
        c.abort(Some(AbortReason::Custom("user".into())));
        assert_eq!(
            c.signal().reason(),
            Some(&AbortReason::Custom("user".into()))
        );
    }

    #[test]
    fn abort_is_idempotent_first_reason_wins() {
        let mut c = AbortController::new();
        c.abort(Some(AbortReason::Custom("first".into())));
        c.abort(Some(AbortReason::Custom("second".into())));
        // Second call is a no-op.
        assert_eq!(
            c.signal().reason(),
            Some(&AbortReason::Custom("first".into()))
        );
    }

    // --- abort_any ------------------------------------------------------

    #[test]
    fn any_of_all_unsignaled_is_unsignaled() {
        let a = AbortController::new();
        let b = AbortController::new();
        let composite = abort_any(&[a.signal(), b.signal()]);
        assert!(!composite.aborted());
        assert!(composite.reason().is_none());
    }

    #[test]
    fn any_of_one_aborted_is_aborted_with_that_reason() {
        let a = AbortController::new();
        let mut b = AbortController::new();
        b.abort(Some(AbortReason::Custom("b-reason".into())));
        let composite = abort_any(&[a.signal(), b.signal()]);
        assert!(composite.aborted());
        assert_eq!(
            composite.reason(),
            Some(&AbortReason::Custom("b-reason".into()))
        );
    }

    #[test]
    fn any_takes_first_aborted_input_reason_in_document_order() {
        let mut a = AbortController::new();
        let mut b = AbortController::new();
        a.abort(Some(AbortReason::Custom("a-first".into())));
        b.abort(Some(AbortReason::Custom("b-second".into())));
        let composite = abort_any(&[a.signal(), b.signal()]);
        // `a` is first in the input list → its reason wins.
        assert_eq!(
            composite.reason(),
            Some(&AbortReason::Custom("a-first".into()))
        );
    }

    #[test]
    fn any_of_empty_list_is_unsignaled() {
        let composite = abort_any(&[]);
        assert!(!composite.aborted());
    }

    #[test]
    fn any_propagates_default_reason_from_aborted_input() {
        let mut a = AbortController::new();
        a.abort(None); // AbortError
        let composite = abort_any(&[a.signal()]);
        assert_eq!(composite.reason(), Some(&AbortReason::AbortError));
    }

    // --- TimeoutSignal --------------------------------------------------

    #[test]
    fn timeout_positive_delay_is_unsignaled() {
        let t = TimeoutSignal::new(5000);
        assert_eq!(t.delay_ms, 5000);
        assert!(!t.signal().aborted());
    }

    #[test]
    fn timeout_zero_delay_is_immediately_aborted() {
        let t = TimeoutSignal::new(0);
        assert!(t.signal().aborted());
        assert_eq!(
            t.signal().reason(),
            Some(&AbortReason::Custom("timedout".into()))
        );
    }

    #[test]
    fn timeout_expire_marks_aborted() {
        let mut t = TimeoutSignal::new(100);
        assert!(!t.signal().aborted());
        t.expire();
        assert!(t.signal().aborted());
        assert_eq!(
            t.signal().reason(),
            Some(&AbortReason::Custom("timedout".into()))
        );
    }

    #[test]
    fn timeout_expire_is_idempotent() {
        let mut t = TimeoutSignal::new(100);
        t.expire();
        t.expire(); // no-op
        assert_eq!(
            t.signal().reason(),
            Some(&AbortReason::Custom("timedout".into()))
        );
    }

    // --- composition ----------------------------------------------------

    #[test]
    fn any_then_timeout_compose() {
        // A composite of a timeout signal + a manual signal: aborting either
        // aborts the composite (snapshot).
        let timeout = TimeoutSignal::new(0); // already expired
        let manual = AbortController::new();
        let composite = abort_any(&[timeout.signal(), manual.signal()]);
        assert!(composite.aborted()); // timeout already fired
    }

    #[test]
    fn controller_default_matches_new() {
        assert_eq!(AbortController::default(), AbortController::new());
        assert_eq!(AbortSignal::default(), AbortSignal::new());
    }
}
