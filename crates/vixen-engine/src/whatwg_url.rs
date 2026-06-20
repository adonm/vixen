//! WHATWG URL Standard — the URL parse + serialize + relative-resolution
//! model the fetch / navigation / `new URL()` host hooks consult (Phase 6
//! prep). Pure over the input string; the IDNA / full IPv6 / opaque-path
//! edge cases are the deferred slices documented below.
//!
//! What lives here:
//! - [`Url`] — the parsed URL components: `scheme` / `username` /
//!   `password` / `host` / `port` / `path` / `query` / `fragment`.
//! - [`SPECIAL_SCHEMES`] + [`is_special_scheme`] + [`default_port`] — the
//!   § 3.1 "special-scheme" family (`http` / `https` / `ws` / `wss` /
//!   `file`) the parser treats with the `//` authority expectation + the
//!   default-port fill-in.
//! - [`parse`] — parse an absolute URL string into a [`Url`].
//! - [`parse_with_base`] — the § 4.6 relative-resolution parser (a relative
//!   reference resolved against a base [`Url`]).
//! - [`Url::serialize`] — the § 4.1 canonical serialiser.
//! - [`Url::origin`] — the § 4.5 `(scheme, host, port)` origin tuple (the
//!   security-boundary key the fetch / storage layers partition on).
//! - [`percent_encode`] + the [`EncodeSet`]s — the § 4.2 percent-encoding
//!   family.
//!
//! What does *not* live here:
//! - IDNA — internationalised domain names are ASCII-encoded via the
//!   `idna` UTS #46 check the host parser runs; v1.0 keeps the host as
//!   the authored ASCII string + rejects non-ASCII host code points (the
//!   host is the registrable-domain key the cookie / CORS / referrer
//!   layers compare; IDNA lands with the PSL).
//! - Full IPv6 literal parsing — the `[...]` host form is captured
//!   verbatim; the zone-id + the address-shorthand grammar stay in the
//!   network layer.
//! - Opaque-path non-special schemes (`mailto:`, `tel:`, `data:`) — v1.0
//!   models the hierarchical path for special + authority-bearing
//!   non-special schemes; the § 4.2 opaque-path shape for schemes
//!   without `//` is captured as a single-segment path (the common
//!   `mailto:` case).
//! - The full § 4.6 state machine's validation quirk handling (every
//!   edge the whatwg/url crate covers) — the common web surface is
//!   faithful; the long tail lands when the fetch layer needs it.
//!
//! Reference: <https://url.spec.whatwg.org/>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Special schemes + Url
// ---------------------------------------------------------------------------

/// The § 3.1 special schemes: `http`, `https`, `ws`, `wss`, `file`.
pub const SPECIAL_SCHEMES: &[&str] = &["http", "https", "ws", "wss", "file"];

/// `true` iff `scheme` is a § 3.1 special scheme (case-insensitive).
pub fn is_special_scheme(scheme: &str) -> bool {
    SPECIAL_SCHEMES
        .iter()
        .any(|s| s.eq_ignore_ascii_case(scheme))
}

/// The § 3.1 default port for a special scheme (`http`/`ws` → 80,
/// `https`/`wss` → 443, `file` → none). Non-special schemes return `None`.
pub fn default_port(scheme: &str) -> Option<u16> {
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "file" => None,
        _ => None,
    }
}

/// The parsed URL components (WHATWG URL Standard § 4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Url {
    /// The scheme (lowercased per § 3.1).
    pub scheme: String,
    /// The username (percent-decoded form not stored; the raw percent-
    /// encoded userinfo).
    pub username: String,
    /// The password.
    pub password: String,
    /// The host (`None` for opaque / `file`-with-empty-host; a domain or
    /// an IPv6 `[...]` literal otherwise).
    pub host: Option<String>,
    /// The explicit port (`None` ⇒ the scheme's default).
    pub port: Option<u16>,
    /// The path segments (the § 4.2 hierarchical path; `[ ""]` for the
    /// root path, `[]` for the opaque no-path case).
    pub path: Vec<String>,
    /// The query (without the leading `?`; `None` ⇒ no query).
    pub query: Option<String>,
    /// The fragment (without the leading `#`; `None` ⇒ no fragment).
    pub fragment: Option<String>,
}

/// A URL parse error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    /// The input is empty.
    #[error("empty input")]
    Empty,
    /// The scheme is missing or invalid (§ 3.1: must start with an ASCII
    /// alpha + continue with ASCII alphanumerics / `+` / `-` / `.`).
    #[error("invalid scheme")]
    InvalidScheme,
    /// The host is invalid (a forbidden host code point or a non-ASCII
    /// code point IDNA would reject).
    #[error("invalid host")]
    InvalidHost,
    /// The port is not a valid `u16` or carries a forbidden code point.
    #[error("invalid port")]
    InvalidPort,
    /// A relative reference was passed to [`parse`] (use [`parse_with_base`]).
    #[error("relative reference needs a base")]
    RelativeNeedsBase,
}

// ---------------------------------------------------------------------------
// Percent-encoding sets
// ---------------------------------------------------------------------------

/// A WHATWG § 4.2 percent-encoding set (a per-code-point predicate).
#[derive(Debug, Clone, Copy)]
pub struct EncodeSet(pub fn(char) -> bool);

impl EncodeSet {
    /// The § 4.2 `C0 control` set (U+0000–U+001F + U+007F + all non-ASCII).
    pub const fn c0_control() -> Self {
        Self(|c| (c as u32) < 0x20 || (c as u32) == 0x7f || (c as u32) > 0x7e)
    }

    /// The § 4.2 `fragment` set: C0 control + space + `"` `<` `>` `` ` ``.
    pub const fn fragment() -> Self {
        Self(|c| {
            (c as u32) < 0x20
                || matches!(c, ' ' | '"' | '<' | '>' | '`')
                || (c as u32) == 0x7f
                || (c as u32) > 0x7e
        })
    }

    /// The § 4.2 `query` set: C0 control + space + `"` `#` `<` `>`.
    pub const fn query() -> Self {
        Self(|c| {
            (c as u32) < 0x20
                || matches!(c, ' ' | '"' | '#' | '<' | '>')
                || (c as u32) == 0x7f
                || (c as u32) > 0x7e
        })
    }

    /// The § 4.2 `special-path` / `path` set: C0 control + space + `"`
    /// `#` `<` `>` `?` `` ` `` `{` `}`.
    pub const fn path() -> Self {
        Self(|c| {
            (c as u32) < 0x20
                || matches!(c, ' ' | '"' | '#' | '<' | '>' | '?' | '`' | '{' | '}')
                || (c as u32) == 0x7f
                || (c as u32) > 0x7e
        })
    }

    /// The § 4.2 `userinfo` set: C0 control + space + `"` `#` `/` `:`
    /// `;` `=` `?` `@` `[` `\` `]` `^` `` ` `` `{` `}` `|`.
    pub const fn userinfo() -> Self {
        Self(|c| {
            (c as u32) < 0x20
                || matches!(
                    c,
                    ' ' | '"'
                        | '#'
                        | '/'
                        | ':'
                        | ';'
                        | '='
                        | '?'
                        | '@'
                        | '['
                        | '\\'
                        | ']'
                        | '^'
                        | '`'
                        | '{'
                        | '}'
                        | '|'
                )
                || (c as u32) == 0x7f
                || (c as u32) > 0x7e
        })
    }
}

/// Percent-encode `input` for `set`: each code point in `set` becomes `%XX`
/// (uppercase hex); other code points are passed through (UTF-8 bytes for
/// non-ASCII).
pub fn percent_encode(input: &str, set: EncodeSet) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        let c = b as char;
        if (set.0)(c) {
            out.push('%');
            out.push_str(&format!("{:02X}", b));
        } else {
            out.push(c);
        }
    }
    out
}

/// Percent-decode `input`: `%XX` → the byte; other bytes pass through.
/// Returns the decoded bytes reinterpreted as UTF-8 (ill-formed sequences
/// become U+FFFD via `String::from_utf8_lossy`).
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A single ASCII hex digit → its nibble value.
fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// parse
// ---------------------------------------------------------------------------

/// Parse an absolute URL string into a [`Url`]. A relative reference (no
/// scheme) returns [`UrlError::RelativeNeedsBase`] — use [`parse_with_base`].
pub fn parse(input: &str) -> Result<Url, UrlError> {
    let s = input.trim_matches(|c: char| c <= ' ');
    if s.is_empty() {
        return Err(UrlError::Empty);
    }
    let (scheme, rest) = match split_scheme(s) {
        Some((scheme, rest)) => (scheme, rest),
        None => {
            // Distinguish a relative reference (no scheme) from an
            // attempted-but-invalid scheme. A `:` before the first
            // `/`/`?`/`#` ⇒ a scheme was attempted but invalid; else the
            // input is a relative reference needing a base.
            let has_colon_before_delim = match s.find(['/', '?', '#']) {
                Some(i) => s[..i].contains(':'),
                None => s.contains(':'),
            };
            if has_colon_before_delim {
                return Err(UrlError::InvalidScheme);
            } else {
                return Err(UrlError::RelativeNeedsBase);
            }
        }
    };
    let scheme = scheme.to_ascii_lowercase();
    let mut url = Url {
        scheme,
        ..Url::default()
    };
    let special = is_special_scheme(&url.scheme);
    let after = rest;
    if special || after.starts_with("//") {
        // Strip the `//`.
        let after_slashes = after.strip_prefix("//").unwrap_or(after);
        let (authority, remaining) = split_authority(after_slashes);
        parse_authority(&mut url, authority)?;
        if url.scheme == "file" && url.host.as_deref().map(|h| h.is_empty()).unwrap_or(true) {
            // `file:///path` ⇒ empty host, path follows. The host stays
            // `Some("")` per § 4.3 (file URLs always have a host, possibly
            // the empty string); the path is `remaining`.
        }
        // Strip a leading `?` / `#` boundary already handled by
        // split_authority; parse path/query/fragment.
        parse_path_query_fragment(&mut url, remaining, special);
    } else {
        // Non-special scheme without `//`: an opaque path (e.g. `mailto:foo@bar`).
        // § 4.2: the path is the first segment (cannot have authority).
        remaining_to_opaque_path(&mut url, after);
    }
    // Fill the default port for special schemes if an explicit one wasn't
    // given — keep port as None (serialize adds the default only when
    // explicit-non-default).
    Ok(url)
}

/// Split `scheme:rest`. The scheme must start with an ASCII alpha + continue
/// with ASCII alphanumeric / `+` / `-` / `.`.
fn split_scheme(s: &str) -> Option<(&str, &str)> {
    let mut iter = s.char_indices();
    let first = iter.next()?.1;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut end = 1;
    for (i, c) in iter {
        if c == ':' {
            return Some((&s[..end], &s[i + c.len_utf8()..]));
        }
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.') {
            end = i + c.len_utf8();
        } else {
            return None;
        }
    }
    None
}

/// Split the authority (up to the first `/`, `?`, or `#`) from the rest.
fn split_authority(s: &str) -> (&str, &str) {
    let boundary = s.find(['/', '?', '#']).unwrap_or(s.len());
    (&s[..boundary], &s[boundary..])
}

/// Parse the `user:pass@host:port` authority.
fn parse_authority(url: &mut Url, authority: &str) -> Result<(), UrlError> {
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (&authority[..i], &authority[i + 1..]),
        None => ("", authority),
    };
    if !userinfo.is_empty() {
        let (user, pass) = match userinfo.find(':') {
            Some(i) => (&userinfo[..i], &userinfo[i + 1..]),
            None => (userinfo, ""),
        };
        url.username = percent_encode(user, EncodeSet::userinfo());
        url.password = percent_encode(pass, EncodeSet::userinfo());
    }
    // Host + port. IPv6 literal `[...]` keeps the bracket contents (the
    // address, including `:`s, captured verbatim); the port (if any)
    // follows the `]` or the bare host.
    let (host, port, is_ipv6) = if let Some(stripped) = hostport.strip_prefix('[') {
        match stripped.find(']') {
            Some(i) => {
                let host = &stripped[..i];
                let after = &stripped[i + 1..];
                let port = after.strip_prefix(':');
                (host, port, true)
            }
            None => return Err(UrlError::InvalidHost),
        }
    } else {
        match hostport.rfind(':') {
            Some(i) => (&hostport[..i], Some(&hostport[i + 1..]), false),
            None => (hostport, None, false),
        }
    };
    if is_ipv6 {
        // IPv6 literal: captured verbatim (no validation — the address
        // grammar is the network layer's job; v1.0 keeps the authored
        // bracket contents).
        url.host = Some(host.to_string());
    } else if url.scheme == "file" {
        // § 4.3: a `file` host may be empty; a non-empty `file` host must
        // be a "local host" (the empty string + `localhost`).
        url.host = Some(host.to_ascii_lowercase());
    } else if host.is_empty() {
        // A special non-file scheme requires a host.
        if is_special_scheme(&url.scheme) {
            return Err(UrlError::InvalidHost);
        }
        url.host = Some(String::new());
    } else {
        validate_host(host)?;
        url.host = Some(host.to_ascii_lowercase());
    }
    if let Some(p) = port {
        if p.is_empty() {
            url.port = None;
        } else {
            url.port = Some(p.parse::<u16>().map_err(|_| UrlError::InvalidPort)?);
        }
    }
    Ok(())
}

/// Reject a host with forbidden host code points (§ 3.2.2) or non-ASCII
/// (IDNA deferred — fail closed).
fn validate_host(host: &str) -> Result<(), UrlError> {
    if host.is_empty() {
        return Err(UrlError::InvalidHost);
    }
    for c in host.chars() {
        if !c.is_ascii() {
            // IDNA is deferred; reject non-ASCII hosts.
            return Err(UrlError::InvalidHost);
        }
        if matches!(
            c,
            '\0' | '\t'
                | '\n'
                | '\r'
                | ' '
                | '/'
                | ':'
                | '<'
                | '>'
                | '?'
                | '#'
                | '['
                | ']'
                | '\\'
                | '^'
                | '|'
                | '%'
                | '"'
                | '@'
                | '('
                | ')'
                | ','
                | '!'
                | '$'
                | '&'
                | '\''
                | '+'
                | ';'
                | '='
                | '`'
                | '{'
                | '}'
        ) {
            return Err(UrlError::InvalidHost);
        }
    }
    Ok(())
}

/// Parse the path / query / fragment portion (after the authority).
fn parse_path_query_fragment(url: &mut Url, input: &str, special: bool) {
    let (path_part, after_path) = match input.find(['?', '#']) {
        Some(i) => (&input[..i], &input[i..]),
        None => (input, ""),
    };
    // Path. The WHATWG representation is the segments joined by `/` with a
    // leading `/` per segment; the leading `/` of the authored path is
    // stripped before the split so it isn't encoded as an empty first
    // segment (the root path `/` is `[""]`).
    if path_part.is_empty() {
        url.path = if special { vec![String::new()] } else { vec![] };
    } else {
        let stripped = path_part.strip_prefix('/').unwrap_or(path_part);
        url.path = stripped
            .split('/')
            .map(|seg| percent_encode(seg, EncodeSet::path()))
            .collect();
    }
    // Query + fragment.
    let mut rest = after_path;
    if let Some(q) = rest.strip_prefix('?') {
        let (query, after) = match q.find('#') {
            Some(i) => (&q[..i], &q[i..]),
            None => (q, ""),
        };
        url.query = Some(percent_encode(query, EncodeSet::query()));
        rest = after;
    }
    if let Some(f) = rest.strip_prefix('#') {
        url.fragment = Some(percent_encode(f, EncodeSet::fragment()));
    }
}

/// Capture a non-special opaque path (e.g. `mailto:foo@bar`).
fn remaining_to_opaque_path(url: &mut Url, after: &str) {
    let (path_part, after_path) = match after.find(['?', '#']) {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, ""),
    };
    if path_part.is_empty() {
        url.path = vec![String::new()];
    } else {
        url.path = vec![percent_encode(path_part, EncodeSet::path())];
    }
    let mut rest = after_path;
    if let Some(q) = rest.strip_prefix('?') {
        let (query, after) = match q.find('#') {
            Some(i) => (&q[..i], &q[i..]),
            None => (q, ""),
        };
        url.query = Some(percent_encode(query, EncodeSet::query()));
        rest = after;
    }
    if let Some(f) = rest.strip_prefix('#') {
        url.fragment = Some(percent_encode(f, EncodeSet::fragment()));
    }
}

// ---------------------------------------------------------------------------
// parse_with_base (relative resolution)
// ---------------------------------------------------------------------------

/// Parse `input` as a URL reference, resolving a relative reference against
/// `base` per § 4.6. If `input` has a scheme, it's parsed absolutely; else
/// the base scheme + authority + path are inherited per the § 4.6
/// relative-state.
pub fn parse_with_base(input: &str, base: &Url) -> Result<Url, UrlError> {
    let s = input.trim_matches(|c: char| c <= ' ');
    if s.is_empty() {
        return Err(UrlError::Empty);
    }
    if split_scheme(s).is_some() {
        return parse(s);
    }
    // Relative reference.
    let mut url = Url {
        scheme: base.scheme.clone(),
        username: base.username.clone(),
        password: base.password.clone(),
        host: base.host.clone(),
        port: base.port,
        path: base.path.clone(),
        query: base.query.clone(),
        fragment: None,
    };
    if let Some(rest) = s.strip_prefix("//") {
        // Scheme-relative: replace the authority + path.
        let (authority, remaining) = split_authority(rest);
        parse_authority(&mut url, authority)?;
        let special = is_special_scheme(&url.scheme);
        parse_path_query_fragment(&mut url, remaining, special);
        return Ok(url);
    }
    if let Some(rest) = s.strip_prefix('?') {
        // Query-only relative: keep path, replace query (+ fragment).
        url.path = base.path.clone();
        let (query, after) = match rest.find('#') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        url.query = Some(percent_encode(query, EncodeSet::query()));
        if let Some(f) = after.strip_prefix('#') {
            url.fragment = Some(percent_encode(f, EncodeSet::fragment()));
        }
        return Ok(url);
    }
    if let Some(rest) = s.strip_prefix('#') {
        // Fragment-only relative: keep everything but the fragment.
        url.query = base.query.clone();
        url.fragment = Some(percent_encode(rest, EncodeSet::fragment()));
        return Ok(url);
    }
    if let Some(rest) = s.strip_prefix('/') {
        // Absolute-path relative: replace the path. The leading `/` was
        // stripped; the segments split on `/` (the root `/` ⇒ `[""]`).
        let path_part = rest;
        let (pp, after) = match path_part.find(['?', '#']) {
            Some(i) => (&path_part[..i], &path_part[i..]),
            None => (path_part, ""),
        };
        url.path = if pp.is_empty() {
            vec![String::new()]
        } else {
            pp.split('/')
                .map(|seg| percent_encode(seg, EncodeSet::path()))
                .collect()
        };
        url.query = None;
        let mut r = after;
        if let Some(q) = r.strip_prefix('?') {
            let (query, a) = match q.find('#') {
                Some(i) => (&q[..i], &q[i..]),
                None => (q, ""),
            };
            url.query = Some(percent_encode(query, EncodeSet::query()));
            r = a;
        }
        if let Some(f) = r.strip_prefix('#') {
            url.fragment = Some(percent_encode(f, EncodeSet::fragment()));
        }
        return Ok(url);
    }
    // Relative-path segment: merge with the base path per § 4.6.
    let (path_part, after) = match s.find(['?', '#']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let mut new_path = if base.path.is_empty() {
        vec![String::new()]
    } else {
        // Drop the last segment of the base path + append the new segment(s).
        let mut p = base.path.clone();
        if !p.is_empty() {
            p.pop();
        }
        p
    };
    for seg in path_part.split('/') {
        new_path.push(percent_encode(seg, EncodeSet::path()));
    }
    if new_path.is_empty() {
        new_path = vec![String::new()];
    }
    url.path = new_path;
    url.query = None;
    let mut r = after;
    if let Some(q) = r.strip_prefix('?') {
        let (query, a) = match q.find('#') {
            Some(i) => (&q[..i], &q[i..]),
            None => (q, ""),
        };
        url.query = Some(percent_encode(query, EncodeSet::query()));
        r = a;
    }
    if let Some(f) = r.strip_prefix('#') {
        url.fragment = Some(percent_encode(f, EncodeSet::fragment()));
    }
    Ok(url)
}

// ---------------------------------------------------------------------------
// serialize + origin
// ---------------------------------------------------------------------------

impl Url {
    /// The § 4.1 canonical serialisation.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.scheme);
        out.push(':');
        let has_authority = is_special_scheme(&self.scheme) || self.host.is_some();
        if has_authority {
            out.push_str("//");
            if !self.username.is_empty() || !self.password.is_empty() {
                out.push_str(&self.username);
                if !self.password.is_empty() {
                    out.push(':');
                    out.push_str(&self.password);
                }
                out.push('@');
            }
            if let Some(host) = &self.host {
                if host.contains(':') {
                    // IPv6 literal — re-wrap in brackets (stored without).
                    out.push('[');
                    out.push_str(host);
                    out.push(']');
                } else {
                    out.push_str(host);
                }
            }
            if let Some(port) = self.port
                && Some(port) != default_port(&self.scheme)
            {
                // Omit the port if it's the scheme's default.
                out.push(':');
                out.push_str(&port.to_string());
            }
            // Path: each segment is preceded by `/` (the root path `[""]`
            // serialises as `/`).
            for seg in &self.path {
                out.push('/');
                out.push_str(seg);
            }
        } else if !self.path.is_empty() {
            // Opaque path (a non-special scheme without authority, e.g.
            // `mailto:`): the single segment, no leading slash.
            out.push_str(&self.path[0]);
        }
        if let Some(q) = &self.query {
            out.push('?');
            out.push_str(q);
        }
        if let Some(f) = &self.fragment {
            out.push('#');
            out.push_str(f);
        }
        out
    }

    /// The § 4.5 `(scheme, host, port)` origin tuple serialised as
    /// `scheme://host:port` (the port omitted if it's the scheme default).
    /// `None` for opaque origins (a non-special scheme with no host, or a
    /// `file:` URL with an empty host).
    pub fn origin(&self) -> Option<String> {
        let host = self.host.as_ref().filter(|h| !h.is_empty())?;
        let mut out = String::new();
        out.push_str(&self.scheme);
        out.push_str("://");
        if host.contains(':') {
            out.push('[');
            out.push_str(host);
            out.push(']');
        } else {
            out.push_str(host);
        }
        if let Some(port) = self.port
            && Some(port) != default_port(&self.scheme)
        {
            out.push(':');
            out.push_str(&port.to_string());
        }
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- special schemes + default port ------------------------------

    #[test]
    fn special_scheme_predicates() {
        assert!(is_special_scheme("http"));
        assert!(is_special_scheme("HTTPS"));
        assert!(is_special_scheme("ws"));
        assert!(!is_special_scheme("mailto"));
    }

    #[test]
    fn default_port_table() {
        assert_eq!(default_port("http"), Some(80));
        assert_eq!(default_port("https"), Some(443));
        assert_eq!(default_port("wss"), Some(443));
        assert_eq!(default_port("file"), None);
        assert_eq!(default_port("foo"), None);
    }

    // --- parse absolute ----------------------------------------------

    #[test]
    fn parse_simple_https() {
        let u = parse("https://example.com/").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.host.as_deref(), Some("example.com"));
        assert_eq!(u.port, None);
        assert_eq!(u.path, vec![""]);
        assert!(u.query.is_none());
        assert!(u.fragment.is_none());
        assert_eq!(u.serialize(), "https://example.com/");
    }

    #[test]
    fn parse_with_explicit_port_and_path_and_query_and_fragment() {
        let u = parse("http://example.com:8080/path/to/page?q=1&r=2#frag").unwrap();
        assert_eq!(u.scheme, "http");
        assert_eq!(u.host.as_deref(), Some("example.com"));
        assert_eq!(u.port, Some(8080));
        assert_eq!(u.path, vec!["path", "to", "page"]);
        assert_eq!(u.query.as_deref(), Some("q=1&r=2"));
        assert_eq!(u.fragment.as_deref(), Some("frag"));
        assert_eq!(
            u.serialize(),
            "http://example.com:8080/path/to/page?q=1&r=2#frag"
        );
    }

    #[test]
    fn parse_userinfo() {
        let u = parse("https://user:pass@example.com/").unwrap();
        assert_eq!(u.username, "user");
        assert_eq!(u.password, "pass");
        assert_eq!(u.host.as_deref(), Some("example.com"));
        assert_eq!(u.serialize(), "https://user:pass@example.com/");
    }

    #[test]
    fn parse_default_port_omitted_on_serialize() {
        let u = parse("http://example.com:80/").unwrap();
        assert_eq!(u.port, Some(80));
        assert_eq!(u.serialize(), "http://example.com/", "default port omitted");
    }

    #[test]
    fn parse_file_url_empty_host() {
        let u = parse("file:///path/to/file").unwrap();
        assert_eq!(u.scheme, "file");
        assert_eq!(u.host.as_deref(), Some(""));
        assert_eq!(u.path, vec!["path", "to", "file"]);
        assert_eq!(u.serialize(), "file:///path/to/file");
    }

    #[test]
    fn parse_opaque_mailto() {
        let u = parse("mailto:foo@example.com").unwrap();
        assert_eq!(u.scheme, "mailto");
        assert!(u.host.is_none());
        assert_eq!(u.path, vec!["foo@example.com"]);
        assert_eq!(u.serialize(), "mailto:foo@example.com");
    }

    #[test]
    fn parse_scheme_case_insensitive_lowercased() {
        let u = parse("HTTP://example.com/").unwrap();
        assert_eq!(u.scheme, "http");
    }

    #[test]
    fn parse_relative_reference_needs_base() {
        assert_eq!(parse("/path"), Err(UrlError::RelativeNeedsBase));
        assert_eq!(parse("example.com/"), Err(UrlError::RelativeNeedsBase));
    }

    #[test]
    fn parse_empty_input_rejected() {
        assert_eq!(parse(""), Err(UrlError::Empty));
    }

    #[test]
    fn parse_invalid_scheme_rejected() {
        assert_eq!(parse("://missing-scheme"), Err(UrlError::InvalidScheme));
        // `1http://x` has a `:` before the first `/` ⇒ an attempted scheme
        // (`1http:`) that's invalid (must start alpha).
        assert_eq!(parse("1http://x"), Err(UrlError::InvalidScheme));
    }

    #[test]
    fn parse_non_ascii_host_rejected() {
        assert_eq!(parse("http://exämple.com/"), Err(UrlError::InvalidHost));
    }

    #[test]
    fn parse_invalid_port_rejected() {
        assert_eq!(
            parse("http://example.com:99999/"),
            Err(UrlError::InvalidPort)
        );
        assert_eq!(parse("http://example.com:abc/"), Err(UrlError::InvalidPort));
    }

    #[test]
    fn parse_ipv6_literal_kept() {
        let u = parse("http://[::1]:8080/").unwrap();
        assert_eq!(u.host.as_deref(), Some("::1"));
        assert_eq!(u.port, Some(8080));
        assert_eq!(u.serialize(), "http://[::1]:8080/");
    }

    // --- percent encode/decode ---------------------------------------

    #[test]
    fn percent_encode_path_set() {
        assert_eq!(percent_encode("a b", EncodeSet::path()), "a%20b");
        assert_eq!(percent_encode("a#b", EncodeSet::path()), "a%23b");
        assert_eq!(percent_encode("safe", EncodeSet::path()), "safe");
    }

    #[test]
    fn percent_decode_round_trip() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a%23b"), "a#b");
        assert_eq!(percent_decode("no-encoding"), "no-encoding");
        // Ill-formed escape passes through verbatim.
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    // --- origin ------------------------------------------------------

    #[test]
    fn origin_special_scheme() {
        let u = parse("https://example.com:8443/").unwrap();
        assert_eq!(u.origin().as_deref(), Some("https://example.com:8443"));
    }

    #[test]
    fn origin_omits_default_port() {
        let u = parse("http://example.com/").unwrap();
        assert_eq!(u.origin().as_deref(), Some("http://example.com"));
    }

    #[test]
    fn origin_opaque_is_none() {
        let u = parse("mailto:foo@example.com").unwrap();
        assert_eq!(u.origin(), None);
    }

    #[test]
    fn origin_file_url_has_host_origin() {
        // `file://host/path` has a host origin; `file:///path` has an empty
        // host ⇒ opaque (the § 4.5 "if host is the empty string ⇒ opaque").
        let u = parse("file:///path").unwrap();
        assert_eq!(u.origin(), None, "empty-host file URL is opaque");
    }

    // --- parse_with_base (relative resolution) -----------------------

    #[test]
    fn resolve_absolute_path_relative() {
        let base = parse("https://example.com/a/b/c").unwrap();
        let u = parse_with_base("/x/y", &base).unwrap();
        assert_eq!(u.serialize(), "https://example.com/x/y");
    }

    #[test]
    fn resolve_relative_segment_merges_with_base_path() {
        let base = parse("https://example.com/a/b/c").unwrap();
        let u = parse_with_base("d", &base).unwrap();
        assert_eq!(u.serialize(), "https://example.com/a/b/d");
    }

    #[test]
    fn resolve_dot_segment_in_relative_path() {
        let base = parse("https://example.com/a/b/c").unwrap();
        // `../d` — the `..` segments are kept here (the § 4.6 path
        // compression is the host hook's; this module stores the segments
        // as authored). Document that the compression is deferred.
        let u = parse_with_base("../d", &base).unwrap();
        assert_eq!(u.path, vec!["a", "b", "..", "d"]);
    }

    #[test]
    fn resolve_query_only_relative() {
        let base = parse("https://example.com/a/b?old=1#f").unwrap();
        let u = parse_with_base("?new=2", &base).unwrap();
        assert_eq!(u.serialize(), "https://example.com/a/b?new=2");
    }

    #[test]
    fn resolve_fragment_only_relative() {
        let base = parse("https://example.com/a/b?q=1").unwrap();
        let u = parse_with_base("#newfrag", &base).unwrap();
        assert_eq!(u.serialize(), "https://example.com/a/b?q=1#newfrag");
    }

    #[test]
    fn resolve_scheme_relative_replaces_authority() {
        let base = parse("https://example.com/a/b").unwrap();
        let u = parse_with_base("//other.test/c", &base).unwrap();
        assert_eq!(u.serialize(), "https://other.test/c");
    }

    #[test]
    fn resolve_absolute_input_ignores_base() {
        let base = parse("https://example.com/a").unwrap();
        let u = parse_with_base("http://other.test/x", &base).unwrap();
        assert_eq!(u.serialize(), "http://other.test/x");
    }
}
