//! URL policy — SSRF / private-IP / reserved-TLD blocklist.
//!
//! Every network fetch passes through [`validate_http_url`]
//! (docs/ARCHITECTURE.md "Trust boundaries"). The blocklist is Vixen's
//! configuration of what counts as a "public" HTTP target. This module is
//! the reference implementation of the code in docs/SPEC.md "URL policy";
//! the public signatures are kept byte-identical to that listing.
//!
//! Fuzz target: `fuzz/url_policy_validate` (docs/PLAN.md Phase 1 gate).

use std::net::{Ipv4Addr, Ipv6Addr};
use url::{Host, Url};

/// Reason a URL was rejected by [`validate_http_url`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlPolicyError {
    #[error("unsupported scheme: {0}")]
    UnsupportedScheme(String),
    #[error("blocked host: {host}")]
    BlockedHost { host: String },
}

/// Validate that `url` is an HTTP(S) URL pointing at a public host.
///
/// Called at every fetch boundary (network entry, JS `fetch`/XHR). Every
/// branch **fails closed**: anything that is not clearly public HTTP(S) is
/// rejected (docs/SPEC.md "URL policy").
pub fn validate_http_url(url: &Url) -> Result<(), UrlPolicyError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlPolicyError::UnsupportedScheme(url.scheme().to_owned()));
    }
    if let Some(host) = url.host()
        && is_private_host(&host)
    {
        return Err(UrlPolicyError::BlockedHost {
            host: host.to_string(),
        });
    }
    Ok(())
}

/// True if `host` is private/loopback/reserved and must not be fetched.
///
/// Matches docs/SPEC.md verbatim. Note the CGNAT check is the *precise*
/// `100.64.0.0/10` range, not all of `100/8` — see the regression tests.
pub fn is_private_host(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(ip) => is_private_ipv4(*ip),
        Host::Ipv6(ip) => is_private_ipv6(*ip),
        Host::Domain(domain) => {
            let lower = domain.to_lowercase();
            lower == "localhost"
                || lower == "localhost.localdomain"
                || lower.ends_with(".local")
                || lower.ends_with(".internal")
                || lower.ends_with(".onion")
                || lower.ends_with(".arpa")
                || lower.ends_with(".test")
                || lower.ends_with(".example")
                || lower.ends_with(".invalid")
        }
    }
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private() // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local() // 169.254/16
        || ip.is_unspecified() // 0.0.0.0 (unspecified only)
        || ip.is_broadcast() // 255.255.255.255
        || ip.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || is_cgnat(ip) // 100.64.0.0/10
}

/// RFC 6598 carrier-grade NAT: `100.64.0.0/10` precisely.
fn is_cgnat(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (o[1] & 0xc0) == 0x40 // 100.64.0.0/10 precisely
}

fn is_private_ipv6(ip: Ipv6Addr) -> bool {
    ip.is_loopback() // ::1
        || ip.is_unspecified() // ::
        || ip.is_unique_local() // fc00::/7
        || (ip.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || ip.to_ipv4_mapped().is_some_and(is_private_ipv4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(s: &str) -> Host<&str> {
        // Construct a Host<&str> directly. Inputs are 'static literals or
        // loop-bound, so the borrowed domain outlives the call.
        if let Some(inner) = s.strip_prefix('[').and_then(|x| x.strip_suffix(']'))
            && let Ok(ip) = inner.parse::<Ipv6Addr>()
        {
            return Host::Ipv6(ip);
        }
        if let Ok(ip) = s.parse::<Ipv4Addr>() {
            return Host::Ipv4(ip);
        }
        Host::Domain(s)
    }

    /// Mandatory CGNAT regression test (docs/ACCEPTANCE.md "Networking").
    #[test]
    fn cgnat_boundary_is_precise() {
        assert!(is_private_host(&host("100.64.0.1")));
        assert!(!is_private_host(&host("100.128.0.1")));
        // Edges of the /10.
        assert!(is_private_host(&host("100.64.0.0")));
        assert!(is_private_host(&host("100.127.255.255")));
        assert!(!is_private_host(&host("100.63.255.255")));
        assert!(!is_private_host(&host("100.128.0.0")));
        // 100/8 outside the /10 stays public (the common bug).
        assert!(!is_private_host(&host("100.0.0.1")));
        assert!(!is_private_host(&host("100.255.255.255")));
    }

    #[test]
    fn ipv4_private_ranges_blocked() {
        for s in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "0.0.0.0",
            "255.255.255.255",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
        ] {
            assert!(is_private_host(&host(s)), "{s} should be private");
        }
    }

    #[test]
    fn ipv4_public_allowed() {
        for s in ["93.184.216.34", "8.8.8.8", "1.1.1.1"] {
            assert!(!is_private_host(&host(s)), "{s} should be public");
        }
    }

    #[test]
    fn ipv6_private_ranges_blocked() {
        assert!(is_private_host(&host("[::1]")));
        assert!(is_private_host(&host("[::]")));
        assert!(is_private_host(&host("[fe80::1]")));
        assert!(is_private_host(&host("[fc00::1]")));
        assert!(is_private_host(&host("[fd00::1]")));
    }

    #[test]
    fn reserved_domains_blocked() {
        for s in [
            "localhost",
            "localhost.localdomain",
            "foo.local",
            "a.b.internal",
            "x.onion",
            "y.arpa",
            "z.test",
            "z.example",
            "z.invalid",
        ] {
            assert!(is_private_host(&host(s)), "{s} should be blocked");
        }
    }

    #[test]
    fn public_domains_allowed() {
        for s in ["example.com", "sub.example.com", "vixen.org"] {
            assert!(!is_private_host(&host(s)), "{s} should be allowed");
        }
    }

    #[test]
    fn validate_rejects_non_http_schemes() {
        for u in [
            "file:///etc/passwd",
            "ftp://example.com/",
            "data:text/plain,hi",
        ] {
            let url = Url::parse(u).unwrap();
            assert!(
                matches!(
                    validate_http_url(&url),
                    Err(UrlPolicyError::UnsupportedScheme(_))
                ),
                "{u} should be rejected"
            );
        }
    }

    #[test]
    fn validate_accepts_public_https() {
        let url = Url::parse("https://example.com/path?q=1").unwrap();
        assert!(validate_http_url(&url).is_ok());
    }

    #[test]
    fn validate_blocks_private_via_url() {
        let url = Url::parse("http://127.0.0.1:8080/").unwrap();
        assert!(matches!(
            validate_http_url(&url),
            Err(UrlPolicyError::BlockedHost { .. })
        ));
    }
}
