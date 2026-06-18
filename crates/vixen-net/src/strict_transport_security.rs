//! HTTP `Strict-Transport-Security` (HSTS) — header parser + cache-entry
//! derivation (Phase 7 security prep, pure logic). Implements RFC 6795 § 6.1's
//! response-header parsing so the network layer can fold an HSTS entry into
//! its host-keyed cache without waiting for the full TLS/network plumbing.
//!
//! What lives here:
//! - [`HstsDirective`] — the parsed `max-age` + `includeSubDomains` + `preload`
//!   flags. The cache stores this plus an expiry.
//! - [`parse_strict_transport_security`] — RFC 6795 § 6.1: the header is
//!   ignored unless a valid `max-age` is present; unknown directives are
//!   skipped; `max-age=0` parses (it's the cache-deletion signal).
//! - [`HstsEntry`] / [`HstsEntry::matches`] — the host-match logic (RFC 6795
//!   § 8.2 "URI Scheme": exact host or, with `includeSubDomains`, a subdomain).
//!
//! What does *not* live here:
//! - The cache itself (a `vixen-net` map keyed by host, expired on read).
//! - The preload list (shipped separately; the preload list is consulted
//!   before the cache lookup and synthesises an [`HstsEntry`] the same way).
//! - Clock + persistence (the caller stamps `received_at` and computes the
//!   expiry against `max-age`, then persists via `vixen-store`).
//!
//! RFC 6795 § 6.1 compliance notes:
//! - Header parsing is case-insensitive on directive *names* (`Max-Age` is
//!   accepted) and tolerant of whitespace around `;` and `=`, matching the
//!   `OWS` rule used by real-world servers.
//! - `max-age` with a negative value or non-digit value is ignored ⇒ the
//!   whole header is ignored (§ 6.1 step 5). We model that as `None`.
//! - A directive appearing twice ⇒ last wins (RFC 7231 § 3.2.2 practice;
//!   RFC 6795 itself is silent on duplicates).
//!
//! Reference: <https://www.rfc-editor.org/rfc/rfc6795> (§ 6.1 parsing,
//! § 8.2 superdomain match), HSTS preload list (<https://hstspreload.org/>).

#![forbid(unsafe_code)]

/// A parsed HSTS response header. `max_age_secs` is `Some` iff the header
/// carried a syntactically valid `max-age` directive (the RFC's prerequisite
/// for honouring the header at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HstsDirective {
    pub max_age_secs: u64,
    pub include_sub_domains: bool,
    /// The non-RFC `preload` hint (<https://hstspreload.org/>). Honoured by
    /// the preload submission tooling; the cache treats it as advisory only.
    pub preload: bool,
}

impl HstsDirective {
    /// `true` when `max-age` is zero — the RFC's cache-deletion signal
    /// (RFC 6795 § 6.1 step 6 / § 8.1). The caller removes the cache entry.
    pub fn is_zero(&self) -> bool {
        self.max_age_secs == 0
    }
}

/// Parse a `Strict-Transport-Security` header (RFC 6795 § 6.1). Returns
/// `None` when the header has no valid `max-age` (the RFC requires the header
/// be ignored in that case).
pub fn parse_strict_transport_security(header: &str) -> Option<HstsDirective> {
    let mut max_age: Option<u64> = None;
    let mut include_sub_domains = false;
    let mut preload = false;

    for raw in header.split(';') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        let (name, value) = match token.split_once('=') {
            Some((n, v)) => (n.trim(), Some(v.trim())),
            None => (token, None),
        };
        match name.to_ascii_lowercase().as_str() {
            "max-age" => {
                if let Some(v) = value
                    && let Ok(n) = v.parse::<u64>()
                {
                    max_age = Some(n);
                }
                // else: invalid max-age ⇒ leave max_age as-is; if the header
                // has no other valid max-age, the header is ignored (None).
            }
            "includesubdomains" => {
                include_sub_domains = true;
            }
            "preload" => {
                preload = true;
            }
            _ => {} // unknown directives ignored (RFC 6795 § 6.1).
        }
    }

    let max_age_secs = max_age?;
    Some(HstsDirective {
        max_age_secs,
        include_sub_domains,
        preload,
    })
}

/// A cached HSTS entry. The caller stores `HstsDirective` plus the expiry it
/// computes from `received_at + max_age`; this type is the match surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HstsEntry {
    /// Lowercased host the entry was learned from (hostnames are
    /// case-insensitive; RFC 6795 § 8.2).
    pub host: String,
    pub include_sub_domains: bool,
}

impl HstsEntry {
    /// Construct an entry from a directive + the host it was received on.
    pub fn from_directive(host: impl Into<String>, d: &HstsDirective) -> Self {
        Self {
            host: host.into().to_lowercase(),
            include_sub_domains: d.include_sub_domains,
        }
    }

    /// RFC 6795 § 8.2 host match. The entry matches:
    /// - exactly, host-equal (case-insensitive); or
    /// - a subdomain of `host`, iff `include_sub_domains`.
    ///
    /// `example.com` does *not* match an entry for `sub.example.com` — the
    /// superdomain rule goes one way (entry host is the suffix).
    pub fn matches(&self, candidate: &str) -> bool {
        let candidate = candidate.to_lowercase();
        if candidate == self.host {
            return true;
        }
        if self.include_sub_domains {
            // `candidate` must be `<something>.<host>` — the dot prevents
            // `evil-example.com` matching `example.com`.
            return candidate.ends_with(&format!(".{}", self.host));
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Parsing -------------------------------------------------------

    #[test]
    fn parse_max_age_only() {
        let d = parse_strict_transport_security("max-age=31536000").unwrap();
        assert_eq!(d.max_age_secs, 31536000);
        assert!(!d.include_sub_domains);
        assert!(!d.preload);
        assert!(!d.is_zero());
    }

    #[test]
    fn parse_all_three_directives() {
        let d = parse_strict_transport_security("max-age=31536000; includeSubDomains; preload")
            .unwrap();
        assert_eq!(d.max_age_secs, 31536000);
        assert!(d.include_sub_domains);
        assert!(d.preload);
    }

    #[test]
    fn parse_case_insensitive_names() {
        // RFC 6795: directive names are case-insensitive in practice; real
        // servers emit `Max-Age` and `IncludeSubDomains`.
        let d = parse_strict_transport_security("Max-Age=3600; IncludeSubDomains").unwrap();
        assert_eq!(d.max_age_secs, 3600);
        assert!(d.include_sub_domains);
    }

    #[test]
    fn parse_tolerates_whitespace() {
        let d = parse_strict_transport_security("  max-age = 60 ;  includeSubDomains  ").unwrap();
        assert_eq!(d.max_age_secs, 60);
        assert!(d.include_sub_domains);
    }

    #[test]
    fn parse_zero_max_age_is_cache_deletion_signal() {
        let d = parse_strict_transport_security("max-age=0").unwrap();
        assert_eq!(d.max_age_secs, 0);
        assert!(d.is_zero());
    }

    #[test]
    fn parse_ignores_unknown_directives() {
        let d = parse_strict_transport_security("max-age=60; madeUp=true; also-unknown").unwrap();
        assert_eq!(d.max_age_secs, 60);
    }

    #[test]
    fn parse_last_max_age_wins() {
        let d = parse_strict_transport_security("max-age=10; max-age=999").unwrap();
        assert_eq!(d.max_age_secs, 999);
    }

    #[test]
    fn parse_rejects_header_without_max_age() {
        // includeSubDomains alone is not enough (RFC 6795 § 6.1: the header
        // is ignored without a valid max-age).
        assert!(parse_strict_transport_security("includeSubDomains").is_none());
        assert!(parse_strict_transport_security("").is_none());
    }

    #[test]
    fn parse_rejects_invalid_max_age_value() {
        // Negative / non-numeric max-age ⇒ directive ignored ⇒ no valid
        // max-age ⇒ whole header ignored (None).
        assert!(parse_strict_transport_security("max-age=-5").is_none());
        assert!(parse_strict_transport_security("max-age=abc").is_none());
    }

    // --- Host matching (RFC 6795 § 8.2) --------------------------------

    #[test]
    fn matches_exact_host_case_insensitively() {
        let e = HstsEntry::from_directive(
            "Example.COM",
            &HstsDirective {
                max_age_secs: 1,
                include_sub_domains: false,
                preload: false,
            },
        );
        assert!(e.matches("example.com"));
        assert!(e.matches("EXAMPLE.com"));
        assert!(!e.matches("sub.example.com"));
    }

    #[test]
    fn matches_subdomain_only_with_include_subdomains() {
        let e = HstsEntry::from_directive(
            "example.com",
            &HstsDirective {
                max_age_secs: 1,
                include_sub_domains: true,
                preload: false,
            },
        );
        assert!(e.matches("example.com"));
        assert!(e.matches("www.example.com"));
        assert!(e.matches("a.b.example.com"));
    }

    #[test]
    fn does_not_match_sibling_suffix_without_dot() {
        // The dot rule prevents `evil-example.com` matching `example.com`.
        let e = HstsEntry::from_directive(
            "example.com",
            &HstsDirective {
                max_age_secs: 1,
                include_sub_domains: true,
                preload: false,
            },
        );
        assert!(!e.matches("notexample.com"));
        assert!(!e.matches("evil.example.com.org"));
    }

    #[test]
    fn superdomain_does_not_match_subdomain_entry() {
        // RFC 6795 § 8.2: the superdomain rule is one-way. An entry learned
        // for `sub.example.com` does NOT cover `example.com`.
        let e = HstsEntry::from_directive(
            "sub.example.com",
            &HstsDirective {
                max_age_secs: 1,
                include_sub_domains: true,
                preload: false,
            },
        );
        assert!(!e.matches("example.com"));
        assert!(e.matches("sub.example.com"));
        assert!(e.matches("deep.sub.example.com"));
    }
}
