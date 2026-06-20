//! Pure form-constraint validation logic — Phase 6 prep (docs/SPEC.md
//! "Form validation edge cases"). Implements the parts of HTML5 constraint
//! validation that are pure arithmetic, so the script layer (Phase 6) and
//! the `:valid`/`:invalid` selector pseudos (Phase 3 follow-up) can share
//! one source of truth.
//!
//! What lives here:
//! - [`email_is_valid`] / [`url_is_valid`] — the pinned email/URL formats
//!   from `SPEC.md` (exactly one `@`, non-empty local-part, etc.).
//! - [`Validity`] — the 11 constraint-validation flags + the `stepMismatch`
//!   arithmetic on canonical integer units.
//!
//! What does *not* live here:
//! - DOM hookup (HTMLInputElement plumbing lands in Phase 6).
//! - Custom validity (`customError`) — caller-supplied.
//! - Date/time parsing into canonical units — lives in `date_units` until a
//!   proper parser lands; this module's `step` API takes the canonical-unit
//!   representation directly so the arithmetic is testable without it.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Email + URL formats (docs/SPEC.md "Form validation edge cases")
// ---------------------------------------------------------------------------

/// HTML5 `type=email` validation per docs/SPEC.md: exactly one `@`,
/// non-empty local-part, and a domain containing at least one `.`.
///
/// This is the *Vixen-pinned* version, not the full HTML5 algorithm (which
/// allows comma-separated lists and a much more permissive grammar). The
/// simplification is deliberate; the test surface is the invariant.
pub fn email_is_valid(s: &str) -> bool {
    let n_at = s.matches('@').count();
    if n_at != 1 {
        return false;
    }
    let (local, domain) = s.split_once('@').unwrap();
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// HTML5 `type=url` validation per docs/SPEC.md: a valid scheme (letters
/// followed by `:`), `://` separator after the scheme, and a non-empty host.
pub fn url_is_valid(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else {
        return false;
    };
    if scheme.is_empty() || !scheme.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    rest.starts_with("//")
        && rest[2..]
            .split('/')
            .next()
            .map(|host| !host.is_empty())
            .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Validity flags + step arithmetic (docs/SPEC.md "Step arithmetic")
// ---------------------------------------------------------------------------

/// Per-element validity state, matching the HTML5 `ValidityState` dictionary
/// minus `customError` (which is caller-supplied via [`Validity::with_custom_error`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Validity {
    pub value_missing: bool,
    pub type_mismatch: bool,
    pub pattern_mismatch: bool,
    pub too_long: bool,
    pub too_short: bool,
    pub range_underflow: bool,
    pub range_overflow: bool,
    pub step_mismatch: bool,
    pub bad_input: bool,
    pub custom_error: bool,
}

impl Validity {
    /// True when every flag is clear (the element satisfies all constraints).
    pub fn is_valid(&self) -> bool {
        !(self.value_missing
            || self.type_mismatch
            || self.pattern_mismatch
            || self.too_long
            || self.too_short
            || self.range_underflow
            || self.range_overflow
            || self.step_mismatch
            || self.bad_input
            || self.custom_error)
    }

    /// Mark a custom error (mirrors `HTMLObjectElement.setCustomValidity`).
    pub fn with_custom_error(mut self, msg: impl Into<String>) -> Self {
        if !msg.into().is_empty() {
            self.custom_error = true;
        }
        self
    }
}

/// Canonical-unit representations for date/time inputs (docs/SPEC.md).
/// `step` arithmetic runs on these so the math is testable independent of
/// the actual datetime parser (which lands in Phase 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateTimeUnit {
    /// Days since 1970-01-01 (`<input type=date>`).
    Days(i64),
    /// Seconds since midnight (`<input type=time>`).
    Seconds(i64),
    /// Weeks since 1970-01-01 (`<input type=week>`).
    Weeks(i64),
    /// Months since year 0 (`<input type=month>`).
    Months(i64),
    /// Seconds since 1970-01-01T00:00 (`<input type=datetime-local>`).
    DateTimeSeconds(i64),
}

impl DateTimeUnit {
    /// Convert to the underlying scalar (the unit carries the meaning).
    pub fn as_scalar(self) -> i64 {
        match self {
            DateTimeUnit::Days(n) => n,
            DateTimeUnit::Seconds(n) => n,
            DateTimeUnit::Weeks(n) => n,
            DateTimeUnit::Months(n) => n,
            DateTimeUnit::DateTimeSeconds(n) => n,
        }
    }
}

/// `stepMismatch` per docs/SPEC.md: `(value - step_base)` is valid when it's
/// within float tolerance of an integer multiple of `step`. Date/time values
/// pass their canonical-unit scalar; numeric values pass raw millidigits.
///
/// `step` must be > 0 (step="0" or "any" disables step checking — caller
/// handles that before calling here).
pub fn step_mismatch(step_base: i64, value: i64, step: i64) -> bool {
    if step <= 0 {
        return false;
    }
    let diff = value.saturating_sub(step_base);
    let r = diff.rem_euclid(step);
    r != 0 && r != step
}

/// Float flavour of [`step_mismatch`] for number inputs (where `step` and
/// the value can be fractional). Uses an epsilon of `1e-9 * step`.
pub fn step_mismatch_f64(step_base: f64, value: f64, step: f64) -> bool {
    if step <= 0.0 {
        return false;
    }
    let diff = value - step_base;
    let n = (diff / step).round();
    let reconstructed = step_base + n * step;
    (value - reconstructed).abs() > 1e-9 * step.abs()
}

/// Range checks for `<input type=number>` (or any input with `min`/`max`).
/// Returns a [`Validity`] with only `range_underflow`/`range_overflow` set.
pub fn range_validity(value: f64, min: Option<f64>, max: Option<f64>) -> Validity {
    let mut v = Validity::default();
    if let Some(m) = min
        && value < m
    {
        v.range_underflow = true;
    }
    if let Some(m) = max
        && value > m
    {
        v.range_overflow = true;
    }
    v
}

/// Length checks for `<input>`/`<textarea>` (`minlength`/`maxlength`).
pub fn length_validity(len: usize, min: Option<usize>, max: Option<usize>) -> Validity {
    let mut v = Validity::default();
    if let Some(m) = min
        && len < m
    {
        v.too_short = true;
    }
    if let Some(m) = max
        && len > m
    {
        v.too_long = true;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Email ----------------------------------------------------------

    #[test]
    fn email_valid_cases() {
        for ok in ["a@b.test", "x.y@sub.example.org", "user+tag@host.co"] {
            assert!(email_is_valid(ok), "expected valid: {ok}");
        }
    }

    #[test]
    fn email_rejects_zero_or_multiple_at() {
        assert!(!email_is_valid("noat.test"));
        assert!(!email_is_valid("two@@at.test"));
        assert!(!email_is_valid("a@b@c.test"));
    }

    #[test]
    fn email_requires_dot_in_domain() {
        assert!(!email_is_valid("a@localhost"));
        assert!(!email_is_valid("a@.test")); // leading dot is not a valid domain
        assert!(!email_is_valid("a@test.")); // trailing dot
    }

    #[test]
    fn email_requires_nonempty_local_part() {
        assert!(!email_is_valid("@test.test"));
    }

    // --- URL ------------------------------------------------------------

    #[test]
    fn url_valid_cases() {
        for ok in [
            "https://example.test/",
            "http://host",
            "ftp://server/path",
            "custom://host/path?q=1",
        ] {
            assert!(url_is_valid(ok), "expected valid: {ok}");
        }
    }

    #[test]
    fn url_requires_scheme_then_slashes() {
        assert!(!url_is_valid("example.test"));
        assert!(!url_is_valid("https:example.test"));
        assert!(!url_is_valid("://host"));
        assert!(!url_is_valid("123://host"));
        assert!(!url_is_valid("http:///path")); // empty host
    }

    // --- Step arithmetic ------------------------------------------------

    #[test]
    fn step_integer_arithmetic() {
        // step=10 from base 0: 0,10,20 valid; 5,7 invalid.
        assert!(!step_mismatch(0, 0, 10));
        assert!(!step_mismatch(0, 10, 10));
        assert!(step_mismatch(0, 5, 10));
        assert!(step_mismatch(0, 7, 10));
        // Non-zero base: from base 1, step 10, valid = 1,11,21,...
        assert!(!step_mismatch(1, 11, 10));
        assert!(step_mismatch(1, 10, 10));
    }

    #[test]
    fn step_with_zero_or_negative_is_noop() {
        // step=0 / step<0 disables checking (caller handles "any" / "0").
        assert!(!step_mismatch(0, 7, 0));
        assert!(!step_mismatch(0, 7, -1));
    }

    #[test]
    fn step_f64_arithmetic() {
        // step=0.1 from base 0: 0.0, 0.1, 0.2 valid; 0.05 invalid.
        assert!(!step_mismatch_f64(0.0, 0.1, 0.1));
        assert!(!step_mismatch_f64(0.0, 0.3, 0.1));
        assert!(step_mismatch_f64(0.0, 0.05, 0.1));
        // Float tolerance: 0.3 is reconstructed to within epsilon.
        assert!(!step_mismatch_f64(0.0, 0.3, 0.1));
    }

    #[test]
    fn datetime_unit_scalar_round_trips() {
        assert_eq!(DateTimeUnit::Days(42).as_scalar(), 42);
        assert_eq!(DateTimeUnit::Months(-1).as_scalar(), -1);
        // Step arithmetic on canonical units is just integer step.
        assert!(!step_mismatch(0, DateTimeUnit::Days(7).as_scalar(), 1));
        assert!(step_mismatch(0, DateTimeUnit::Days(7).as_scalar(), 10));
    }

    // --- Range / length -------------------------------------------------

    #[test]
    fn range_validity_bounds() {
        let v = range_validity(5.0, Some(0.0), Some(10.0));
        assert!(v.is_valid());
        let v = range_validity(-1.0, Some(0.0), Some(10.0));
        assert!(v.range_underflow && !v.range_overflow);
        let v = range_validity(11.0, Some(0.0), Some(10.0));
        assert!(!v.range_underflow && v.range_overflow);
        // Unbounded sides are ignored.
        let v = range_validity(f64::INFINITY, None, None);
        assert!(v.is_valid());
    }

    #[test]
    fn length_validity_bounds() {
        let v = length_validity(5, Some(1), Some(10));
        assert!(v.is_valid());
        let v = length_validity(0, Some(1), Some(10));
        assert!(v.too_short);
        let v = length_validity(15, Some(1), Some(10));
        assert!(v.too_long);
    }

    // --- Validity composition ------------------------------------------

    #[test]
    fn validity_aggregates() {
        let mut v = Validity::default();
        assert!(v.is_valid());
        v.value_missing = true;
        assert!(!v.is_valid());
        let v = v.with_custom_error("boom");
        assert!(v.custom_error);
        assert!(!v.is_valid());
        // Empty custom message is a no-op (matches HTML5 semantics).
        let v = Validity::default().with_custom_error("");
        assert!(!v.custom_error);
    }
}
