//! Fetch § 3.2.2 — the JavaScript `Headers` object data model (pure logic).
//! The DOM-side host-hook layer (`new Headers()`, `init.headers`, the
//! `Request`/`Response` constructors) consults this one source of truth for
//! header-name/value validation, normalization, forbidden-header gating, and
//! the CORS-safelist predicate. Complements [`vixen_net::cors`] (the
//! response-side `Access-Control-*` filtering) and the network layer.
//!
//! What lives here:
//! - [`validate_header_name`] / [`validate_header_value`] — RFC 9110 § 5.5
//!   `token` validation + the Fetch § 3.2.2 normalize (lowercase name, OWS
//!   trim, NUL/CRLF rejection, code-point-`≤ U+00FF` gating).
//! - [`is_forbidden_request_header`] / [`is_forbidden_response_header_name`] —
//!   Fetch § 3.2.2 forbidden predicates the Request/Response init consults
//!   (the exact 21-name list + the `proxy-`/`sec-` prefix rules).
//! - [`is_cors_safelisted_request_header`] — Fetch § 3.2.1.2 CORS-safelist
//!   predicate (the `Accept`/`Accept-Language`/`Content-Language`/
//!   `Content-Type` (+ `Range`) family with the value-byte + MIME-essence
//!   gates the preflight logic depends on).
//! - [`Headers`] — the normalized store: append/set/get/delete/getAll/has +
//!   combined (comma-joined) + sorted (byte-order) iteration, the exact shape
//!   the `Headers` JS object reflects.
//!
//! What does *not* live here:
//! - The `Access-Control-*` response-header filtering ([`vixen_net::cors`]).
//! - The actual HTTP send/receive (Phase 1 `vixen_net::network`).
//! - The JS reflection (the `Headers` prototype methods — Phase 6 host hook).
//!
//! ## Forbidden-header layering
//!
//! The `Headers` object itself accepts forbidden header names (per Fetch
//! § 3.2.2 its append algorithm does not filter); the **Request**/**Response**
//! constructors strip forbidden names at init using
//! [`is_forbidden_request_header`] / [`is_forbidden_response_header_name`].
//! So [`Headers::append`] stores everything; the filtering is a separate
//! predicate the caller applies.
//!
//! Reference: <https://fetch.spec.whatwg.org/#headers-class>.
//! RFC 9110 § 5.5 (field-name token grammar):
//! <https://www.rfc-editor.org/rfc/rfc9110#section-5.5>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Name + value validation + normalization
// ---------------------------------------------------------------------------

/// Error from [`validate_header_name`] / [`validate_header_value`] /
/// [`Headers`] mutations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    /// The name is empty or contains a non-token byte (RFC 9110 § 5.5).
    #[error("invalid header name: {0:?}")]
    InvalidName(String),
    /// The value contains a NUL byte, a CR/LF (header-injection), or a code
    /// point > `U+00FF` (not representable in the Fetch latin1 value model).
    #[error("invalid header value")]
    InvalidValue,
}

/// Validate + normalize a header name (Fetch § 3.2.2 normalize: lowercased
/// `token`). Returns the lowercased name on success.
///
/// ```
/// # use vixen_engine::headers::validate_header_name;
/// assert_eq!(validate_header_name("Content-Type").unwrap(), "content-type");
/// assert!(validate_header_name("bad name").is_err()); // space isn't a token byte
/// ```
pub fn validate_header_name(name: &str) -> Result<String, HeaderError> {
    if name.is_empty() || !name.bytes().all(is_tchar) {
        return Err(HeaderError::InvalidName(name.to_owned()));
    }
    Ok(name.to_ascii_lowercase())
}

/// Validate + normalize a header value (Fetch § 3.2.2 normalize: OWS-trim,
/// reject NUL / CR / LF / code points > `U+00FF`). Returns the trimmed value.
pub fn validate_header_value(value: &str) -> Result<String, HeaderError> {
    // Reject code points outside the latin1 range the Fetch value model uses.
    if value.chars().any(|c| c > '\u{00FF}') {
        return Err(HeaderError::InvalidValue);
    }
    // No embedded NUL / CR / LF (header-injection defence; the network layer
    // also enforces this but the host-hook boundary rejects early).
    if value.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n') {
        return Err(HeaderError::InvalidValue);
    }
    Ok(value.trim_matches(|c| c == ' ' || c == '\t').to_owned())
}

/// RFC 9110 § 5.5 `tchar`: the token-byte set (field-name grammar).
const fn is_tchar(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~'
            | b'0'..=b'9'
            | b'A'..=b'Z'
            | b'a'..=b'z'
    )
}

// HTTP whitespace = SP / HT (RFC 9110 § 6.2.3 OWS). The value-trim above
// uses an inline `char` predicate because `str::trim_matches` takes `char`,
// not `u8`; no separate byte predicate is needed on the host-hook side.

// ---------------------------------------------------------------------------
// Forbidden-header predicates (Fetch § 3.2.2)
// ---------------------------------------------------------------------------

/// The 21 forbidden request-header names (Fetch § 3.2.2). A `Headers` object
/// accepts them, but the Request constructor strips them. Stored lowercased
/// for byte-case-insensitive comparison.
const FORBIDDEN_REQUEST_HEADERS: &[&str] = &[
    "accept-charset",
    "accept-encoding",
    "access-control-request-headers",
    "access-control-request-method",
    "connection",
    "content-length",
    "cookie",
    "cookie2",
    "date",
    "dnt",
    "expect",
    "host",
    "keep-alive",
    "origin",
    "referer",
    "set-cookie",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "via",
];

/// Whether `name` (any case; should already be lowercased by
/// [`validate_header_name`], but this is tolerant) is a forbidden request
/// header (Fetch § 3.2.2): a byte-case-insensitive match for one of the 21, or
/// starts with `proxy-` or `sec-`.
pub fn is_forbidden_request_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if FORBIDDEN_REQUEST_HEADERS.iter().any(|&f| f == lower) {
        return true;
    }
    lower.starts_with("proxy-") || lower.starts_with("sec-")
}

/// Whether `name` is a forbidden response-header name (Fetch § 3.2.2):
/// `set-cookie` / `set-cookie2`. The Response constructor strips these so JS
/// can't forge cookie headers.
pub fn is_forbidden_response_header_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "set-cookie" || lower == "set-cookie2"
}

// ---------------------------------------------------------------------------
// CORS-safelist predicate (Fetch § 3.2.1.2)
// ---------------------------------------------------------------------------

/// The § 3.2.1.2 value-byte cap. Headers above this are not safelisted (must
/// be preflighted).
const CORS_SAFELIST_MAX_VALUE_BYTES: usize = 1024;

/// Whether `(name, value)` is a CORS-safelisted request header (Fetch
/// § 3.2.1.2): one of `accept` / `accept-language` / `content-language` /
/// `content-type` / `range`, value ≤ 1024 bytes, no CORS-unsafe bytes, and
/// (for `content-type`) the MIME essence is one of the three safelisted forms;
/// (for `range`) the value matches the `bytes=` grammar.
///
/// The preflight logic uses this to decide which request headers need
/// `Access-Control-Allow-Headers` coverage.
pub fn is_cors_safelisted_request_header(name: &str, value: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // Value-byte cap.
    if value.len() > CORS_SAFELIST_MAX_VALUE_BYTES {
        return false;
    }
    // No CORS-unsafe bytes (§ 3.2.1.1: ≤ 0x20 except 0x09, or > 0x7E).
    if value.bytes().any(is_cors_unsafe_byte) {
        return false;
    }
    match lower.as_str() {
        "accept" | "accept-language" | "content-language" => true,
        "content-type" => matches!(
            crate::mime::MimeType::parse(value.trim())
                .map(|m| m.essence())
                .as_deref(),
            Some("application/x-www-form-urlencoded")
                | Some("multipart/form-data")
                | Some("text/plain")
        ),
        "range" => is_safelisted_range_value(value),
        _ => false,
    }
}

/// § 3.2.1.1 CORS-unsafe request-header byte: ≤ 0x20 except HT (0x09), or
/// > 0x7E (obs-text outside the ASCII printable range).
const fn is_cors_unsafe_byte(b: u8) -> bool {
    (b <= 0x20 && b != 0x09) || b > 0x7E
}

/// The § 3.2.1.2 `Range` safelist grammar: `bytes=[0-9]+-[0-9]*` (open-ended
/// end allowed; no multi-range, no suffix-range).
fn is_safelisted_range_value(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("bytes=") else {
        return false;
    };
    let Some((start, end)) = rest.split_once('-') else {
        return false;
    };
    !start.is_empty()
        && start.bytes().all(|b| b.is_ascii_digit())
        && (end.is_empty() || end.bytes().all(|b| b.is_ascii_digit()))
}

// ---------------------------------------------------------------------------
// Headers — the normalized store
// ---------------------------------------------------------------------------

/// The Fetch § 3.2.2 `Headers` store. Names are stored lowercased; values are
/// OWS-trimmed. Multiple values for the same name are kept as separate entries
/// (append semantics) and combined (comma-joined) on read via [`Headers::get`]
/// / [`Headers::iter`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    // `(name, value)` pairs in insertion order; duplicates allowed (append).
    entries: Vec<(String, String)>,
}

impl Headers {
    /// An empty header store.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Build from a `(name, value)` iterator (each validated + normalized;
    /// duplicates appended, not overwritten — use [`Headers::set`] to
    /// replace). Named `from_records` rather than `from_iter` to avoid
    /// shadowing the `FromIterator` trait surface.
    pub fn from_records<I: IntoIterator<Item = (String, String)>>(
        iter: I,
    ) -> Result<Self, HeaderError> {
        let mut headers = Self::new();
        for (name, value) in iter {
            headers.append(&name, &value)?;
        }
        Ok(headers)
    }

    /// Append a `name: value` pair (Fetch § 3.2.2 append). The name is
    /// lowercased; the value is OWS-trimmed; both are validated. An existing
    /// header of the same name gains a second entry (combined on read).
    pub fn append(&mut self, name: &str, value: &str) -> Result<(), HeaderError> {
        let n = validate_header_name(name)?;
        let v = validate_header_value(value)?;
        self.entries.push((n, v));
        Ok(())
    }

    /// Set `name` to `value`, replacing every existing entry of that name
    /// (Fetch § 3.2.2 set).
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), HeaderError> {
        let n = validate_header_name(name)?;
        let v = validate_header_value(value)?;
        // Replace the first existing occurrence in place (preserving its
        // position) and drop the rest — matches the Fetch set semantics where
        // the header ends up with exactly one value.
        let mut replaced = false;
        self.entries.retain(|(existing, _)| {
            if existing == &n {
                if !replaced {
                    replaced = true;
                    true // keep this slot; overwritten below
                } else {
                    false
                }
            } else {
                true
            }
        });
        if replaced {
            // Overwrite the kept slot.
            for (existing, val) in &mut self.entries {
                if existing == &n {
                    *val = v;
                    break;
                }
            }
        } else {
            self.entries.push((n, v));
        }
        Ok(())
    }

    /// Whether `name` is present.
    pub fn has(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.entries.iter().any(|(n, _)| n == &lower)
    }

    /// The combined value for `name` (Fetch § 3.2.2 get: comma-join the values
    /// of every entry of that name, in insertion order), or `None` if absent.
    pub fn get(&self, name: &str) -> Option<String> {
        let lower = name.to_ascii_lowercase();
        let mut found: Vec<&str> = Vec::new();
        for (n, v) in &self.entries {
            if n == &lower {
                found.push(v);
            }
        }
        if found.is_empty() {
            None
        } else {
            Some(found.join(", "))
        }
    }

    /// Every value for `name` in insertion order (the § 3.2.2 `getAll`
    /// surface; duplicates preserved).
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        let lower = name.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|(n, _)| n == &lower)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Delete every entry of `name` (Fetch § 3.2.2 delete). No-op if absent.
    pub fn delete(&mut self, name: &str) {
        let lower = name.to_ascii_lowercase();
        self.entries.retain(|(n, _)| n != &lower);
    }

    /// The number of distinct header names.
    pub fn len(&self) -> usize {
        self.entries
            .iter()
            .map(|(n, _)| n)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// Whether the store has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate the combined `(name, value)` pairs (one per distinct name,
    /// values comma-joined), in **insertion order** (the `Headers` iteration
    /// contract — stable across engines).
    pub fn iter(&self) -> impl Iterator<Item = (&str, String)> {
        // Preserve first-occurrence order; combine later duplicates.
        let mut seen: Vec<String> = Vec::new();
        self.entries.iter().filter_map(move |(n, _)| {
            if seen.iter().any(|s| s == n) {
                None
            } else {
                seen.push(n.clone());
                // Combine is guaranteed present (we just saw n).
                self.get(n).map(|v| (n.as_str(), v))
            }
        })
    }

    /// Iterate the combined `(name, value)` pairs sorted by name (byte order)
    /// — the stable serialization form for HTTP send + signature schemes.
    pub fn sorted_iter(&self) -> impl Iterator<Item = (&str, String)> + '_ {
        let mut names: Vec<&str> = self
            .entries
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        // BTreeSet already yields sorted; this is a no-op but keeps the intent
        // explicit + decoupled from the BTreeSet ordering guarantee.
        names.sort();
        names
            .into_iter()
            .filter_map(move |n| self.get(n).map(|v| (n, v)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- name validation -----------------------------------------------

    #[test]
    fn name_lowercases_and_accepts_tokens() {
        assert_eq!(
            validate_header_name("Content-Type").unwrap(),
            "content-type"
        );
        assert_eq!(
            validate_header_name("X-Custom_Foo").unwrap(),
            "x-custom_foo"
        );
        // Every tchar is accepted.
        assert!(validate_header_name("!#$%&'*+-.^_`|~0").is_ok());
    }

    #[test]
    fn name_rejects_non_token_bytes() {
        assert!(validate_header_name("").is_err()); // empty
        assert!(validate_header_name("bad name").is_err()); // space
        assert!(validate_header_name("bad\tname").is_err()); // tab
        assert!(validate_header_name("a:b").is_err()); // colon
        assert!(validate_header_name("a/b").is_err()); // slash
    }

    // --- value validation ----------------------------------------------

    #[test]
    fn value_trows_trims_ows() {
        assert_eq!(validate_header_value("  hi  ").unwrap(), "hi");
        assert_eq!(validate_header_value("\tval\t").unwrap(), "val");
        assert_eq!(validate_header_value("").unwrap(), ""); // empty allowed
    }

    #[test]
    fn value_rejects_injection_and_out_of_range() {
        assert!(validate_header_value("a\rb").is_err());
        assert!(validate_header_value("a\nb").is_err());
        assert!(validate_header_value("a\0b").is_err());
        assert!(validate_header_value("€").is_err()); // € = U+20AC > U+00FF
    }

    #[test]
    fn value_accepts_latin1_range() {
        // Code points up to U+00FF are representable in the Fetch latin1 value
        // model; the network layer encodes them per RFC 9110 obs-text.
        assert!(validate_header_value("naïve").is_ok()); // ï = U+00EF
        assert!(validate_header_value("ÿ").is_ok()); // U+00FF — boundary
    }

    // --- forbidden request headers -------------------------------------

    #[test]
    fn forbidden_request_header_exact_matches() {
        for &name in &["Host", "Referer", "Content-Length", "Cookie", "Set-Cookie"] {
            assert!(
                is_forbidden_request_header(name),
                "{name} should be forbidden"
            );
        }
        // Case-insensitive.
        assert!(is_forbidden_request_header("HOST"));
    }

    #[test]
    fn forbidden_request_header_prefix_rules() {
        assert!(is_forbidden_request_header("Sec-Fetch-Mode"));
        assert!(is_forbidden_request_header("Proxy-Authorization"));
        assert!(is_forbidden_request_header("sec-foo"));
        assert!(is_forbidden_request_header("proxy-x"));
    }

    #[test]
    fn normal_headers_are_not_forbidden() {
        assert!(!is_forbidden_request_header("Content-Type"));
        assert!(!is_forbidden_request_header("Accept"));
        assert!(!is_forbidden_request_header("Authorization"));
        assert!(!is_forbidden_request_header("X-Custom-Header"));
    }

    // --- forbidden response headers ------------------------------------

    #[test]
    fn forbidden_response_header_names() {
        assert!(is_forbidden_response_header_name("Set-Cookie"));
        assert!(is_forbidden_response_header_name("set-cookie2"));
        assert!(is_forbidden_response_header_name("SET-COOKIE"));
        assert!(!is_forbidden_response_header_name("Content-Type"));
    }

    // --- CORS safelist --------------------------------------------------

    #[test]
    fn cors_safelist_accepts_simple_headers() {
        assert!(is_cors_safelisted_request_header("Accept", "text/html"));
        assert!(is_cors_safelisted_request_header(
            "Content-Type",
            "application/x-www-form-urlencoded"
        ));
        assert!(is_cors_safelisted_request_header(
            "Content-Type",
            "text/plain"
        ));
        assert!(is_cors_safelisted_request_header(
            "Content-Language",
            "en-US"
        ));
    }

    #[test]
    fn cors_safelist_rejects_unsafe_content_type() {
        assert!(!is_cors_safelisted_request_header(
            "Content-Type",
            "application/json"
        ));
        assert!(!is_cors_safelisted_request_header(
            "Content-Type",
            "multipart/mixed"
        ));
    }

    #[test]
    fn cors_safelist_rejects_unsafe_bytes_and_size() {
        // CR is a CORS-unsafe byte.
        assert!(!is_cors_safelisted_request_header("Accept", "a\rb"));
        // > 1024 bytes.
        let big = "x".repeat(CORS_SAFELIST_MAX_VALUE_BYTES + 1);
        assert!(!is_cors_safelisted_request_header("Accept", &big));
        // Exactly at the cap is fine.
        let exact = "x".repeat(CORS_SAFELIST_MAX_VALUE_BYTES);
        assert!(is_cors_safelisted_request_header("Accept", &exact));
    }

    #[test]
    fn cors_safelist_rejects_non_safelisted_names() {
        assert!(!is_cors_safelisted_request_header(
            "Authorization",
            "Bearer x"
        ));
        assert!(!is_cors_safelisted_request_header("X-Custom", "v"));
    }

    #[test]
    fn cors_safelist_range_grammar() {
        assert!(is_cors_safelisted_request_header("Range", "bytes=0-1023"));
        assert!(is_cors_safelisted_request_header("Range", "bytes=0-")); // open end
        assert!(!is_cors_safelisted_request_header("Range", "bytes=-1023")); // suffix-range
        assert!(!is_cors_safelisted_request_header(
            "Range",
            "bytes=0-100,200-"
        )); // multi
        assert!(!is_cors_safelisted_request_header("Range", "items=0-10"));
    }

    // --- Headers store: append / combine --------------------------------

    #[test]
    fn append_combines_on_read() {
        let mut h = Headers::new();
        h.append("X-Foo", "a").unwrap();
        h.append("x-foo", "b").unwrap(); // same name, different case
        assert_eq!(h.get("X-Foo").as_deref(), Some("a, b"));
        assert_eq!(h.get_all("X-Foo"), vec!["a", "b"]);
    }

    #[test]
    fn set_replaces_all_entries() {
        let mut h = Headers::new();
        h.append("X", "1").unwrap();
        h.append("X", "2").unwrap();
        h.set("X", "3").unwrap();
        assert_eq!(h.get("X").as_deref(), Some("3"));
        assert_eq!(h.get_all("X"), vec!["3"]);
    }

    #[test]
    fn delete_removes_all_entries() {
        let mut h = Headers::new();
        h.append("X", "1").unwrap();
        h.append("X", "2").unwrap();
        h.append("Y", "3").unwrap();
        h.delete("X");
        assert!(!h.has("X"));
        assert_eq!(h.get("Y").as_deref(), Some("3"));
    }

    #[test]
    fn has_and_get_case_insensitive() {
        let mut h = Headers::new();
        h.append("Content-Type", "text/html").unwrap();
        assert!(h.has("content-type"));
        assert!(h.has("CONTENT-TYPE"));
        assert_eq!(h.get("CONTENT-type").as_deref(), Some("text/html"));
    }

    #[test]
    fn iter_preserves_insertion_order_and_combines() {
        let mut h = Headers::new();
        h.append("B", "1").unwrap();
        h.append("A", "x").unwrap();
        h.append("B", "2").unwrap();
        let collected: Vec<_> = h.iter().collect();
        assert_eq!(
            collected,
            vec![("b", "1, 2".to_owned()), ("a", "x".to_owned())]
        );
    }

    #[test]
    fn sorted_iter_is_byte_order() {
        let mut h = Headers::new();
        h.append("Z", "1").unwrap();
        h.append("a", "2").unwrap();
        h.append("A", "3").unwrap();
        // Byte order: lowercase letters, 'a' (0x61) < 'z' (0x7a).
        let names: Vec<_> = h.sorted_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["a", "z"]); // all names lowercased on insert
        // 'A' was lowercased to 'a' so it merged with the existing 'a',
        // preserving insertion order ("2" first, then "3").
        assert_eq!(h.get("A").as_deref(), Some("2, 3"));
    }

    #[test]
    fn len_counts_distinct_names() {
        let mut h = Headers::new();
        h.append("A", "1").unwrap();
        h.append("A", "2").unwrap();
        h.append("B", "3").unwrap();
        assert_eq!(h.len(), 2);
        assert!(!h.is_empty());
    }

    #[test]
    fn from_records_validates() {
        let h = Headers::from_records([
            ("Content-Type".to_string(), "text/html".to_string()),
            ("X-Foo".to_string(), "bar".to_string()),
        ])
        .unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h.get("content-type").as_deref(), Some("text/html"));

        // An invalid name fails the whole build.
        assert!(Headers::from_records([("bad name".to_string(), "v".to_string())]).is_err());
    }

    #[test]
    fn append_rejects_invalid_input() {
        let mut h = Headers::new();
        assert!(h.append("bad name", "v").is_err());
        assert!(h.append("good", "bad\nvalue").is_err());
        assert!(h.is_empty()); // rejected append didn't mutate
    }

    #[test]
    fn empty_headers() {
        let h = Headers::new();
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
        assert_eq!(h.get("anything"), None);
        assert!(!h.has("anything"));
    }
}
