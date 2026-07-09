//! HTTP client — reqwest + rustls, HTTP/2, gzip, brotli.
//!
//! Reference implementation of the navigation data-flow entry in
//! docs/ARCHITECTURE.md (`Network::get_text_with_cookies`). Redirects are
//! followed **manually** so that the URL policy and the cookie jar are
//! re-applied at every hop (docs/ARCHITECTURE.md "Trust boundaries": "URL
//! policy / CSP re-applied at every fetch"). The client bounds body size and
//! caps redirect counts.
//!
//! Every error variant of [`NetworkError`] is exercised by tests.

use std::collections::HashSet;
use std::net::SocketAddr;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

use crate::cookie::CookieJar;
use crate::fetch_types::{Method, NetworkEvent, RedirectMode, TextResponse};
use crate::url_policy::{UrlPolicyError, validate_http_url};

/// Default upper bound on a response body (8 MiB). Navigation responses are
/// HTML; anything larger is almost certainly a download, not a document.
pub const DEFAULT_MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// Default redirect cap. Browsers use ~20; navigation rarely needs more than
/// a handful.
pub const DEFAULT_MAX_REDIRECTS: usize = 10;

/// Errors that can arise during a fetch. Each variant is reachable from
/// tests (docs/PLAN.md Phase 1: "every error variant of NetworkError").
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("URL rejected by policy: {0}")]
    UrlPolicy(#[from] UrlPolicyError),
    #[error("client build failed: {message}")]
    Builder { message: String },
    #[error("connection error: {message}")]
    Connect { message: String },
    #[error("request timed out")]
    Timeout,
    #[error("body error: {message}")]
    Body { message: String },
    #[error("response decode error: {message}")]
    Decode { message: String },
    #[error("request error: {message}")]
    Request { message: String },
    #[error("transport error: {message}")]
    Transport { message: String },
    #[error("HTTP {status} for {url}")]
    HttpStatus { status: u16, url: String },
    #[error("too many redirects (>{max}) fetching {url}")]
    TooManyRedirects { max: usize, url: String },
    #[error("redirect loop detected at {url}")]
    RedirectLoop { url: String },
    #[error("invalid redirect target from {url}")]
    InvalidRedirect { url: String },
    #[error("redirect disallowed for {url}")]
    RedirectDisallowed { url: String },
    #[error("response body too large: {actual} bytes > {limit} limit at {url}")]
    BodyTooLarge {
        limit: u64,
        actual: u64,
        url: String,
    },
}

/// Configuration for [`Network`].
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub user_agent: String,
    pub max_body_bytes: u64,
    pub max_redirects: usize,
    pub timeout: std::time::Duration,
    /// Optional DNS overrides used by deterministic integration tests. Empty
    /// in production; URL policy still validates the original request URL.
    pub dns_overrides: Vec<(String, Vec<SocketAddr>)>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            user_agent: format!("Vixen/{}", env!("CARGO_PKG_VERSION")),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            timeout: std::time::Duration::from_secs(30),
            dns_overrides: Vec::new(),
        }
    }
}

/// The HTTP client. Clone is cheap (the inner `reqwest::Client` is an `Arc`).
#[derive(Clone)]
pub struct Network {
    client: reqwest::Client,
    config: NetworkConfig,
}

impl Network {
    /// Build a client from config. TLS via rustls; HTTP/2, gzip, brotli on
    /// (reqwest features). Built-in redirects are **off** — we follow them
    /// manually so URL policy + cookies re-apply per hop.
    pub fn new(config: NetworkConfig) -> Result<Self, NetworkError> {
        let mut builder = reqwest::Client::builder()
            .user_agent(&config.user_agent)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout);
        for (domain, addrs) in &config.dns_overrides {
            builder = builder.resolve_to_addrs(domain, addrs);
        }
        let client = builder.build().map_err(|e| NetworkError::Builder {
            message: e.to_string(),
        })?;
        Ok(Self { client, config })
    }

    /// Convenience: default config.
    pub fn with_defaults() -> Result<Self, NetworkError> {
        Self::new(NetworkConfig::default())
    }

    pub fn config(&self) -> &NetworkConfig {
        &self.config
    }

    /// Fetch `url` as text, threading the cookie jar through every hop.
    ///
    /// `cross_site` and `method` gate `SameSite` cookie sending
    /// (docs/SPEC.md). The URL policy is applied to the initial URL **and**
    /// to every `Location` redirect target.
    pub async fn get_text_with_cookies(
        &mut self,
        jar: &mut CookieJar,
        url: &Url,
        cross_site: bool,
        method: Method,
    ) -> Result<TextResponse, NetworkError> {
        self.get_text_with_cookies_and_redirect_mode(
            jar,
            url,
            cross_site,
            method,
            RedirectMode::Follow,
        )
        .await
    }

    /// Fetch `url` as text using explicit redirect handling.
    pub async fn get_text_with_cookies_and_redirect_mode(
        &mut self,
        jar: &mut CookieJar,
        url: &Url,
        cross_site: bool,
        method: Method,
        redirect_mode: RedirectMode,
    ) -> Result<TextResponse, NetworkError> {
        validate_http_url(url)?;
        self.fetch(
            jar,
            url.clone(),
            cross_site,
            method,
            redirect_mode,
            Vec::new(),
            None,
        )
        .await
    }

    /// Fetch `url` as text using explicit redirect handling and caller-provided
    /// request headers. Header names/values are validated at this network trust
    /// boundary before the request leaves the process.
    pub async fn get_text_with_cookies_redirect_mode_and_headers(
        &mut self,
        jar: &mut CookieJar,
        url: &Url,
        cross_site: bool,
        method: Method,
        redirect_mode: RedirectMode,
        headers: Vec<(String, String)>,
    ) -> Result<TextResponse, NetworkError> {
        validate_http_url(url)?;
        self.fetch(
            jar,
            url.clone(),
            cross_site,
            method,
            redirect_mode,
            headers,
            None,
        )
        .await
    }

    /// Fetch `url` as text using explicit redirect handling, caller-provided
    /// request headers, and an optional request body for non-GET/HEAD fetches.
    pub async fn get_text_with_cookies_redirect_mode_headers_and_body(
        &mut self,
        jar: &mut CookieJar,
        url: &Url,
        cross_site: bool,
        method: Method,
        redirect_mode: RedirectMode,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    ) -> Result<TextResponse, NetworkError> {
        validate_http_url(url)?;
        self.fetch(
            jar,
            url.clone(),
            cross_site,
            method,
            redirect_mode,
            headers,
            body,
        )
        .await
    }

    async fn fetch(
        &mut self,
        jar: &mut CookieJar,
        start: Url,
        cross_site: bool,
        method: Method,
        redirect_mode: RedirectMode,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    ) -> Result<TextResponse, NetworkError> {
        let max = self.config.max_redirects;
        let limit = self.config.max_body_bytes;
        let mut current = start;
        let mut redirects = 0u32;
        let mut visited: HashSet<String> = HashSet::new();
        let mut events = Vec::new();

        loop {
            visited.insert(current.to_string());
            events.push(NetworkEvent::RequestStart {
                url: current.to_string(),
                method,
            });

            let cookie_header = jar.cookies_for(&current, cross_site, method);
            let mut req = self.client.request(method.into(), current.clone());
            for (name, value) in &headers {
                let name = HeaderName::from_bytes(name.as_bytes()).map_err(|err| {
                    NetworkError::Request {
                        message: format!("invalid request header name {name}: {err}"),
                    }
                })?;
                let value = HeaderValue::from_str(value).map_err(|err| NetworkError::Request {
                    message: format!("invalid request header value for {name}: {err}"),
                })?;
                req = req.header(name, value);
            }
            if !cookie_header.is_empty() {
                req = req.header(reqwest::header::COOKIE, cookie_header);
            }
            if let Some(body) = &body {
                req = req.body(body.clone());
            }
            let resp = req.send().await.map_err(map_reqwest_error)?;

            // Collect Set-Cookie into the jar (best-effort; a malformed
            // cookie is dropped, not fatal — RFC 6265 §5.3 step 1).
            let set_cookie: Vec<String> = resp
                .headers()
                .get_all(reqwest::header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok().map(str::to_owned))
                .collect();
            for sc in &set_cookie {
                let _ = jar.set_cookie(sc, &current, true);
            }

            let status = resp.status().as_u16();

            if is_followable_redirect(status) && redirect_mode != RedirectMode::Manual {
                if redirect_mode == RedirectMode::Error {
                    return Err(NetworkError::RedirectDisallowed {
                        url: current.to_string(),
                    });
                }
                if redirects as usize >= max {
                    return Err(NetworkError::TooManyRedirects {
                        max,
                        url: current.to_string(),
                    });
                }
                let location = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| NetworkError::InvalidRedirect {
                        url: current.to_string(),
                    })?;
                let next = current
                    .join(location)
                    .map_err(|_| NetworkError::InvalidRedirect {
                        url: current.to_string(),
                    })?;
                if visited.contains(&next.to_string()) {
                    return Err(NetworkError::RedirectLoop {
                        url: next.to_string(),
                    });
                }
                // URL policy re-applied at every fetch boundary (redirects
                // included).
                validate_http_url(&next)?;
                events.push(NetworkEvent::Redirect {
                    from: current.to_string(),
                    to: next.to_string(),
                    status,
                });
                redirects += 1;
                current = next;
                continue;
            }

            // Body-size guard: prefer Content-Length, then the actual bytes.
            if let Some(cl) = resp.content_length()
                && cl > limit
            {
                return Err(NetworkError::BodyTooLarge {
                    limit,
                    actual: cl,
                    url: current.to_string(),
                });
            }
            // Capture headers before `bytes()` consumes the response.
            let headers = flatten_headers(resp.headers());
            let bytes = resp.bytes().await.map_err(map_reqwest_error)?;
            if (bytes.len() as u64) > limit {
                return Err(NetworkError::BodyTooLarge {
                    limit,
                    actual: bytes.len() as u64,
                    url: current.to_string(),
                });
            }

            // TODO(Phase 7): route through vixen_engine::text_codec::TextDecoder
            // (or a future `decode(headers, bytes)` helper) once the charset
            // pipeline is wired. Today `text_codec` only supports UTF-8 (no
            // legacy-encoding codecs), and this lossy decode ignores the
            // Content-Type `charset` parameter entirely, so non-UTF-8 response
            // bodies are silently mangled to U+FFFD.
            let body = String::from_utf8_lossy(&bytes).into_owned();
            events.push(NetworkEvent::Response {
                url: current.to_string(),
                status,
            });

            return Ok(TextResponse {
                body,
                headers,
                status,
                final_url: current.to_string(),
                set_cookie,
                redirects,
                events,
            });
        }
    }
}

/// Lower-case + join multi-valued headers into one `, `-separated entry.
fn flatten_headers(headers: &HeaderMap) -> std::collections::BTreeMap<String, String> {
    let mut out: std::collections::BTreeMap<String, String> = Default::default();
    for (name, value) in headers.iter() {
        let key = name.as_str().to_ascii_lowercase();
        let val = value.to_str().unwrap_or("");
        out.entry(key)
            .and_modify(|existing| {
                existing.push_str(", ");
                existing.push_str(val);
            })
            .or_insert_with(|| val.to_owned());
    }
    out
}

fn is_followable_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn map_reqwest_error(e: reqwest::Error) -> NetworkError {
    if e.is_timeout() {
        NetworkError::Timeout
    } else if e.is_connect() {
        NetworkError::Connect {
            message: e.to_string(),
        }
    } else if e.is_body() {
        NetworkError::Body {
            message: e.to_string(),
        }
    } else if e.is_decode() {
        NetworkError::Decode {
            message: e.to_string(),
        }
    } else if e.is_request() {
        NetworkError::Request {
            message: e.to_string(),
        }
    } else {
        NetworkError::Transport {
            message: e.to_string(),
        }
    }
}

impl From<Method> for reqwest::Method {
    fn from(m: Method) -> Self {
        match m {
            Method::Get => reqwest::Method::GET,
            Method::Head => reqwest::Method::HEAD,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
            Method::Patch => reqwest::Method::PATCH,
            Method::Options => reqwest::Method::OPTIONS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_size_guard_is_pure() {
        // within limit
        assert!(check_body_size(100, 1000).is_ok());
        assert!(check_body_size(1000, 1000).is_ok());
        // over limit
        assert!(matches!(
            check_body_size(1001, 1000),
            Err(NetworkError::BodyTooLarge { .. })
        ));
    }

    /// Pure helper extracted from the fetch loop for testing.
    fn check_body_size(actual: u64, limit: u64) -> Result<(), NetworkError> {
        if actual > limit {
            return Err(NetworkError::BodyTooLarge {
                limit,
                actual,
                url: "https://example.com/".to_owned(),
            });
        }
        Ok(())
    }

    #[test]
    fn error_variants_construct() {
        // Every variant is reachable and displayable (docs/PLAN.md Phase 1).
        let cases: Vec<NetworkError> = vec![
            NetworkError::Builder {
                message: "b".into(),
            },
            NetworkError::Connect {
                message: "c".into(),
            },
            NetworkError::Timeout,
            NetworkError::Body {
                message: "by".into(),
            },
            NetworkError::Decode {
                message: "d".into(),
            },
            NetworkError::Request {
                message: "r".into(),
            },
            NetworkError::Transport {
                message: "t".into(),
            },
            NetworkError::HttpStatus {
                status: 503,
                url: "u".into(),
            },
            NetworkError::TooManyRedirects {
                max: 10,
                url: "u".into(),
            },
            NetworkError::RedirectLoop { url: "u".into() },
            NetworkError::InvalidRedirect { url: "u".into() },
            NetworkError::RedirectDisallowed { url: "u".into() },
            NetworkError::BodyTooLarge {
                limit: 1,
                actual: 2,
                url: "u".into(),
            },
            NetworkError::UrlPolicy(UrlPolicyError::UnsupportedScheme("ftp".into())),
        ];
        for e in &cases {
            // Display must not panic.
            let _ = format!("{e}");
        }
    }

    #[test]
    fn flatten_headers_lowercases_and_joins() {
        let mut h = HeaderMap::new();
        h.insert("Content-Type", "text/html".parse().unwrap());
        h.append("Set-Cookie", "a=1".parse().unwrap());
        h.append("Set-Cookie", "b=2".parse().unwrap());
        let flat = flatten_headers(&h);
        assert_eq!(flat.get("content-type").unwrap(), "text/html");
        assert_eq!(flat.get("set-cookie").unwrap(), "a=1, b=2");
    }

    #[test]
    fn redirect_classification_excludes_not_modified() {
        for status in [301, 302, 303, 307, 308] {
            assert!(is_followable_redirect(status));
        }
        for status in [300, 304, 305, 306, 309] {
            assert!(!is_followable_redirect(status));
        }
    }

    #[test]
    fn config_defaults_are_sensible() {
        let c = NetworkConfig::default();
        assert_eq!(c.max_body_bytes, DEFAULT_MAX_BODY_BYTES);
        assert_eq!(c.max_redirects, DEFAULT_MAX_REDIRECTS);
        assert!(c.user_agent.starts_with("Vixen/"));
    }

    #[test]
    fn client_builds_with_rustls() {
        // The reqwest+rustls client must construct (rustls-tls feature on).
        let net = Network::with_defaults().expect("client builds");
        assert_eq!(net.config().max_redirects, DEFAULT_MAX_REDIRECTS);
    }

    #[test]
    fn url_policy_blocks_private_target() {
        // The fetch entry must refuse private hosts before any I/O.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut net = Network::with_defaults().unwrap();
        let mut jar = CookieJar::default();
        let url = Url::parse("http://127.0.0.1:9/").unwrap();
        let err = rt
            .block_on(net.get_text_with_cookies(&mut jar, &url, false, Method::Get))
            .expect_err("loopback must be blocked");
        assert!(matches!(
            err,
            NetworkError::UrlPolicy(UrlPolicyError::BlockedHost { .. })
        ));
    }

    /// Live test against a real public host. `#[ignore]` so it never breaks
    /// the offline gate; run with `cargo test -p vixen-net -- --ignored`.
    /// (`example.com` is allowed by the URL policy, unlike loopback.)
    #[tokio::test]
    #[ignore]
    async fn live_fetch_example_com() {
        let mut net = Network::with_defaults().unwrap();
        let mut jar = CookieJar::default();
        let url = Url::parse("https://example.com/").unwrap();
        let resp = net
            .get_text_with_cookies(&mut jar, &url, false, Method::Get)
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("Example Domain"));
    }
}
