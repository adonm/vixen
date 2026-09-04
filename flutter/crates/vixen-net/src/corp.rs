//! Fetch § 4.5.3 `Cross-Origin-Resource-Policy` — the response-header
//! boundary the fetch layer consults (together with [`crate::coep::Coep`])
//! before letting a no-cors subresource load into a COEP-hardened document
//! (Phase 7 prep). Pure over the two origins + the parsed header; the
//! fetch-layer CORS-mode flag is the caller's input.
//!
//! What lives here:
//! - [`Corp`] — the `Cross-Origin-Resource-Policy` value (`same-origin` /
//!   `same-site` / `cross-origin`).
//! - [`parse_corp`] — the header value parse (case-insensitive token;
//!   `None` for an absent / unparseable header).
//! - [`is_same_site`] — the § 4.5.3 schemeful, PSL-backed "same-site"
//!   registrable-domain predicate.
//! - [`check_corp`] — the § 4.5.3 CORP check given the request + resource
//!   origins + the parsed [`Corp`] (`Allow` / `Block`).
//! - [`coep_corp_gate`] — the combined COEP + CORP gate the fetch layer
//!   consults: `unsafe-none` ⇒ allow; `require-corp` ⇒ block a cross-origin
//!   no-cors response without a CORP opt-in (same-origin ⇒ the opt-in);
//!   `credentialless` ⇒ allow a cross-origin no-cors response without
//!   credentials. CORS-mode responses are always allowed (CORS is the
//!   alternative opt-in).
//!
//! What does *not* live here:
//! - The COEP parse ([`crate::coep::parse_coep`]) — re-used as-is.
//! - The CORS check itself — [`crate::cors::cors_check`]; the caller passes
//!   `is_cors` (whether the request mode is `cors` + the response passed
//!   the CORS check).
//!
//! Reference: <https://fetch.spec.whatwg.org/#cross-origin-resource-policy-header>,
//! COEP <https://fetch.spec.whatwg.org/#cross-origin-embedder-policy>.

#![forbid(unsafe_code)]

use crate::coep::Coep;
use crate::origin::Origin;
use crate::site::same_registrable_domain;

// ---------------------------------------------------------------------------
// Corp
// ---------------------------------------------------------------------------

/// The `Cross-Origin-Resource-Policy` value (Fetch § 4.5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Corp {
    /// `same-origin` (the most restrictive) — allow only same-origin
    /// requesters.
    SameOrigin,
    /// `same-site` — allow same-site requesters (the registrable-domain
    /// match).
    SameSite,
    /// `cross-origin` (the least restrictive) — allow any requester.
    #[default]
    CrossOrigin,
}

impl Corp {
    /// Parse a single `Cross-Origin-Resource-Policy` header value. The
    /// value is a case-insensitive token (the structured-header item form
    /// without parameters per § 4.5.3; a `;`-suffix is dropped). Returns
    /// `None` for an empty / unrecognised value (the caller treats an
    /// absent header distinctly from a present one via the `Option<Corp>`
    /// the gate takes).
    pub fn parse(header: &str) -> Option<Self> {
        let token = header.split(';').next()?.trim();
        match token.to_ascii_lowercase().as_str() {
            "same-origin" => Some(Self::SameOrigin),
            "same-site" => Some(Self::SameSite),
            "cross-origin" => Some(Self::CrossOrigin),
            _ => None,
        }
    }

    /// The canonical serialised form.
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::SameOrigin => "same-origin",
            Self::SameSite => "same-site",
            Self::CrossOrigin => "cross-origin",
        }
    }
}

/// Parse the `Cross-Origin-Resource-Policy` header. Convenience wrapper
/// around [`Corp::parse`] for the fetch-layer call site.
pub fn parse_corp(header: &str) -> Option<Corp> {
    Corp::parse(header)
}

// ---------------------------------------------------------------------------
// same-site + the CORP check
// ---------------------------------------------------------------------------

/// The § 4.5.3 "same-site" predicate. Opaque origins are never same-site
/// with anything (including each other).
pub fn is_same_site(a: &Origin, b: &Origin) -> bool {
    if a.is_opaque() || b.is_opaque() {
        return false;
    }
    if a.scheme() != b.scheme() {
        return false;
    }
    if a == b {
        return true;
    }
    same_registrable_domain(a.host(), b.host())
}

/// The § 4.5.3 CORP check outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CorpOutcome {
    /// The resource policy allows the request.
    Allow,
    /// The resource policy blocks the request (a network error).
    Block,
}

/// The § 4.5.3 CORP check given the request origin, the resource origin,
/// and the parsed [`Corp`] value. An opaque origin (either side) is treated
/// as not same-origin / not same-site — only [`Corp::CrossOrigin`] allows
/// it.
pub fn check_corp(request: &Origin, resource: &Origin, corp: Corp) -> CorpOutcome {
    match corp {
        Corp::CrossOrigin => CorpOutcome::Allow,
        Corp::SameOrigin => {
            if same_origin_tuple(request, resource) {
                CorpOutcome::Allow
            } else {
                CorpOutcome::Block
            }
        }
        Corp::SameSite => {
            if is_same_site(request, resource) {
                CorpOutcome::Allow
            } else {
                CorpOutcome::Block
            }
        }
    }
}

/// `true` iff both origins are non-opaque tuple origins that compare equal
/// (scheme / host / port).
fn same_origin_tuple(a: &Origin, b: &Origin) -> bool {
    !a.is_opaque() && !b.is_opaque() && a == b
}

// ---------------------------------------------------------------------------
// the combined COEP + CORP gate
// ---------------------------------------------------------------------------

/// The combined COEP + CORP gate outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoepCorpOutcome {
    /// The response is allowed through (with credentials, as authored).
    Allow,
    /// The response is blocked (a network error the fetch layer surfaces).
    Block,
    /// The response is allowed but credentials are stripped (the
    /// `credentialless` cross-origin no-cors case).
    AllowWithoutCredentials,
}

/// The combined COEP + CORP gate the fetch layer consults before applying a
/// no-cors subresource response into a COEP-hardened document. `is_cors`
/// is `true` when the request mode is `cors` **and** the response passed
/// the CORS check (CORS is the alternative opt-in that bypasses CORP).
/// `corp` is `None` when the response carried no `Cross-Origin-Resource-
/// Policy` header.
///
/// ```text
/// unsafe-none        ⇒ Allow (no COEP enforcement)
/// is_cors            ⇒ Allow (CORS is an alternative opt-in)
/// require-corp:
///   same-origin      ⇒ Allow (the same-origin opt-in)
///   cross-origin:
///     corp present   ⇒ check_corp (Allow / Block)
///     corp absent    ⇒ Block
/// credentialless:
///   same-origin      ⇒ Allow
///   cross-origin     ⇒ AllowWithoutCredentials
/// ```
pub fn coep_corp_gate(
    coep: Coep,
    is_cors: bool,
    request: &Origin,
    resource: &Origin,
    corp: Option<Corp>,
) -> CoepCorpOutcome {
    if coep == Coep::UnsafeNone {
        return CoepCorpOutcome::Allow;
    }
    if is_cors {
        // CORS is the alternative opt-in; a CORS-successful response
        // bypasses the CORP requirement.
        return CoepCorpOutcome::Allow;
    }
    let same_origin = same_origin_tuple(request, resource);
    match coep {
        Coep::RequireCorp => {
            if same_origin {
                CoepCorpOutcome::Allow
            } else {
                match corp {
                    Some(c) => match check_corp(request, resource, c) {
                        CorpOutcome::Allow => CoepCorpOutcome::Allow,
                        CorpOutcome::Block => CoepCorpOutcome::Block,
                    },
                    None => CoepCorpOutcome::Block,
                }
            }
        }
        Coep::Credentialless => {
            if same_origin {
                CoepCorpOutcome::Allow
            } else {
                CoepCorpOutcome::AllowWithoutCredentials
            }
        }
        Coep::UnsafeNone => CoepCorpOutcome::Allow,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn origin(u: &str) -> Origin {
        Origin::from_url(&Url::parse(u).unwrap())
    }

    // --- parse -------------------------------------------------------

    #[test]
    fn parse_corp_keywords() {
        assert_eq!(Corp::parse("same-origin"), Some(Corp::SameOrigin));
        assert_eq!(Corp::parse("  SAME-SITE  "), Some(Corp::SameSite));
        assert_eq!(
            Corp::parse("cross-origin; report-to=\"grp\""),
            Some(Corp::CrossOrigin)
        );
        assert_eq!(Corp::parse("nonsense"), None);
        assert_eq!(Corp::parse(""), None);
    }

    #[test]
    fn corp_keyword_round_trip() {
        for c in [Corp::SameOrigin, Corp::SameSite, Corp::CrossOrigin] {
            assert_eq!(Corp::parse(c.keyword()), Some(c));
        }
    }

    // --- is_same_site ------------------------------------------------

    #[test]
    fn same_site_registrable_domain() {
        let a = origin("https://a.example.com/");
        let b = origin("https://b.example.com/");
        assert!(is_same_site(&a, &b), "same registrable domain ⇒ same-site");
    }

    #[test]
    fn same_site_requires_same_scheme() {
        let a = origin("http://a.example.com/");
        let b = origin("https://a.example.com/");
        assert!(!is_same_site(&a, &b), "different scheme ⇒ not same-site");
    }

    #[test]
    fn same_site_different_registrable_domain() {
        let a = origin("https://a.example.com/");
        let b = origin("https://a.other.test/");
        assert!(!is_same_site(&a, &b));
    }

    #[test]
    fn same_site_blocks_distinct_icann_and_private_suffix_registrants() {
        for (a, b) in [
            ("https://a.co.uk/", "https://b.co.uk/"),
            ("https://a.github.io/", "https://b.github.io/"),
        ] {
            assert!(!is_same_site(&origin(a), &origin(b)));
            assert_eq!(
                check_corp(&origin(a), &origin(b), Corp::SameSite),
                CorpOutcome::Block
            );
        }
    }

    #[test]
    fn same_site_fails_closed_for_localhost_and_ips_across_ports() {
        for (a, b) in [
            ("http://localhost:8000/", "http://localhost:8001/"),
            ("http://127.0.0.1:8000/", "http://127.0.0.1:8001/"),
        ] {
            assert!(!is_same_site(&origin(a), &origin(b)));
        }
    }

    #[test]
    fn same_site_two_label_host_matches_itself() {
        let a = origin("https://example.com/");
        let b = origin("https://example.com/");
        assert!(is_same_site(&a, &b));
    }

    #[test]
    fn opaque_origin_never_same_site() {
        let a = Origin::opaque();
        let b = origin("https://example.com/");
        assert!(!is_same_site(&a, &b));
        assert!(
            !is_same_site(&a, &a),
            "opaque is not same-site even with itself"
        );
    }

    // --- check_corp --------------------------------------------------

    #[test]
    fn corp_cross_origin_always_allows() {
        let a = origin("https://a.test/");
        let b = origin("https://b.test/");
        assert_eq!(check_corp(&a, &b, Corp::CrossOrigin), CorpOutcome::Allow);
    }

    #[test]
    fn corp_same_origin_blocks_cross_origin() {
        let a = origin("https://a.test/");
        let b = origin("https://b.test/");
        assert_eq!(check_corp(&a, &b, Corp::SameOrigin), CorpOutcome::Block);
        assert_eq!(check_corp(&a, &a, Corp::SameOrigin), CorpOutcome::Allow);
    }

    #[test]
    fn corp_same_site_allows_registrable_domain() {
        let a = origin("https://a.example.com/");
        let b = origin("https://b.example.com/");
        assert_eq!(check_corp(&a, &b, Corp::SameSite), CorpOutcome::Allow);
        let c = origin("https://other.test/");
        assert_eq!(check_corp(&a, &c, Corp::SameSite), CorpOutcome::Block);
    }

    #[test]
    fn corp_same_origin_opaque_blocks() {
        let a = Origin::opaque();
        let b = origin("https://b.test/");
        assert_eq!(check_corp(&a, &b, Corp::SameOrigin), CorpOutcome::Block);
    }

    // --- coep_corp_gate ----------------------------------------------

    #[test]
    fn gate_unsafe_none_always_allows() {
        let a = origin("https://a.test/");
        let b = origin("https://b.test/");
        assert_eq!(
            coep_corp_gate(Coep::UnsafeNone, false, &a, &b, None),
            CoepCorpOutcome::Allow
        );
    }

    #[test]
    fn gate_cors_bypasses_corp() {
        let a = origin("https://a.test/");
        let b = origin("https://b.test/");
        assert_eq!(
            coep_corp_gate(Coep::RequireCorp, true, &a, &b, None),
            CoepCorpOutcome::Allow,
            "CORS is the alternative opt-in"
        );
    }

    #[test]
    fn gate_require_corp_same_origin_allows_without_corp() {
        let a = origin("https://a.test/");
        assert_eq!(
            coep_corp_gate(Coep::RequireCorp, false, &a, &a, None),
            CoepCorpOutcome::Allow,
            "same-origin is the opt-in; CORP header not required"
        );
    }

    #[test]
    fn gate_require_corp_cross_origin_without_corp_blocks() {
        let a = origin("https://a.test/");
        let b = origin("https://b.test/");
        assert_eq!(
            coep_corp_gate(Coep::RequireCorp, false, &a, &b, None),
            CoepCorpOutcome::Block
        );
    }

    #[test]
    fn gate_require_corp_cross_origin_with_corp_cross_origin_allows() {
        let a = origin("https://a.test/");
        let b = origin("https://b.test/");
        assert_eq!(
            coep_corp_gate(Coep::RequireCorp, false, &a, &b, Some(Corp::CrossOrigin)),
            CoepCorpOutcome::Allow
        );
    }

    #[test]
    fn gate_require_corp_corp_same_origin_blocks_cross_origin() {
        let a = origin("https://a.test/");
        let b = origin("https://b.test/");
        assert_eq!(
            coep_corp_gate(Coep::RequireCorp, false, &a, &b, Some(Corp::SameOrigin)),
            CoepCorpOutcome::Block
        );
    }

    #[test]
    fn gate_credentialless_cross_origin_strips_credentials() {
        let a = origin("https://a.test/");
        let b = origin("https://b.test/");
        assert_eq!(
            coep_corp_gate(Coep::Credentialless, false, &a, &b, None),
            CoepCorpOutcome::AllowWithoutCredentials
        );
    }

    #[test]
    fn gate_credentialless_same_origin_allows() {
        let a = origin("https://a.test/");
        assert_eq!(
            coep_corp_gate(Coep::Credentialless, false, &a, &a, None),
            CoepCorpOutcome::Allow
        );
    }
}
