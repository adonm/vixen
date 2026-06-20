//! `Referrer-Policy` — parser + resolver for the outgoing `Referer` header
//! (Phase 7 security prep, pure logic). Implements Fetch § 3.4 "Referrer
//! policy" + § 4.3.7 "Determine request's Referrer", which together decide
//! what `Referer` value Vixen attaches to every outgoing fetch, script-initiated
//! or browser-initiated.
//!
//! What lives here:
//! - [`ReferrerPolicy`] — every policy token Fetch § 3.4 defines.
//! - [`parse_referrer_policy`] — parse a `Referrer-Policy` response header.
//!   Multiple tokens ⇒ the *last* known one wins (Fetch § 3.4.1); unknown
//!   tokens are ignored; an empty/unknown header yields `None` (caller uses
//!   its default, [`ReferrerPolicy::default`] = `strict-origin-when-cross-origin`).
//! - [`resolve_referrer`] — apply the policy to a (source, destination) pair,
//!   returning [`ReferrerValue::None`] / [`ReferrerValue::Origin`] /
//!   [`ReferrerValue::FullUrl`].
//! - [`is_potentially_trustworthy`] — the secure-context test the downgrade
//!   rules reduce to (Fetch § 4.3, mixed-content reasoning).
//!
//! What does *not* live here:
//! - The actual header injection (that's `network::Network`).
//! - Per-element `referrerpolicy=""` attribute resolution (host-hook layer).
//! - Non-network referrer leakage (`window.opener`, link decoration) — the
//!   Fetch `Referrer-Policy` surface only governs the `Referer` *header*.
//!
//! Origin serialization follows RFC 6454 (scheme/host/port), with the default
//! port elided. "Full URL" serializes with credentials and fragment stripped
//! (Fetch § 4.3.7 step 6). "Downgrade" = source is potentially trustworthy
//! and destination is not.
//!
//! Reference: <https://fetch.spec.whatwg.org/#referrer-policy>,
//! § 4.3.7 "Determine request's Referrer".

#![forbid(unsafe_code)]

use url::Url;

/// Fetch § 3.4. Every policy token the spec defines, plus [`Self::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReferrerPolicy {
    NoReferrer,
    NoReferrerWhenDowngrade,
    SameOrigin,
    Origin,
    OriginWhenCrossOrigin,
    UnsafeUrl,
    StrictOrigin,
    /// The modern browser default (Chrome 85+, Firefox 87+).
    #[default]
    StrictOriginWhenCrossOrigin,
}

impl ReferrerPolicy {
    /// Parse a single policy token to a policy; unknown tokens return `None`.
    fn parse_token(token: &str) -> Option<ReferrerPolicy> {
        Some(match token.trim() {
            "no-referrer" => ReferrerPolicy::NoReferrer,
            "no-referrer-when-downgrade" => ReferrerPolicy::NoReferrerWhenDowngrade,
            "same-origin" => ReferrerPolicy::SameOrigin,
            "origin" => ReferrerPolicy::Origin,
            "origin-when-cross-origin" => ReferrerPolicy::OriginWhenCrossOrigin,
            "unsafe-url" => ReferrerPolicy::UnsafeUrl,
            "strict-origin" => ReferrerPolicy::StrictOrigin,
            "strict-origin-when-cross-origin" => ReferrerPolicy::StrictOriginWhenCrossOrigin,
            _ => return None,
        })
    }
}

/// What `Referer` value a request carries under a given policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferrerValue {
    /// No `Referer` header.
    None,
    /// The origin string (`scheme://host[:port]`).
    Origin(String),
    /// The full URL (credentials + fragment stripped).
    FullUrl(String),
}

/// Parse a `Referrer-Policy` header value. Tokens are comma- and/or
/// whitespace-separated; per Fetch § 3.4.1 the last-known directive wins.
/// Returns `None` when no token parses (caller falls back to the default).
pub fn parse_referrer_policy(header: &str) -> Option<ReferrerPolicy> {
    header
        .split([',', ' ', '\t', '\n', '\r'])
        .filter(|t| !t.is_empty())
        .filter_map(ReferrerPolicy::parse_token)
        .next_back()
}

/// Resolve the outgoing `Referer` for a request from `source` to `dest`
/// under `policy`. `source` is the document/environment that initiated the
/// fetch; `dest` is the request URL.
pub fn resolve_referrer(policy: ReferrerPolicy, source: &Url, dest: &Url) -> ReferrerValue {
    use ReferrerPolicy as P;
    let src_trustworthy = is_potentially_trustworthy(source);
    let dest_trustworthy = is_potentially_trustworthy(dest);
    let downgrade = src_trustworthy && !dest_trustworthy;
    let same_origin = source.origin() == dest.origin();

    match policy {
        P::NoReferrer => ReferrerValue::None,
        P::UnsafeUrl => full_url(source),
        // Downgrade ⇒ no referrer; otherwise full URL.
        P::NoReferrerWhenDowngrade => {
            if downgrade {
                ReferrerValue::None
            } else {
                full_url(source)
            }
        }
        // Same-origin only; cross-origin ⇒ nothing.
        P::SameOrigin => {
            if same_origin {
                full_url(source)
            } else {
                ReferrerValue::None
            }
        }
        // Always origin, but downgrade ⇒ none.
        P::StrictOrigin => {
            if downgrade {
                ReferrerValue::None
            } else {
                origin_string(source)
            }
        }
        // Always origin.
        P::Origin => origin_string(source),
        // Same-origin ⇒ full; cross-origin ⇒ origin.
        P::OriginWhenCrossOrigin => {
            if same_origin {
                full_url(source)
            } else {
                origin_string(source)
            }
        }
        // Same-origin ⇒ full; cross-origin equal-security ⇒ origin;
        // cross-origin downgrade ⇒ none. (The modern default.)
        P::StrictOriginWhenCrossOrigin => {
            if same_origin {
                full_url(source)
            } else if downgrade {
                ReferrerValue::None
            } else {
                origin_string(source)
            }
        }
    }
}

/// The "potentially trustworthy origin" test (Fetch § 4.3 + WPSA). `https`/
/// `wss` are trustworthy; `file:` is too (local file); `localhost` and loopback
/// are; everything else is not. Used for the downgrade + mixed-content rules.
pub fn is_potentially_trustworthy(url: &Url) -> bool {
    match url.scheme() {
        "https" | "wss" | "file" => true,
        "http" | "ws" => is_local_trustworthy(url),
        _ => false,
    }
}

/// `localhost`, `*.localhost`, and loopback IPs count as trustworthy for the
/// purpose of treating `http://localhost` as a secure context (WPSA § 3.2).
fn is_local_trustworthy(url: &Url) -> bool {
    match url.host() {
        Some(host) => match host {
            url::Host::Domain(d) => {
                let d = d.to_lowercase();
                d == "localhost" || d.ends_with(".localhost")
            }
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
        },
        None => false,
    }
}

/// Serialize a URL as an origin string: `scheme://host[:port]` (RFC 6454).
fn origin_string(url: &Url) -> ReferrerValue {
    ReferrerValue::Origin(url.origin().ascii_serialization())
}

/// Serialize a URL for the `Referer` header: strip credentials + fragment
/// (Fetch § 4.3.7 step 6). The path is otherwise preserved verbatim — the
/// spec does not call for trailing-slash trimming, so `https://a.test/p/`
/// serializes as `https://a.test/p/`.
fn full_url(url: &Url) -> ReferrerValue {
    let mut u = url.clone();
    u.set_fragment(None);
    u.set_username("").ok();
    // `set_password(None)` removes the password; ignore the result (it errors
    // only on cannot-have-a-host URLs, which we don't encounter here).
    u.set_password(None).ok();
    ReferrerValue::FullUrl(u.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    // --- Parsing -------------------------------------------------------

    #[test]
    fn parse_single_token() {
        assert_eq!(
            parse_referrer_policy("no-referrer"),
            Some(ReferrerPolicy::NoReferrer)
        );
        assert_eq!(
            parse_referrer_policy("  unsafe-url  "),
            Some(ReferrerPolicy::UnsafeUrl)
        );
    }

    #[test]
    fn parse_last_known_token_wins() {
        // Fetch § 3.4.1: last valid directive wins; unknowns skipped.
        assert_eq!(
            parse_referrer_policy("no-referrer, origin"),
            Some(ReferrerPolicy::Origin)
        );
        assert_eq!(
            parse_referrer_policy("garbage, same-origin, more-garbage"),
            Some(ReferrerPolicy::SameOrigin)
        );
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert_eq!(parse_referrer_policy(""), None);
        assert_eq!(parse_referrer_policy("nonsense"), None);
        assert_eq!(parse_referrer_policy(",,,"), None);
    }

    #[test]
    fn parse_accepts_whitespace_and_comma_separators() {
        assert_eq!(
            parse_referrer_policy("no-referrer origin"),
            Some(ReferrerPolicy::Origin)
        );
        assert_eq!(
            parse_referrer_policy("no-referrer\torigin"),
            Some(ReferrerPolicy::Origin)
        );
    }

    #[test]
    fn default_is_strict_origin_when_cross_origin() {
        assert_eq!(
            ReferrerPolicy::default(),
            ReferrerPolicy::StrictOriginWhenCrossOrigin
        );
    }

    // --- Resolution: no-referrer / unsafe-url --------------------------

    #[test]
    fn no_referrer_sends_nothing() {
        let r = resolve_referrer(
            ReferrerPolicy::NoReferrer,
            &url("https://a.test/page"),
            &url("https://b.test/"),
        );
        assert_eq!(r, ReferrerValue::None);
    }

    #[test]
    fn unsafe_url_always_full() {
        let r = resolve_referrer(
            ReferrerPolicy::UnsafeUrl,
            &url("https://a.test/page?q=1"),
            &url("http://b.test/"),
        );
        let ReferrerValue::FullUrl(s) = r else {
            panic!("expected full url, got {r:?}");
        };
        assert!(s.starts_with("https://a.test/page"));
    }

    // --- Downgrade (https → http) --------------------------------------

    #[test]
    fn no_referrer_when_downgrade_blocks_downgrade() {
        let r = resolve_referrer(
            ReferrerPolicy::NoReferrerWhenDowngrade,
            &url("https://a.test/"),
            &url("http://b.test/"),
        );
        assert_eq!(r, ReferrerValue::None);
    }

    #[test]
    fn no_referrer_when_downgrade_keeps_equal_security() {
        // https → https: full url.
        let r = resolve_referrer(
            ReferrerPolicy::NoReferrerWhenDowngrade,
            &url("https://a.test/p"),
            &url("https://b.test/"),
        );
        assert!(matches!(r, ReferrerValue::FullUrl(_)));
        // http → http: also full (no downgrade from an already-untrustworthy origin).
        let r = resolve_referrer(
            ReferrerPolicy::NoReferrerWhenDowngrade,
            &url("http://a.test/p"),
            &url("http://b.test/"),
        );
        assert!(matches!(r, ReferrerValue::FullUrl(_)));
    }

    // --- Same-origin ---------------------------------------------------

    #[test]
    fn same_origin_blocks_cross_origin() {
        let r = resolve_referrer(
            ReferrerPolicy::SameOrigin,
            &url("https://a.test/"),
            &url("https://b.test/"),
        );
        assert_eq!(r, ReferrerValue::None);
    }

    #[test]
    fn same_origin_keeps_same_origin() {
        let r = resolve_referrer(
            ReferrerPolicy::SameOrigin,
            &url("https://a.test/p1"),
            &url("https://a.test/p2"),
        );
        assert!(matches!(r, ReferrerValue::FullUrl(_)));
    }

    // --- Origin flavours -----------------------------------------------

    #[test]
    fn origin_policy_sends_origin_only() {
        let r = resolve_referrer(
            ReferrerPolicy::Origin,
            &url("https://a.test/p?q=1"),
            &url("https://b.test/"),
        );
        assert_eq!(r, ReferrerValue::Origin("https://a.test".to_owned()));
    }

    #[test]
    fn origin_when_cross_origin_branches() {
        // Same-origin → full.
        let r = resolve_referrer(
            ReferrerPolicy::OriginWhenCrossOrigin,
            &url("https://a.test/p"),
            &url("https://a.test/q"),
        );
        assert!(matches!(r, ReferrerValue::FullUrl(_)));
        // Cross-origin → origin.
        let r = resolve_referrer(
            ReferrerPolicy::OriginWhenCrossOrigin,
            &url("https://a.test/p"),
            &url("https://b.test/"),
        );
        assert_eq!(r, ReferrerValue::Origin("https://a.test".to_owned()));
    }

    #[test]
    fn strict_origin_blocks_downgrade() {
        // https→http downgrade ⇒ none.
        let r = resolve_referrer(
            ReferrerPolicy::StrictOrigin,
            &url("https://a.test/"),
            &url("http://b.test/"),
        );
        assert_eq!(r, ReferrerValue::None);
        // https→https ⇒ origin.
        let r = resolve_referrer(
            ReferrerPolicy::StrictOrigin,
            &url("https://a.test/"),
            &url("https://b.test/"),
        );
        assert_eq!(r, ReferrerValue::Origin("https://a.test".to_owned()));
    }

    // --- The default: strict-origin-when-cross-origin ------------------

    #[test]
    fn sowco_default_three_branches() {
        let p = ReferrerPolicy::StrictOriginWhenCrossOrigin;
        // same-origin ⇒ full.
        assert!(matches!(
            resolve_referrer(p, &url("https://a.test/p"), &url("https://a.test/q")),
            ReferrerValue::FullUrl(_)
        ));
        // cross-origin equal-security ⇒ origin.
        assert_eq!(
            resolve_referrer(p, &url("https://a.test/"), &url("https://b.test/")),
            ReferrerValue::Origin("https://a.test".to_owned())
        );
        // cross-origin downgrade ⇒ none.
        assert_eq!(
            resolve_referrer(p, &url("https://a.test/"), &url("http://b.test/")),
            ReferrerValue::None
        );
    }

    // --- Trustworthiness -----------------------------------------------

    #[test]
    fn trustworthy_classification() {
        assert!(is_potentially_trustworthy(&url("https://a.test/")));
        assert!(is_potentially_trustworthy(&url("wss://a.test/")));
        assert!(is_potentially_trustworthy(&url("file:///etc/passwd")));
        // Localhost http is trustworthy (treated as secure context).
        assert!(is_potentially_trustworthy(&url("http://localhost/")));
        assert!(is_potentially_trustworthy(&url("http://127.0.0.1/")));
        assert!(is_potentially_trustworthy(&url("http://[::1]/")));
        // Plain http to a public host is not.
        assert!(!is_potentially_trustworthy(&url("http://a.test/")));
        assert!(!is_potentially_trustworthy(&url("ws://a.test/")));
    }

    // --- Full-URL sanitisation -----------------------------------------

    #[test]
    fn full_url_strips_fragment_and_credentials() {
        let r = resolve_referrer(
            ReferrerPolicy::UnsafeUrl,
            &url("https://user:pass@a.test/p?q=1#frag"),
            &url("https://b.test/"),
        );
        let ReferrerValue::FullUrl(s) = r else {
            panic!("expected full url");
        };
        assert!(!s.contains("user"), "{s}");
        assert!(!s.contains("pass"), "{s}");
        assert!(!s.contains('#'), "{s}");
        assert!(s.contains("/p?q=1"), "{s}");
    }

    #[test]
    fn full_url_preserves_trailing_path_slash() {
        // Fetch § 4.3.7 step 6 strips only credentials + fragment; it does
        // not trim the path. So a directory URL keeps its trailing slash.
        let r = resolve_referrer(
            ReferrerPolicy::UnsafeUrl,
            &url("https://a.test/p/"),
            &url("https://b.test/"),
        );
        assert_eq!(
            r,
            ReferrerValue::FullUrl("https://a.test/p/".to_owned())
        );
    }
}
