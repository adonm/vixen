//! GTK-independent address-bar input normalization.

use std::net::IpAddr;
use std::path::Path;

pub const START_URI: &str = "about:vixen";

const SEARCH_URL_PREFIX: &str = "https://duckduckgo.com/";

pub fn normalize_address(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "about:blank".to_owned();
    }
    let path = Path::new(trimmed);
    if path.exists()
        && let Ok(abs) = path.canonicalize()
        && let Ok(uri) = url::Url::from_file_path(abs)
    {
        return uri.to_string();
    }
    if is_probable_web_address(trimmed) {
        return format!("https://{trimmed}");
    }
    if has_url_scheme(trimmed) {
        return trimmed.to_owned();
    }
    search_url(trimmed)
}

fn search_url(query: &str) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("q", query);
    format!("{SEARCH_URL_PREFIX}?{}", serializer.finish())
}

fn is_probable_web_address(input: &str) -> bool {
    if input.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return false;
    }
    let authority_end = input.find(['/', '?', '#']).unwrap_or(input.len());
    let authority = &input[..authority_end];
    if authority.is_empty() || authority.starts_with('.') || authority.ends_with('.') {
        return false;
    }
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    let Some((host, port_ok)) = host_and_optional_port(host_port) else {
        return false;
    };
    port_ok && is_probable_host(host)
}

fn host_and_optional_port(input: &str) -> Option<(&str, bool)> {
    if let Some(rest) = input.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']')?;
        let port_ok = suffix
            .strip_prefix(':')
            .is_none_or(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()));
        return Some((host, port_ok));
    }
    if let Some((host, port)) = input.rsplit_once(':') {
        return Some((
            host,
            !host.is_empty() && !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()),
        ));
    }
    Some((input, true))
}

fn is_probable_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok() {
        return true;
    }
    host.contains('.')
        && host
            .split('.')
            .all(|label| !label.is_empty() && label.bytes().all(host_label_byte))
}

fn host_label_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}

fn has_url_scheme(input: &str) -> bool {
    let Some((scheme, _)) = input.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_explicit_urls_and_special_pages() {
        assert_eq!(
            normalize_address(" https://example.com/a "),
            "https://example.com/a"
        );
        assert_eq!(
            normalize_address("file:///tmp/page.html"),
            "file:///tmp/page.html"
        );
        assert_eq!(normalize_address("about:blank"), "about:blank");
        assert_eq!(
            normalize_address("data:text/html,<h1>Hello world</h1>"),
            "data:text/html,<h1>Hello world</h1>"
        );
    }

    #[test]
    fn detects_bare_web_addresses() {
        assert_eq!(normalize_address("example.com"), "https://example.com");
        assert_eq!(
            normalize_address("example.com/docs?q=rust"),
            "https://example.com/docs?q=rust"
        );
        assert_eq!(
            normalize_address("localhost:8080"),
            "https://localhost:8080"
        );
        assert_eq!(
            normalize_address("127.0.0.1:8080"),
            "https://127.0.0.1:8080"
        );
    }

    #[test]
    fn turns_search_like_input_into_search_urls() {
        assert_eq!(
            normalize_address("rust browser"),
            "https://duckduckgo.com/?q=rust+browser"
        );
        assert_eq!(
            normalize_address("vixen"),
            "https://duckduckgo.com/?q=vixen"
        );
    }
}
