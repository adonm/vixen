//! Small HTTP header helpers. The important one is [`assert_safe_header_value`],
//! which rejects CRLF / control bytes to prevent header injection at the
//! network trust boundary (docs/ARCHITECTURE.md "Trust boundaries").

/// Reject header field values containing CR, LF, or other control bytes.
///
/// Outgoing request headers (e.g. cookies, `User-Agent`) are assembled from
/// partially-untrusted input. RFC 7230 §3.2.4 forbids CTL characters in
/// field values; a raw `\r\n` would let an attacker smuggle a second
/// header/request. This is the fail-closed gate before bytes reach the wire.
pub fn assert_safe_header_value(value: &str) -> Result<(), HeaderError> {
    for b in value.bytes() {
        // field-content = *( HTAB / SP / VCHAR / obs-text ); reject everything else.
        let ok =
            b == b'\t' || b == b' ' || (0x21..=0x7e).contains(&b) || (0x80..=0xff).contains(&b);
        if !ok {
            return Err(HeaderError::ControlByte(b));
        }
    }
    Ok(())
}

/// Reject header field *names* containing anything outside the RFC 7230
/// `token` grammar (must be ASCII, no separators).
pub fn assert_valid_header_name(name: &str) -> Result<(), HeaderError> {
    if name.is_empty() {
        return Err(HeaderError::EmptyName);
    }
    for b in name.bytes() {
        // token = "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "."
        //       / "^" / "_" / "`" / "|" / "~" / DIGIT / ALPHA
        let ok = matches!(b, b'!' | b'#'..=b'\'' | b'*' | b'+' | b'-' | b'.' | b'^'..=b'`' | b'|' | b'~' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z');
        if !ok {
            return Err(HeaderError::InvalidNameByte(b));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    #[error("control byte 0x{0:02x} in header value")]
    ControlByte(u8),
    #[error("invalid byte 0x{0:02x} in header name")]
    InvalidNameByte(u8),
    #[error("empty header name")]
    EmptyName,
}

/// Parse a `name=value` parameter (used for `Max-Age=3600`, `Path=/`, etc.).
/// Returns `(name_trimmed, value_trimmed)` or `None` if there is no `=`.
pub fn split_attribute(s: &str) -> Option<(&str, &str)> {
    s.split_once('=')
        .map(|(k, v)| (k.trim(), v.trim().trim_matches('"')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_in_value_is_rejected() {
        assert!(assert_safe_header_value("good value").is_ok());
        assert!(assert_safe_header_value("bad\r\nX-Injected: yes").is_err());
        assert!(assert_safe_header_value("line\nbreak").is_err());
        assert!(assert_safe_header_value("null\0byte").is_err());
        assert!(assert_safe_header_value("tab\tok").is_ok()); // HTAB allowed
        assert!(assert_safe_header_value("unicode café").is_ok()); // obs-text allowed
    }

    #[test]
    fn header_name_must_be_token() {
        assert!(assert_valid_header_name("Content-Type").is_ok());
        assert!(assert_valid_header_name("X_Custom").is_ok());
        assert!(assert_valid_header_name("Bad:Name").is_err()); // ':' is a separator
        assert!(assert_valid_header_name("").is_err());
        assert!(assert_valid_header_name("Space Name").is_err());
    }

    #[test]
    fn split_attribute_trims_and_unquotes() {
        assert_eq!(split_attribute("Max-Age = 3600"), Some(("Max-Age", "3600")));
        assert_eq!(split_attribute("Path=/"), Some(("Path", "/")));
        assert_eq!(split_attribute("nominal"), None);
        assert_eq!(split_attribute("name=\"quoted\""), Some(("name", "quoted")));
    }
}
