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

/// Compare a CSP `'hash-...'` source against `content`. SHA-256 is
/// implemented here (compact, dependency-free); SHA-384/512 are wired to a
/// system SHA-2 at the Phase 7 script-execution boundary, where hashing
/// actually runs on script bytes. Until then 384/512 sources fail closed.
fn hash_matches(alg: HashAlg, expected: &str, content: &str) -> bool {
    let expected_bytes = base64_decode_or_empty(expected);
    match alg {
        HashAlg::Sha256 => {
            let d = sha256(content);
            constant_time_eq(&d, &expected_bytes)
        }
        HashAlg::Sha384 | HashAlg::Sha512 => false,
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

// --- minimal SHA-256 + base64 (so csp sha256 hash matching is real) ---------
// Compact, dependency-free SHA-256 used only to verify CSP `'sha256-'` hash
// sources. The script-execution boundary (Phase 7) reuses it on real bytes.

fn base64_decode_or_empty(s: &str) -> Vec<u8> {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0;
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        let Some(idx) = TABLE.iter().position(|&t| t == c) else {
            return Vec::new();
        };
        buf = (buf << 6) | idx as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
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

/// FIPS 180-4 SHA-256 over the UTF-8 bytes of `msg`.
fn sha256(msg: &str) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bytes = msg.as_bytes();
    let bit_len = (bytes.len() as u64) * 8;
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in padded.chunks(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod sha256_known_answers {
    use super::sha256;

    #[test]
    fn empty_string() {
        // sha256("") = e3b0c44298fc1c149afbf4c8996fb924...
        let d = sha256("");
        let hex: String = d.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn abc() {
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let d = sha256("abc");
        let hex: String = d.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
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
        // sha256("alert(1)") base64 — computed below.
        let content = "alert(1)";
        let digest = sha256(content);
        let b64 = base64_encode(&digest);
        let policy = format!("script-src 'sha256-{b64}'");
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

    fn base64_encode(bytes: &[u8]) -> String {
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
