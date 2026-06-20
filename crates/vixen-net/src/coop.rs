//! HTML § 7.8 `Cross-Origin-Opener-Policy` (COOP) — Phase 7 security prep.
//! The response-header policy that opts a top-level document into
//! process-isolation from its openers / openees. Together with
//! [`crate::coep`] (the `Cross-Origin-Embedder-Policy`), a `same-origin`
//! COOP is one half of the "cross-origin isolated" context gate that
//! unlocks the high-resolution timers ([`crate::coep::is_cross_origin_isolated`])
//! and `postMessage` of `SharedArrayBuffer`.
//!
//! What lives here:
//! - [`Coop`] — the three § 7.8.4 policy values (`unsafe-none` default,
//!   `same-origin-allow-popups`, `same-origin`).
//! - [`Coop::parse`] — the § 7.8.1 structured-header item parse
//!   (case-insensitive token, `report-to` parameter carried separately).
//! - [`Coop::isolates_opener`] — the § 7.8.4 "does this policy isolate the
//!   document from its opener?" predicate the navigation layer consults
//!   before sharing the browsing context.
//!
//! What does *not* live here:
//! - The actual process/browser-context switching (the embedder; Phase 9).
//! - The reporting API surface (`report-to` is captured but the endpoint
//!   resolution lives with the reporting host hook).
//! - The `same-origin-plus-coep` variant (a recent extension that pairs COOP
//!   with a per-response COEP; deferred until the host-hook layer lands).
//!
//! ## Trust boundary
//!
//! COOP is a Spectre-mitigation boundary: without it, a document that opens
//! (or is opened by) a cross-origin page shares a browsing-context group,
//! letting either side reach the other's window object. `same-origin` breaks
//! that reachability at navigation time. The fetch/navigation layer consults
//! [`Coop::isolates_opener`] before reusing a browsing context, and the
//! cross-origin-isolation gate ([`crate::coep::is_cross_origin_isolated`])
//! gates the powerful APIs on the *both policies set* combination.
//!
//! Reference: <https://html.spec.whatwg.org/multipage/origin.html#the-cross-origin-opener-policy-header>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Coop
// ---------------------------------------------------------------------------

/// The `Cross-Origin-Opener-Policy` value (HTML § 7.8.4). The structured-
/// header token the response carries; the default (no header) is
/// [`Coop::UnsafeNone`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Coop {
    /// `unsafe-none` — the default. The document is *not* isolated from its
    /// opener/openee; the browsing-context group is shared unless the other
    /// side forces isolation. This is the value the parser returns for an
    /// absent header.
    #[default]
    UnsafeNone,
    /// `same-origin-allow-popups` — the document retains its opener (so it
    /// can script windows it opens), but isolates itself from cross-origin
    /// openees that navigate away to a cross-origin document. The
    /// intermediate isolation tier.
    SameOriginAllowPopups,
    /// `same-origin` — the document is fully isolated from any cross-origin
    /// opener/openee. This is the value that, combined with a COEP of
    /// [`crate::coep::Coep::RequireCorp`] or
    /// [`crate::coep::Coep::Credentialless`], makes the context
    /// cross-origin-isolated.
    SameOrigin,
}

impl Coop {
    /// Parse the `Cross-Origin-Opener-Policy` header value per HTML § 7.8.1.
    /// The value is a structured-header item: a single token (case-
    /// insensitive) optionally followed by parameters (`report-to="…"`). The
    /// token selects the policy; an unknown token fails closed to the
    /// default [`Coop::UnsafeNone`] (per § 7.8.1 "if value is not one of
    /// these, default"). The `report-to` parameter, if present, is returned
    /// alongside the policy.
    ///
    /// ```
    /// # use vixen_net::coop::{Coop, parse_coop};
    /// let (policy, report_to) = parse_coop("same-origin; report-to=\"coop-group\"");
    /// assert_eq!(policy, Coop::SameOrigin);
    /// assert_eq!(report_to.as_deref(), Some("coop-group"));
    /// ```
    pub fn parse(header: &str) -> (Self, Option<String>) {
        // Structured-header item: the token is everything up to the first
        // `;`; parameters follow as `key=value` / bare tokens, `=`-separated
        // and `;`-delimited.
        let (token, rest) = header
            .split_once(';')
            .map(|(t, r)| (t, Some(r)))
            .unwrap_or((header, None));
        let policy = match token.trim().to_ascii_lowercase().as_str() {
            "same-origin" => Coop::SameOrigin,
            "same-origin-allow-popups" => Coop::SameOriginAllowPopups,
            "unsafe-none" => Coop::UnsafeNone,
            // § 7.8.1: an unknown token is treated as the default. Browsers
            // log a console warning; the policy is UnsafeNone.
            _ => Coop::UnsafeNone,
        };
        let report_to = rest.and_then(find_report_to);
        (policy, report_to)
    }

    /// Whether this policy isolates the document from a cross-origin opener
    /// (HTML § 7.8.4 "the result of matching the openers"). Only
    /// [`Coop::SameOrigin`] does; the other two tiers keep the opener
    /// reachable.
    pub const fn isolates_opener(self) -> bool {
        matches!(self, Coop::SameOrigin)
    }

    /// The CSS keyword / structured-header token for this value (canonical
    /// lowercase form).
    pub const fn keyword(self) -> &'static str {
        match self {
            Coop::UnsafeNone => "unsafe-none",
            Coop::SameOriginAllowPopups => "same-origin-allow-popups",
            Coop::SameOrigin => "same-origin",
        }
    }
}

/// Parse the `Cross-Origin-Opener-Policy` header into the [`Coop`] policy +
/// the optional `report-to` endpoint name. Convenience wrapper around
/// [`Coop::parse`] for the fetch-layer call site that wants both.
pub fn parse_coop(header: &str) -> (Coop, Option<String>) {
    Coop::parse(header)
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
        assert_eq!(Coop::parse("same-origin").0, Coop::SameOrigin);
        assert_eq!(
            Coop::parse("same-origin-allow-popups").0,
            Coop::SameOriginAllowPopups
        );
        assert_eq!(Coop::parse("unsafe-none").0, Coop::UnsafeNone);
    }

    #[test]
    fn parse_case_insensitive_token() {
        assert_eq!(Coop::parse("SAME-ORIGIN").0, Coop::SameOrigin);
        assert_eq!(Coop::parse("  Same-Origin  ").0, Coop::SameOrigin);
    }

    #[test]
    fn parse_empty_or_absent_is_unsafe_none() {
        assert_eq!(Coop::parse("").0, Coop::UnsafeNone);
    }

    #[test]
    fn parse_unknown_token_fails_closed_to_unsafe_none() {
        // § 7.8.1: an unknown token is treated as the default (UnsafeNone).
        assert_eq!(Coop::parse("same-site").0, Coop::UnsafeNone);
        assert_eq!(Coop::parse("garbage").0, Coop::UnsafeNone);
    }

    #[test]
    fn parse_report_to_parameter() {
        let (policy, report) = Coop::parse("same-origin; report-to=\"coop-group\"");
        assert_eq!(policy, Coop::SameOrigin);
        assert_eq!(report.as_deref(), Some("coop-group"));
    }

    #[test]
    fn parse_report_to_unquoted() {
        let (policy, report) = Coop::parse("same-origin; report-to=endpoint");
        assert_eq!(policy, Coop::SameOrigin);
        assert_eq!(report.as_deref(), Some("endpoint"));
    }

    #[test]
    fn parse_report_to_absent_when_not_present() {
        let (policy, report) = Coop::parse("same-origin");
        assert_eq!(policy, Coop::SameOrigin);
        assert!(report.is_none());
    }

    #[test]
    fn parse_report_to_empty_value_is_dropped() {
        // An empty quoted value carries no endpoint name; drop it.
        let (_, report) = Coop::parse("same-origin; report-to=\"\"");
        assert!(report.is_none());
    }

    // --- Predicates -----------------------------------------------------

    #[test]
    fn only_same_origin_isolates_opener() {
        assert!(Coop::SameOrigin.isolates_opener());
        assert!(!Coop::SameOriginAllowPopups.isolates_opener());
        assert!(!Coop::UnsafeNone.isolates_opener());
    }

    #[test]
    fn keyword_round_trip() {
        for v in [
            Coop::UnsafeNone,
            Coop::SameOriginAllowPopups,
            Coop::SameOrigin,
        ] {
            assert_eq!(Coop::parse(v.keyword()).0, v);
        }
    }

    #[test]
    fn default_is_unsafe_none() {
        assert_eq!(Coop::default(), Coop::UnsafeNone);
    }

    // --- parse_coop wrapper --------------------------------------------

    #[test]
    fn parse_coop_wrapper_matches_method() {
        let header = "same-origin; report-to=\"g\"";
        assert_eq!(parse_coop(header), Coop::parse(header));
    }
}
