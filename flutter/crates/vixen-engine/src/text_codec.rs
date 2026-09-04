//! WHATWG Encoding API — Phase 6 host-bindings prep (docs/PLAN.md Phase 6
//! step 5 "Network: … TextEncoder/TextDecoder"). Implements the JS
//! `TextEncoder` and `TextDecoder` surfaces (Encoding § 7) so the script
//! layer reflects `new TextEncoder()` / `new TextDecoder("utf-8", {fatal})`
//! against one source of truth.
//!
//! What lives here:
//! - [`TextEncoder::encode`] — `TextEncoder.prototype.encode(input)`. UTF-8
//!   only (the modern web has converged on UTF-8; the Encoding API removed
//!   every other label). Rust `&str` is already UTF-8, so this is the byte
//!   sequence verbatim.
//! - [`TextEncoder::encode_into`] — `TextEncoder.prototype.encodeInto(input,
//!   dest)`: fill `dest` with as many UTF-8 bytes as fit, returning how many
//!   UTF-16 code units of `input` were consumed and how many bytes written.
//!   This is the streaming surface workers use to avoid per-chunk allocation.
//! - [`TextDecoder::decode`] — `TextDecoder.prototype.decode(input, opts)`:
//!   UTF-8 byte slice → `String`, with the `fatal` flag (reject invalid
//!   sequences) and the `ignoreBOM` flag (skip the leading BOM sniff).
//!
//! What does *not* live here:
//! - Legacy encodings (Shift_JIS, ISO-8859-1, …). The Encoding API labels
//!   them but v1 only ships UTF-8 — the spec's only "required" codec.
//!   Unknown labels fail closed with [`DecodeError::LabelNotSupported`].
//! - Streams. The `ReadableStream` integration is the DOM layer; this module
//!   is the per-chunk codec.
//! - The `TextDecoder` constructor label resolution table. The only label we
//!   accept at v1 is `"utf-8"` (case-insensitive, aliasing the spec's name).
//!
//! ## Line-break normalisation
//!
//! `TextDecoder.prototype.decode` normalises line breaks in its output: every
//! `CR` not followed by `LF`, and every `CRLF`, becomes `LF` (Encoding § 7.1
//! step 14). This matches every other browser and is the subtle invariant the
//! SPEC.md "behavioural invariants" category calls out. [`TextDecoder::decode`]
//! applies it after the BOM sniff + UTF-8 decode.
//!
//! ## Trust boundary
//!
//! `fetch()` response bodies and `XMLHttpRequest.responseText` arrive over the
//! network and may be arbitrarily malformed. The `fatal` flag is the security-
//! relevant choice: when `true`, invalid UTF-8 surfaces as a `TypeError` to
//! script (preventing silent corruption); when `false` (default), invalid
//! sequences become U+FFFD per WHATWG § 4.6 "maximal ill-formed subpart".
//!
//! Reference: <https://encoding.spec.whatwg.org/>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// TextEncoder (Encoding § 7.2)
// ---------------------------------------------------------------------------

/// The `TextEncoder` host-binding surface (Encoding § 7.2). Always UTF-8 —
/// the Encoding API removed the legacy-encoding parameter, so the
/// `.encoding` property is the constant `"utf-8"`. Construct with `default()`.
#[derive(Debug, Default, Clone, Copy)]
pub struct TextEncoder;

/// The `encoding` property value (always `"utf-8"` for `TextEncoder`).
pub const TEXT_ENCODER_ENCODING: &str = "utf-8";

impl TextEncoder {
    /// `TextEncoder.prototype.encode(input)` (Encoding § 7.2). Returns the
    /// UTF-8 bytes of `input`. `&str` is already UTF-8 in Rust, so this is
    /// `input.as_bytes().to_vec()` — no transcoding, no line-break
    /// normalisation (only `TextDecoder.decode` normalises; see its docs).
    ///
    /// `input` defaults to the empty string when the JS call omits it.
    pub fn encode(&self, input: &str) -> Vec<u8> {
        input.as_bytes().to_vec()
    }

    /// `TextEncoder.prototype.encodeInto(input, dest)` (Encoding § 7.2).
    /// UTF-8 encodes `input` into `dest` until either `input` is exhausted or
    /// `dest` is full, returning how many UTF-16 code units of `input` were
    /// consumed (`read`) and how many bytes were written (`written`).
    ///
    /// `read` is counted in UTF-16 code units (not Rust `char`s) because the
    /// JS-visible `input` is a UTF-16 `String`. A supplementary-plane scalar
    /// value contributes 2 UTF-16 code units (a surrogate pair).
    ///
    /// A scalar value is never split: if its 1–4 UTF-8 bytes don't fit in the
    /// remaining destination, `read`/`written` stop at the previous scalar.
    pub fn encode_into(&self, input: &str, dest: &mut [u8]) -> EncodeIntoResult {
        let mut written = 0usize;
        let mut read_utf16 = 0usize;
        // Reusable 4-byte buffer for one scalar value's UTF-8 encoding.
        let mut buf = [0u8; 4];
        for c in input.chars() {
            let encoded = c.encode_utf8(&mut buf);
            let n = encoded.len();
            if written + n > dest.len() {
                break;
            }
            dest[written..written + n].copy_from_slice(&buf[..n]);
            written += n;
            read_utf16 += c.len_utf16();
        }
        EncodeIntoResult {
            read_utf16,
            written,
        }
    }
}

/// Result of [`TextEncoder::encode_into`] (mirrors the
/// `TextEncoderEncodeIntoResult` dictionary, Encoding § 7.2).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EncodeIntoResult {
    /// UTF-16 code units of `input` consumed (the JS `read` field).
    pub read_utf16: usize,
    /// Bytes written to `dest` (the JS `written` field).
    pub written: usize,
}

// ---------------------------------------------------------------------------
// TextDecoder (Encoding § 7.1)
// ---------------------------------------------------------------------------

/// The `TextDecoder` host-binding surface (Encoding § 7.1). Construct with
/// [`TextDecoder::new`] (the only label v1 supports is `"utf-8"`); the
/// `fatal` and `ignore_bom` options mirror the WebIDL constructor dictionary.
#[derive(Debug, Clone, Copy)]
pub struct TextDecoder {
    fatal: bool,
    ignore_bom: bool,
}

/// The `encoding` property value (always `"utf-8"` at v1).
pub const TEXT_DECODER_ENCODING: &str = "utf-8";

/// Why a `TextDecoder.decode` call failed. The `fatal`-mode variants map 1:1
/// to the `TypeError` reasons the JS surface throws.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The constructor label isn't supported. At v1 only `"utf-8"` is.
    #[error("unsupported encoding label: {label:?}")]
    LabelNotSupported { label: String },
    /// `fatal: true` was set and the input contained invalid UTF-8 bytes.
    #[error("invalid UTF-8 sequence in input")]
    InvalidUtf8,
}

impl TextDecoder {
    /// `new TextDecoder(label = "utf-8", options = {})` (Encoding § 7.1).
    /// `label` is matched case-insensitively; the only accepted value at v1
    /// is `"utf-8"`. `fatal` and `ignore_bom` mirror the WebIDL dictionary.
    pub fn new(label: &str, fatal: bool, ignore_bom: bool) -> Result<Self, DecodeError> {
        if !label.eq_ignore_ascii_case("utf-8") && !label.is_empty() {
            return Err(DecodeError::LabelNotSupported {
                label: label.to_string(),
            });
        }
        Ok(Self { fatal, ignore_bom })
    }

    /// `new TextDecoder()` with the UTF-8 default + non-fatal + BOM sniff.
    pub fn utf8() -> Self {
        Self {
            fatal: false,
            ignore_bom: false,
        }
    }

    /// The `fatal` option (errors on invalid UTF-8 instead of replacing).
    pub fn fatal(&self) -> bool {
        self.fatal
    }

    /// The `ignoreBOM` option (skips the leading BOM sniff).
    pub fn ignore_bom(&self) -> bool {
        self.ignore_bom
    }

    /// `TextDecoder.prototype.decode(input, options)` (Encoding § 7.1).
    ///
    /// 1. Strip a leading UTF-8 BOM (`EF BB BF`) unless `ignore_bom` is set.
    /// 2. Decode the bytes as UTF-8. On invalid bytes: if `fatal`, return
    ///    [`DecodeError::InvalidUtf8`]; otherwise replace each maximal
    ///    ill-formed subpart with U+FFFD (matching WHATWG § 4.6 + Rust's
    ///    `String::from_utf8_lossy`, which agrees on the replacement count).
    /// 3. Normalise line breaks: `CRLF` → `LF`, lone `CR` → `LF`.
    pub fn decode(&self, input: &[u8]) -> Result<String, DecodeError> {
        // Step 1: BOM sniff.
        let body = if !self.ignore_bom && input.starts_with(b"\xef\xbb\xbf") {
            &input[3..]
        } else {
            input
        };
        // Step 2: UTF-8 decode.
        let decoded = if self.fatal {
            std::str::from_utf8(body)
                .map(|s| s.to_string())
                .map_err(|_| DecodeError::InvalidUtf8)?
        } else {
            // from_utf8_lossy emits one U+FFFD per maximal ill-formed subpart,
            // matching WHATWG § 4.6 "UTF-8 decoder" replacement rules.
            String::from_utf8_lossy(body).into_owned()
        };
        // Step 3: line-break normalisation.
        Ok(normalise_line_breaks(&decoded))
    }
}

// ---------------------------------------------------------------------------
// Line-break normalisation (Encoding § 7.1 step 14)
// ---------------------------------------------------------------------------

/// Replace every `CR` not followed by `LF`, and every `CRLF`, with `LF`.
/// `LF` passes through unchanged. This is the WHATWG Encoding line-break
/// normalisation applied to `TextDecoder.decode` output.
fn normalise_line_breaks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            out.push('\n');
            // If the CR is followed by LF, consume the LF (CRLF → LF).
            if matches!(chars.peek(), Some('\n')) {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- TextEncoder.encode ---------------------------------------------

    #[test]
    fn encode_ascii_is_byte_for_byte() {
        let bytes = TextEncoder::encode(&TextEncoder, "Hello");
        assert_eq!(bytes, b"Hello");
    }

    #[test]
    fn encode_utf8_multibyte_preserved() {
        // "é" = 0xC3 0xA9 in UTF-8; "€" = 0xE2 0x82 0xAC.
        let bytes = TextEncoder::encode(&TextEncoder, "é€");
        assert_eq!(bytes, &[0xC3, 0xA9, 0xE2, 0x82, 0xAC]);
    }

    #[test]
    fn encode_empty_is_empty() {
        assert!(TextEncoder::encode(&TextEncoder, "").is_empty());
    }

    #[test]
    fn encode_does_not_normalise_line_breaks() {
        // CR and CRLF are NOT normalised on the encode side (only decode).
        let bytes = TextEncoder::encode(&TextEncoder, "a\rb\r\nc\n");
        assert_eq!(bytes, b"a\rb\r\nc\n");
    }

    #[test]
    fn text_encoder_default_constructs() {
        let e = TextEncoder;
        assert_eq!(e.encode("x"), b"x");
    }

    // --- TextEncoder.encode_into ----------------------------------------

    #[test]
    fn encode_into_fits_completely() {
        let mut dest = [0u8; 16];
        let r = TextEncoder.encode_into("abc", &mut dest);
        assert_eq!(r.read_utf16, 3);
        assert_eq!(r.written, 3);
        assert_eq!(&dest[..r.written], b"abc");
    }

    #[test]
    fn encode_into_truncates_at_dest_boundary() {
        let mut dest = [0u8; 4];
        let r = TextEncoder.encode_into("hello", &mut dest);
        // Only "hell" fits (4 bytes), so read=4, written=4.
        assert_eq!(r.read_utf16, 4);
        assert_eq!(r.written, 4);
        assert_eq!(&dest[..r.written], b"hell");
    }

    #[test]
    fn encode_into_does_not_split_multibyte_scalar() {
        // "é" is 2 UTF-8 bytes. A 1-byte dest can't hold it; the scalar is
        // not split, so 0 written and 0 read.
        let mut dest = [0u8; 1];
        let r = TextEncoder.encode_into("é", &mut dest);
        assert_eq!(r.read_utf16, 0);
        assert_eq!(r.written, 0);
        // 2-byte dest holds "é" entirely.
        let mut dest = [0u8; 2];
        let r = TextEncoder.encode_into("é", &mut dest);
        assert_eq!(r.read_utf16, 1); // 1 UTF-16 code unit (BMP)
        assert_eq!(r.written, 2);
        assert_eq!(&dest, &[0xC3, 0xA9]);
    }

    #[test]
    fn encode_into_read_counts_utf16_code_units() {
        // 😀 = U+1F600, a supplementary-plane scalar value. In UTF-16 it's a
        // surrogate pair (2 code units); in UTF-8 it's 4 bytes.
        let mut dest = [0u8; 8];
        let r = TextEncoder.encode_into("a😀b", &mut dest);
        // read: 'a'(1) + 😀(2) + 'b'(1) = 4 UTF-16 code units.
        assert_eq!(r.read_utf16, 4);
        // written: 1 + 4 + 1 = 6 bytes.
        assert_eq!(r.written, 6);
    }

    #[test]
    fn encode_into_empty_input() {
        let mut dest = [0u8; 8];
        let r = TextEncoder.encode_into("", &mut dest);
        assert_eq!(r.read_utf16, 0);
        assert_eq!(r.written, 0);
    }

    #[test]
    fn encode_into_zero_len_dest() {
        let r = TextEncoder.encode_into("abc", &mut []);
        assert_eq!(r.read_utf16, 0);
        assert_eq!(r.written, 0);
    }

    #[test]
    fn encode_into_partial_multibyte_then_full() {
        // "aé": 'a' fits in 1 byte, 'é' needs 2. With a 3-byte dest, both fit.
        let mut dest = [0u8; 3];
        let r = TextEncoder.encode_into("aé", &mut dest);
        assert_eq!(r.read_utf16, 2);
        assert_eq!(r.written, 3);
        // With a 2-byte dest, only 'a' fits (é would need 2 more bytes; only 1 left).
        let mut dest = [0u8; 2];
        let r = TextEncoder.encode_into("aé", &mut dest);
        assert_eq!(r.read_utf16, 1);
        assert_eq!(r.written, 1);
    }

    // --- TextDecoder construction ---------------------------------------

    #[test]
    fn decoder_utf8_default() {
        let d = TextDecoder::utf8();
        assert!(!d.fatal());
        assert!(!d.ignore_bom());
        assert!(d.decode(b"ok").is_ok());
    }

    #[test]
    fn decoder_accepts_utf8_label_case_insensitive() {
        for label in ["utf-8", "UTF-8", "Utf-8", "uTf-8"] {
            assert!(TextDecoder::new(label, false, false).is_ok(), "{label}");
        }
    }

    #[test]
    fn decoder_accepts_empty_label_as_utf8_default() {
        // JS `new TextDecoder()` (no arg) defaults to "utf-8".
        assert!(TextDecoder::new("", false, false).is_ok());
    }

    #[test]
    fn decoder_rejects_unsupported_label() {
        // v1 supports only UTF-8; legacy encodings fail closed.
        for label in ["shift_jis", "iso-8859-1", "windows-1252", "gbk"] {
            let err = TextDecoder::new(label, false, false).unwrap_err();
            assert!(matches!(err, DecodeError::LabelNotSupported { .. }));
        }
    }

    // --- TextDecoder.decode: happy path ---------------------------------

    #[test]
    fn decode_ascii() {
        let d = TextDecoder::utf8();
        assert_eq!(d.decode(b"Hello").unwrap(), "Hello");
    }

    #[test]
    fn decode_utf8_multibyte() {
        let d = TextDecoder::utf8();
        assert_eq!(d.decode(&[0xC3, 0xA9, 0xE2, 0x82, 0xAC]).unwrap(), "é€");
    }

    #[test]
    fn decode_empty_input() {
        let d = TextDecoder::utf8();
        assert_eq!(d.decode(b"").unwrap(), "");
    }

    // --- TextDecoder.decode: BOM ----------------------------------------

    #[test]
    fn decode_strips_leading_utf8_bom_by_default() {
        let d = TextDecoder::utf8();
        // EF BB BF "hi"
        assert_eq!(d.decode(b"\xef\xbb\xbfhi").unwrap(), "hi");
    }

    #[test]
    fn decode_keeps_bom_when_ignore_bom_set() {
        let d = TextDecoder::new("utf-8", false, true).unwrap();
        // BOM is preserved as U+FEFF in the output.
        assert_eq!(d.decode(b"\xef\xbb\xbfhi").unwrap(), "\u{FEFF}hi");
    }

    #[test]
    fn decode_bom_only_stripped_at_start() {
        // A BOM in the middle of input is a normal ZWNBSP code point.
        let d = TextDecoder::utf8();
        assert_eq!(d.decode(b"a\xef\xbb\xbfb").unwrap(), "a\u{FEFF}b");
    }

    // --- TextDecoder.decode: fatal mode ---------------------------------

    #[test]
    fn decode_fatal_rejects_invalid_utf8() {
        let d = TextDecoder::new("utf-8", true, false).unwrap();
        assert_eq!(d.decode(b"\xff").unwrap_err(), DecodeError::InvalidUtf8);
        // 0xC3 alone is an incomplete 2-byte sequence.
        assert_eq!(d.decode(b"a\xc3").unwrap_err(), DecodeError::InvalidUtf8);
    }

    #[test]
    fn decode_non_fatal_replaces_with_replacement_char() {
        let d = TextDecoder::utf8();
        // 0xFF is a single invalid lead byte → one U+FFFD (one maximal subpart).
        assert_eq!(d.decode(b"\xff").unwrap(), "\u{FFFD}");
        // 0xC3 alone is an incomplete 2-byte sequence at EOF → one U+FFFD.
        assert_eq!(d.decode(b"\xc3").unwrap(), "\u{FFFD}");
    }

    #[test]
    fn decode_non_fatal_counts_replacements_per_maximal_subpart() {
        // Two separated invalid bytes → two replacement chars.
        let d = TextDecoder::utf8();
        assert_eq!(d.decode(b"\xff\xff").unwrap(), "\u{FFFD}\u{FFFD}");
        // Valid + invalid + valid.
        assert_eq!(d.decode(b"a\xffb").unwrap(), "a\u{FFFD}b");
        // WHATWG § 4.6 edge case: 0xE0 expects its first continuation in
        // 0xA0–0xBF (preventing overlong encodings). 0x80 is below that, so
        // the partial sequence (0xE0) emits one U+FFFD, and 0x80 is then
        // reprocessed as a fresh lead (also invalid) → a second U+FFFD.
        // Rust's `from_utf8_lossy` agrees with the WHATWG count.
        assert_eq!(d.decode(b"\xe0\x80").unwrap(), "\u{FFFD}\u{FFFD}");
    }

    // --- TextDecoder.decode: line-break normalisation -------------------

    #[test]
    fn decode_normalises_lone_cr_to_lf() {
        let d = TextDecoder::utf8();
        assert_eq!(d.decode(b"a\rb").unwrap(), "a\nb");
    }

    #[test]
    fn decode_normalises_crlf_to_lf() {
        let d = TextDecoder::utf8();
        assert_eq!(d.decode(b"a\r\nb").unwrap(), "a\nb");
    }

    #[test]
    fn decode_preserves_lone_lf() {
        let d = TextDecoder::utf8();
        assert_eq!(d.decode(b"a\nb").unwrap(), "a\nb");
    }

    #[test]
    fn decode_mixed_line_endings_all_become_lf() {
        let d = TextDecoder::utf8();
        // CR, CRLF, LF in sequence → all three become single LF.
        assert_eq!(d.decode(b"a\r\nb\rc\r\nd\ne").unwrap(), "a\nb\nc\nd\ne");
    }

    #[test]
    fn decode_consecutive_crs_each_become_lf() {
        let d = TextDecoder::utf8();
        // "\r\r\n" → first CR becomes LF (not followed by LF — the next is CR),
        // then "\r\n" becomes LF. Result: two LFs. This is the spec edge case.
        assert_eq!(d.decode(b"\r\r\n").unwrap(), "\n\n");
    }

    // --- Round trip -----------------------------------------------------

    #[test]
    fn encode_decode_round_trip_ascii() {
        let e = TextEncoder;
        let d = TextDecoder::utf8();
        let original = "The quick brown fox";
        let bytes = e.encode(original);
        assert_eq!(d.decode(&bytes).unwrap(), original);
    }

    #[test]
    fn encode_decode_round_trip_unicode() {
        let e = TextEncoder;
        let d = TextDecoder::utf8();
        let original = "Héllo, 世界! 😀";
        let bytes = e.encode(original);
        assert_eq!(d.decode(&bytes).unwrap(), original);
    }

    #[test]
    fn encode_decode_round_trip_with_lf_only() {
        // LF-only input round-trips unchanged (no CR to normalise).
        let e = TextEncoder;
        let d = TextDecoder::utf8();
        let original = "line1\nline2\n";
        let bytes = e.encode(original);
        assert_eq!(d.decode(&bytes).unwrap(), original);
    }

    #[test]
    fn encode_then_decode_normalises_crlf() {
        // Encode CRLF (preserved), decode normalises to LF.
        let e = TextEncoder;
        let d = TextDecoder::utf8();
        let bytes = e.encode("a\r\nb");
        assert_eq!(d.decode(&bytes).unwrap(), "a\nb");
    }

    // --- normalise_line_breaks (unit) -----------------------------------

    #[test]
    fn normalise_empty_is_empty() {
        assert_eq!(normalise_line_breaks(""), "");
    }

    #[test]
    fn normalise_no_cr_unchanged() {
        assert_eq!(normalise_line_breaks("abc\ndef"), "abc\ndef");
    }

    #[test]
    fn normalise_trailing_cr_becomes_lf() {
        assert_eq!(normalise_line_breaks("abc\r"), "abc\n");
    }
}
