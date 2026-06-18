//! RFC 2397 `data:` URL parsing — Phase 6 host-bindings + Phase 7 fetch prep
//! (pure logic called out by `docs/PLAN.md` "Testing strategy"). Implements
//! the `data:` URL grammar every `fetch()` / `<img src>` / `<iframe>` /
//! `XMLHttpRequest` consumer reduces a `data:` URL to before handing its
//! payload to the MIME layer.
//!
//! What lives here:
//! - [`parse_data_url`] — the RFC 2397 splitter: scheme check, `;base64`
//!   flag, mediatype defaulting, payload decode (base64 or percent-decode).
//! - [`DataUrl`] — the decoded `(MimeType, payload bytes)` record the fetch
//!   layer hands the MIME-sniff + response body constructors.
//!
//! What does *not* live here:
//! - The MIME-sniff step (Phase 7): `data:` URLs are *not* sniffed per the
//!   Fetch standard; the declared mediatype is authoritative. The caller
//!   enforces that by using [`DataUrl::mime_type`] verbatim.
//! - The full URL record construction (the `url` crate owns generic URL
//!   parsing; this module is the `data:`-scheme-specific body processor the
//!   `url` crate deliberately leaves to its consumers).
//!
//! ## Grammar (RFC 2397 § 2)
//!
//! ```text
//! dataurl    := "data:" [ mediatype ] [ ";base64" ] "," data
//! mediatype  := [ type "/" subtype ] *( ";" parameter )
//! ```
//!
//! Defaulting rules (RFC 2397 § 2 + browser parity):
//! - Omitted mediatype (`",data"` form) ⇒ `text/plain;charset=US-ASCII`.
//! - Parameters-only mediatype (`";charset=utf-8,data"` form) ⇒ the
//!   `type`/`subtype` default to `text/plain`; user parameters are kept as
//!   authored (no implicit US-ASCII added).
//! - `;base64` may appear only as the final parameter before the comma; its
//!   presence switches the payload to base64 decoding.
//!
//! Reference: <https://datatracker.ietf.org/doc/html/rfc2397>.

#![forbid(unsafe_code)]

use crate::mime::MimeType;

/// A decoded RFC 2397 `data:` URL: the authoritative MIME type (the Fetch
/// standard does *not* sniff `data:` URLs) + the raw payload bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataUrl {
    /// The declared MIME type (defaulted per RFC 2397 when the URL omitted it).
    pub mime_type: MimeType,
    /// Whether the payload was base64-encoded. Browsers surface this only
    /// indirectly (it affects nothing once decoded); kept for diagnostics.
    pub is_base64: bool,
    /// The decoded payload bytes.
    pub data: Vec<u8>,
}

/// Why a `data:` URL failed to parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataUrlError {
    /// The input is not a `data:` URL (missing or wrong scheme).
    #[error("not a data: URL")]
    NotDataUrl,
    /// No `,` separator between the head and the payload.
    #[error("data: URL is missing the comma separator")]
    MissingComma,
    /// The head's mediatype could not be parsed as a MIME type.
    #[error("invalid mediatype: {0:?}")]
    InvalidMediaType(String),
    /// The base64 payload was malformed (non-alphabet char or bad length).
    #[error("invalid base64 payload")]
    InvalidBase64,
}

/// Parse an RFC 2397 `data:` URL. Accepts the scheme case-insensitively
/// (`DATA:`, `Data:`); everything after the colon is the body. See the module
/// docs for the defaulting + `;base64` rules.
///
/// ```
/// # use vixen_core::data_url::parse_data_url;
/// let d = parse_data_url("data:text/plain;base64,SGVsbG8=").unwrap();
/// assert_eq!(d.mime_type.essence(), "text/plain");
/// assert_eq!(d.data, b"Hello");
/// assert!(d.is_base64);
/// ```
pub fn parse_data_url(input: &str) -> Result<DataUrl, DataUrlError> {
    // Scheme check (case-insensitive on the 5-byte prefix "data:").
    let body = match input.as_bytes().get(..5) {
        Some(prefix) if prefix.eq_ignore_ascii_case(b"data:") => &input[5..],
        _ => return Err(DataUrlError::NotDataUrl),
    };

    // Split at the first comma (RFC 2397: the data part starts at the first
    // ','; any commas inside the payload are part of the payload).
    let comma = body.find(',').ok_or(DataUrlError::MissingComma)?;
    let head = &body[..comma];
    let data_str = &body[comma + 1..];

    // `;base64` flag: it must be the final parameter (RFC 2397 § 2). Match
    // case-insensitively against the head tail.
    let (mediatype_str, is_base64) = strip_base64_flag(head);

    let mime_type = resolve_mediatype(mediatype_str)?;

    let data = if is_base64 {
        decode_base64_lenient(data_str).ok_or(DataUrlError::InvalidBase64)?
    } else {
        percent_decode(data_str)
    };

    Ok(DataUrl {
        mime_type,
        is_base64,
        data,
    })
}

/// Remove a trailing `;base64` parameter (case-insensitive) from `head`,
/// returning `(remaining_head, is_base64)`.
fn strip_base64_flag(head: &str) -> (&str, bool) {
    if head.len() >= 7 && head[head.len() - 7..].eq_ignore_ascii_case(";base64") {
        (&head[..head.len() - 7], true)
    } else {
        (head, false)
    }
}

/// Resolve the mediatype per RFC 2397 § 2 defaulting rules. Empty ⇒ the
/// `text/plain;charset=US-ASCII` default; parameters-only ⇒ `text/plain` with
/// the authored parameters; full ⇒ parsed directly.
fn resolve_mediatype(mediatype_str: &str) -> Result<MimeType, DataUrlError> {
    if mediatype_str.is_empty() {
        return MimeType::parse("text/plain;charset=US-ASCII")
            .ok_or_else(|| DataUrlError::InvalidMediaType(mediatype_str.to_owned()));
    }
    // Parameters-only form: prepend text/plain and let the MIME parser split
    // the parameter list (the user authored `;charset=utf-8` etc.).
    if mediatype_str.starts_with(';') {
        let synth = format!("text/plain{mediatype_str}");
        return MimeType::parse(&synth)
            .ok_or_else(|| DataUrlError::InvalidMediaType(mediatype_str.to_owned()));
    }
    MimeType::parse(mediatype_str)
        .ok_or_else(|| DataUrlError::InvalidMediaType(mediatype_str.to_owned()))
}

// ---------------------------------------------------------------------------
// percent-decoding (non-base64 data: payloads)
// ---------------------------------------------------------------------------

/// Percent-decode `input` per RFC 3986 § 2.1. `%XX` (hex) sequences become the
/// matching byte; every other byte passes through unchanged. An invalid `%`
/// escape (non-hex digits, or `%` at end of string) is passed through as a
/// literal `%` byte (the Fetch standard's "percent-decode" never errors).
pub fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// base64 decoding (base64 data: payloads)
// ---------------------------------------------------------------------------

/// Decode a base64 payload leniently: ASCII whitespace is skipped, a single
/// non-alphabet / non-padding code point fails closed. Standard alphabet
/// `A-Za-z0-9+/` with `=` padding; missing trailing padding is tolerated
/// (data URLs commonly omit it).
fn decode_base64_lenient(input: &str) -> Option<Vec<u8>> {
    // Step 1: gather the significant characters, validating as we go.
    let mut chars: Vec<u8> = Vec::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            // ASCII whitespace (the WHATWG forgiving-decode set) is skipped.
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' => continue,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' | b'=' => chars.push(b),
            _ => return None, // any other byte is invalid
        }
    }

    // Step 2: walk 4-char groups, enforcing standard padding placement
    // ('=' only in positions 2/3 of the final group).
    let mut out = Vec::with_capacity(chars.len() * 3 / 4);
    let n = chars.len();
    let mut i = 0;
    while i + 4 <= n {
        let g = [chars[i], chars[i + 1], chars[i + 2], chars[i + 3]];
        let pad = g.iter().filter(|&&b| b == b'=').count();
        if pad > 0 {
            // Padding must be in the last group, at positions 2/3 only, and
            // contiguous from the end (a single '=' sits at position 3).
            let well_placed = match pad {
                1 => g[3] == b'=' && g[2] != b'=',
                2 => g[2] == b'=' && g[3] == b'=',
                _ => false,
            };
            if !well_placed || i + 4 != n {
                return None;
            }
        }
        decode_group(&g, &mut out)?;
        i += 4;
        if pad > 0 {
            break; // padded group terminates the stream
        }
    }

    // Step 3: leftover tail (missing trailing padding, common in data: URLs).
    match n - i {
        0 => {}
        2 => {
            let v0 = b64_val(chars[i])?;
            let v1 = b64_val(chars[i + 1])?;
            out.push((v0 << 2) | (v1 >> 4));
        }
        3 => {
            let v0 = b64_val(chars[i])?;
            let v1 = b64_val(chars[i + 1])?;
            let v2 = b64_val(chars[i + 2])?;
            out.push((v0 << 2) | (v1 >> 4));
            out.push((v1 << 4) | (v2 >> 2));
        }
        _ => return None, // 1 leftover char can't produce a byte
    }
    Some(out)
}

/// Decode one 4-character group into up to 3 bytes, pushing them to `out`.
/// Caller guarantees padding placement (only positions 2/3, final group).
fn decode_group(group: &[u8; 4], out: &mut Vec<u8>) -> Option<()> {
    let pad = group.iter().filter(|&&b| b == b'=').count();
    let v0 = b64_val(group[0])?;
    let v1 = b64_val(group[1])?;
    out.push((v0 << 2) | (v1 >> 4));
    if pad < 2 {
        let v2 = b64_val(group[2])?;
        out.push((v1 << 4) | (v2 >> 2));
        if pad < 1 {
            let v3 = b64_val(group[3])?;
            out.push((v2 << 6) | v3);
        }
    }
    Some(())
}

/// Map a base64 alphabet byte to its 6-bit value. Returns `None` for padding
/// or any non-alphabet byte (the caller has already separated padding).
fn b64_val(b: u8) -> Option<u8> {
    Some(match b {
        b'A'..=b'Z' => b - b'A',
        b'a'..=b'z' => b - b'a' + 26,
        b'0'..=b'9' => b - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mt(essence: &str) -> MimeType {
        MimeType::parse(essence).expect("test mediatype")
    }

    // --- scheme + structure --------------------------------------------

    #[test]
    fn rejects_non_data_scheme() {
        assert_eq!(
            parse_data_url("http://example.com/").unwrap_err(),
            DataUrlError::NotDataUrl
        );
        assert_eq!(
            parse_data_url("data").unwrap_err(),
            DataUrlError::NotDataUrl
        );
    }

    #[test]
    fn scheme_case_insensitive() {
        // The URL spec parses schemes case-insensitively.
        let d = parse_data_url("DATA:text/plain,x").unwrap();
        assert_eq!(d.data, b"x");
        let d = parse_data_url("DaTa:text/plain,x").unwrap();
        assert_eq!(d.data, b"x");
    }

    #[test]
    fn missing_comma_errors() {
        assert_eq!(
            parse_data_url("data:text/plain;base64").unwrap_err(),
            DataUrlError::MissingComma
        );
    }

    // --- mediatype defaulting ------------------------------------------

    #[test]
    fn omitted_mediatype_defaults_to_text_plain_us_ascii() {
        let d = parse_data_url("data:,hello").unwrap();
        assert_eq!(d.mime_type.essence(), "text/plain");
        // RFC 2397 default charset is the literal `US-ASCII`; the MIME parser
        // preserves parameter-value case.
        assert_eq!(
            d.mime_type.parameters.get("charset").map(|s| s.as_str()),
            Some("US-ASCII")
        );
        assert_eq!(d.data, b"hello");
        assert!(!d.is_base64);
    }

    #[test]
    fn full_mediatype_kept() {
        let d = parse_data_url("data:text/html,<b>hi</b>").unwrap();
        assert_eq!(d.mime_type.essence(), "text/html");
        assert_eq!(d.data, b"<b>hi</b>");
    }

    #[test]
    fn mediatype_with_parameters() {
        let d = parse_data_url("data:text/plain;charset=utf-8,café").unwrap();
        assert_eq!(d.mime_type.essence(), "text/plain");
        assert_eq!(
            d.mime_type.parameters.get("charset").map(|s| s.as_str()),
            Some("utf-8")
        );
    }

    #[test]
    fn parameters_only_mediatype_defaults_type_to_text_plain() {
        // RFC 2397: [ type "/" subtype ] is optional; params carry through.
        let d = parse_data_url("data:;charset=utf-8,foo").unwrap();
        assert_eq!(d.mime_type.essence(), "text/plain");
        assert_eq!(
            d.mime_type.parameters.get("charset").map(|s| s.as_str()),
            Some("utf-8")
        );
    }

    #[test]
    fn invalid_mediatype_errors() {
        // "bad mediatype" with no slash and no leading ';' is not a MIME type.
        assert!(matches!(
            parse_data_url("data:badmediatype,foo"),
            Err(DataUrlError::InvalidMediaType(_))
        ));
    }

    // --- base64 payload ------------------------------------------------

    #[test]
    fn base64_basic() {
        // "Hello" → SGVsbG8=
        let d = parse_data_url("data:text/plain;base64,SGVsbG8=").unwrap();
        assert!(d.is_base64);
        assert_eq!(d.data, b"Hello");
    }

    #[test]
    fn base64_without_padding_tolerated() {
        // data: URLs commonly omit trailing '='.
        let d = parse_data_url("data:text/plain;base64,SGVsbG8").unwrap();
        assert_eq!(d.data, b"Hello");
    }

    #[test]
    fn base64_whitespace_inside_ignored() {
        // Forging base64 skips ASCII whitespace.
        let d = parse_data_url("data:;base64,SGVs\nbG 8=").unwrap();
        assert_eq!(d.data, b"Hello");
    }

    #[test]
    fn base64_binary_payload() {
        // 3 bytes {0x00, 0xFF, 0x10} → "AP8Q"
        let d = parse_data_url("data:application/octet-stream;base64,AP8Q").unwrap();
        assert_eq!(d.data, vec![0x00, 0xff, 0x10]);
    }

    #[test]
    fn base64_invalid_char_errors() {
        assert_eq!(
            parse_data_url("data:;base64,SGVsbG8!").unwrap_err(),
            DataUrlError::InvalidBase64
        );
        // A byte outside the forgiving alphabet (0x80) is invalid.
        assert_eq!(
            parse_data_url("data:;base64,\u{0080}").unwrap_err(),
            DataUrlError::InvalidBase64
        );
    }

    // --- percent-decoded payload ---------------------------------------

    #[test]
    fn percent_decode_in_payload() {
        let d = parse_data_url("data:,hello%20world").unwrap();
        assert_eq!(d.data, b"hello world");
    }

    #[test]
    fn percent_decode_passes_through_invalid_escape() {
        // `%GG` isn't hex ⇒ the `%` is taken literally (Fetch never errors).
        let d = parse_data_url("data:,%GG").unwrap();
        assert_eq!(d.data, b"%GG");
    }

    #[test]
    fn percent_decode_trailing_percent_kept() {
        let d = parse_data_url("data:,100%").unwrap();
        assert_eq!(d.data, b"100%");
    }

    #[test]
    fn non_ascii_utf8_bytes_preserved() {
        // The non-base64 payload is raw bytes; UTF-8 just passes through.
        let d = parse_data_url("data:,café").unwrap();
        assert_eq!(d.data, "café".as_bytes());
    }

    // --- percent_decode unit -------------------------------------------

    #[test]
    fn percent_decode_helper_roundtrip() {
        assert_eq!(percent_decode(""), b"");
        assert_eq!(percent_decode("a+b"), b"a+b"); // '+' is literal here
        assert_eq!(percent_decode("%41%42%43"), b"ABC");
        assert_eq!(percent_decode("%2f"), b"/");
    }

    // --- empty payloads ------------------------------------------------

    #[test]
    fn empty_payload() {
        let d = parse_data_url("data:text/plain,").unwrap();
        assert!(d.data.is_empty());
        assert_eq!(d.mime_type, mt("text/plain"));
    }

    #[test]
    fn empty_payload_base64() {
        let d = parse_data_url("data:;base64,").unwrap();
        assert!(d.data.is_empty());
        assert!(d.is_base64);
    }

    // --- commas inside payload -----------------------------------------

    #[test]
    fn commas_in_payload_are_part_of_data() {
        // Only the FIRST comma separates head from data.
        let d = parse_data_url("data:text/plain,a,b,c").unwrap();
        assert_eq!(d.data, b"a,b,c");
    }
}
