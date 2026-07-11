//! Shared DNS site and cookie-domain policy backed by the static Mozilla PSL.

use url::Host;

pub(crate) enum CookieDomain {
    HostOnly(String),
    Domain(String),
}

impl CookieDomain {
    pub(crate) fn into_parts(self) -> (String, bool) {
        match self {
            Self::HostOnly(domain) => (domain, true),
            Self::Domain(domain) => (domain, false),
        }
    }
}

/// Compare DNS hosts by registrable domain. IP literals, single-label names,
/// and names for which the PSL cannot establish a registrable domain fail
/// closed.
pub(crate) fn same_registrable_domain(a: &str, b: &str) -> bool {
    let Some(a) = registrable_domain(a) else {
        return false;
    };
    let Some(b) = registrable_domain(b) else {
        return false;
    };
    a.eq_ignore_ascii_case(&b)
}

fn registrable_domain(host: &str) -> Option<String> {
    let Host::Domain(domain) = Host::parse(host).ok()? else {
        return None;
    };
    let domain = dns_name_for_psl(&domain)?;
    psl::domain_str(domain).map(str::to_ascii_lowercase)
}

fn dns_name_for_psl(domain: &str) -> Option<&str> {
    let domain = domain.trim_end_matches('.');
    (!domain.is_empty()).then_some(domain)
}

fn is_public_suffix(host: &Host<String>) -> bool {
    let Host::Domain(domain) = host else {
        return false;
    };
    let Some(domain) = dns_name_for_psl(domain) else {
        return false;
    };
    psl::suffix_str(domain).is_some_and(|suffix| suffix.eq_ignore_ascii_case(domain))
}

/// Apply RFC 6265bis public-suffix and domain-match validation. An exact-host
/// public suffix is accepted only as a host-only cookie; a parent public suffix
/// is rejected.
pub(crate) fn cookie_domain(request_host: &Host<&str>, attribute: &str) -> Option<CookieDomain> {
    let attribute = Host::parse(attribute).ok()?;
    let request_host = request_host.to_owned();

    if is_public_suffix(&attribute) {
        return (request_host == attribute)
            .then(|| CookieDomain::HostOnly(request_host.to_string()));
    }
    if !cookie_domain_matches_host(&request_host, false, &attribute) {
        return None;
    }
    Some(CookieDomain::Domain(attribute.to_string()))
}

pub(crate) fn cookie_domain_matches(
    request_host: &Host<&str>,
    host_only: bool,
    domain: &str,
) -> bool {
    let Ok(domain) = Host::parse(domain) else {
        return false;
    };
    cookie_domain_matches_host(&request_host.to_owned(), host_only, &domain)
}

fn cookie_domain_matches_host(
    request_host: &Host<String>,
    host_only: bool,
    domain: &Host<String>,
) -> bool {
    if request_host == domain {
        return true;
    }
    if host_only {
        return false;
    }
    let (Host::Domain(host), Host::Domain(domain)) = (request_host, domain) else {
        return false;
    };
    host.strip_suffix(domain)
        .is_some_and(|prefix| prefix.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrable_domains_cover_icann_private_and_ordinary_suffixes() {
        assert!(!same_registrable_domain("a.co.uk", "b.co.uk"));
        assert!(same_registrable_domain("www.a.co.uk", "api.a.co.uk"));
        assert!(!same_registrable_domain("a.github.io", "b.github.io"));
        assert!(same_registrable_domain(
            "www.a.github.io",
            "api.a.github.io"
        ));
        assert!(same_registrable_domain(
            "www.example.com",
            "api.example.com"
        ));
    }

    #[test]
    fn hosts_without_registrable_domains_fail_closed() {
        assert!(!same_registrable_domain("localhost", "localhost"));
        assert!(!same_registrable_domain("127.0.0.1", "127.0.0.1"));
        assert!(!same_registrable_domain("[::1]", "[::1]"));
    }

    #[test]
    fn dns_comparison_uses_url_host_normalization() {
        assert!(same_registrable_domain(
            "WWW.Example.COM.",
            "api.example.com"
        ));
        assert!(same_registrable_domain(
            "www.bücher.example",
            "api.xn--bcher-kva.example"
        ));
    }
}
