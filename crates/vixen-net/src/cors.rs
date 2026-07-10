//! CORS (Cross-Origin Resource Sharing) — Phase 7 prep (docs/SPEC.md
//! "Security" + Fetch § 3.2.1 / § 4.1.5 / § 4.1.6). Implements the pure logic
//! the network layer consults at every cross-origin fetch:
//!
//! - [`CorsResponseHeaders`] — the parsed `Access-Control-*` response headers.
//!   [`CorsResponseHeaders::from_headers`] walks an HTTP header list and
//!   extracts the six CORS directives (origin, credentials, methods, headers,
//!   expose-headers, max-age) with tolerant parsing per Fetch § 3.2.1.
//! - [`cors_check`] — Fetch § 4.1.5 "CORS check" algorithm: does the response
//!   authorise the request's origin + credentials mode? Returns
//!   [`CorsCheckOutcome::Pass`] or a [`CorsError`] naming the exact reason.
//! - [`CorsCredentialsMode`] — the three `fetch()` credentials modes (Fetch
//!   § 3.4.3) the request carries; the check is stricter when credentials
//!   are included.
//! - [`cors_filtered_headers`] — Fetch § 4.1.6 "CORS-filtered response":
//!   strip every response header that isn't on the safelist (`cache-control`,
//!   `content-language`, `content-length`, `content-type`, `expires`,
//!   `last-modified`, `pragma`) and isn't named in `Access-Control-Expose-
//!   Headers`. `Set-Cookie` / `Set-Cookie2` are always stripped (Fetch
//!   § 3.2.1 "forbidden response-header name").
//!
//! What does *not* live here:
//! - Preflight request construction (`OPTIONS` + `Access-Control-Request-
//!   Method` / `-Headers`). That's a fetch-time concern handled by the
//!   network layer; this module parses the response side.
//! - Caching of preflight results (`Access-Control-Max-Age` is parsed here;
//!   the bounded per-runtime cache that consults it lives at the engine fetch
//!   boundary).
//! - The actual `fetch()` request state machine (Fetch § 4.1) — that's
//!   upstream reqwest's job; we filter the resulting response.
//!
//! ## Trust boundary
//!
//! CORS is a browser-enforced cross-origin trust boundary (Fetch § 3.2.1).
//! The check runs *after* the response arrives from the network but *before*
//! script sees any of its data; on failure, the response is replaced with a
//! network error and the only visible headers are the safelisted ones. Vixen
//! enforces this at the script→fetch host hook (`vixen-engine::script`), per
//! `docs/ARCHITECTURE.md` "Trust boundaries".
//!
//! References:
//! - Fetch § 3.2.1 "CORS protocol"
//!   (<https://fetch.spec.whatwg.org/#http-cors-protocol>).
//! - Fetch § 4.1.5 "CORS check", § 4.1.6 "CORS-filtered response".
//! - Fetch § 3.4.3 "`credentials` enum" (`omit` / `same-origin` / `include`).

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Credentials mode (Fetch § 3.4.3)
// ---------------------------------------------------------------------------

/// The `fetch()` credentials mode (Fetch § 3.4.3). Controls whether cookies
/// and HTTP auth travel with the request, which in turn tightens the CORS
/// check (the response cannot use `Access-Control-Allow-Origin: *` when
/// credentials are included).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CorsCredentialsMode {
    /// `omit` — no cookies, no HTTP auth. The most permissive CORS response
    /// (`Access-Control-Allow-Origin: *`) is allowed.
    Omit,
    /// `same-origin` (the default for `fetch()`) — credentials only on
    /// same-origin requests; treated as `omit` for the CORS check.
    #[default]
    SameOrigin,
    /// `include` — credentials on every request, including cross-origin. The
    /// response MUST echo the specific origin (no wildcard).
    Include,
}

impl CorsCredentialsMode {
    /// Whether credentials are *sent* on cross-origin requests in this mode.
    /// The CORS check is strict when this is `true` (Fetch § 4.1.5 step 2).
    pub fn sends_cross_origin_credentials(self) -> bool {
        matches!(self, CorsCredentialsMode::Include)
    }
}

// ---------------------------------------------------------------------------
// Parsed CORS response headers
// ---------------------------------------------------------------------------

/// The six CORS protocol response directives (Fetch § 3.2.1) parsed from an
/// HTTP response header block. Every field is `Option`/default because CORS
/// is opt-in: a response without `Access-Control-Allow-Origin` simply fails
/// the check rather than being malformed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorsResponseHeaders {
    /// `Access-Control-Allow-Origin`: the literal value, lowercased.
    /// `None` if the header is absent; otherwise `Some("null")`,
    /// `Some("*")`, or `Some(<origin>)`. We keep the raw value rather than
    /// interpreting it as "any"/"specific" so the check can do the
    /// string-equality Fetch § 4.1.5 step 4 prescribes.
    pub allow_origin: Option<String>,
    /// `Access-Control-Allow-Credentials: true` (any other value is `false`).
    pub allow_credentials: bool,
    /// `Access-Control-Allow-Methods`: the methods, lowercased + de-duplicated.
    pub allow_methods: Vec<String>,
    /// `Access-Control-Allow-Headers`: the header names, lowercased + de-duped.
    pub allow_headers: Vec<String>,
    /// `Access-Control-Expose-Headers`: header names script may read in
    /// addition to the safelist. Lowercased + de-duplicated.
    pub expose_headers: Vec<String>,
    /// `Access-Control-Max-Age`: seconds the preflight result may be cached.
    /// `None` if absent or unparseable; the network layer applies a sane cap.
    pub max_age: Option<u64>,
}

impl CorsResponseHeaders {
    /// Parse the CORS directives out of an HTTP header list. Header name
    /// matching is case-insensitive per RFC 9110 § 5.1; values are decoded
    /// per Fetch § 3.2.1 (lists are comma-separated, items trimmed, case
    /// normalised where the spec is case-insensitive).
    ///
    /// Repeated `Access-Control-*` headers are merged (last wins for the
    /// scalar fields, concatenated for the list fields) — most servers emit
    /// one of each, but the tolerant path matches browser behaviour.
    pub fn from_headers<I, K, V>(headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut out = Self::default();
        for (name, value) in headers {
            let name = name.as_ref();
            let value = value.as_ref();
            match name.to_ascii_lowercase().as_str() {
                "access-control-allow-origin" => {
                    // Multiple Allow-Origin values are invalid (the response
                    // would be a network error); browsers take the first.
                    if out.allow_origin.is_none() {
                        out.allow_origin = Some(value.trim().to_owned());
                    }
                }
                "access-control-allow-credentials" => {
                    // Fetch § 3.2.1: only the literal `true` (case-insensitive)
                    // enables credentials; anything else is `false`.
                    out.allow_credentials |= value.trim().eq_ignore_ascii_case("true");
                }
                "access-control-allow-methods" => {
                    extend_lower_dedup(&mut out.allow_methods, value);
                }
                "access-control-allow-headers" => {
                    extend_lower_dedup(&mut out.allow_headers, value);
                }
                "access-control-expose-headers" => {
                    extend_lower_dedup(&mut out.expose_headers, value);
                }
                "access-control-max-age" if out.max_age.is_none() => {
                    out.max_age = value.trim().parse::<u64>().ok();
                }
                _ => {}
            }
        }
        out
    }

    /// `true` when `Access-Control-Allow-Origin` is the literal `*` (Fetch
    /// § 3.2.1 wildcard).
    pub fn is_wildcard_origin(&self) -> bool {
        self.allow_origin.as_deref() == Some("*")
    }

    /// `true` when `Access-Control-Allow-Origin` is the literal `null` (Fetch
    /// § 3.2.1: the "null" origin — sandboxed iframes, `file:` URIs, redirects
    /// through such origins).
    pub fn is_null_origin(&self) -> bool {
        self.allow_origin.as_deref() == Some("null")
    }
}

/// Append comma-separated items from `value` to `list`, lowercasing + trimming
/// each + skipping empties + preserving order while de-duplicating.
fn extend_lower_dedup(list: &mut Vec<String>, value: &str) {
    for part in value.split(',') {
        let s = part.trim().to_ascii_lowercase();
        if s.is_empty() || list.contains(&s) {
            continue;
        }
        list.push(s);
    }
}

// ---------------------------------------------------------------------------
// CORS check (Fetch § 4.1.5)
// ---------------------------------------------------------------------------

/// Outcome of [`cors_check`]: pass, or a [`CorsError`] naming the exact
/// failure reason (so the script-side error surface can distinguish them
/// for diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorsCheckOutcome {
    Pass,
    Fail(CorsError),
}

/// Why a CORS check failed (Fetch § 4.1.5). Used in inspector diagnostics;
/// every value cites the spec step that fires it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorsError {
    /// § 4.1.5 step 2: the response has `Access-Control-Allow-Origin: *` but
    /// the request sends credentials (which require a specific origin).
    #[error("wildcard origin with credentials")]
    WildcardWithCredentials,
    /// § 4.1.5 step 7: a credentialed cross-origin request requires the
    /// literal `Access-Control-Allow-Credentials: true` response header.
    #[error("credentials included without Access-Control-Allow-Credentials: true")]
    MissingAllowCredentials,
    /// § 4.1.5 step 4: the response's `Access-Control-Allow-Origin` does not
    /// match the request's origin (and isn't the wildcard).
    #[error("origin mismatch: response={response_origin:?}, request={request_origin:?}")]
    OriginMismatch {
        response_origin: Option<String>,
        request_origin: String,
    },
    /// § 4.1.5 step 1: the response has no `Access-Control-Allow-Origin`
    /// header at all (or it's empty). The most common CORS failure.
    #[error("no Access-Control-Allow-Origin header")]
    NoAllowOrigin,
}

/// Fetch § 4.1.5 "CORS check". Returns [`CorsCheckOutcome::Pass`] when the
/// response authorises the request's origin + credentials mode; otherwise a
/// [`CorsError`] naming the failing step.
///
/// `request_origin` is the request's serialized origin ("null" for sandboxed
/// or `file:`); `response` is the parsed CORS headers from
/// [`CorsResponseHeaders::from_headers`].
pub fn cors_check(
    response: &CorsResponseHeaders,
    request_origin: &str,
    credentials_mode: CorsCredentialsMode,
) -> CorsCheckOutcome {
    let Some(allow_origin) = response.allow_origin.as_deref() else {
        return CorsCheckOutcome::Fail(CorsError::NoAllowOrigin);
    };
    // Step 2: wildcard + credentials is forbidden.
    if allow_origin == "*" && credentials_mode.sends_cross_origin_credentials() {
        return CorsCheckOutcome::Fail(CorsError::WildcardWithCredentials);
    }
    // Step 4: origin string equality. The wildcard already passed above;
    // otherwise the response origin must equal the request origin (case-
    // sensitive — origins are canonicalised before comparison).
    if allow_origin != "*" && allow_origin != request_origin {
        return CorsCheckOutcome::Fail(CorsError::OriginMismatch {
            response_origin: response.allow_origin.clone(),
            request_origin: request_origin.to_owned(),
        });
    }
    if credentials_mode.sends_cross_origin_credentials() && !response.allow_credentials {
        CorsCheckOutcome::Fail(CorsError::MissingAllowCredentials)
    } else {
        CorsCheckOutcome::Pass
    }
}

// ---------------------------------------------------------------------------
// CORS-filtered response headers (Fetch § 4.1.6)
// ---------------------------------------------------------------------------

/// Headers always exposed to cross-origin script, regardless of
/// `Access-Control-Expose-Headers` (Fetch § 3.6.8 / § 4.1.6). The list is
/// frozen by the spec — adding to it would be a breaking change for every
/// server relying on CORS for security.
pub const CORS_SAFELISTED_RESPONSE_HEADERS: &[&str] = &[
    "cache-control",
    "content-language",
    "content-length",
    "content-type",
    "expires",
    "last-modified",
    "pragma",
];

/// Headers always stripped from a CORS response, even if listed in
/// `Access-Control-Expose-Headers` (Fetch § 3.2.1 "forbidden response-header
/// name"). Cookies must never cross the CORS boundary to script.
pub const CORS_FORBIDDEN_RESPONSE_HEADERS: &[&str] = &["set-cookie", "set-cookie2"];

/// Apply the § 4.1.6 CORS filter: keep only the safelisted headers plus any
/// named in `expose_headers`; strip everything else; strip the forbidden
/// names even if exposed.
///
/// Header name matching is case-insensitive. The returned order matches the
/// input order (browsers group headers but the filter itself is order-
/// preserving; the network layer may sort if it cares).
pub fn cors_filtered_headers<I, K, V>(
    headers: I,
    expose_headers: &[String],
) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut out = Vec::new();
    for (name, value) in headers {
        let lower = name.as_ref().to_ascii_lowercase();
        let is_forbidden = CORS_FORBIDDEN_RESPONSE_HEADERS.contains(&lower.as_str());
        if is_forbidden {
            continue;
        }
        let is_safelisted = CORS_SAFELISTED_RESPONSE_HEADERS.contains(&lower.as_str());
        let is_exposed = expose_headers.iter().any(|e| e == &lower);
        if is_safelisted || is_exposed {
            out.push((name.as_ref().to_owned(), value.as_ref().to_owned()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(name: &str, value: &str) -> (String, String) {
        (name.to_owned(), value.to_owned())
    }

    // --- CorsCredentialsMode --------------------------------------------

    #[test]
    fn credentials_mode_sends_cross_origin_only_for_include() {
        assert!(!CorsCredentialsMode::Omit.sends_cross_origin_credentials());
        assert!(!CorsCredentialsMode::SameOrigin.sends_cross_origin_credentials());
        assert!(CorsCredentialsMode::Include.sends_cross_origin_credentials());
    }

    #[test]
    fn default_credentials_mode_is_same_origin() {
        assert_eq!(
            CorsCredentialsMode::default(),
            CorsCredentialsMode::SameOrigin
        );
    }

    // --- CorsResponseHeaders parsing ------------------------------------

    #[test]
    fn parses_wildcard_origin() {
        let r = CorsResponseHeaders::from_headers([h("Access-Control-Allow-Origin", "*")]);
        assert_eq!(r.allow_origin.as_deref(), Some("*"));
        assert!(r.is_wildcard_origin());
        assert!(!r.is_null_origin());
    }

    #[test]
    fn parses_specific_origin() {
        let r = CorsResponseHeaders::from_headers([h(
            "Access-Control-Allow-Origin",
            "https://example.com",
        )]);
        assert_eq!(r.allow_origin.as_deref(), Some("https://example.com"));
        assert!(!r.is_wildcard_origin());
    }

    #[test]
    fn parses_null_origin() {
        let r = CorsResponseHeaders::from_headers([h("Access-Control-Allow-Origin", "null")]);
        assert!(r.is_null_origin());
    }

    #[test]
    fn parses_allow_credentials_only_for_literal_true() {
        let r = CorsResponseHeaders::from_headers([h("Access-Control-Allow-Credentials", "true")]);
        assert!(r.allow_credentials);
        // False for non-`true` values.
        for v in ["TRUE", "yes", "1", "false", "", "anything"] {
            let r = CorsResponseHeaders::from_headers([h("Access-Control-Allow-Credentials", v)]);
            assert!(
                !r.allow_credentials || v.eq_ignore_ascii_case("true"),
                "{v:?} should not enable credentials"
            );
        }
    }

    #[test]
    fn credentials_true_case_insensitive() {
        let r = CorsResponseHeaders::from_headers([h("Access-Control-Allow-Credentials", "TRUE")]);
        assert!(r.allow_credentials);
    }

    #[test]
    fn parses_methods_list_case_normalised_and_deduped() {
        let r = CorsResponseHeaders::from_headers([h(
            "Access-Control-Allow-Methods",
            "GET, POST, GET, OPTIONS",
        )]);
        assert_eq!(r.allow_methods, vec!["get", "post", "options"]);
    }

    #[test]
    fn parses_headers_list() {
        let r = CorsResponseHeaders::from_headers([h(
            "Access-Control-Allow-Headers",
            "X-Custom, Content-Type, X-Custom",
        )]);
        assert_eq!(r.allow_headers, vec!["x-custom", "content-type"]);
    }

    #[test]
    fn parses_expose_headers_list() {
        let r = CorsResponseHeaders::from_headers([h(
            "Access-Control-Expose-Headers",
            "X-Total, X-Page",
        )]);
        assert_eq!(r.expose_headers, vec!["x-total", "x-page"]);
    }

    #[test]
    fn parses_max_age_numeric() {
        let r = CorsResponseHeaders::from_headers([h("Access-Control-Max-Age", "600")]);
        assert_eq!(r.max_age, Some(600));
    }

    #[test]
    fn max_age_non_numeric_is_none() {
        let r = CorsResponseHeaders::from_headers([h("Access-Control-Max-Age", "forever")]);
        assert_eq!(r.max_age, None);
    }

    #[test]
    fn header_name_matching_is_case_insensitive() {
        // RFC 9110 § 5.1: header field names are case-insensitive.
        let r = CorsResponseHeaders::from_headers([h("access-control-ALLOW-origin", "*")]);
        assert!(r.is_wildcard_origin());
    }

    #[test]
    fn value_whitespace_trimmed() {
        let r = CorsResponseHeaders::from_headers([h("Access-Control-Allow-Origin", "  *  ")]);
        assert!(r.is_wildcard_origin());
    }

    #[test]
    fn repeated_origin_header_first_wins() {
        // Multiple Access-Control-Allow-Origin values are invalid; browsers
        // take the first and let the check fail on the second if it differs.
        let r = CorsResponseHeaders::from_headers([
            h("Access-Control-Allow-Origin", "https://a.example"),
            h("Access-Control-Allow-Origin", "https://b.example"),
        ]);
        assert_eq!(r.allow_origin.as_deref(), Some("https://a.example"));
    }

    #[test]
    fn empty_input_yields_defaults() {
        let r = CorsResponseHeaders::from_headers::<Vec<(String, String)>, String, String>(vec![]);
        assert!(r.allow_origin.is_none());
        assert!(!r.allow_credentials);
        assert!(r.allow_methods.is_empty());
        assert!(r.max_age.is_none());
    }

    // --- cors_check -----------------------------------------------------

    #[test]
    fn check_passes_for_wildcard_without_credentials() {
        let r = CorsResponseHeaders::from_headers([h("Access-Control-Allow-Origin", "*")]);
        let outcome = cors_check(
            &r,
            "https://anywhere.example",
            CorsCredentialsMode::SameOrigin,
        );
        assert_eq!(outcome, CorsCheckOutcome::Pass);
    }

    #[test]
    fn check_fails_for_wildcard_with_credentials() {
        let r = CorsResponseHeaders::from_headers([h("Access-Control-Allow-Origin", "*")]);
        let outcome = cors_check(&r, "https://anywhere.example", CorsCredentialsMode::Include);
        assert_eq!(
            outcome,
            CorsCheckOutcome::Fail(CorsError::WildcardWithCredentials)
        );
    }

    #[test]
    fn check_passes_for_matching_specific_origin() {
        let r = CorsResponseHeaders::from_headers([
            h("Access-Control-Allow-Origin", "https://app.example"),
            h("Access-Control-Allow-Credentials", "true"),
        ]);
        let outcome = cors_check(&r, "https://app.example", CorsCredentialsMode::Include);
        assert_eq!(outcome, CorsCheckOutcome::Pass);
    }

    #[test]
    fn check_fails_when_credentials_are_not_authorized() {
        let r = CorsResponseHeaders::from_headers([h(
            "Access-Control-Allow-Origin",
            "https://app.example",
        )]);
        assert_eq!(
            cors_check(&r, "https://app.example", CorsCredentialsMode::Include),
            CorsCheckOutcome::Fail(CorsError::MissingAllowCredentials)
        );
    }

    #[test]
    fn check_fails_for_non_matching_specific_origin() {
        let r = CorsResponseHeaders::from_headers([h(
            "Access-Control-Allow-Origin",
            "https://app.example",
        )]);
        let outcome = cors_check(&r, "https://attacker.example", CorsCredentialsMode::Omit);
        match outcome {
            CorsCheckOutcome::Fail(CorsError::OriginMismatch { request_origin, .. }) => {
                assert_eq!(request_origin, "https://attacker.example");
            }
            other => panic!("expected OriginMismatch, got {other:?}"),
        }
    }

    #[test]
    fn check_fails_for_missing_origin_header() {
        let r = CorsResponseHeaders::default();
        let outcome = cors_check(&r, "https://x.example", CorsCredentialsMode::Omit);
        assert_eq!(outcome, CorsCheckOutcome::Fail(CorsError::NoAllowOrigin));
    }

    #[test]
    fn check_with_null_origin_passes_when_request_is_null() {
        // file: or sandboxed origins send "null"; the response can echo that.
        let r = CorsResponseHeaders::from_headers([
            h("Access-Control-Allow-Origin", "null"),
            h("Access-Control-Allow-Credentials", "true"),
        ]);
        let outcome = cors_check(&r, "null", CorsCredentialsMode::Include);
        assert_eq!(outcome, CorsCheckOutcome::Pass);
    }

    #[test]
    fn check_with_null_origin_fails_for_real_request_origin() {
        let r = CorsResponseHeaders::from_headers([h("Access-Control-Allow-Origin", "null")]);
        let outcome = cors_check(&r, "https://x.example", CorsCredentialsMode::Omit);
        assert!(matches!(
            outcome,
            CorsCheckOutcome::Fail(CorsError::OriginMismatch { .. })
        ));
    }

    // --- cors_filtered_headers ------------------------------------------

    #[test]
    fn filtered_keeps_safelisted_headers() {
        let headers = vec![
            h("cache-control", "no-cache"),
            h("content-type", "application/json"),
            h("last-modified", "Wed, 21 Oct 2026 07:28:00 GMT"),
        ];
        let out = cors_filtered_headers(headers, &[]);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn filtered_drops_unexposed_custom_headers() {
        let headers = vec![h("x-secret", "sshhh"), h("cache-control", "no-cache")];
        let out = cors_filtered_headers(headers, &[]);
        // x-secret is not safelisted and not exposed → dropped.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "cache-control");
    }

    #[test]
    fn filtered_keeps_exposed_headers() {
        let headers = vec![h("x-total-count", "42"), h("x-secret", "nope")];
        let expose = vec!["x-total-count".to_string()];
        let out = cors_filtered_headers(headers, &expose);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "x-total-count");
    }

    #[test]
    fn filtered_strips_set_cookie_even_if_exposed() {
        // Set-Cookie is a "forbidden response-header name" (Fetch § 3.2.1):
        // never exposed to cross-origin script, even via Access-Control-
        // Expose-Headers. This is a hard security invariant.
        let headers = vec![h("set-cookie", "session=abc; HttpOnly")];
        let expose = vec!["set-cookie".to_string()];
        let out = cors_filtered_headers(headers, &expose);
        assert!(out.is_empty());
    }

    #[test]
    fn filtered_strips_set_cookie2() {
        let headers = vec![h("set-cookie2", "legacy=1"), h("cache-control", "no-cache")];
        let out = cors_filtered_headers(headers, &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "cache-control");
    }

    #[test]
    fn filtered_header_name_matching_is_case_insensitive() {
        let headers = vec![h("Content-Type", "text/html")];
        let out = cors_filtered_headers(headers, &[]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn filtered_expose_matching_is_case_insensitive() {
        // Exposed headers come from Access-Control-Expose-Headers parsing
        // (already lowercased); the input header's name is matched case-
        // insensitively against them.
        let headers = vec![h("X-Custom", "v")];
        let expose = vec!["x-custom".to_string()];
        let out = cors_filtered_headers(headers, &expose);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn filtered_preserves_input_order() {
        let headers = vec![
            h("cache-control", "1"),
            h("x-foo", "2"),
            h("content-type", "3"),
        ];
        let expose = vec!["x-foo".to_string()];
        let out = cors_filtered_headers(headers, &expose);
        // Three items survive, in input order.
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].0, "cache-control");
        assert_eq!(out[1].0, "x-foo");
        assert_eq!(out[2].0, "content-type");
    }

    #[test]
    fn filtered_empty_input_returns_empty() {
        let out: Vec<(String, String)> =
            cors_filtered_headers::<Vec<(String, String)>, String, String>(vec![], &[]);
        assert!(out.is_empty());
    }

    // --- Safelist constants (regression guard) --------------------------

    #[test]
    fn safelist_matches_spec() {
        // Fetch § 3.6.8 — exactly these seven. Adding to this list would be
        // a spec-violating change with security implications.
        assert_eq!(
            CORS_SAFELISTED_RESPONSE_HEADERS,
            &[
                "cache-control",
                "content-language",
                "content-length",
                "content-type",
                "expires",
                "last-modified",
                "pragma",
            ]
        );
    }

    #[test]
    fn forbidden_list_includes_both_cookie_variants() {
        assert!(CORS_FORBIDDEN_RESPONSE_HEADERS.contains(&"set-cookie"));
        assert!(CORS_FORBIDDEN_RESPONSE_HEADERS.contains(&"set-cookie2"));
    }
}
