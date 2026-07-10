//! Cookie jar — RFC 6265 with Vixen-specific defaults.
//!
//! Reference: RFC 6265 (and 6265bis for `SameSite`). The Vixen-specific
//! configuration is pinned in docs/SPEC.md "Cookie defaults":
//!
//! - Default `SameSite` is **Lax** (matches modern browsers).
//! - Storage cap: **512 entries per jar**, FIFO eviction by insertion order
//!   (a deliberate simplification of RFC 6265's full eviction algorithm).
//! - `HttpOnly` is **rejected from `document.cookie`** but accepted from a
//!   `Set-Cookie` HTTP response.
//! - Outgoing `Cookie` header: `SameSite=Lax` cookies are sent cross-site
//!   only for safe methods (GET/HEAD/OPTIONS); `SameSite=Strict` only to
//!   same-site requests; `HttpOnly` cookies never appear in
//!   `document.cookie` reads.
//!
//! Everything else (domain matching, path matching, secure-gating, expiry,
//! `Max-Age` semantics) follows RFC 6265.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicU64, Ordering};

use time::OffsetDateTime;
use url::Url;

use crate::fetch_types::Method;

/// Vixen's per-jar entry cap (docs/SPEC.md "Cookie defaults").
pub const MAX_COOKIES: usize = 512;

/// `SameSite` cookie attribute. Vixen's default is [`SameSite::Lax`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SameSite {
    Strict,
    #[default]
    Lax,
    None,
}

/// A single stored cookie.
#[derive(Debug, Clone)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Lower-cased domain this cookie is scoped to.
    pub domain: String,
    /// `true` when no `Domain` attribute was supplied (host-only cookie).
    pub host_only: bool,
    pub path: String,
    /// `None` ⇒ session cookie (cleared on exit by the store layer).
    pub expires: Option<OffsetDateTime>,
    pub created: OffsetDateTime,
    pub last_access: OffsetDateTime,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    /// Monotonic insertion sequence for FIFO eviction.
    seq: u64,
}

/// Serializable cookie state for profile stores. This intentionally mirrors the
/// public, policy-relevant fields of [`Cookie`] without exposing the jar's FIFO
/// sequence internals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieSnapshot {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    pub expires_unix: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    pub creation_unix: i64,
}

/// Identity-keyed cookie changes made by an isolated fetch jar.
///
/// The fields stay private so callers can only construct deltas by comparing a
/// worker jar with the snapshots that seeded it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CookieJarDelta {
    upserts: Vec<CookieSnapshot>,
    removals: Vec<(String, String, String)>,
}

/// Why a `Set-Cookie` / `document.cookie` write was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CookieError {
    #[error("malformed cookie: missing name=value pair")]
    Malformed,
    #[error("HttpOnly cookies cannot be set from script (document.cookie)")]
    HttpOnlyFromScript,
    #[error("Secure cookies require an https context")]
    SecureRequiresHttps,
    #[error("SameSite=None cookies require the Secure attribute")]
    SameSiteNoneRequiresSecure,
    #[error("Set-Cookie Domain '{domain}' does not domain-match request host '{host}'")]
    DomainMismatch { domain: String, host: String },
}

type Clock = Box<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// RFC 6265 cookie jar with FIFO eviction and injectable clock (for tests).
pub struct CookieJar {
    cookies: Vec<Cookie>,
    next_seq: AtomicU64,
    now: Clock,
}

impl std::fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CookieJar")
            .field("len", &self.cookies.len())
            .finish_non_exhaustive()
    }
}

impl Default for CookieJar {
    fn default() -> Self {
        Self::with_clock(Box::new(OffsetDateTime::now_utc))
    }
}

impl CookieJar {
    /// Construct with a custom `now()` clock. Tests inject a fixed time.
    pub fn with_clock(now: Clock) -> Self {
        Self {
            cookies: Vec::new(),
            next_seq: AtomicU64::new(0),
            now,
        }
    }

    fn tick(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Number of stored cookies (before lazy expiry purge).
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    fn now(&self) -> OffsetDateTime {
        (self.now)()
    }

    /// Rehydrate a jar from trusted profile-store records.
    pub fn from_snapshots<I>(snapshots: I) -> Self
    where
        I: IntoIterator<Item = CookieSnapshot>,
    {
        let mut jar = Self::default();
        jar.replace_with_snapshots(snapshots);
        jar
    }

    /// Replace all cookies with trusted profile-store records. Expired records
    /// are discarded at the trust boundary.
    pub fn replace_with_snapshots<I>(&mut self, snapshots: I)
    where
        I: IntoIterator<Item = CookieSnapshot>,
    {
        self.cookies.clear();
        self.next_seq.store(0, Ordering::Relaxed);
        let now = self.now();
        for snapshot in snapshots {
            let expires = snapshot
                .expires_unix
                .and_then(|ts| OffsetDateTime::from_unix_timestamp(ts).ok());
            let created =
                OffsetDateTime::from_unix_timestamp(snapshot.creation_unix).unwrap_or(now);
            let cookie = Cookie {
                name: snapshot.name,
                value: snapshot.value,
                domain: snapshot.domain.to_ascii_lowercase(),
                host_only: snapshot.host_only,
                path: if snapshot.path.is_empty() {
                    "/".to_owned()
                } else {
                    snapshot.path
                },
                expires,
                created,
                last_access: now,
                secure: snapshot.secure,
                http_only: snapshot.http_only,
                same_site: snapshot.same_site,
                seq: 0,
            };
            if is_expired(&cookie, now) {
                continue;
            }
            let _ = self.upsert(cookie);
        }
    }

    /// Snapshot unexpired cookies for profile persistence.
    pub fn snapshots(&self) -> Vec<CookieSnapshot> {
        let now = self.now();
        self.cookies
            .iter()
            .filter(|cookie| !is_expired(cookie, now))
            .map(|cookie| CookieSnapshot {
                name: cookie.name.clone(),
                value: cookie.value.clone(),
                domain: cookie.domain.clone(),
                host_only: cookie.host_only,
                path: cookie.path.clone(),
                expires_unix: cookie.expires.map(|expires| expires.unix_timestamp()),
                secure: cookie.secure,
                http_only: cookie.http_only,
                same_site: cookie.same_site,
                creation_unix: cookie.created.unix_timestamp(),
            })
            .collect()
    }

    /// Return only the cookie identities changed since `baseline` seeded this
    /// jar. This is intended for isolated fetch tasks that must not replace a
    /// concurrently updated profile jar wholesale.
    pub fn delta_from_snapshots(&self, baseline: &[CookieSnapshot]) -> CookieJarDelta {
        let baseline = snapshots_by_identity(baseline.iter().cloned());
        let current = snapshots_by_identity(self.snapshots());
        let upserts = current
            .iter()
            .filter(|(identity, snapshot)| baseline.get(*identity) != Some(*snapshot))
            .map(|(_, snapshot)| snapshot.clone())
            .collect();
        let removals = baseline
            .keys()
            .filter(|identity| !current.contains_key(*identity))
            .cloned()
            .collect();
        CookieJarDelta { upserts, removals }
    }

    /// Merge an isolated fetch jar's changes without disturbing unrelated
    /// cookies added to this jar after the worker snapshot was taken.
    pub fn apply_delta(&mut self, delta: CookieJarDelta) {
        let removals: BTreeSet<_> = delta.removals.into_iter().collect();
        self.cookies.retain(|cookie| {
            !removals.contains(&(
                cookie.name.clone(),
                cookie.domain.clone(),
                cookie.path.clone(),
            ))
        });
        let now = self.now();
        for snapshot in delta.upserts {
            if let Some(cookie) = cookie_from_snapshot(snapshot, now) {
                let _ = self.upsert(cookie);
            }
        }
    }

    /// Store a cookie parsed from a `Set-Cookie` header value.
    ///
    /// `from_http_response` should be `true` for actual HTTP responses
    /// (HttpOnly accepted) and `false` for `document.cookie` writes
    /// (HttpOnly rejected — docs/SPEC.md).
    pub fn set_cookie(
        &mut self,
        header: &str,
        request_url: &Url,
        from_http_response: bool,
    ) -> Result<(), CookieError> {
        let now = self.now();
        let parsed = parse_set_cookie(header, request_url, now, from_http_response)?;
        let cookie = Cookie {
            name: parsed.name,
            value: parsed.value,
            domain: parsed.domain,
            host_only: parsed.host_only,
            path: parsed.path,
            expires: parsed.expires,
            created: parsed.created,
            last_access: parsed.created,
            secure: parsed.secure,
            http_only: parsed.http_only,
            same_site: parsed.same_site,
            seq: 0,
        };
        self.upsert(cookie)
    }

    fn upsert(&mut self, mut cookie: Cookie) -> Result<(), CookieError> {
        let now = self.now();
        // Replace existing (name, domain, path), preserving creation time
        // per RFC 6265 §5.3 step 11.3.
        if let Some(existing) = self
            .cookies
            .iter_mut()
            .find(|c| c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
        {
            cookie.created = existing.created;
            cookie.seq = existing.seq;
            cookie.last_access = now;
            *existing = cookie;
            return Ok(());
        }
        // FIFO eviction at the cap (docs/SPEC.md — FIFO, not RFC's full algo).
        while self.cookies.len() >= MAX_COOKIES {
            if let Some(idx) = self
                .cookies
                .iter()
                .enumerate()
                .min_by_key(|(_, c)| c.seq)
                .map(|(i, _)| i)
            {
                self.cookies.swap_remove(idx);
            } else {
                break;
            }
        }
        cookie.seq = self.tick();
        cookie.last_access = now;
        self.cookies.push(cookie);
        Ok(())
    }

    /// Drop expired cookies. Returns the number removed.
    pub fn purge_expired(&mut self) -> usize {
        let now = self.now();
        let before = self.cookies.len();
        self.cookies.retain(|c| !is_expired(c, now));
        before - self.cookies.len()
    }

    /// Build the outgoing `Cookie` header value for `url`.
    ///
    /// `cross_site` is `true` when the request is cross-site (the caller —
    /// the network layer — knows the top-level context). `method` gates
    /// `SameSite=Lax` cross-site sending for safe methods only.
    pub fn cookies_for(&mut self, url: &Url, cross_site: bool, method: Method) -> String {
        let now = self.now();
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let secure_context = url.scheme() == "https";

        let mut matching: Vec<usize> = self
            .cookies
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                if is_expired(c, now) {
                    return false;
                }
                if !domain_match(host, c.host_only, &c.domain) {
                    return false;
                }
                if !path_match(path, &c.path) {
                    return false;
                }
                if c.secure && !secure_context {
                    return false;
                }
                // SameSite rules (docs/SPEC.md).
                if cross_site {
                    match c.same_site {
                        SameSite::Strict => return false,
                        SameSite::Lax => {
                            if !method.is_safe() {
                                return false;
                            }
                        }
                        SameSite::None => {}
                    }
                }
                true
            })
            .map(|(i, _)| i)
            .collect();

        // RFC 6265 §5.4: longer paths first, then earlier creation.
        matching.sort_by(|&a, &b| {
            let ca = &self.cookies[a];
            let cb = &self.cookies[b];
            cb.path
                .len()
                .cmp(&ca.path.len())
                .then_with(|| ca.seq.cmp(&cb.seq))
        });

        let parts: Vec<String> = matching
            .iter()
            .map(|&i| {
                let c = &mut self.cookies[i];
                c.last_access = now;
                format!("{}={}", c.name, c.value)
            })
            .collect();
        parts.join("; ")
    }

    /// `document.cookie` read: matching cookies, **excluding HttpOnly**
    /// (docs/SPEC.md). `SameSite` does not affect readability.
    pub fn document_cookie_string(&self, url: &Url) -> String {
        let now = self.now();
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let secure_context = url.scheme() == "https";

        let mut matching: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                if c.http_only {
                    return false;
                }
                if is_expired(c, now) {
                    return false;
                }
                if !domain_match(host, c.host_only, &c.domain) {
                    return false;
                }
                if !path_match(path, &c.path) {
                    return false;
                }
                if c.secure && !secure_context {
                    return false;
                }
                true
            })
            .collect();
        matching.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then_with(|| a.seq.cmp(&b.seq))
        });
        matching
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn snapshots_by_identity(
    snapshots: impl IntoIterator<Item = CookieSnapshot>,
) -> BTreeMap<(String, String, String), CookieSnapshot> {
    snapshots
        .into_iter()
        .map(|snapshot| {
            (
                (
                    snapshot.name.clone(),
                    snapshot.domain.clone(),
                    snapshot.path.clone(),
                ),
                snapshot,
            )
        })
        .collect()
}

fn cookie_from_snapshot(snapshot: CookieSnapshot, now: OffsetDateTime) -> Option<Cookie> {
    let expires = snapshot
        .expires_unix
        .and_then(|timestamp| OffsetDateTime::from_unix_timestamp(timestamp).ok());
    let cookie = Cookie {
        name: snapshot.name,
        value: snapshot.value,
        domain: snapshot.domain.to_ascii_lowercase(),
        host_only: snapshot.host_only,
        path: if snapshot.path.is_empty() {
            "/".to_owned()
        } else {
            snapshot.path
        },
        expires,
        created: OffsetDateTime::from_unix_timestamp(snapshot.creation_unix).unwrap_or(now),
        last_access: now,
        secure: snapshot.secure,
        http_only: snapshot.http_only,
        same_site: snapshot.same_site,
        seq: 0,
    };
    (!is_expired(&cookie, now)).then_some(cookie)
}

fn is_expired(c: &Cookie, now: OffsetDateTime) -> bool {
    match c.expires {
        Some(e) => e <= now,
        None => false,
    }
}

// --- RFC 6265 §5.1.3 domain matching ----------------------------------------

fn looks_like_ip(host: &str) -> bool {
    host.parse::<Ipv4Addr>().is_ok() || host.parse::<Ipv6Addr>().is_ok()
}

/// RFC 6265 §5.1.3 domain-match.
fn domain_match(host: &str, host_only: bool, domain: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();
    if host == domain {
        return true;
    }
    if host_only {
        return false;
    }
    if looks_like_ip(&host) {
        return false;
    }
    // domain must be a suffix of host preceded by '.'.
    host.ends_with(&domain)
        && host.len() > domain.len()
        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
}

// --- RFC 6265 §5.1.4 default-path & path matching ---------------------------

/// RFC 6265 §5.1.4.
pub(crate) fn default_path(uri_path: &str) -> String {
    if uri_path.is_empty() || !uri_path.starts_with('/') {
        return "/".to_owned();
    }
    if !uri_path[1..].contains('/') {
        return "/".to_owned();
    }
    let last_slash = uri_path.rfind('/').unwrap();
    uri_path[..last_slash].to_owned()
}

/// RFC 6265 §5.1.4 path-match.
fn path_match(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if request_path.starts_with(cookie_path)
        && (cookie_path.ends_with('/')
            || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/'))
    {
        return true;
    }
    false
}

// --- Set-Cookie parsing -----------------------------------------------------

struct ParsedCookie {
    name: String,
    value: String,
    domain: String,
    host_only: bool,
    path: String,
    expires: Option<OffsetDateTime>,
    secure: bool,
    http_only: bool,
    same_site: SameSite,
    created: OffsetDateTime,
}

fn parse_set_cookie(
    header: &str,
    request_url: &Url,
    now: OffsetDateTime,
    from_http_response: bool,
) -> Result<ParsedCookie, CookieError> {
    let mut parts = header.split(';');
    let pair = parts.next().ok_or(CookieError::Malformed)?.trim();
    let (name, value) = pair.split_once('=').ok_or(CookieError::Malformed)?;
    let name = name.trim();
    if name.is_empty() {
        return Err(CookieError::Malformed);
    }
    let value = strip_quotes(value.trim());

    let mut domain: Option<String> = None;
    let mut path_attr: Option<String> = None;
    let mut expires: Option<OffsetDateTime> = None;
    let mut max_age: Option<i64> = None;
    let mut secure = false;
    let mut http_only = false;
    let mut same_site: Option<SameSite> = None;

    for seg in parts {
        let seg = seg.trim();
        if let Some((k, v)) = seg.split_once('=') {
            match k.trim().to_ascii_lowercase().as_str() {
                "domain" => {
                    let d = v.trim().trim_start_matches('.').to_ascii_lowercase();
                    if !d.is_empty() {
                        domain = Some(d);
                    }
                }
                "path" => {
                    let p = v.trim();
                    if !p.is_empty() {
                        path_attr = Some(p.to_owned());
                    }
                }
                "max-age" => {
                    if let Ok(n) = v.trim().parse::<i64>() {
                        max_age = Some(n);
                    }
                }
                "expires" => {
                    if let Some(t) = parse_cookie_date(v.trim()) {
                        expires = Some(t);
                    }
                }
                "samesite" => {
                    same_site = match v.trim().to_ascii_lowercase().as_str() {
                        "strict" => Some(SameSite::Strict),
                        "lax" => Some(SameSite::Lax),
                        "none" => Some(SameSite::None),
                        // Unknown value → Vixen default (Lax).
                        _ => Some(SameSite::Lax),
                    };
                }
                _ => {}
            }
        } else {
            match seg.to_ascii_lowercase().as_str() {
                "secure" => secure = true,
                "httponly" => http_only = true,
                _ => {}
            }
        }
    }

    // Max-Age takes precedence over Expires (RFC 6265 §5.2.2). Use checked
    // arithmetic: an attacker-controlled Max-Age far outside the representable
    // date range must NOT panic this boundary — clamp instead.
    let expiry = match max_age {
        Some(n) => match now.checked_add(time::Duration::seconds(n)) {
            Some(t) => Some(t),
            None => Some(clamp_max_age(now, n)),
        },
        None => expires,
    };

    let request_host = request_url.host_str().unwrap_or("").to_ascii_lowercase();
    let request_secure = request_url.scheme() == "https";

    // Resolve domain + host-only.
    let (domain, host_only) = match domain {
        Some(d) => {
            // The Domain attribute must domain-match the request host,
            // otherwise reject (fail closed).
            if !domain_match(&request_host, false, &d) {
                return Err(CookieError::DomainMismatch {
                    domain: d,
                    host: request_host,
                });
            }
            (d, false)
        }
        None => (request_host, true),
    };

    // Resolve path.
    let path = match path_attr {
        Some(p) if p.starts_with('/') => p,
        _ => default_path(request_url.path()),
    };

    // Fail-closed policy gates.
    if http_only && !from_http_response {
        return Err(CookieError::HttpOnlyFromScript);
    }
    if secure && !request_secure {
        // RFC 6265 §5.3 step 11: ignore Secure cookies over non-secure
        // channels. Vixen surfaces it as an explicit rejection.
        return Err(CookieError::SecureRequiresHttps);
    }
    let same_site = same_site.unwrap_or_default();
    if matches!(same_site, SameSite::None) && !secure {
        // RFC 6265bis: SameSite=None requires Secure.
        return Err(CookieError::SameSiteNoneRequiresSecure);
    }

    Ok(ParsedCookie {
        name: name.to_owned(),
        value,
        domain,
        host_only,
        path,
        expires: expiry,
        secure,
        http_only,
        same_site,
        created: now,
    })
}

fn strip_quotes(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        v[1..v.len() - 1].to_owned()
    } else {
        v.to_owned()
    }
}

/// Far-future representable instant (~year 9999) used to clamp an
/// out-of-range positive `Max-Age` to "effectively permanent".
const FAR_FUTURE_UNIX: i64 = 253_402_300_799;

/// Clamp a `Max-Age` whose value falls outside the representable date range.
/// Positive ⇒ permanent (far future); negative ⇒ already expired (epoch-1).
/// Used only when `now + Max-Age` overflows; never panics.
fn clamp_max_age(_now: OffsetDateTime, n: i64) -> OffsetDateTime {
    let ts = if n >= 0 { FAR_FUTURE_UNIX } else { -1 };
    OffsetDateTime::from_unix_timestamp(ts).unwrap_or({
        // `from_unix_timestamp` is infallible for these well-in-range values;
        // the `unwrap_or_else` is purely defensive.
        OffsetDateTime::UNIX_EPOCH
    })
}

// --- RFC 6265 §5.1.1 cookie-date parser -------------------------------------

/// Parse a cookie `Expires` date per RFC 6265 §5.1.1. Handles IMF-fixdate,
/// RFC 850, and asctime uniformly via the generic algorithm. Returns the
/// UTC instant, or `None` if no valid date could be derived.
pub(crate) fn parse_cookie_date(s: &str) -> Option<OffsetDateTime> {
    let mut found_time = false;
    let mut found_day = false;
    let mut found_month = false;
    let mut found_year = false;
    let (mut hour, mut minute, mut second) = (0i64, 0i64, 0i64);
    let (mut day, mut month, mut year) = (0i64, 0i64, 0i64);

    for token in tokenize_date(s) {
        if !found_time && let Some((h, m, sec)) = parse_time_token(&token) {
            hour = h;
            minute = m;
            second = sec;
            found_time = true;
            continue;
        }
        if !found_day && let Some(d) = first_1_2_digit_run(&token, 1, 31) {
            day = d;
            found_day = true;
            continue;
        }
        if !found_month && let Some(m) = month_of(&token) {
            month = m;
            found_month = true;
            continue;
        }
        if !found_year && let Some(y) = year_of(&token) {
            year = y;
            found_year = true;
            continue;
        }
    }

    if !(found_day && found_month && found_year) {
        return None;
    }
    let secs = unix_seconds(year, month, day, hour, minute, second)?;
    OffsetDateTime::from_unix_timestamp(secs).ok()
}

/// RFC 6265 §5.1.1 delimiters. A char is a *delimiter* in these ranges.
fn is_date_delimiter(b: u8) -> bool {
    matches!(b,
        0x09
        | 0x20..=0x2F
        | 0x3B..=0x40
        | 0x5B..=0x60
        | 0x7B..=0x7E)
}

fn tokenize_date(s: &str) -> Vec<String> {
    s.split(|c: char| is_date_delimiter(c as u8))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_owned())
        .collect()
}

/// `time` non-terminal: `HH:MM:SS` prefix (3 colon-separated 1-2 digit fields).
fn parse_time_token(token: &str) -> Option<(i64, i64, i64)> {
    let mut it = token.split(':');
    let h = it.next()?.trim_start_matches(|c: char| !c.is_ascii_digit());
    let h = parse_bounded(h, 0, 23)?;
    // remaining fields must exist in the same token
    let rest = &token[token.find(':')? + 1..];
    let mut it = rest.split(':');
    let m = parse_bounded(it.next()?, 0, 59)?;
    let sec = parse_bounded(it.next()?, 0, 59)?;
    Some((h, m, sec))
}

fn parse_bounded(s: &str, lo: i64, hi: i64) -> Option<i64> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n = digits.parse::<i64>().ok()?;
    (lo..=hi).contains(&n).then_some(n)
}

/// First run of 1-2 digits within the token, if its value is in `[lo, hi]`.
fn first_1_2_digit_run(token: &str, lo: i64, hi: i64) -> Option<i64> {
    let bytes = token.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = &token[start..i];
            if run.len() <= 2
                && let Ok(n) = run.parse::<i64>()
                && (lo..=hi).contains(&n)
            {
                return Some(n);
            }
            // runs longer than 2 digits can't be a day-of-month; keep scanning
        } else {
            i += 1;
        }
    }
    None
}

fn month_of(token: &str) -> Option<i64> {
    let prefix: String = token
        .chars()
        .take(3)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let m = match prefix.as_str() {
        "JAN" => 1,
        "FEB" => 2,
        "MAR" => 3,
        "APR" => 4,
        "MAY" => 5,
        "JUN" => 6,
        "JUL" => 7,
        "AUG" => 8,
        "SEP" => 9,
        "OCT" => 10,
        "NOV" => 11,
        "DEC" => 12,
        _ => return None,
    };
    Some(m)
}

fn year_of(token: &str) -> Option<i64> {
    // First run of 2-4 digits.
    let bytes = token.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = &token[start..i];
            if (2..=4).contains(&run.len()) {
                let n = run.parse::<i64>().ok()?;
                // 3-4 digit years are used as-is; 2-digit years map to a
                // century (RFC 6265 §5.1.1): 70-99 → 19xx, 00-69 → 20xx.
                let y = if n >= 100 {
                    n
                } else if n >= 70 {
                    1900 + n
                } else {
                    2000 + n
                };
                return Some(y);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Civil (Y,M,D) + time → unix seconds (UTC). Howard Hinnant's algorithm.
fn unix_seconds(y: i64, m: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (m - 3) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146097 + doe - 719468;
    if !(-100_000_000..=100_000_000).contains(&days) {
        return None;
    }
    Some(days * 86400 + h * 3600 + mi * 60 + s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn fixed(t: OffsetDateTime) -> Clock {
        Box::new(move || t)
    }

    fn jar(t: OffsetDateTime) -> CookieJar {
        CookieJar::with_clock(fixed(t))
    }

    #[test]
    fn cookie_snapshots_round_trip_and_drop_expired_records() {
        let t = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let mut jar = jar(t);
        let url = Url::parse("https://example.com/path/page.html").unwrap();
        jar.set_cookie("sid=abc; Path=/path; HttpOnly; SameSite=Strict", &url, true)
            .unwrap();
        jar.set_cookie("old=gone; Max-Age=0", &url, true).unwrap();

        let snapshots = jar.snapshots();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].name, "sid");
        assert!(snapshots[0].http_only);
        assert_eq!(snapshots[0].same_site, SameSite::Strict);

        let mut restored = CookieJar::with_clock(fixed(t));
        restored.replace_with_snapshots(snapshots);
        assert_eq!(restored.cookies_for(&url, false, Method::Get), "sid=abc");
    }

    #[test]
    fn cookie_delta_merges_worker_changes_without_replacing_unrelated_cookies() {
        let t = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let url = Url::parse("https://example.com/path").unwrap();
        let mut profile = jar(t);
        profile.set_cookie("updated=old", &url, true).unwrap();
        profile.set_cookie("removed=old", &url, true).unwrap();
        let baseline = profile.snapshots();

        let mut worker = CookieJar::from_snapshots(baseline.clone());
        worker.set_cookie("updated=new", &url, true).unwrap();
        worker
            .set_cookie("removed=gone; Max-Age=0", &url, true)
            .unwrap();
        worker.set_cookie("added=new", &url, true).unwrap();
        let delta = worker.delta_from_snapshots(&baseline);

        profile.set_cookie("concurrent=kept", &url, true).unwrap();
        profile.apply_delta(delta);
        let cookies = profile.cookies_for(&url, false, Method::Get);
        assert!(cookies.contains("updated=new"));
        assert!(cookies.contains("added=new"));
        assert!(cookies.contains("concurrent=kept"));
        assert!(!cookies.contains("removed="));
    }

    fn now() -> OffsetDateTime {
        time::PrimitiveDateTime::new(
            time::Date::from_calendar_date(2026, time::Month::June, 17).unwrap(),
            time::Time::from_hms(12, 0, 0).unwrap(),
        )
        .assume_utc()
    }

    #[test]
    fn default_samesite_is_lax() {
        let now = now();
        let mut j = jar(now);
        j.set_cookie("sid=1", &Url::parse("https://example.com/").unwrap(), true)
            .unwrap();
        assert_eq!(j.cookies[0].same_site, SameSite::Lax);
        assert!(j.cookies[0].host_only);
        assert_eq!(j.cookies[0].path, "/");
    }

    #[test]
    fn httponly_rejected_from_script_accepted_from_http() {
        let now = now();
        let mut j = jar(now);
        let url = Url::parse("https://example.com/").unwrap();
        // document.cookie write: rejected.
        assert_eq!(
            j.set_cookie("a=1; HttpOnly", &url, false).err(),
            Some(CookieError::HttpOnlyFromScript)
        );
        // HTTP response: accepted.
        j.set_cookie("a=1; HttpOnly", &url, true).unwrap();
        assert!(j.cookies[0].http_only);
    }

    #[test]
    fn document_cookie_excludes_httponly() {
        let now = now();
        let mut j = jar(now);
        let url = Url::parse("https://example.com/").unwrap();
        j.set_cookie("a=1; HttpOnly", &url, true).unwrap();
        j.set_cookie("b=2", &url, true).unwrap();
        let doc = j.document_cookie_string(&url);
        assert_eq!(doc, "b=2");
        // But HttpOnly IS sent on the wire.
        let wire = j.cookies_for(&url, false, Method::Get);
        assert!(wire.contains("a=1") && wire.contains("b=2"));
    }

    #[test]
    fn secure_cookie_not_sent_over_http_url() {
        let now = now();
        let mut j = jar(now);
        let https = Url::parse("https://example.com/").unwrap();
        j.set_cookie("s=1; Secure", &https, true).unwrap();
        // Secure cookie requires https on store.
        assert_eq!(
            j.set_cookie(
                "s=1; Secure",
                &Url::parse("http://example.com/").unwrap(),
                true
            )
            .err(),
            Some(CookieError::SecureRequiresHttps)
        );
        // Not returned for an http URL.
        let http = Url::parse("http://example.com/").unwrap();
        assert_eq!(j.cookies_for(&http, false, Method::Get), "");
        // Returned for https.
        assert_eq!(j.cookies_for(&https, false, Method::Get), "s=1");
    }

    #[test]
    fn samesite_none_requires_secure() {
        let now = now();
        let mut j = jar(now);
        let url = Url::parse("https://example.com/").unwrap();
        assert_eq!(
            j.set_cookie("x=1; SameSite=None", &url, true).err(),
            Some(CookieError::SameSiteNoneRequiresSecure)
        );
        j.set_cookie("x=1; SameSite=None; Secure", &url, true)
            .unwrap();
    }

    #[test]
    fn lax_cross_site_only_for_safe_methods() {
        let now = now();
        let mut j = jar(now);
        let url = Url::parse("https://example.com/").unwrap();
        j.set_cookie("l=1; SameSite=Lax", &url, true).unwrap();
        // Same-site GET/POST both send.
        assert!(j.cookies_for(&url, false, Method::Get).contains("l=1"));
        assert!(j.cookies_for(&url, false, Method::Post).contains("l=1"));
        // Cross-site GET sends (safe), POST does not.
        assert!(j.cookies_for(&url, true, Method::Get).contains("l=1"));
        assert_eq!(j.cookies_for(&url, true, Method::Post), "");
    }

    #[test]
    fn strict_not_sent_cross_site() {
        let now = now();
        let mut j = jar(now);
        let url = Url::parse("https://example.com/").unwrap();
        j.set_cookie("k=1; SameSite=Strict", &url, true).unwrap();
        assert!(j.cookies_for(&url, false, Method::Get).contains("k=1"));
        assert_eq!(j.cookies_for(&url, true, Method::Get), "");
    }

    #[test]
    fn domain_matching_subdomain() {
        let now = now();
        let mut j = jar(now);
        let root = Url::parse("https://example.com/").unwrap();
        j.set_cookie("d=1; Domain=example.com", &root, true)
            .unwrap();
        // Sent to subdomain (not host-only).
        let sub = Url::parse("https://www.example.com/app").unwrap();
        assert!(j.cookies_for(&sub, false, Method::Get).contains("d=1"));
        // Domain mismatch is rejected at store time.
        assert!(matches!(
            j.set_cookie("e=1; Domain=other.com", &root, true),
            Err(CookieError::DomainMismatch { .. })
        ));
    }

    #[test]
    fn path_matching() {
        let now = now();
        let mut j = jar(now);
        let url = Url::parse("https://example.com/a/b/c").unwrap();
        j.set_cookie("p=1; Path=/a/b", &url, true).unwrap();
        assert!(
            j.cookies_for(
                &Url::parse("https://example.com/a/b/x").unwrap(),
                false,
                Method::Get
            )
            .contains("p=1")
        );
        assert_eq!(
            j.cookies_for(
                &Url::parse("https://example.com/a/x").unwrap(),
                false,
                Method::Get
            ),
            ""
        );
    }

    #[test]
    fn default_path_directory() {
        assert_eq!(default_path(""), "/");
        assert_eq!(default_path("/"), "/");
        assert_eq!(default_path("/x"), "/");
        assert_eq!(default_path("/a/b/c"), "/a/b");
        assert_eq!(default_path("/a/b/"), "/a/b");
    }

    #[test]
    fn max_age_sets_expiry_and_expires() {
        let now = now();
        let mut j = jar(now);
        let url = Url::parse("https://example.com/").unwrap();
        j.set_cookie("t=1; Max-Age=3600", &url, true).unwrap();
        let exp = j.cookies[0].expires.unwrap();
        assert_eq!(exp, now + Duration::seconds(3600));
        // After expiry, gone.
        let mut later = jar(now + Duration::seconds(3700));
        later.cookies = j.cookies.clone();
        assert_eq!(later.cookies_for(&url, false, Method::Get), "");
    }

    #[test]
    fn negative_max_age_deletes_cookie() {
        let now = now();
        let mut j = jar(now);
        let url = Url::parse("https://example.com/").unwrap();
        j.set_cookie("t=1", &url, true).unwrap();
        // Max-Age=0 (or negative) marks it already-expired.
        j.set_cookie("t=1; Max-Age=0", &url, true).unwrap();
        assert_eq!(j.cookies_for(&url, false, Method::Get), "");
    }

    #[test]
    fn fifo_eviction_at_512_cap() {
        let now = now();
        let mut j = jar(now);
        let url = Url::parse("https://example.com/").unwrap();
        for i in 0..MAX_COOKIES {
            j.set_cookie(&format!("c{i}=1; Path=/p{i}"), &url, true)
                .unwrap();
        }
        assert_eq!(j.len(), MAX_COOKIES);
        // Adding one more evicts the oldest (c0 at /p0).
        j.set_cookie("new=1; Path=/new", &url, true).unwrap();
        assert_eq!(j.len(), MAX_COOKIES);
        assert_eq!(
            j.cookies_for(
                &Url::parse("https://example.com/p0").unwrap(),
                false,
                Method::Get
            ),
            ""
        );
        assert!(
            j.cookies_for(
                &Url::parse("https://example.com/new").unwrap(),
                false,
                Method::Get
            )
            .contains("new=1")
        );
    }

    #[test]
    fn overwrite_preserves_identity() {
        let now = now();
        let mut j = jar(now);
        let url = Url::parse("https://example.com/").unwrap();
        j.set_cookie("a=1", &url, true).unwrap();
        let original_seq = j.cookies[0].seq;
        j.set_cookie("a=2", &url, true).unwrap();
        assert_eq!(j.len(), 1);
        assert_eq!(j.cookies[0].value, "2");
        assert_eq!(j.cookies[0].seq, original_seq);
    }

    // --- cookie-date parser tests -------------------------------------------

    #[test]
    fn date_imf_fixdate() {
        let t = parse_cookie_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(t.unix_timestamp(), 784_111_777);
    }

    #[test]
    fn date_rfc850() {
        let t = parse_cookie_date("Sunday, 06-Nov-94 08:49:37 GMT").unwrap();
        assert_eq!(t.unix_timestamp(), 784_111_777);
    }

    #[test]
    fn date_asctime() {
        let t = parse_cookie_date("Sun Nov  6 08:49:37 1994").unwrap();
        assert_eq!(t.unix_timestamp(), 784_111_777);
    }

    #[test]
    fn date_two_digit_year_century() {
        // 80 → 1980, 70 → 1970, 20 → 2020 (RFC 6265 §5.1.1 year rule).
        assert_eq!(
            parse_cookie_date("Mon, 01 Jan 80 00:00:00 GMT")
                .unwrap()
                .year(),
            1980
        );
        assert_eq!(
            parse_cookie_date("Thu, 01 Jan 70 00:00:00 GMT")
                .unwrap()
                .year(),
            1970
        );
        assert_eq!(
            parse_cookie_date("Wed, 01 Jan 20 00:00:00 GMT")
                .unwrap()
                .year(),
            2020
        );
        assert_eq!(
            parse_cookie_date("Thu, 01 Jan 70 00:00:00 GMT")
                .unwrap()
                .unix_timestamp(),
            0
        );
    }

    #[test]
    fn date_garbage_returns_none() {
        assert!(parse_cookie_date("not a date at all").is_none());
        assert!(parse_cookie_date("").is_none());
        // missing month/day
        assert!(parse_cookie_date("1994").is_none());
    }

    #[test]
    fn unix_seconds_known_anchor() {
        // 1970-01-01 00:00:00 UTC == 0.
        assert_eq!(unix_seconds(1970, 1, 1, 0, 0, 0), Some(0));
        // 1994-11-06 08:49:37 UTC == 784111777.
        assert_eq!(unix_seconds(1994, 11, 6, 8, 49, 37), Some(784_111_777));
    }
}
