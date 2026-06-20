//! Fetch § 3.2 `Cross-Origin-Embedder-Policy` (COEP) — Phase 7 security
//! prep. The response-header policy that, combined with [`crate::coop`]
//! `same-origin`, makes a browsing context "cross-origin isolated" — the
//! gate that unlocks the high-resolution timers
//! ([`vixen_engine::high_res_time::coarsen`]), `SharedArrayBuffer`, and the
//! other powerful APIs that depend on a Spectre-hardened context.
//!
//! What lives here:
//! - [`Coep`] — the three § 3.2 policy values (`unsafe-none` default,
//!   `require-corp`, `credentialless`).
//! - [`Coep::parse`] — the structured-header item parse (case-insensitive
//!   token, `report-to` parameter carried separately).
//! - [`is_cross_origin_isolated`] — the combined COOP+COEP predicate
//!   ([`crate::coop::Coop::SameOrigin`] + a COEP of [`Coep::RequireCorp`] or
//!   [`Coep::Credentialless`]) that the `performance.now()` coarsening gate
//!   and the `SharedArrayBuffer` exposure consult.
//!
//! What does *not* live here:
//! - The CORP enforcement on every subresource response (the fetch pipeline
//!   consults [`Coep`] + the response's `Cross-Origin-Resource-Policy`
//!   header; the CORP parser lands with the fetch layer).
//! - The reporting surface (`report-to` is captured; endpoint resolution is
//!   the reporting host hook).
//! - COEP inheritance across nested contexts (the embedder + the parent's
//!   policy combine; that logic lives in the navigation/host-hook layer).
//!
//! ## Trust boundary
//!
//! COEP is the second Spectre-mitigation boundary: it forces every
//! cross-origin subresource the document loads to either opt-in via CORP
//! (`Cross-Origin-Resource-Policy: same-origin` &c.) or be loaded with CORS
//! (under `require-corp`), removing the side-channel reachability that a
//! Spectre gadget would need. [`is_cross_origin_isolated`] is the gate the
//! powerful APIs check before exposing themselves: only when the document is
//! *both* COOP-isolated (no cross-origin opener can reach it) *and* COEP-
//! hardened (no cross-origin subresource can be read) are the high-resolution
//! side-channels safe to expose.
//!
//! Reference: <https://fetch.spec.whatwg.org/#http-header-layer>
//! (§ 3.2 `Cross-Origin-Embedder-Policy`).
//! Cross-origin isolation: <https://html.spec.whatwg.org/multipage/origin.html#is-cross-origin-isolated>.

#![forbid(unsafe_code)]

use crate::coop::Coop;

// ---------------------------------------------------------------------------
// Coep
// ---------------------------------------------------------------------------

/// The `Cross-Origin-Embedder-Policy` value (Fetch § 3.2). The structured-
/// header token the response carries; the default (no header) is
/// [`Coep::UnsafeNone`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Coep {
    /// `unsafe-none` — the default. Cross-origin subresources load without a
    /// CORP opt-in or CORS requirement; the document is not COEP-hardened.
    #[default]
    UnsafeNone,
    /// `require-corp` — every cross-origin subresource must either carry a
    /// `Cross-Origin-Resource-Policy` header opting it in, or be loaded via
    /// CORS. The original COEP tier.
    RequireCorp,
    /// `credentialless` — cross-origin subresources load without credentials
    /// and are treated as same-origin for the purposes of the no-CORS read
    /// gate (a newer, more permissive tier that still hardens against
    /// Spectre without requiring every third party to ship CORP).
    Credentialless,
}

impl Coep {
    /// Parse the `Cross-Origin-Embedder-Policy` header value per Fetch § 3.2.
    /// The value is a structured-header item: a single token (case-
    /// insensitive) optionally followed by parameters (`report-to="…"`). An
    /// unknown token fails closed to the default [`Coep::UnsafeNone`].
    ///
    /// ```
    /// # use vixen_net::coep::{Coep, parse_coep};
    /// let (policy, report_to) = parse_coep("require-corp; report-to=\"ceep\"");
    /// assert_eq!(policy, Coep::RequireCorp);
    /// assert_eq!(report_to.as_deref(), Some("ceep"));
    /// ```
    pub fn parse(header: &str) -> (Self, Option<String>) {
        let (token, rest) = header
            .split_once(';')
            .map(|(t, r)| (t, Some(r)))
            .unwrap_or((header, None));
        let policy = match token.trim().to_ascii_lowercase().as_str() {
            "require-corp" => Coep::RequireCorp,
            "credentialless" => Coep::Credentialless,
            "unsafe-none" => Coep::UnsafeNone,
            // Unknown token ⇒ the default (browsers log a console warning).
            _ => Coep::UnsafeNone,
        };
        let report_to = rest.and_then(find_report_to);
        (policy, report_to)
    }

    /// Whether this COEP value, combined with a `same-origin` COOP, makes the
    /// context cross-origin isolated. Only [`Coep::RequireCorp`] and
    /// [`Coep::Credentialless`] do; [`Coep::UnsafeNone`] does not.
    pub const fn enables_cross_origin_isolation(self) -> bool {
        matches!(self, Coep::RequireCorp | Coep::Credentialless)
    }

    /// The structured-header token for this value (canonical lowercase form).
    pub const fn keyword(self) -> &'static str {
        match self {
            Coep::UnsafeNone => "unsafe-none",
            Coep::RequireCorp => "require-corp",
            Coep::Credentialless => "credentialless",
        }
    }
}

/// Parse the `Cross-Origin-Embedder-Policy` header into the [`Coep`] policy +
/// the optional `report-to` endpoint name. Convenience wrapper around
/// [`Coep::parse`] for the fetch-layer call site that wants both.
pub fn parse_coep(header: &str) -> (Coep, Option<String>) {
    Coep::parse(header)
}

// ---------------------------------------------------------------------------
// Cross-origin isolation gate (HTML § 7.2 "is cross-origin isolated")
// ---------------------------------------------------------------------------

/// The HTML § 7.2 "is the context cross-origin isolated?" predicate. `true`
/// iff the COOP is [`Coop::SameOrigin`] **and** the COEP is
/// [`Coep::RequireCorp`] or [`Coep::Credentialless`]. This is the gate the
/// high-resolution-timer coarsening
/// ([`vixen_engine::high_res_time::coarsen`]) and the `SharedArrayBuffer`
/// exposure consult.
///
/// ```
/// # use vixen_net::coop::Coop;
/// # use vixen_net::coep::{is_cross_origin_isolated, Coep};
/// assert!(is_cross_origin_isolated(Coop::SameOrigin, Coep::RequireCorp));
/// assert!(is_cross_origin_isolated(Coop::SameOrigin, Coep::Credentialless));
/// // Missing either half ⇒ not isolated.
/// assert!(!is_cross_origin_isolated(Coop::UnsafeNone, Coep::RequireCorp));
/// assert!(!is_cross_origin_isolated(Coop::SameOrigin, Coep::UnsafeNone));
/// ```
pub const fn is_cross_origin_isolated(coop: Coop, coep: Coep) -> bool {
    coop.isolates_opener() && coep.enables_cross_origin_isolation()
}

/// Extract the `report-to` parameter value from the trailing structured-
/// header parameters (the part after the first `;`). Returns `None` when
/// the parameter is absent or not a quoted/bare string.
fn find_report_to(params: &str) -> Option<String> {
    for param in params.split(';') {
        let param = param.trim();
        if let Some(rest) = param
            .strip_prefix("report-to")
            .or_else(|| param.strip_prefix("reporting-endpoints"))
        {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim().trim_matches('"');
                if !value.is_empty() {
                    return Some(value.to_owned());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Parse ----------------------------------------------------------

    #[test]
    fn parse_three_tiers() {
        assert_eq!(Coep::parse("require-corp").0, Coep::RequireCorp);
        assert_eq!(Coep::parse("credentialless").0, Coep::Credentialless);
        assert_eq!(Coep::parse("unsafe-none").0, Coep::UnsafeNone);
    }

    #[test]
    fn parse_case_insensitive_token() {
        assert_eq!(Coep::parse("REQUIRE-CORP").0, Coep::RequireCorp);
        assert_eq!(Coep::parse("  Credentialless  ").0, Coep::Credentialless);
    }

    #[test]
    fn parse_empty_or_absent_is_unsafe_none() {
        assert_eq!(Coep::parse("").0, Coep::UnsafeNone);
    }

    #[test]
    fn parse_unknown_token_fails_closed_to_unsafe_none() {
        assert_eq!(Coep::parse("require-same-origin").0, Coep::UnsafeNone);
        assert_eq!(Coep::parse("garbage").0, Coep::UnsafeNone);
    }

    #[test]
    fn parse_report_to_parameter() {
        let (policy, report) = Coep::parse("require-corp; report-to=\"ceep\"");
        assert_eq!(policy, Coep::RequireCorp);
        assert_eq!(report.as_deref(), Some("ceep"));
    }

    #[test]
    fn parse_report_to_unquoted() {
        let (policy, report) = Coep::parse("credentialless; report-to=endpoint");
        assert_eq!(policy, Coep::Credentialless);
        assert_eq!(report.as_deref(), Some("endpoint"));
    }

    #[test]
    fn parse_report_to_absent_when_not_present() {
        let (policy, report) = Coep::parse("require-corp");
        assert_eq!(policy, Coep::RequireCorp);
        assert!(report.is_none());
    }

    // --- Predicates -----------------------------------------------------

    #[test]
    fn require_corp_and_credentialless_enable_isolation() {
        assert!(Coep::RequireCorp.enables_cross_origin_isolation());
        assert!(Coep::Credentialless.enables_cross_origin_isolation());
        assert!(!Coep::UnsafeNone.enables_cross_origin_isolation());
    }

    #[test]
    fn keyword_round_trip() {
        for v in [Coep::UnsafeNone, Coep::RequireCorp, Coep::Credentialless] {
            assert_eq!(Coep::parse(v.keyword()).0, v);
        }
    }

    #[test]
    fn default_is_unsafe_none() {
        assert_eq!(Coep::default(), Coep::UnsafeNone);
    }

    // --- Cross-origin isolation gate -----------------------------------

    #[test]
    fn isolation_requires_both_halves() {
        use crate::coop::Coop;
        // Both halves set ⇒ isolated.
        assert!(is_cross_origin_isolated(
            Coop::SameOrigin,
            Coep::RequireCorp
        ));
        assert!(is_cross_origin_isolated(
            Coop::SameOrigin,
            Coep::Credentialless
        ));
        // Missing COOP ⇒ not isolated.
        assert!(!is_cross_origin_isolated(
            Coop::UnsafeNone,
            Coep::RequireCorp
        ));
        assert!(!is_cross_origin_isolated(
            Coop::SameOriginAllowPopups,
            Coep::RequireCorp
        ));
        // Missing COEP ⇒ not isolated.
        assert!(!is_cross_origin_isolated(
            Coop::SameOrigin,
            Coep::UnsafeNone
        ));
    }

    #[test]
    fn parse_coep_wrapper_matches_method() {
        let header = "require-corp; report-to=\"g\"";
        assert_eq!(parse_coep(header), Coep::parse(header));
    }
}
