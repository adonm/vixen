//! Content Security Policy — parser + enforcer.
//!
//! CSP is parsed from `Content-Security-Policy` headers and
//! `<meta http-equiv="Content-Security-Policy">` (docs/SPEC.md "CSP
//! enforcement points"). Enforcement happens at three boundaries:
//!
//! 1. **Script execution** — `script-src` (or `default-src` fallback).
//!    Inline scripts blocked unless `'unsafe-inline'` or a matching
//!    hash/nonce is present.
//! 2. **Fetch** — `connect-src`, `img-src`, `style-src`, `font-src`,
//!    `media-src`, `object-src`, etc. URLs matched against the source-list.
//! 3. **Plugin content** — `<embed>`/`<object>` gated by `object-src`.
//!
//! Source-list grammar follows the CSP Level 3 spec. Multiple policies
//! (multiple headers) are **intersected**: a request is allowed only if
//! *every* policy allows it. Each boundary fails closed.
//!
//! Fuzz target: `fuzz/csp_parse` (docs/PLAN.md Phase 1 gate).

use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha384, Sha512};
use url::Url;

use crate::origin::Origin;

/// A hash algorithm used in a CSP `'hash-...'` source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlg {
    Sha256,
    Sha384,
    Sha512,
}

/// A single CSP source expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    KeywordSelf,
    /// `'none'` — the only entry in a list meaning "deny all".
    KeywordNone,
    UnsafeInline,
    UnsafeEval,
    StrictDynamic,
    Nonce(String),
    Hash(HashAlg, String),
    /// `scheme:` source, e.g. `https:` or `data:`.
    Scheme(String),
    /// `host-source` with optional scheme, port, path.
    Host(HostSource),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSource {
    pub scheme: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub path: Option<String>,
}

/// One policy (one header's worth). Directives keyed by lower-cased name.
#[derive(Debug, Clone, Default)]
pub struct CspPolicy {
    pub directives: BTreeMap<String, Vec<Source>>,
}

/// A full CSP, possibly the intersection of several policies (one per
/// `Content-Security-Policy` header).
#[derive(Debug, Clone, Default)]
pub struct ContentSecurityPolicy {
    policies: Vec<CspPolicy>,
}

impl ContentSecurityPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse one `Content-Security-Policy` header value and add it as a new
    /// policy. Multiple calls (one per header) intersect.
    pub fn add_header(&mut self, value: &str) {
        self.policies.push(parse_policy(value));
    }

    /// Build a CSP from `(name, value)` header pairs. Non-CSP headers are
    /// ignored; report-only headers (`Content-Security-Policy-Report-Only`)
    /// are also ignored (report-only does not enforce).
    pub fn from_headers<'a, I>(headers: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut csp = Self::new();
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("content-security-policy") {
                csp.add_header(value);
            }
        }
        csp
    }

    /// True when there are no enforcing policies (i.e. nothing restricts).
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    /// The effective source-list for `directive` in a policy, falling back to
    /// `default-src` when the specific directive is absent. Returns `None`
    /// when neither is present (→ no restriction for this resource type).
    fn sources_for<'p>(&'p self, policy: &'p CspPolicy, directive: &str) -> Option<&'p [Source]> {
        if let Some(s) = policy.directives.get(directive) {
            return Some(s);
        }
        if directive != "default-src"
            && let Some(s) = policy.directives.get("default-src")
        {
            return Some(s);
        }
        None
    }

    /// Allow an inline script? `nonce` and the script `content` (for hash
    /// matching) are optional. Allowed iff every policy allows it.
    pub fn allows_inline_script(
        &self,
        doc_origin: &Origin,
        content: Option<&str>,
        nonce: Option<&str>,
    ) -> bool {
        self.policies.iter().all(|p| {
            let Some(srcs) = self.sources_for(p, "script-src") else {
                return true; // no restriction
            };
            inline_allowed(srcs, doc_origin, content, nonce)
        })
    }

    /// Allow an inline style? (Symmetric with scripts.)
    pub fn allows_inline_style(
        &self,
        doc_origin: &Origin,
        content: Option<&str>,
        nonce: Option<&str>,
    ) -> bool {
        self.policies.iter().all(|p| {
            let Some(srcs) = self.sources_for(p, "style-src") else {
                return true;
            };
            inline_allowed(srcs, doc_origin, content, nonce)
        })
    }

    /// Allow fetching `url` for a fetch directive
    /// (`connect-src`/`img-src`/`style-src`/`font-src`/`media-src`/
    /// `object-src`/`frame-src`/etc.)? Allowed iff every policy allows it.
    pub fn allows_fetch(&self, directive: &str, url: &Url, doc_origin: &Origin) -> bool {
        self.policies.iter().all(|p| {
            let Some(srcs) = self.sources_for(p, directive) else {
                return true;
            };
            url_allowed(srcs, url, doc_origin)
        })
    }

    /// Allow plugin content (`<embed>`/`<object>`)? Gated by `object-src`.
    pub fn allows_plugin(&self, url: &Url, doc_origin: &Origin) -> bool {
        self.allows_fetch("object-src", url, doc_origin)
    }
}

/// Inline-allowed per a single source-list.
fn inline_allowed(
    srcs: &[Source],
    _doc_origin: &Origin,
    content: Option<&str>,
    nonce: Option<&str>,
) -> bool {
    // A lone `'none'` denies everything.
    if srcs.len() == 1 && matches!(srcs[0], Source::KeywordNone) {
        return false;
    }
    for s in srcs {
        match s {
            Source::UnsafeInline => return true,
            Source::Nonce(n) if Some(n.as_str()) == nonce => return true,
            Source::Hash(alg, expected) => {
                if let Some(c) = content
                    && hash_matches(*alg, expected, c)
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// URL-allowed per a single source-list (fetch directives).
fn url_allowed(srcs: &[Source], url: &Url, doc_origin: &Origin) -> bool {
    if srcs.len() == 1 && matches!(srcs[0], Source::KeywordNone) {
        return false;
    }
    let mut has_positive = false;
    for s in srcs {
        if matches!(s, Source::KeywordNone) {
            continue;
        }
        has_positive = true;
        if url_matches_source(s, url, doc_origin) {
            return true;
        }
    }
    // A list with only keywords like unsafe-inline/unsafe-eval (which never
    // match a URL) denies the fetch.
    !has_positive
}

fn url_matches_source(s: &Source, url: &Url, doc_origin: &Origin) -> bool {
    match s {
        Source::KeywordSelf => url_origin_matches(url, doc_origin),
        Source::Scheme(scheme) => url
            .scheme()
            .eq_ignore_ascii_case(scheme.trim_end_matches(':')),
        Source::Host(h) => host_source_matches(h, url, doc_origin),
        // Nonces/hashes/unsafe-inline/unsafe-eval/strict-dynamic don't match
        // fetch URLs.
        _ => false,
    }
}

fn url_origin_matches(url: &Url, doc_origin: &Origin) -> bool {
    let o = Origin::from_url(url);
    !o.is_opaque() && o == *doc_origin
}

fn host_source_matches(h: &HostSource, url: &Url, _doc_origin: &Origin) -> bool {
    if let Some(scheme) = &h.scheme {
        if !url.scheme().eq_ignore_ascii_case(scheme) {
            return false;
        }
    } else {
        // Host source without a scheme: only matches secure-origin URLs when
        // itself... CSP: a host-source without scheme matches only http/https
        // and must not match unique origins. Keep it simple: require an http(s)
        // URL and fall through to host match.
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    let pat = h.host.to_ascii_lowercase();
    let host_ok = if pat == "*" {
        // CSP `*` matches any host with a (non-empty) host component.
        !host.is_empty()
    } else if let Some(suffix) = pat.strip_prefix("*.") {
        host == suffix || host.ends_with(&format!(".{suffix}"))
    } else {
        host == pat
    };
    if !host_ok {
        return false;
    }
    if let Some(port) = h.port {
        let url_port = url
            .port()
            .unwrap_or_else(|| if url.scheme() == "https" { 443 } else { 80 });
        if port != url_port {
            return false;
        }
    }
    if let Some(path) = &h.path
        && !url.path().starts_with(path.as_str())
    {
        return false;
    }
    true
}

/// Compare a CSP `'hash-...'` source against `content`. All three
/// algorithms (SHA-256/384/512) are computed via the vetted `sha2` crate
/// and compared in constant time against the base64-decoded expected bytes.
/// A mismatched expected-length (e.g. a truncated digest) is rejected by
/// `constant_time_eq`'s length check.
fn hash_matches(alg: HashAlg, expected: &str, content: &str) -> bool {
    let expected_bytes = base64_decode_or_empty(expected);
    match alg {
        HashAlg::Sha256 => constant_time_eq(
            Sha256::digest(content.as_bytes()).as_slice(),
            &expected_bytes,
        ),
        HashAlg::Sha384 => constant_time_eq(
            Sha384::digest(content.as_bytes()).as_slice(),
            &expected_bytes,
        ),
        HashAlg::Sha512 => constant_time_eq(
            Sha512::digest(content.as_bytes()).as_slice(),
            &expected_bytes,
        ),
    }
}

// --- parsing ----------------------------------------------------------------

/// Parse one policy (one header). Directives are `;`-separated; within a
/// directive, the first whitespace-delimited token is the name and the rest
/// are sources.
pub fn parse_policy(value: &str) -> CspPolicy {
    let mut policy = CspPolicy::default();
    for directive in value.split(';') {
        let mut tokens = directive.split_whitespace();
        let Some(name) = tokens.next() else { continue };
        let name = name.to_ascii_lowercase();
        let sources: Vec<Source> = tokens.map(parse_source).collect();
        // CSP: if a directive is duplicated within a policy, the first wins
        // (the rest ignored). We keep the first.
        policy.directives.entry(name).or_insert(sources);
    }
    policy
}

fn parse_source(tok: &str) -> Source {
    // Keyword matching is case-insensitive; nonce/hash *values* and
    // host-sources must preserve their original case (base64 is case-sensitive).
    match tok.to_ascii_lowercase().as_str() {
        "'self'" => return Source::KeywordSelf,
        "'none'" => return Source::KeywordNone,
        "'unsafe-inline'" => return Source::UnsafeInline,
        "'unsafe-eval'" => return Source::UnsafeEval,
        "'strict-dynamic'" => return Source::StrictDynamic,
        _ => {}
    }
    if let Some(mid) = tok
        .strip_prefix("'nonce-")
        .and_then(|s| s.strip_suffix('\''))
    {
        return Source::Nonce(mid.to_owned());
    }
    for (alg, prefix) in [
        (HashAlg::Sha256, "'sha256-"),
        (HashAlg::Sha384, "'sha384-"),
        (HashAlg::Sha512, "'sha512-"),
    ] {
        // Algorithm names are case-insensitive; match on the lowercased prefix.
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = lower
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix('\''))
        {
            // Preserve the original-case digest from `tok` (base64 is case-sensitive).
            let value = &tok[prefix.len()..tok.len() - 1];
            debug_assert_eq!(rest.to_ascii_lowercase(), value.to_ascii_lowercase());
            return Source::Hash(alg, value.to_owned());
        }
    }
    // scheme-source: "data:", "https:", "blob:", "filesystem:".
    let lower = tok.to_ascii_lowercase();
    if lower.ends_with(':') && !lower[..lower.len() - 1].contains(':') {
        let scheme = lower.trim_end_matches(':');
        if scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        {
            return Source::Scheme(scheme.to_owned());
        }
    }
    // host-source: best-effort parse of [scheme://]host[:port][/path].
    Source::Host(parse_host_source(tok))
}

fn parse_host_source(tok: &str) -> HostSource {
    let (scheme, rest) = match tok.split_once("://") {
        Some((s, r)) => (Some(s.to_ascii_lowercase()), r),
        None => (None, tok),
    };
    // Split host[:port] from path at the first '/'.
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, Some(format!("/{}", p))),
        None => (rest, None),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_ascii_lowercase(), p.parse::<u16>().ok())
        }
        _ => (authority.to_ascii_lowercase(), None),
    };
    HostSource {
        scheme,
        host,
        port,
        path,
    }
}

// --- base64 decode + constant-time compare for `'hash-...'` sources --------
// SHA-2 itself is provided by the vetted `sha2` crate above; the helpers
// below only decode the base64 source-value and compare digests in
// constant time.

/// Decode the base64 source-value of a `'hash-...'` source via the vetted
/// `base64` crate using the strict STANDARD engine (RFC 4648 § 4 standard
/// alphabet `A-Za-z0-9+/`). The STANDARD engine errors on non-alphabet bytes
/// *and* on ASCII whitespace — matching the historical CSP strictness, since
/// a garbage/whitespace-laced hash source is meaningless. Any decode error
/// maps to empty so `hash_matches`'s constant-time compare fails the length
/// check and never matches a real digest.
fn base64_decode_or_empty(s: &str) -> Vec<u8> {
    STANDARD.decode(s).unwrap_or_default()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod hash_known_answers {
    //! KATs for `hash_matches` against FIPS 180-4 known-answer vectors for
    //! SHA-256/384/512, covering both `""` and `"abc"`. The SHA-384 and
    //! SHA-512 cases would have been silently denied before the `sha2`
    //! rewrite (the old arms returned `false` unconditionally).
    use super::*;

    fn hex_to_base64(hex: &str) -> String {
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect::<Vec<_>>();
        super::tests::base64_encode(&bytes)
    }

    // FIPS 180-4 / NIST known-answer vectors.
    // SHA-256
    const SHA256_EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    // SHA-384
    const SHA384_EMPTY: &str = "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b";
    const SHA384_ABC: &str = "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7";
    // SHA-512
    const SHA512_EMPTY: &str = "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";
    const SHA512_ABC: &str = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";

    #[test]
    fn sha256_empty() {
        let b64 = hex_to_base64(SHA256_EMPTY);
        assert!(hash_matches(HashAlg::Sha256, &b64, ""));
        // Wrong content must NOT match.
        assert!(!hash_matches(HashAlg::Sha256, &b64, "abc"));
    }

    #[test]
    fn sha256_abc() {
        let b64 = hex_to_base64(SHA256_ABC);
        assert!(hash_matches(HashAlg::Sha256, &b64, "abc"));
        assert!(!hash_matches(HashAlg::Sha256, &b64, ""));
    }

    #[test]
    fn sha384_empty() {
        let b64 = hex_to_base64(SHA384_EMPTY);
        // Previously this returned `false` unconditionally.
        assert!(hash_matches(HashAlg::Sha384, &b64, ""));
        assert!(!hash_matches(HashAlg::Sha384, &b64, "abc"));
    }

    #[test]
    fn sha384_abc() {
        let b64 = hex_to_base64(SHA384_ABC);
        assert!(hash_matches(HashAlg::Sha384, &b64, "abc"));
        assert!(!hash_matches(HashAlg::Sha384, &b64, ""));
    }

    #[test]
    fn sha512_empty() {
        let b64 = hex_to_base64(SHA512_EMPTY);
        // Previously this returned `false` unconditionally.
        assert!(hash_matches(HashAlg::Sha512, &b64, ""));
        assert!(!hash_matches(HashAlg::Sha512, &b64, "abc"));
    }

    #[test]
    fn sha512_abc() {
        let b64 = hex_to_base64(SHA512_ABC);
        assert!(hash_matches(HashAlg::Sha512, &b64, "abc"));
        assert!(!hash_matches(HashAlg::Sha512, &b64, ""));
    }

    #[test]
    fn mismatched_length_rejected() {
        // SHA-256 digest is 32 bytes; feeding a SHA-384 (48-byte) expected
        // value to a SHA-256 source must fail the length check, not match.
        let b64_384 = hex_to_base64(SHA384_ABC);
        assert!(!hash_matches(HashAlg::Sha256, &b64_384, "abc"));
        // And vice versa.
        let b64_256 = hex_to_base64(SHA256_ABC);
        assert!(!hash_matches(HashAlg::Sha384, &b64_256, "abc"));
    }

    #[test]
    fn malformed_base64_rejected() {
        // Non-base64 garbage decodes to empty → never matches any digest.
        assert!(!hash_matches(HashAlg::Sha256, "!!!not-base64!!!", "abc"));
        assert!(!hash_matches(HashAlg::Sha384, "!!!not-base64!!!", "abc"));
        assert!(!hash_matches(HashAlg::Sha512, "!!!not-base64!!!", "abc"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_origin() -> Origin {
        Origin::from_url(&Url::parse("https://example.com").unwrap())
    }

    #[test]
    fn parse_basic_policy() {
        let p = parse_policy(
            "default-src 'self'; script-src 'self' 'unsafe-inline'; connect-src https://api.example.com",
        );
        assert_eq!(p.directives.len(), 3);
        assert_eq!(p.directives["default-src"], vec![Source::KeywordSelf]);
        assert_eq!(
            p.directives["script-src"],
            vec![Source::KeywordSelf, Source::UnsafeInline]
        );
        assert!(matches!(
            &p.directives["connect-src"][0],
            Source::Host(h) if h.host == "api.example.com"
        ));
    }

    #[test]
    fn inline_blocked_by_default_src_self() {
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header("default-src 'self'");
        assert!(!csp.allows_inline_script(&doc_origin(), Some("alert(1)"), None));
    }

    #[test]
    fn inline_allowed_with_unsafe_inline() {
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header("script-src 'self' 'unsafe-inline'");
        assert!(csp.allows_inline_script(&doc_origin(), Some("alert(1)"), None));
    }

    #[test]
    fn inline_allowed_with_matching_nonce() {
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header("script-src 'nonce-abc123'");
        assert!(csp.allows_inline_script(&doc_origin(), None, Some("abc123")));
        assert!(!csp.allows_inline_script(&doc_origin(), None, Some("wrong")));
    }

    #[test]
    fn inline_allowed_with_matching_sha256_hash() {
        // sha256("alert(1)") base64 — computed via `sha2` below.
        let content = "alert(1)";
        let digest = Sha256::digest(content.as_bytes());
        let b64 = base64_encode(&digest);
        let policy = format!("script-src 'sha256-{b64}'");
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header(&policy);
        assert!(csp.allows_inline_script(&doc_origin(), Some(content), None));
        assert!(!csp.allows_inline_script(&doc_origin(), Some("other"), None));
    }

    #[test]
    fn inline_allowed_with_matching_sha384_hash() {
        // sha384("alert(1)") — previously this was ALWAYS denied because
        // `hash_matches` returned `false` for the Sha384 arm.
        let content = "alert(1)";
        let digest = Sha384::digest(content.as_bytes());
        let b64 = base64_encode(&digest);
        let policy = format!("script-src 'sha384-{b64}'");
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header(&policy);
        assert!(csp.allows_inline_script(&doc_origin(), Some(content), None));
        assert!(!csp.allows_inline_script(&doc_origin(), Some("other"), None));
    }

    #[test]
    fn inline_allowed_with_matching_sha512_hash() {
        // sha512("alert(1)") — previously this was ALWAYS denied because
        // `hash_matches` returned `false` for the Sha512 arm.
        let content = "alert(1)";
        let digest = Sha512::digest(content.as_bytes());
        let b64 = base64_encode(&digest);
        let policy = format!("script-src 'sha512-{b64}'");
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header(&policy);
        assert!(csp.allows_inline_script(&doc_origin(), Some(content), None));
        assert!(!csp.allows_inline_script(&doc_origin(), Some("other"), None));
    }

    #[test]
    fn fetch_self_allows_same_origin() {
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header("default-src 'self'");
        assert!(csp.allows_fetch(
            "connect-src",
            &Url::parse("https://example.com/api").unwrap(),
            &doc_origin()
        ));
        assert!(!csp.allows_fetch(
            "connect-src",
            &Url::parse("https://evil.com/x").unwrap(),
            &doc_origin()
        ));
    }

    #[test]
    fn fetch_connect_src_falls_back_to_default_src() {
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header("default-src 'self'; img-src *");
        // connect-src absent → default-src 'self'.
        assert!(!csp.allows_fetch(
            "connect-src",
            &Url::parse("https://evil.com").unwrap(),
            &doc_origin()
        ));
        // img-src present → allows any host.
        assert!(csp.allows_fetch(
            "img-src",
            &Url::parse("https://cdn.example.net/i.png").unwrap(),
            &doc_origin()
        ));
    }

    #[test]
    fn none_blocks_everything() {
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header("default-src 'none'");
        assert!(!csp.allows_fetch(
            "connect-src",
            &Url::parse("https://example.com").unwrap(),
            &doc_origin()
        ));
        assert!(!csp.allows_inline_script(&doc_origin(), Some("x"), None));
    }

    #[test]
    fn multiple_policies_intersect() {
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header("script-src 'self'"); // policy A: no inline
        csp.add_header("script-src 'unsafe-inline'"); // policy B: inline ok
        // Intersection: A blocks → blocked.
        assert!(!csp.allows_inline_script(&doc_origin(), Some("x"), None));
    }

    #[test]
    fn scheme_source_matches() {
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header("img-src data: https:");
        assert!(csp.allows_fetch(
            "img-src",
            &Url::parse("data:image/png;base64,AAAA").unwrap(),
            &doc_origin()
        ));
        assert!(csp.allows_fetch(
            "img-src",
            &Url::parse("https://anywhere.example/x.png").unwrap(),
            &doc_origin()
        ));
        assert!(!csp.allows_fetch(
            "img-src",
            &Url::parse("http://insecure.example/x.png").unwrap(),
            &doc_origin()
        ));
    }

    #[test]
    fn host_source_wildcard() {
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header("connect-src https://*.example.com");
        assert!(csp.allows_fetch(
            "connect-src",
            &Url::parse("https://api.example.com/x").unwrap(),
            &doc_origin()
        ));
        assert!(csp.allows_fetch(
            "connect-src",
            &Url::parse("https://example.com").unwrap(),
            &doc_origin()
        ));
        assert!(!csp.allows_fetch(
            "connect-src",
            &Url::parse("https://notexample.com").unwrap(),
            &doc_origin()
        ));
    }

    #[test]
    fn plugin_gated_by_object_src() {
        let mut csp = ContentSecurityPolicy::new();
        csp.add_header("object-src 'none'");
        assert!(!csp.allows_plugin(
            &Url::parse("https://example.com/a.swf").unwrap(),
            &doc_origin()
        ));
        // With object-src absent and default-src 'self'.
        let mut csp2 = ContentSecurityPolicy::new();
        csp2.add_header("default-src 'self'");
        assert!(csp2.allows_plugin(
            &Url::parse("https://example.com/a.swf").unwrap(),
            &doc_origin()
        ));
    }

    #[test]
    fn report_only_header_ignored() {
        let csp = ContentSecurityPolicy::from_headers([
            ("content-security-policy-report-only", "default-src 'none'"),
            ("content-security-policy", "default-src 'self'"),
        ]);
        assert_eq!(csp.policies.len(), 1);
    }

    pub(super) fn base64_encode(bytes: &[u8]) -> String {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        let mut buf: u32 = 0;
        let mut bits = 0;
        for &b in bytes {
            buf = (buf << 8) | b as u32;
            bits += 8;
            while bits >= 6 {
                bits -= 6;
                out.push(TABLE[((buf >> bits) & 0x3f) as usize] as char);
            }
        }
        if bits > 0 {
            out.push(TABLE[((buf << (6 - bits)) & 0x3f) as usize] as char);
        }
        while !out.len().is_multiple_of(4) {
            out.push('=');
        }
        out
    }
}
