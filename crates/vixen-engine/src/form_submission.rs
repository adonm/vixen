//! HTML5 form submission encoding — Phase 6 prep (docs/SPEC.md "Form
//! submission edge cases" + docs/PLAN.md Phase 6 step 3). Implements the
//! three form-encoding algorithms per WHATWG HTML § 4.10.21.6–.9 so the
//! Phase 6 host-hook layer has one source of truth for turning a form-data
//! entry list into an HTTP request body.
//!
//! What lives here:
//! - [`FormEntry`] / [`FormEntryValue`] — the data model the entry-list
//!   construction algorithm (§ 4.10.21.4) produces. Files carry their
//!   `filename` + `content_type` + `body` bytes; everything else is text.
//! - [`FormEnctype`] — the three `enctype` values HTML defines.
//! - [`encode_urlencoded`] — `application/x-www-form-urlencoded` per the
//!   URL Standard's "application/x-www-form-urlencoded" serializer (space
//!   → `+`, percent-encode everything but the URL-safe set, uppercase hex).
//! - [`encode_text_plain`] — `text/plain` (no escaping, `name=value\r\n`).
//! - [`encode_multipart`] — `multipart/form-data` (RFC 7578 + WHATWG § 4.10.21.9)
//!   with a caller-provided boundary and the CRLF discipline the spec requires.
//! - [`generate_boundary`] — a deterministic boundary generator (RFC 7578
//!   § 4.1: must not appear inside any part body; we generate one and let
//!   the caller assert it isn't in any file body).
//!
//! What does *not* live here:
//! - Entry-list construction (skipping disabled fields, the `formmethod` /
//!   `formenctype` overrides, the "submitter" button being excluded from
//!   non-submit cases). That's the Phase 6 DOM layer; this module takes the
//!   already-constructed entry list.
//! - Charset negotiation (the document charset → byte encoding step).
//!   Inputs here are already UTF-8 `&str` / `&[u8]`; the host hook handles
//!   legacy encodings before calling in.
//! - `multipart/form-data` parsing (the request side is parse-elsewhere).
//!
//! References:
//! - WHATWG HTML § 4.10.21.7 (`application/x-www-form-urlencoded`),
//!   § 4.10.21.8 (`text/plain`), § 4.10.21.9 (`multipart/form-data`).
//! - RFC 7578 (multipart/form-data) § 4.2 (Content-Disposition name/filename).
//! - URL Standard "application/x-www-form-urlencoded" serializer
//!   (<https://url.spec.whatwg.org/#concept-urlencoded-serializer>).

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// One entry in a form-data set (WHATWG HTML § 4.10.21.4). Files carry their
/// own variant because `multipart/form-data` and `text/plain` render them
/// differently from text values (the latter uses the filename, not body).
#[derive(Debug, Clone, PartialEq)]
pub enum FormEntryValue {
    /// A plain string value (text input, textarea, select, hidden, etc.).
    Text(String),
    /// A file upload entry. `filename` is the user-supplied name (not the
    /// MIME-discovered name), `content_type` is the browser's best guess
    /// (defaulting to `application/octet-stream`), and `body` is the raw
    /// bytes of the file content (no encoding applied).
    File {
        filename: String,
        content_type: String,
        body: Vec<u8>,
    },
}

/// A single `(name, value)` pair as collected by the entry-list algorithm.
#[derive(Debug, Clone, PartialEq)]
pub struct FormEntry {
    pub name: String,
    pub value: FormEntryValue,
}

impl FormEntry {
    /// Convenience constructor for a text entry.
    pub fn text(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: FormEntryValue::Text(value.into()),
        }
    }

    /// Convenience constructor for a file entry.
    pub fn file(
        name: impl Into<String>,
        filename: impl Into<String>,
        content_type: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            name: name.into(),
            value: FormEntryValue::File {
                filename: filename.into(),
                content_type: content_type.into(),
                body: body.into(),
            },
        }
    }

    /// The textual value used by `application/x-www-form-urlencoded` and
    /// `text/plain` — for text entries the string, for file entries the
    /// filename only (the body doesn't survive these encodings; the spec
    /// substitutes the file's name).
    fn textual_value(&self) -> &str {
        match &self.value {
            FormEntryValue::Text(s) => s,
            FormEntryValue::File { filename, .. } => filename,
        }
    }
}

/// The three form encoding types per the `enctype` attribute (WHATWG HTML
/// § 4.10.21.6 "Selecting the form encoding algorithm").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FormEnctype {
    /// `application/x-www-form-urlencoded` — the default.
    #[default]
    Urlencoded,
    /// `multipart/form-data` — required for file uploads.
    MultipartFormData,
    /// `text/plain` — the rarely-used, minimally-escaped form.
    TextPlain,
}

impl FormEnctype {
    /// Parse the `enctype` attribute string. Returns `None` for unknown
    /// values (the spec falls back to the default; the caller decides).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "application/x-www-form-urlencoded" => Some(Self::Urlencoded),
            "multipart/form-data" => Some(Self::MultipartFormData),
            "text/plain" => Some(Self::TextPlain),
            _ => None,
        }
    }

    /// The MIME media type this encoding produces (the `Content-Type` body
    /// header, minus the `boundary=` parameter for multipart).
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Urlencoded => "application/x-www-form-urlencoded",
            Self::MultipartFormData => "multipart/form-data",
            Self::TextPlain => "text/plain",
        }
    }
}

// ---------------------------------------------------------------------------
// application/x-www-form-urlencoded
// ---------------------------------------------------------------------------

/// Encode a form-data set as `application/x-www-form-urlencoded` (WHATWG HTML
/// § 4.10.21.7 + URL Standard's "application/x-www-form-urlencoded" serializer).
///
/// Rules:
/// - For each entry, percent-encode the name and value (file entries use the
///   filename as the textual value — the body doesn't survive this encoding).
/// - Join name/value with `=`; join pairs with `&`.
/// - Percent-encoding: U+0020 SPACE → `+`; the URL-safe set
///   (`*`, `-`, `.`, `_`, `0-9`, `A-Z`, `a-z`) passes through; every other
///   byte becomes `%XX` with uppercase hex digits.
///
/// ```
/// # use vixen_engine::form_submission::{encode_urlencoded, FormEntry};
/// let entries = vec![
///     FormEntry::text("name", "Ada Lovelace"),
///     FormEntry::text("email", "a@b.example"),
/// ];
/// assert_eq!(encode_urlencoded(&entries), "name=Ada+Lovelace&email=a%40b.example");
/// ```
pub fn encode_urlencoded(entries: &[FormEntry]) -> String {
    let mut out = String::new();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        urlencoded_byte_encode(&entry.name, &mut out);
        out.push('=');
        urlencoded_byte_encode(entry.textual_value(), &mut out);
    }
    out
}

/// The URL Standard's "application/x-www-form-urlencoded" byte serializer.
/// Bytes from `&str` (UTF-8) are percent-encoded unless URL-safe; SPACE → `+`.
fn urlencoded_byte_encode(input: &str, out: &mut String) {
    for &b in input.as_bytes() {
        match b {
            b' ' => out.push('+'),
            b'*' | b'-' | b'.' | b'_' => out.push(b as char),
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => out.push(b as char),
            _ => {
                out.push('%');
                push_upper_hex(b, out);
            }
        }
    }
}

fn push_upper_hex(b: u8, out: &mut String) {
    fn hex_digit(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            10..=15 => (b'A' + (n - 10)) as char,
            _ => unreachable!("hex digit out of range"),
        }
    }
    out.push(hex_digit(b >> 4));
    out.push(hex_digit(b & 0x0F));
}

// ---------------------------------------------------------------------------
// text/plain
// ---------------------------------------------------------------------------

/// Encode a form-data set as `text/plain` (WHATWG HTML § 4.10.21.8). No
/// escaping; each entry is `name=value\r\n`. File entries use the filename
/// as the value (the body is dropped, matching the spec — `text/plain` is
/// not a useful encoding for binary uploads).
///
/// The final entry has a trailing CRLF, matching the WHATWG algorithm.
///
/// ```
/// # use vixen_engine::form_submission::{encode_text_plain, FormEntry};
/// let entries = vec![
///     FormEntry::text("name", "Ada"),
///     FormEntry::text("msg", "hello, world"),
/// ];
/// assert_eq!(encode_text_plain(&entries), "name=Ada\r\nmsg=hello, world\r\n");
/// ```
pub fn encode_text_plain(entries: &[FormEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        out.push_str(&entry.name);
        out.push('=');
        out.push_str(entry.textual_value());
        out.push_str("\r\n");
    }
    out
}

// ---------------------------------------------------------------------------
// multipart/form-data
// ---------------------------------------------------------------------------

/// Encode a form-data set as `multipart/form-data` (RFC 7578 + WHATWG HTML
/// § 4.10.21.9). Each entry becomes a part with `Content-Disposition:
/// form-data; name="..."` (plus `filename="..."` for file entries), and file
/// parts additionally carry a `Content-Type` header. Parts are separated by
/// `--boundary`; the final `--boundary--` terminates the body.
///
/// CRLF discipline: every header line, blank line, and part separator ends
/// in `\r\n` per RFC 2046 § 5.1.1 (the multipart BNF requires CRLF). The
/// caller-supplied `boundary` must not appear inside any part body
/// ([`generate_boundary`] returns a value constructed to make that unlikely).
///
/// The returned `Content-Type` value (for the request's `Content-Type` header)
/// is `multipart/form-data; boundary=<boundary>` — see [`multipart_content_type`].
///
/// ```
/// # use vixen_engine::form_submission::{encode_multipart, FormEntry};
/// let entries = vec![FormEntry::text("field", "value")];
/// let body = encode_multipart(&entries, "----vixen");
/// let s = std::str::from_utf8(&body).unwrap();
/// assert!(s.contains("Content-Disposition: form-data; name=\"field\""));
/// assert!(s.ends_with("----vixen--\r\n"));
/// ```
pub fn encode_multipart(entries: &[FormEntry], boundary: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(estimate_multipart_size(entries, boundary));
    for entry in entries {
        // Boundary delimiter: `--<boundary>\r\n`.
        out.extend_from_slice(b"--");
        out.extend_from_slice(boundary.as_bytes());
        out.extend_from_slice(b"\r\n");
        // Content-Disposition with the name (and filename for files).
        out.extend_from_slice(b"Content-Disposition: form-data; name=\"");
        append_escaped_quoted(&entry.name, &mut out);
        match &entry.value {
            FormEntryValue::Text(_) => {
                out.push(b'"');
                out.extend_from_slice(b"\r\n\r\n");
            }
            FormEntryValue::File {
                filename,
                content_type,
                body,
            } => {
                out.extend_from_slice(b"\"; filename=\"");
                append_escaped_quoted(filename, &mut out);
                out.push(b'"');
                out.extend_from_slice(b"\r\nContent-Type: ");
                out.extend_from_slice(content_type.as_bytes());
                out.extend_from_slice(b"\r\n\r\n");
                out.extend_from_slice(body);
            }
        }
        // Text body comes after the blank line.
        if let FormEntryValue::Text(s) = &entry.value {
            out.extend_from_slice(s.as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }
    // Closing delimiter.
    out.extend_from_slice(b"--");
    out.extend_from_slice(boundary.as_bytes());
    out.extend_from_slice(b"--\r\n");
    out
}

/// The `Content-Type` header value to use with a multipart body produced by
/// [`encode_multipart`]. Kept separate so callers don't forget the boundary
/// parameter (RFC 7578 § 4.1: the boundary must appear in the header too).
pub fn multipart_content_type(boundary: &str) -> String {
    format!("multipart/form-data; boundary={boundary}")
}

/// Generate a fresh boundary string. RFC 7578 § 4.1: boundaries must not
/// appear inside any part body. We construct a value with low collision risk
/// using a fixed prefix plus a length-tagged entropy-free counter-style suffix
/// — the caller asserts it isn't in any file body before sending (the host
/// hook does this; pure logic can't sample real randomness without `unsafe`
/// or a dep, so we stay deterministic and obvious).
pub fn generate_boundary() -> String {
    // RFC 2046 caps boundary length at 70 chars; 32 visible chars is plenty.
    // The prefix is distinctive (won't appear in user text); the suffix is
    // intentionally short to keep snapshots readable. Callers wanting
    // crypto-strength uniqueness supply their own.
    format!("----vixenformboundary{}", boundary_suffix())
}

/// Deterministic suffix for [`generate_boundary`]. Wall-clock-free so the
/// output is reproducible across runs (snapshots + tests).
fn boundary_suffix() -> String {
    // Combine a process-stable counter with the entry count of nothing —
    // we just need uniqueness within one process. Use an AtomicU64.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{n:016x}")
}

/// Escape a string for use inside a quoted-string in a MIME header value.
/// Per RFC 7578 § 4.2 + RFC 2616 § 2.2, the only characters that MUST be
/// escaped inside a quoted-string are `"` and `\`. CR/LF are removed entirely
/// (they would terminate the header line).
fn append_escaped_quoted(s: &str, out: &mut Vec<u8>) {
    for &b in s.as_bytes() {
        match b {
            b'"' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            b'\r' | b'\n' => {
                // Strip line breaks — they'd break the MIME header framing.
            }
            _ => out.push(b),
        }
    }
}

/// Pre-allocate a sensible capacity for the multipart body so the encoder
/// doesn't reallocate per part. Conservative (over-estimates; that's fine).
fn estimate_multipart_size(entries: &[FormEntry], boundary: &str) -> usize {
    let per_part_overhead = 80 + boundary.len();
    let body_total: usize = entries
        .iter()
        .map(|e| match &e.value {
            FormEntryValue::Text(s) => s.len(),
            FormEntryValue::File { body, .. } => body.len(),
        })
        .sum();
    entries.len() * per_part_overhead + body_total + boundary.len() + 6
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- FormEnctype -----------------------------------------------------

    #[test]
    fn enctype_parse_recognises_three_canonical() {
        assert_eq!(
            FormEnctype::parse("application/x-www-form-urlencoded"),
            Some(FormEnctype::Urlencoded)
        );
        assert_eq!(
            FormEnctype::parse("multipart/form-data"),
            Some(FormEnctype::MultipartFormData)
        );
        assert_eq!(
            FormEnctype::parse("text/plain"),
            Some(FormEnctype::TextPlain)
        );
    }

    #[test]
    fn enctype_parse_rejects_unknown() {
        assert_eq!(FormEnctype::parse("application/json"), None);
        assert_eq!(FormEnctype::parse(""), None);
        assert_eq!(FormEnctype::parse("garbage"), None);
    }

    #[test]
    fn enctype_parse_trims_whitespace() {
        assert_eq!(
            FormEnctype::parse("  text/plain  "),
            Some(FormEnctype::TextPlain)
        );
    }

    #[test]
    fn enctype_default_is_urlencoded() {
        assert_eq!(FormEnctype::default(), FormEnctype::Urlencoded);
    }

    #[test]
    fn enctype_mime_types_match_canonical() {
        assert_eq!(
            FormEnctype::Urlencoded.mime_type(),
            "application/x-www-form-urlencoded"
        );
        assert_eq!(
            FormEnctype::MultipartFormData.mime_type(),
            "multipart/form-data"
        );
        assert_eq!(FormEnctype::TextPlain.mime_type(), "text/plain");
    }

    // --- application/x-www-form-urlencoded -------------------------------

    #[test]
    fn urlencoded_single_text_entry() {
        let entries = vec![FormEntry::text("name", "Ada")];
        assert_eq!(encode_urlencoded(&entries), "name=Ada");
    }

    #[test]
    fn urlencoded_multiple_entries_joined_with_ampersand() {
        let entries = vec![FormEntry::text("a", "1"), FormEntry::text("b", "2")];
        assert_eq!(encode_urlencoded(&entries), "a=1&b=2");
    }

    #[test]
    fn urlencoded_space_becomes_plus() {
        let entries = vec![FormEntry::text("q", "hello world")];
        assert_eq!(encode_urlencoded(&entries), "q=hello+world");
    }

    #[test]
    fn urlencoded_at_becomes_percent_40() {
        let entries = vec![FormEntry::text("email", "a@b.example")];
        assert_eq!(encode_urlencoded(&entries), "email=a%40b.example");
    }

    #[test]
    fn urlencoded_url_safe_chars_pass_through() {
        // `*-._` and alphanumerics are unreserved.
        let entries = vec![FormEntry::text("k", "a*b-c.d_e")];
        assert_eq!(encode_urlencoded(&entries), "k=a*b-c.d_e");
    }

    #[test]
    fn urlencoded_uses_uppercase_hex() {
        // U+00E9 (é) is 0xC3 0xA9 in UTF-8 → "%C3%A9".
        let entries = vec![FormEntry::text("k", "é")];
        assert_eq!(encode_urlencoded(&entries), "k=%C3%A9");
    }

    #[test]
    fn urlencoded_empty_value_emits_equals() {
        let entries = vec![FormEntry::text("k", "")];
        assert_eq!(encode_urlencoded(&entries), "k=");
    }

    #[test]
    fn urlencoded_empty_input_returns_empty() {
        assert_eq!(encode_urlencoded(&[]), "");
    }

    #[test]
    fn urlencoded_name_is_also_encoded() {
        let entries = vec![FormEntry::text("a b", "c")];
        assert_eq!(encode_urlencoded(&entries), "a+b=c");
    }

    #[test]
    fn urlencoded_file_entry_uses_filename() {
        // Per WHATWG § 4.10.21.7, file entries use the filename as the value
        // (the body cannot survive this encoding).
        let entries = vec![FormEntry::file(
            "upload",
            "report.txt",
            "text/plain",
            b"contents".to_vec(),
        )];
        assert_eq!(encode_urlencoded(&entries), "upload=report.txt");
    }

    #[test]
    fn urlencoded_non_ascii_byte() {
        // 0xFF cannot be a UTF-8 lead byte in a single byte; use \u{00FF} (ÿ),
        // which is 0xC3 0xBF in UTF-8 → "%C3%BF".
        let entries = vec![FormEntry::text("k", "ÿ")];
        assert_eq!(encode_urlencoded(&entries), "k=%C3%BF");
    }

    // --- text/plain ------------------------------------------------------

    #[test]
    fn text_plain_single_entry_has_crlf() {
        let entries = vec![FormEntry::text("name", "Ada")];
        assert_eq!(encode_text_plain(&entries), "name=Ada\r\n");
    }

    #[test]
    fn text_plain_multiple_entries_each_have_crlf() {
        let entries = vec![
            FormEntry::text("name", "Ada"),
            FormEntry::text("msg", "hello, world"),
        ];
        assert_eq!(
            encode_text_plain(&entries),
            "name=Ada\r\nmsg=hello, world\r\n"
        );
    }

    #[test]
    fn text_plain_does_not_escape_specials() {
        // text/plain is intentionally minimal: ampersands, equals, etc. are
        // not encoded. This is why the spec warns it's only useful for
        // debugging, not real form processing.
        let entries = vec![FormEntry::text("q", "a=b&c=d")];
        assert_eq!(encode_text_plain(&entries), "q=a=b&c=d\r\n");
    }

    #[test]
    fn text_plain_file_entry_uses_filename() {
        let entries = vec![FormEntry::file(
            "upload",
            "report.txt",
            "text/plain",
            b"contents".to_vec(),
        )];
        assert_eq!(encode_text_plain(&entries), "upload=report.txt\r\n");
    }

    #[test]
    fn text_plain_empty_input_returns_empty() {
        assert_eq!(encode_text_plain(&[]), "");
    }

    // --- multipart/form-data ---------------------------------------------

    #[test]
    fn multipart_text_entry_layout() {
        let entries = vec![FormEntry::text("field", "value")];
        let body = encode_multipart(&entries, "----vixen");
        let s = std::str::from_utf8(&body).unwrap();
        let expected = concat!(
            "------vixen\r\n",
            "Content-Disposition: form-data; name=\"field\"\r\n",
            "\r\n",
            "value\r\n",
            "------vixen--\r\n",
        );
        assert_eq!(s, expected);
    }

    #[test]
    fn multipart_file_entry_has_filename_and_content_type() {
        let entries = vec![FormEntry::file(
            "upload",
            "report.txt",
            "text/plain",
            b"hello".to_vec(),
        )];
        let body = encode_multipart(&entries, "BND");
        let s = std::str::from_utf8(&body).unwrap();
        let expected = concat!(
            "--BND\r\n",
            "Content-Disposition: form-data; name=\"upload\"; filename=\"report.txt\"\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "hello\r\n",
            "--BND--\r\n",
        );
        assert_eq!(s, expected);
    }

    #[test]
    fn multipart_multiple_entries_each_get_part() {
        let entries = vec![FormEntry::text("a", "1"), FormEntry::text("b", "2")];
        let body = encode_multipart(&entries, "X");
        let s = std::str::from_utf8(&body).unwrap();
        // Two `--X\r\n` delimiters and a final `--X--\r\n`.
        assert_eq!(s.matches("--X\r\n").count(), 2);
        assert!(s.ends_with("--X--\r\n"));
    }

    #[test]
    fn multipart_terminator_is_boundary_dash_dash_crlf() {
        let entries = vec![FormEntry::text("x", "y")];
        let body = encode_multipart(&entries, "abc");
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.ends_with("--abc--\r\n"));
    }

    #[test]
    fn multipart_empty_entries_just_terminator() {
        let body = encode_multipart(&[], "B");
        let s = std::str::from_utf8(&body).unwrap();
        assert_eq!(s, "--B--\r\n");
    }

    #[test]
    fn multipart_carriage_returns_in_part_body_are_preserved_as_bytes() {
        // Binary-safe: the body bytes go in verbatim, including CRLF pairs.
        let entries = vec![FormEntry::file(
            "f",
            "data.bin",
            "application/octet-stream",
            b"\r\n\x00\xFF".to_vec(),
        )];
        let body = encode_multipart(&entries, "B");
        assert!(body.windows(4).any(|w| w == b"\r\n\x00\xFF"));
    }

    #[test]
    fn multipart_escapes_quotes_in_name_and_filename() {
        // Per RFC 7578 § 4.2, `"` and `\` inside the quoted-string must be
        // backslash-escaped so they don't break the MIME header.
        let entries = vec![FormEntry::file(
            "na\"me",
            "fi\\le.txt",
            "text/plain",
            b"x".to_vec(),
        )];
        let body = encode_multipart(&entries, "B");
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.contains(r#"name="na\"me""#));
        assert!(s.contains(r#"filename="fi\\le.txt""#));
    }

    #[test]
    fn multipart_strips_crlf_from_name_to_protect_headers() {
        // A name containing CR/LF would inject extra header lines if not
        // stripped; we drop the bytes entirely.
        let entries = vec![FormEntry::text("ev\r\nil", "v")];
        let body = encode_multipart(&entries, "B");
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.contains(r#"name="evil""#));
        assert!(!s.contains("il\r\nContent"));
    }

    #[test]
    fn multipart_content_type_includes_boundary() {
        let ct = multipart_content_type("----vixen");
        assert_eq!(ct, "multipart/form-data; boundary=----vixen");
    }

    // --- generate_boundary -----------------------------------------------

    #[test]
    fn boundary_has_distinctive_prefix() {
        let b = generate_boundary();
        assert!(b.starts_with("----vixenformboundary"));
        // The deterministic suffix is 16 hex chars.
        assert_eq!(b.len(), "----vixenformboundary".len() + 16);
    }

    #[test]
    fn boundary_is_unique_within_one_process() {
        let a = generate_boundary();
        let b = generate_boundary();
        assert_ne!(a, b, "boundaries must be unique within a process");
    }

    #[test]
    fn boundary_does_not_exceed_rfc_2046_max() {
        // RFC 2046 § 5.1.1: boundary ≤ 70 chars.
        let b = generate_boundary();
        assert!(b.len() <= 70, "boundary too long: {}", b.len());
    }

    // --- FormEntryValue helpers -----------------------------------------

    #[test]
    fn form_entry_constructors_set_fields() {
        let t = FormEntry::text("k", "v");
        assert_eq!(t.name, "k");
        assert_eq!(t.value, FormEntryValue::Text("v".to_string()));

        let f = FormEntry::file("k", "f.txt", "text/plain", b"abc".to_vec());
        match f.value {
            FormEntryValue::File {
                filename,
                content_type,
                body,
            } => {
                assert_eq!(filename, "f.txt");
                assert_eq!(content_type, "text/plain");
                assert_eq!(body, b"abc");
            }
            _ => panic!("expected File"),
        }
    }
}
