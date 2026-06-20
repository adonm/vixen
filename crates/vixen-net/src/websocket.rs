//! RFC 6455 — the WebSocket protocol's pure-logic boundary (handshake
//! validation + frame parsing + masking + close-code validation). The network
//! layer consults this for the HTTP `Upgrade` negotiation and the framed
//! read/write loop. Phase 6 host hooks (`new WebSocket(...)`) sit on top.
//!
//! What lives here:
//! - [`compute_accept`] — RFC 6455 § 4.2.2 `Sec-WebSocket-Accept` value:
//!   `base64(SHA1(client_key + GUID))`. The one piece of crypto the handshake
//!   needs.
//! - [`validate_client_handshake`] — § 4.1 the client `Upgrade` request
//!   header set the server enforces (`Upgrade: websocket`, `Connection:
//!   Upgrade`, the `Sec-WebSocket-Key` 16-byte base64, `Sec-WebSocket-Version:
//!   13`).
//! - [`validate_server_response`] — § 4.2.2 the `101 Switching Protocols`
//!   response the client enforces (status + the header set + the `Accept`
//!   value matching the sent key).
//! - [`FrameHeader`] / [`parse_frame_header`] — § 5.2 the 2–14-byte frame
//!   header decoder (FIN/RSV/opcode/mask/length) + the § 5.5 control-frame
//!   validation (≤ 125 bytes, FIN set).
//! - [`apply_mask`] — § 5.3 the payload demasking XOR.
//! - [`CloseCode`] / [`validate_close_code`] — § 7.4 the close-code range +
//!   reserved-status-code rule.
//!
//! What does *not* live here:
//! - The actual TCP+TLS transport + the framed I/O loop (the network layer).
//! - The `WebSocket` JS host-hook surface (`readyState`, `send`, `onmessage`)
//!   — Phase 6 host hook.
//! - The extension negotiation (`permessage-deflate`) — deferred.
//! - `Sec-WebSocket-Protocol` preference selection (a small selection step the
//!   host hook owns; the parser here exposes the offered list verbatim).
//!
//! Reference: <https://www.rfc-editor.org/rfc/rfc6455>.

#![forbid(unsafe_code)]

use base64::Engine;
use sha1::{Digest, Sha1};

/// The RFC 6455 § 1.3 magic GUID appended to the client key for the
/// `Sec-WebSocket-Accept` computation.
pub const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The WebSocket protocol version (RFC 6455 § 4.1: `Sec-WebSocket-Version`).
pub const WEBSOCKET_VERSION: u8 = 13;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Error from the handshake / frame validation surface.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebSocketError {
    /// The handshake was missing a required header (or had the wrong value).
    #[error("handshake missing or invalid: {0}")]
    InvalidHandshake(&'static str),
    /// The server's `Sec-WebSocket-Accept` did not match the sent key.
    #[error("Sec-WebSocket-Accept mismatch")]
    AcceptMismatch,
    /// A frame header was malformed (truncated, oversized, reserved bits set).
    #[error("invalid frame: {0}")]
    InvalidFrame(&'static str),
    /// A close code was outside the valid range or in a reserved band.
    #[error("invalid close code: {0}")]
    InvalidCloseCode(u16),
}

// ---------------------------------------------------------------------------
// § 4.2.2 — Sec-WebSocket-Accept computation
// ---------------------------------------------------------------------------

/// Compute the RFC 6455 § 4.2.2 `Sec-WebSocket-Accept` value for a client
/// `Sec-WebSocket-Key`: `base64(SHA1(key + GUID))`. The server returns this;
/// the client checks it matches via [`validate_server_response`].
///
/// ```
/// # use vixen_net::websocket::compute_accept;
/// // RFC 6455 § 4.2.2 worked example.
/// assert_eq!(compute_accept("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
/// ```
pub fn compute_accept(client_key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(client_key.as_bytes());
    hasher.update(WEBSOCKET_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

// ---------------------------------------------------------------------------
// Header lookup helper (case-insensitive)
// ---------------------------------------------------------------------------

/// A pre-normalised header lookup over a `&(name, value)` iterator. Headers
/// are matched byte-case-insensitively (RFC 9110 § 5.5 doesn't matter here —
/// the WebSocket headers are all ASCII tokens).
fn header<'a, I: IntoIterator<Item = (&'a str, &'a str)>>(
    headers: I,
    name: &str,
) -> Option<String> {
    headers
        .into_iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim().to_owned())
}

// ---------------------------------------------------------------------------
// § 4.1 — client handshake validation (server side)
// ---------------------------------------------------------------------------

/// Validate the § 4.1 client request header set a server must enforce. The
/// caller passes the request headers as a slice of `(name, value)` pairs (the
/// network layer already has the header map).
///
/// Checks: `Upgrade: websocket` (case-insensitive token), `Connection:
/// Upgrade` (case-insensitive, token may appear in a comma-list),
/// `Sec-WebSocket-Version: 13`, and a `Sec-WebSocket-Key` that base64-decodes
/// to exactly 16 bytes. Returns the decoded key bytes on success (the server
/// uses them to compute [`compute_accept`]).
pub fn validate_client_handshake(headers: &[(&str, &str)]) -> Result<Vec<u8>, WebSocketError> {
    let upgrade = header(headers.iter().copied(), "Upgrade")
        .ok_or(WebSocketError::InvalidHandshake("missing Upgrade"))?;
    if !token_list_contains(&upgrade, "websocket") {
        return Err(WebSocketError::InvalidHandshake("Upgrade is not websocket"));
    }
    let connection = header(headers.iter().copied(), "Connection")
        .ok_or(WebSocketError::InvalidHandshake("missing Connection"))?;
    if !token_list_contains(&connection, "upgrade") {
        return Err(WebSocketError::InvalidHandshake("Connection lacks Upgrade"));
    }
    let version = header(headers.iter().copied(), "Sec-WebSocket-Version").ok_or(
        WebSocketError::InvalidHandshake("missing Sec-WebSocket-Version"),
    )?;
    if version.trim() != WEBSOCKET_VERSION.to_string() {
        return Err(WebSocketError::InvalidHandshake(
            "Sec-WebSocket-Version is not 13",
        ));
    }
    let key = header(headers.iter().copied(), "Sec-WebSocket-Key").ok_or(
        WebSocketError::InvalidHandshake("missing Sec-WebSocket-Key"),
    )?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(key.as_bytes())
        .map_err(|_| WebSocketError::InvalidHandshake("Sec-WebSocket-Key is not valid base64"))?;
    if decoded.len() != 16 {
        return Err(WebSocketError::InvalidHandshake(
            "Sec-WebSocket-Key must decode to 16 bytes",
        ));
    }
    Ok(decoded)
}

// ---------------------------------------------------------------------------
// § 4.2.2 — server response validation (client side)
// ---------------------------------------------------------------------------

/// Validate the § 4.2.2 `101 Switching Protocols` response the client enforces
/// against the key it sent. Checks the status (caller-supplied), the
/// `Upgrade`/`Connection` headers, and that `Sec-WebSocket-Accept` equals
/// [`compute_accept`]`(client_key)`.
pub fn validate_server_response(
    status: u16,
    headers: &[(&str, &str)],
    client_key: &str,
) -> Result<(), WebSocketError> {
    if status != 101 {
        return Err(WebSocketError::InvalidHandshake("status is not 101"));
    }
    let upgrade = header(headers.iter().copied(), "Upgrade")
        .ok_or(WebSocketError::InvalidHandshake("missing Upgrade"))?;
    if !token_list_contains(&upgrade, "websocket") {
        return Err(WebSocketError::InvalidHandshake("Upgrade is not websocket"));
    }
    let connection = header(headers.iter().copied(), "Connection")
        .ok_or(WebSocketError::InvalidHandshake("missing Connection"))?;
    if !token_list_contains(&connection, "upgrade") {
        return Err(WebSocketError::InvalidHandshake("Connection lacks Upgrade"));
    }
    let accept = header(headers.iter().copied(), "Sec-WebSocket-Accept").ok_or(
        WebSocketError::InvalidHandshake("missing Sec-WebSocket-Accept"),
    )?;
    let expected = compute_accept(client_key);
    if accept != expected {
        return Err(WebSocketError::AcceptMismatch);
    }
    Ok(())
}

/// Whether a comma-separated token list contains `token` (case-insensitive,
/// per RFC 9110 § 5.2.2 token-list semantics). Tolerates surrounding OWS.
fn token_list_contains(list: &str, token: &str) -> bool {
    list.split(',')
        .any(|t| t.trim().eq_ignore_ascii_case(token))
}

// ---------------------------------------------------------------------------
// § 5.2 — frame header parsing
// ---------------------------------------------------------------------------

/// A WebSocket opcode (RFC 6455 § 5.2). The low nibble of the first frame
/// byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Opcode {
    /// `0x0` — a continuation of a fragmented message.
    Continuation = 0x0,
    /// `0x1` — a text frame (UTF-8 payload).
    Text = 0x1,
    /// `0x2` — a binary frame.
    Binary = 0x2,
    /// `0x8` — a close frame (carries the close code + reason).
    Close = 0x8,
    /// `0x9` — a ping (application-level keepalive).
    Ping = 0x9,
    /// `0xA` — a pong (ping reply).
    Pong = 0xA,
}

impl Opcode {
    /// Decode the low nibble of a frame's first byte. Returns `None` for the
    /// reserved-for-further-versions opcodes (`0x3`–`0x7`, `0xB`–`0xF`) per
    /// § 5.2.
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b & 0x0F {
            0x0 => Self::Continuation,
            0x1 => Self::Text,
            0x2 => Self::Binary,
            0x8 => Self::Close,
            0x9 => Self::Ping,
            0xA => Self::Pong,
            _ => return None,
        })
    }

    /// The raw opcode byte.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// § 5.5: control frames are `Close`/`Ping`/`Pong` (opcode `≥ 0x8`).
    pub const fn is_control(self) -> bool {
        matches!(self, Self::Close | Self::Ping | Self::Pong)
    }
}

/// A parsed frame header (RFC 6455 § 5.2). The 2–14-byte prefix of every frame;
/// the payload (possibly masked) follows. The header carries everything needed
/// to validate + route the frame before the payload is demasked/processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Whether this is the final fragment of a message.
    pub fin: bool,
    /// The opcode.
    pub opcode: Opcode,
    /// Whether the payload is masked (client→server frames must be; § 5.3).
    pub masked: bool,
    /// The declared payload length in bytes.
    pub payload_len: u64,
    /// The 4-byte masking key, present iff `masked`.
    pub mask_key: [u8; 4],
}

impl FrameHeader {
    /// The total byte length of the header (including the mask key) so the
    /// caller knows where the payload starts.
    pub fn header_len(&self) -> usize {
        let mut len = 2; // FIN/opcode + MASK/length
        len += match self.payload_len {
            0..=125 => 0,
            126..=65535 => 2,
            _ => 8,
        };
        if self.masked {
            len += 4;
        }
        len
    }
}

/// Parse the § 5.2 frame header from `bytes`. Returns the header + the number
/// of bytes consumed (the payload follows). Enforces the § 5.2 / § 5.5
/// invariants: no reserved RSV bits set, a valid opcode, client frames masked,
/// and control frames ≤ 125 bytes + FIN set.
///
/// Returns [`WebSocketError::InvalidFrame`] if `bytes` is truncated or any
/// invariant fails.
pub fn parse_frame_header(bytes: &[u8]) -> Result<(FrameHeader, usize), WebSocketError> {
    if bytes.len() < 2 {
        return Err(WebSocketError::InvalidFrame("frame shorter than 2 bytes"));
    }
    let b0 = bytes[0];
    let b1 = bytes[1];
    // § 5.2: RSV1/RSV2/RSV3 MUST be 0 unless an extension is negotiated.
    if b0 & 0x70 != 0 {
        return Err(WebSocketError::InvalidFrame("reserved RSV bits set"));
    }
    let fin = b0 & 0x80 != 0;
    let opcode = Opcode::from_byte(b0).ok_or(WebSocketError::InvalidFrame("reserved opcode"))?;
    let masked = b1 & 0x80 != 0;
    let len7 = b1 & 0x7F;
    // Decode the extended length.
    let (payload_len, consumed) = match len7 {
        0..=125 => (u64::from(len7), 2usize),
        126 => {
            if bytes.len() < 4 {
                return Err(WebSocketError::InvalidFrame("truncated 16-bit length"));
            }
            let len = u16::from_be_bytes([bytes[2], bytes[3]]) as u64;
            // § 5.2: the 16-bit form must carry a value > 125 (else
            // non-canonical encoding).
            if len <= 125 {
                return Err(WebSocketError::InvalidFrame("non-canonical 16-bit length"));
            }
            (len, 4)
        }
        127 => {
            if bytes.len() < 10 {
                return Err(WebSocketError::InvalidFrame("truncated 64-bit length"));
            }
            let len = u64::from_be_bytes([
                bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9],
            ]);
            // § 5.2: the high bit MUST be 0 (this is a frame length, not a
            // signed value); and the 64-bit form must carry > 65535.
            if len >> 63 != 0 {
                return Err(WebSocketError::InvalidFrame("64-bit length high bit set"));
            }
            if len <= 65535 {
                return Err(WebSocketError::InvalidFrame("non-canonical 64-bit length"));
            }
            (len, 10)
        }
        // Unreachable: len7 is 0..=127.
        _ => unreachable!("len7 masked to 7 bits"),
    };
    // § 5.5 control-frame invariants: ≤ 125 bytes + FIN set.
    if opcode.is_control() {
        if payload_len > 125 {
            return Err(WebSocketError::InvalidFrame("control frame > 125 bytes"));
        }
        if !fin {
            return Err(WebSocketError::InvalidFrame(
                "control frame fragmented (FIN=0)",
            ));
        }
    }
    // The mask key (4 bytes) follows the length, iff masked.
    let mut mask_key = [0u8; 4];
    if masked {
        let end = consumed + 4;
        if bytes.len() < end {
            return Err(WebSocketError::InvalidFrame("truncated mask key"));
        }
        mask_key.copy_from_slice(&bytes[consumed..end]);
    }
    let total = consumed + if masked { 4 } else { 0 };
    Ok((
        FrameHeader {
            fin,
            opcode,
            masked,
            payload_len,
            mask_key,
        },
        total,
    ))
}

// ---------------------------------------------------------------------------
// § 5.3 — payload masking
// ---------------------------------------------------------------------------

/// Apply the § 5.3 WebSocket XOR mask to `payload` in place (demasking is the
/// same operation as masking — both are XOR with the 4-byte key cycled).
/// `payload[i] ^= mask_key[i % 4]`. Used for both client-side masking (send)
/// and server-side demasking (receive).
pub fn apply_mask(payload: &mut [u8], mask_key: &[u8; 4]) {
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask_key[i & 3];
    }
}

// ---------------------------------------------------------------------------
// § 7.4 — close codes
// ---------------------------------------------------------------------------

/// A WebSocket close code (RFC 6455 § 7.4). Valid codes are `1000..=1003`,
/// `1007`, `1008`, `1010`, `1011`, `1015`, and `3000..=4999` (the
/// application-defined band). The `1004`/`1005`/`1006`/`1012`–`1014` codes are
/// reserved (never sent on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CloseCode(u16);

impl CloseCode {
    /// 1000 — normal closure.
    pub const NORMAL: Self = Self(1000);
    /// 1001 — endpoint going away.
    pub const GOING_AWAY: Self = Self(1001);
    /// 1002 — protocol error.
    pub const PROTOCOL_ERROR: Self = Self(1002);
    /// 1003 — unsupported data.
    pub const UNSUPPORTED_DATA: Self = Self(1003);
    /// 1007 — invalid UTF-8 in a text frame.
    pub const INVALID_PAYLOAD: Self = Self(1007);
    /// 1008 — policy violation.
    pub const POLICY_VIOLATION: Self = Self(1008);
    /// 1010 — mandatory extension missing.
    pub const MANDATORY_EXTENSION: Self = Self(1010);
    /// 1011 — internal server error.
    pub const INTERNAL_ERROR: Self = Self(1011);
    /// 1015 — TLS handshake failure (reserved; never sent on the wire).
    pub const TLS_HANDSHAKE_FAILURE: Self = Self(1015);

    /// The raw code.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Whether `code` is a § 7.4 status code that may legally appear in a `Close`
/// frame payload. Reserved codes (`1004`, `1005`, `1006`, `1012`, `1013`,
/// `1014`) and the 0–999 / 1004–1006 / 1012–1014 / ≥ 5000 bands are rejected.
/// The `3000..=4999` application band is allowed; `1000..=1003`, `1007`,
/// `1008`, `1010`, `1011`, `1015` are the defined protocol codes.
pub fn validate_close_code(code: u16) -> Result<CloseCode, WebSocketError> {
    // § 7.4.2: 0–999 are never used; 1004/1005/1006 are reserved (never sent);
    // 1012/1013/1014 are reserved.
    const RESERVED: &[u16] = &[1004, 1005, 1006, 1012, 1013, 1014];
    if !(1000..5000).contains(&code) {
        return Err(WebSocketError::InvalidCloseCode(code));
    }
    if RESERVED.contains(&code) {
        return Err(WebSocketError::InvalidCloseCode(code));
    }
    // 1xxx range: only the defined codes are valid; 1004–1006 + 1012–1014 are
    // reserved (handled above); 1009 + 1016–1999 are undefined → reject.
    if (1000..=1999).contains(&code) {
        let allowed = matches!(
            code,
            1000 | 1001 | 1002 | 1003 | 1007 | 1008 | 1010 | 1011 | 1015
        );
        if !allowed {
            return Err(WebSocketError::InvalidCloseCode(code));
        }
    }
    // 2000–2999 is reserved (undefined); reject.
    if (2000..=2999).contains(&code) {
        return Err(WebSocketError::InvalidCloseCode(code));
    }
    // 3000–4999 is the application band → always valid.
    Ok(CloseCode(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- § 4.2.2 accept computation ------------------------------------

    #[test]
    fn accept_rfc_worked_example() {
        // RFC 6455 § 4.2.2 worked example.
        assert_eq!(
            compute_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn accept_is_deterministic_and_base64() {
        let a = compute_accept("x3JJHMbDL1EzLkh9GBhXDw==");
        let b = compute_accept("x3JJHMbDL1EzLkh9GBhXDw==");
        assert_eq!(a, b);
        // The accept is 28 chars (base64 of 20 SHA-1 bytes).
        assert_eq!(a.len(), 28);
    }

    #[test]
    fn accept_changes_with_key() {
        assert_ne!(compute_accept("key-one"), compute_accept("key-two"));
    }

    // --- § 4.1 client handshake validation -----------------------------

    fn good_client_headers() -> Vec<(&'static str, &'static str)> {
        vec![
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="), // 16 bytes
        ]
    }

    #[test]
    fn valid_client_handshake_returns_decoded_key() {
        let key = validate_client_handshake(&good_client_headers()).unwrap();
        assert_eq!(key.len(), 16);
    }

    #[test]
    fn client_handshake_case_insensitive_upgrade() {
        let mut h = good_client_headers();
        h[0] = ("Upgrade", "WebSocket");
        assert!(validate_client_handshake(&h).is_ok());
    }

    #[test]
    fn client_handshake_connection_token_list() {
        let mut h = good_client_headers();
        h[1] = ("Connection", "keep-alive, Upgrade");
        assert!(validate_client_handshake(&h).is_ok());
    }

    #[test]
    fn client_handshake_wrong_version_rejected() {
        let mut h = good_client_headers();
        h[2] = ("Sec-WebSocket-Version", "8");
        assert!(matches!(
            validate_client_handshake(&h),
            Err(WebSocketError::InvalidHandshake(_))
        ));
    }

    #[test]
    fn client_handshake_missing_key_rejected() {
        let mut h = good_client_headers();
        h.retain(|(n, _)| *n != "Sec-WebSocket-Key");
        assert!(matches!(
            validate_client_handshake(&h),
            Err(WebSocketError::InvalidHandshake(_))
        ));
    }

    #[test]
    fn client_handshake_short_key_rejected() {
        let mut h = good_client_headers();
        // 8 bytes instead of 16.
        h[3] = ("Sec-WebSocket-Key", "YWJjZGVmZ2g="); // "abcdefgh"
        assert!(matches!(
            validate_client_handshake(&h),
            Err(WebSocketError::InvalidHandshake(_))
        ));
    }

    #[test]
    fn client_handshake_bad_upgrade_rejected() {
        let mut h = good_client_headers();
        h[0] = ("Upgrade", "h2c");
        assert!(matches!(
            validate_client_handshake(&h),
            Err(WebSocketError::InvalidHandshake(_))
        ));
    }

    // --- § 4.2.2 server response validation ----------------------------

    fn good_server_headers(client_key: &str) -> Vec<(String, String)> {
        vec![
            ("Upgrade".to_string(), "websocket".to_string()),
            ("Connection".to_string(), "Upgrade".to_string()),
            (
                "Sec-WebSocket-Accept".to_string(),
                compute_accept(client_key),
            ),
        ]
    }

    fn as_str_refs(h: &[(String, String)]) -> Vec<(&str, &str)> {
        h.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect()
    }

    #[test]
    fn valid_server_response_accepts() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let h = good_server_headers(key);
        assert!(validate_server_response(101, &as_str_refs(&h), key).is_ok());
    }

    #[test]
    fn server_response_wrong_status_rejected() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let h = good_server_headers(key);
        assert!(matches!(
            validate_server_response(200, &as_str_refs(&h), key),
            Err(WebSocketError::InvalidHandshake(_))
        ));
    }

    #[test]
    fn server_response_wrong_accept_rejected() {
        let sent_key = "dGhlIHNhbXBsZSBub25jZQ==";
        let mut h = good_server_headers(sent_key);
        // Tamper: use the accept for a different key.
        h[2] = (
            "Sec-WebSocket-Accept".to_string(),
            compute_accept("some-other-key"),
        );
        assert_eq!(
            validate_server_response(101, &as_str_refs(&h), sent_key),
            Err(WebSocketError::AcceptMismatch)
        );
    }

    // --- § 5.2 frame header parsing ------------------------------------

    #[test]
    fn parse_minimal_unmasked_text_frame() {
        // FIN=1, opcode=1 (text), no mask, len=0.
        let bytes = [0x81, 0x00];
        let (hdr, n) = parse_frame_header(&bytes).unwrap();
        assert!(hdr.fin);
        assert_eq!(hdr.opcode, Opcode::Text);
        assert!(!hdr.masked);
        assert_eq!(hdr.payload_len, 0);
        assert_eq!(n, 2);
        assert_eq!(hdr.header_len(), 2);
    }

    #[test]
    fn parse_masked_frame_with_payload() {
        // FIN=1, opcode=2 (binary), MASK=1, len=5, mask=01020304.
        let bytes = [
            0x82, 0x85, 0x01, 0x02, 0x03, 0x04, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        ];
        let (hdr, n) = parse_frame_header(&bytes).unwrap();
        assert!(hdr.fin);
        assert_eq!(hdr.opcode, Opcode::Binary);
        assert!(hdr.masked);
        assert_eq!(hdr.payload_len, 5);
        assert_eq!(hdr.mask_key, [0x01, 0x02, 0x03, 0x04]);
        assert_eq!(n, 6); // 2 + 4 mask
        assert_eq!(hdr.header_len(), 6);
    }

    #[test]
    fn parse_16_bit_length() {
        // FIN=1, opcode=2, len=126 → next 2 bytes = 200.
        let mut bytes = vec![0x82, 0x7E, 0x00, 0xC8];
        bytes.extend(std::iter::repeat_n(0u8, 200));
        let (hdr, n) = parse_frame_header(&bytes).unwrap();
        assert_eq!(hdr.payload_len, 200);
        assert_eq!(n, 4);
    }

    #[test]
    fn parse_16_bit_length_non_canonical_rejected() {
        // len=126 form carrying ≤ 125 is non-canonical.
        let bytes = [0x82, 0x7E, 0x00, 0x7D]; // 125
        assert!(matches!(
            parse_frame_header(&bytes),
            Err(WebSocketError::InvalidFrame(_))
        ));
    }

    #[test]
    fn parse_64_bit_length() {
        let mut bytes = vec![0x82, 0x7F];
        bytes.extend_from_slice(&70000u64.to_be_bytes());
        let (hdr, n) = parse_frame_header(&bytes).unwrap();
        assert_eq!(hdr.payload_len, 70000);
        assert_eq!(n, 10);
    }

    #[test]
    fn reserved_rsv_bits_rejected() {
        // RSV1 set (0x40).
        let bytes = [0xC1, 0x00];
        assert!(matches!(
            parse_frame_header(&bytes),
            Err(WebSocketError::InvalidFrame(_))
        ));
    }

    #[test]
    fn reserved_opcode_rejected() {
        // Opcode 0x3 (reserved).
        let bytes = [0x83, 0x00];
        assert!(matches!(
            parse_frame_header(&bytes),
            Err(WebSocketError::InvalidFrame(_))
        ));
    }

    #[test]
    fn control_frame_over_125_bytes_rejected() {
        // A close frame (0x88) with len=126.
        let bytes = [0x88, 0x7E, 0x00, 0x80];
        assert!(matches!(
            parse_frame_header(&bytes),
            Err(WebSocketError::InvalidFrame(_))
        ));
    }

    #[test]
    fn fragmented_control_frame_rejected() {
        // FIN=0, opcode=Close → control frame without FIN.
        let bytes = [0x08, 0x00];
        assert!(matches!(
            parse_frame_header(&bytes),
            Err(WebSocketError::InvalidFrame(_))
        ));
    }

    #[test]
    fn truncated_frame_rejected() {
        assert!(matches!(
            parse_frame_header(&[0x81]),
            Err(WebSocketError::InvalidFrame(_))
        ));
    }

    #[test]
    fn truncated_mask_key_rejected() {
        // Claims masked (0x80) but only 2 mask bytes present.
        let bytes = [0x81, 0x82, 0x01, 0x02];
        assert!(matches!(
            parse_frame_header(&bytes),
            Err(WebSocketError::InvalidFrame(_))
        ));
    }

    #[test]
    fn opcode_decode() {
        assert_eq!(Opcode::from_byte(0x01), Some(Opcode::Text));
        assert_eq!(Opcode::from_byte(0x82), Some(Opcode::Binary)); // masked bit ignored
        assert_eq!(Opcode::from_byte(0x0A), Some(Opcode::Pong));
        assert_eq!(Opcode::from_byte(0x03), None); // reserved
        assert!(Opcode::Close.is_control());
        assert!(!Opcode::Text.is_control());
    }

    // --- § 5.3 masking --------------------------------------------------

    #[test]
    fn mask_round_trips() {
        let original = [0x48, 0x65, 0x6C, 0x6C, 0x6F]; // "Hello"
        let mask = [0x37, 0xFA, 0x21, 0x3D];
        let mut buf = original;
        apply_mask(&mut buf, &mask);
        assert_ne!(buf, original); // it changed
        apply_mask(&mut buf, &mask); // XOR again restores
        assert_eq!(buf, original);
    }

    #[test]
    fn mask_rfc_worked_example() {
        // RFC 6455 § 5.7 worked example: mask 37FA213D over "Hello".
        let mask = [0x37, 0xFA, 0x21, 0x3D];
        let mut buf = *b"Hello";
        apply_mask(&mut buf, &mask);
        assert_eq!(buf, [0x7F, 0x9F, 0x4D, 0x51, 0x58]);
    }

    #[test]
    fn mask_empty_payload_noop() {
        let mut buf: [u8; 0] = [];
        apply_mask(&mut buf, &[1, 2, 3, 4]);
    }

    // --- § 7.4 close codes ---------------------------------------------

    #[test]
    fn close_code_defined_protocol_codes() {
        for code in [1000, 1001, 1002, 1003, 1007, 1008, 1010, 1011, 1015] {
            assert!(validate_close_code(code).is_ok(), "{code} should be valid");
        }
    }

    #[test]
    fn close_code_application_band() {
        assert!(validate_close_code(3000).is_ok());
        assert!(validate_close_code(4000).is_ok());
        assert!(validate_close_code(4999).is_ok());
    }

    #[test]
    fn close_code_below_1000_rejected() {
        assert!(validate_close_code(0).is_err());
        assert!(validate_close_code(999).is_err());
    }

    #[test]
    fn close_code_reserved_rejected() {
        for code in [1004, 1005, 1006, 1012, 1013, 1014] {
            assert!(validate_close_code(code).is_err(), "{code} is reserved");
        }
    }

    #[test]
    fn close_code_undefined_1xxx_rejected() {
        assert!(validate_close_code(1009).is_err());
        assert!(validate_close_code(1099).is_err());
        assert!(validate_close_code(1999).is_err());
    }

    #[test]
    fn close_code_reserved_2xxx_rejected() {
        assert!(validate_close_code(2000).is_err());
        assert!(validate_close_code(2999).is_err());
    }

    #[test]
    fn close_code_at_or_above_5000_rejected() {
        assert!(validate_close_code(5000).is_err());
        assert!(validate_close_code(u16::MAX).is_err());
    }

    #[test]
    fn close_code_named_constants() {
        assert_eq!(CloseCode::NORMAL.get(), 1000);
        assert_eq!(CloseCode::GOING_AWAY.get(), 1001);
        assert_eq!(CloseCode::PROTOCOL_ERROR.get(), 1002);
    }
}
