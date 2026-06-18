//! WHATWG MIME type parsing & serialization — Phase 6 host-bindings prep
//! (docs/PLAN.md Phase 6 step 5 + Phase 5 paint). Implements MIME Sniffing
//! § 2.1 "parse a MIME type" + § 2.2 "serialize a MIME type" so the network
//! layer (`Content-Type`), `fetch()`/`XHR` (`.type`/`overrideMimeType`), and
//! `<object>`/`<embed>` plugin negotiation all share one source of truth.
//!
//! What lives here:
//! - [`MimeType`] — the parsed record: lowercased `type` + lowercased
//!   `subtype` + an ordered `parameters` map (first occurrence of a name
//!   wins, per § 2.1 step 12).
//! - [`MimeType::parse`] — § 2.1: leading/trailing HTTP whitespace trim,
//!   `type` up to `/`, `subtype` up to `;`, then `;`-separated `name=value`
//!   parameters with quoted-string support (RFC 9110 § 3.2.6).
//! - [`MimeType::serialize`] — § 2.2: `type/subtype` then `;name=value` for
//!   each parameter (quoting values that aren't pure HTTP token chars).
//! - [`MimeType::essence`] — the `type/subtype` concatenation (the most-used
//!   accessor; MIME § 2.4 "MIME type essence").
//!
//! What does *not* live here:
//! - MIME *sniffing* (computing the effective type from response bytes).
//!   That lives in the network layer once it can see real response bodies.
//! - `charset` resolution. Parameters are stored verbatim; the host hook
//!   reads `parameters["charset"]` and decides what to do with it.
//! - `Data:` URL media-type parsing. The `data:` parser will call into this.
//!
//! ## Trust boundary
//!
//! `Content-Type` arrives from the network (untrusted). [`MimeType::parse`]
//! never panics on malformed input — it returns `None` on every spec failure
//! step so the caller can fall back to a safe default (e.g. `text/plain`).
//!
//! Reference: <https://mimesniff.spec.whatwg.org/#parsing-a-mime-type>.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// A parsed MIME type record (MIME Sniffing § 2.1). `type` and `subtype` are
/// lowercased on parse; `parameters` is an ordered map keyed by lowercased
/// parameter name, with only the first occurrence of a duplicate name kept.
///
/// The ordered map is exposed as a [`BTreeMap`] for stable, deterministic
/// iteration (and trivial `PartialEq`); browsers preserve insertion order,
/// but for Vixen the deterministic alphabetic order makes round-trip tests
/// robust. The difference is observable only for parameters whose names
/// share a prefix — none in practice.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MimeType {
    /// The major type, ASCII-lowercased (e.g. `"text"`, `"application"`).
    pub r#type: String,
    /// The subtype, ASCII-lowercased (e.g. `"html"`, `"json"`).
    pub subtype: String,
    /// Parameters, keyed by ASCII-lowercased name. First occurrence wins.
    pub parameters: BTreeMap<String, String>,
}

impl MimeType {
    /// Parse a MIME type string per MIME Sniffing § 2.1. Returns `None` if
    /// the input is missing the `type`/`subtype`, contains a non-token code
    /// point in either, or has no `/` separator.
    ///
    /// ```
    /// # use vixen_core::mime::MimeType;
    /// let m = MimeType::parse("text/html; charset=utf-8").unwrap();
    /// assert_eq!(m.essence(), "text/html");
    /// assert_eq!(m.parameters.get("charset").map(|s| s.as_str()), Some("utf-8"));
    /// ```
    pub fn parse(input: &str) -> Option<Self> {
        // Step 1: trim leading/trailing HTTP whitespace.
        let input = input.trim_matches(is_http_whitespace);
        let bytes = input; // rename for readability
        let mut pos = 0;
        let chars: Vec<char> = bytes.chars().collect();

        // Step 2: collect type up to '/'.
        let mut type_str = String::new();
        while pos < chars.len() && chars[pos] != '/' {
            type_str.push(chars[pos]);
            pos += 1;
        }
        // Step 3: must have the '/'.
        if pos >= chars.len() {
            return None;
        }
        // Step 4: strip + lowercase type.
        type_str = type_str
            .trim_matches(is_http_whitespace)
            .to_ascii_lowercase();
        if type_str.is_empty() || !type_str.chars().all(is_http_token_code_point) {
            return None;
        }

        // Step 5: skip the '/'.
        pos += 1;

        // Step 6: collect subtype up to ';'.
        let mut subtype_str = String::new();
        while pos < chars.len() && chars[pos] != ';' {
            subtype_str.push(chars[pos]);
            pos += 1;
        }
        // Step 7: strip + lowercase subtype.
        subtype_str = subtype_str
            .trim_matches(is_http_whitespace)
            .to_ascii_lowercase();
        if subtype_str.is_empty() || !subtype_str.chars().all(is_http_token_code_point) {
            return None;
        }

        let mut mime = MimeType {
            r#type: type_str,
            subtype: subtype_str,
            parameters: BTreeMap::new(),
        };

        // Step 8: parameter loop.
        while pos < chars.len() {
            // Skip the ';' (we stopped at one).
            if chars[pos] != ';' {
                // Defensive: malformed input past subtype — bail.
                break;
            }
            pos += 1;
            // Skip ASCII whitespace before the name.
            while pos < chars.len() && is_http_whitespace(chars[pos]) {
                pos += 1;
            }
            // Collect name up to ';' or '='.
            let mut name = String::new();
            while pos < chars.len() && chars[pos] != ';' && chars[pos] != '=' {
                name.push(chars[pos]);
                pos += 1;
            }
            name = name.to_ascii_lowercase();
            // If the next char is '=', parse a value.
            let mut value = String::new();
            if pos < chars.len() && chars[pos] == '=' {
                pos += 1;
                if pos < chars.len() && chars[pos] == '"' {
                    // Quoted-string (RFC 9110 § 3.2.6): consume up to closing
                    // `"`, with backslash-pair escaping.
                    pos += 1;
                    while pos < chars.len() && chars[pos] != '"' {
                        if chars[pos] == '\\' && pos + 1 < chars.len() {
                            // Escaped: take the next char literally.
                            pos += 1;
                            value.push(chars[pos]);
                        } else {
                            value.push(chars[pos]);
                        }
                        pos += 1;
                    }
                    // Skip past closing `"` if present.
                    if pos < chars.len() && chars[pos] == '"' {
                        pos += 1;
                    }
                } else {
                    // Unquoted: collect up to ';', then strip whitespace.
                    let mut raw = String::new();
                    while pos < chars.len() && chars[pos] != ';' {
                        raw.push(chars[pos]);
                        pos += 1;
                    }
                    value = raw.trim_matches(is_http_whitespace).to_string();
                }
            }
            // Step 8.d: only the first occurrence of a name is kept.
            if !name.is_empty() && !mime.parameters.contains_key(&name) {
                mime.parameters.insert(name, value);
            }
            // Continue; next iteration handles the next `;`.
        }

        Some(mime)
    }

    /// Serialise the MIME type per MIME Sniffing § 2.2: `type/subtype` then
    /// `;name=value` for each parameter, quoting values that aren't pure
    /// HTTP token chars.
    ///
    /// ```
    /// # use vixen_core::mime::MimeType;
    /// let m = MimeType::parse("text/html; charset=utf-8").unwrap();
    /// assert_eq!(m.serialize(), "text/html;charset=utf-8");
    /// ```
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.r#type);
        out.push('/');
        out.push_str(&self.subtype);
        for (name, value) in &self.parameters {
            out.push(';');
            out.push_str(name);
            if !value.is_empty() {
                out.push('=');
                if value.chars().all(is_http_token_code_point) {
                    out.push_str(value);
                } else {
                    // Quoted-string: escape `\` and `"`, wrap in `"`.
                    out.push('"');
                    for c in value.chars() {
                        match c {
                            '\\' | '"' => {
                                out.push('\\');
                                out.push(c);
                            }
                            _ => out.push(c),
                        }
                    }
                    out.push('"');
                }
            }
        }
        out
    }

    /// The MIME type essence (MIME Sniffing § 2.4): `type/subtype` without
    /// parameters. This is the form most code compares against (e.g.
    /// `if essence == "text/html"`).
    pub fn essence(&self) -> String {
        format!("{}/{}", self.r#type, self.subtype)
    }

    /// Convenience: `true` when `essence()` equals `expected` (e.g. `"text/html"`).
    pub fn is_essence(&self, expected: &str) -> bool {
        self.essence() == expected
    }
}

// ---------------------------------------------------------------------------
// Code-point tables (RFC 9110 § 3.2.3 + WHATWG MIME § 2.1.1)
// ---------------------------------------------------------------------------

/// HTTP whitespace per WHATWG (SP / HTAB / CR / LF). Note MIME parsing uses
/// this for trimming; inside parameter values only SP/HTAB are "ASCII
/// whitespace" for stripping. We use the union for trim simplicity.
fn is_http_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// HTTP `tchar` (RFC 9110 § 3.2.3) — the code points allowed in a MIME
/// `type`, `subtype`, or parameter name without quoting.
fn is_http_token_code_point(c: char) -> bool {
    matches!(
        c,
        '!' | '#' | '$' | '%' | '&' | '\'' | '*' | '+' | '-' | '.' | '^' | '_' | '`' | '|' | '~'
    ) || c.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param<'a>(m: &'a MimeType, name: &str) -> Option<&'a str> {
        m.parameters.get(name).map(|s| s.as_str())
    }

    // --- parse: happy path ----------------------------------------------

    #[test]
    fn parse_simple_type_subtype() {
        let m = MimeType::parse("text/html").unwrap();
        assert_eq!(m.r#type, "text");
        assert_eq!(m.subtype, "html");
        assert!(m.parameters.is_empty());
    }

    #[test]
    fn parse_one_parameter() {
        let m = MimeType::parse("text/html; charset=utf-8").unwrap();
        assert_eq!(m.essence(), "text/html");
        assert_eq!(param(&m, "charset"), Some("utf-8"));
    }

    #[test]
    fn parse_multiple_parameters() {
        let m = MimeType::parse("text/html; charset=utf-8; boundary=abc").unwrap();
        assert_eq!(param(&m, "charset"), Some("utf-8"));
        assert_eq!(param(&m, "boundary"), Some("abc"));
    }

    #[test]
    fn parse_parameter_without_value() {
        // A flag parameter: no `=`, value defaults to empty.
        let m = MimeType::parse("application/octet-stream; flag").unwrap();
        assert_eq!(param(&m, "flag"), Some(""));
    }

    #[test]
    fn parse_parameter_with_empty_value() {
        let m = MimeType::parse("text/plain; charset=").unwrap();
        assert_eq!(param(&m, "charset"), Some(""));
    }

    // --- parse: case + whitespace normalisation -------------------------

    #[test]
    fn parse_lowercases_type_and_subtype() {
        let m = MimeType::parse("TEXT/HTML").unwrap();
        assert_eq!(m.r#type, "text");
        assert_eq!(m.subtype, "html");
    }

    #[test]
    fn parse_lowercases_parameter_names_not_values() {
        // Names are case-insensitive (lowercased); values are case-sensitive
        // (preserved). RFC 9110 § 3.1.1.1.
        let m = MimeType::parse("text/html; CHARSET=UTF-8").unwrap();
        assert_eq!(param(&m, "charset"), Some("UTF-8"));
    }

    #[test]
    fn parse_trims_leading_and_trailing_whitespace() {
        let m = MimeType::parse("  text/html  ").unwrap();
        assert_eq!(m.essence(), "text/html");
    }

    #[test]
    fn parse_tolerates_whitespace_around_type_and_subtype() {
        let m = MimeType::parse("text / html").unwrap();
        assert_eq!(m.essence(), "text/html");
    }

    #[test]
    fn parse_tolerates_whitespace_around_parameter_value() {
        let m = MimeType::parse("text/html; charset= utf-8 ").unwrap();
        assert_eq!(param(&m, "charset"), Some("utf-8"));
    }

    // --- parse: quoted-string values ------------------------------------

    #[test]
    fn parse_quoted_string_value() {
        let m = MimeType::parse(r#"text/plain; name="hello world""#).unwrap();
        assert_eq!(param(&m, "name"), Some("hello world"));
    }

    #[test]
    fn parse_quoted_string_with_special_chars() {
        // A value containing `;` must be quoted (it'd otherwise terminate).
        let m = MimeType::parse(r#"multipart/form-data; boundary="a;b""#).unwrap();
        assert_eq!(param(&m, "boundary"), Some("a;b"));
    }

    #[test]
    fn parse_quoted_string_backslash_escape() {
        // RFC 9110 § 3.2.6 quoted-pair: `\` escapes the next char.
        let m = MimeType::parse(r#"text/plain; name="a\"b""#).unwrap();
        assert_eq!(param(&m, "name"), Some(r#"a"b"#));
    }

    #[test]
    fn parse_quoted_string_missing_close_quote_tolerant() {
        // Unterminated quoted-string: consume to end (browsers are lenient).
        let m = MimeType::parse(r#"text/plain; name="abc"#).unwrap();
        assert_eq!(param(&m, "name"), Some("abc"));
    }

    // --- parse: duplicate parameters ------------------------------------

    #[test]
    fn parse_first_occurrence_of_duplicate_parameter_wins() {
        let m = MimeType::parse("text/html; charset=latin-1; charset=utf-8").unwrap();
        assert_eq!(param(&m, "charset"), Some("latin-1"));
    }

    // --- parse: failures ------------------------------------------------

    #[test]
    fn parse_missing_slash_returns_none() {
        assert!(MimeType::parse("texthtml").is_none());
    }

    #[test]
    fn parse_empty_type_returns_none() {
        assert!(MimeType::parse("/html").is_none());
    }

    #[test]
    fn parse_empty_subtype_returns_none() {
        assert!(MimeType::parse("text/").is_none());
    }

    #[test]
    fn parse_invalid_token_char_in_type_returns_none() {
        // `/` inside the type is the separator; a space mid-type breaks too.
        assert!(MimeType::parse("te xt/html").is_none());
    }

    #[test]
    fn parse_empty_input_returns_none() {
        assert!(MimeType::parse("").is_none());
        assert!(MimeType::parse("   ").is_none());
    }

    // --- serialize ------------------------------------------------------

    #[test]
    fn serialize_round_trip_simple() {
        let m = MimeType::parse("text/html").unwrap();
        assert_eq!(m.serialize(), "text/html");
    }

    #[test]
    fn serialize_round_trip_with_parameter() {
        let m = MimeType::parse("text/html; charset=utf-8").unwrap();
        assert_eq!(m.serialize(), "text/html;charset=utf-8");
    }

    #[test]
    fn serialize_quotes_value_with_special_chars() {
        // A value containing a space (not a token char) gets quoted.
        let mut m = MimeType::parse("text/plain").unwrap();
        m.parameters
            .insert("name".to_string(), "hello world".to_string());
        assert_eq!(m.serialize(), r#"text/plain;name="hello world""#);
    }

    #[test]
    fn serialize_escapes_quote_and_backslash_in_quoted_value() {
        let mut m = MimeType::parse("text/plain").unwrap();
        m.parameters
            .insert("name".to_string(), r#"a"b\c"#.to_string());
        assert_eq!(m.serialize(), r#"text/plain;name="a\"b\\c""#);
    }

    #[test]
    fn serialize_token_value_unquoted() {
        let m = MimeType::parse("text/html; charset=utf-8").unwrap();
        // `utf-8` is pure token chars → unquoted.
        assert_eq!(m.serialize(), "text/html;charset=utf-8");
    }

    #[test]
    fn serialize_empty_value_omits_equals() {
        let m = MimeType::parse("application/octet-stream; flag").unwrap();
        // Empty value → just the name, no `=`.
        assert_eq!(m.serialize(), "application/octet-stream;flag");
    }

    // --- essence --------------------------------------------------------

    #[test]
    fn essence_drops_parameters() {
        let m = MimeType::parse("text/html; charset=utf-8; boundary=xyz").unwrap();
        assert_eq!(m.essence(), "text/html");
        assert!(m.is_essence("text/html"));
        assert!(!m.is_essence("text/plain"));
    }

    #[test]
    fn essence_for_vendor_subtype() {
        let m = MimeType::parse("application/vnd.api+json").unwrap();
        assert_eq!(m.essence(), "application/vnd.api+json");
    }

    // --- Real-world Content-Type headers --------------------------------

    #[test]
    fn parse_typical_content_type_headers() {
        for (raw, expected_essence) in [
            ("text/html; charset=utf-8", "text/html"),
            ("application/json", "application/json"),
            ("application/ld+json", "application/ld+json"),
            ("image/png", "image/png"),
            (
                "multipart/form-data; boundary=----WebKitFormBoundary",
                "multipart/form-data",
            ),
            ("text/css", "text/css"),
            (
                "application/javascript; charset=UTF-8",
                "application/javascript",
            ),
        ] {
            let m = MimeType::parse(raw).unwrap_or_else(|| panic!("failed to parse {raw:?}"));
            assert_eq!(m.essence(), expected_essence, "for {raw:?}");
        }
    }

    #[test]
    fn parse_form_data_boundary_with_special_chars() {
        // Boundaries with hyphens are common; the value is a token.
        let m = MimeType::parse("multipart/form-data; boundary=----vixen123").unwrap();
        assert_eq!(param(&m, "boundary"), Some("----vixen123"));
    }

    // --- Parse-then-serialize stability ---------------------------------

    #[test]
    fn parse_serialize_round_trip_preserves_essence_and_params() {
        for raw in [
            "text/html; charset=utf-8",
            "application/json",
            "multipart/form-data; boundary=abc",
            "text/plain; name=\"hello world\"",
        ] {
            let m = MimeType::parse(raw).unwrap();
            let reserialized = m.serialize();
            let m2 = MimeType::parse(&reserialized).unwrap();
            assert_eq!(m.r#type, m2.r#type, "type mismatch for {raw:?}");
            assert_eq!(m.subtype, m2.subtype, "subtype mismatch for {raw:?}");
            assert_eq!(m.parameters, m2.parameters, "params mismatch for {raw:?}");
        }
    }
}
