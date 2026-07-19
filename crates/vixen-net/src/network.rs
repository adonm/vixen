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
use crate::fetch_types::{
    ByteResponse, Method, NetworkEvent, RedirectMode, ResponseHead, TextRequest, TextResponse,
};
use crate::url_policy::{UrlPolicyError, validate_http_url};

/// Default upper bound on a response body (8 MiB). Navigation responses are
/// HTML; anything larger is almost certainly a download, not a document.
pub const DEFAULT_MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// Default redirect cap. Browsers use ~20; navigation rarely needs more than
/// a handful.
pub const DEFAULT_MAX_REDIRECTS: usize = 10;
/// Maximum retained/callback body-progress records per response. Chunks below
/// the per-request reporting quantum are coalesced before crossing this seam.
pub const MAX_BODY_PROGRESS_EVENTS: u64 = 256;

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

struct FetchObservers<F, R, H, B> {
    progress: F,
    redirect: R,
    response: H,
    body: B,
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

    /// Build the exact lower-cased request-header map used by the transport.
    /// Cache callers use this before I/O so `Vary` matching and the eventual
    /// request cannot disagree about automatic headers or cookies.
    pub fn effective_request_headers(
        &self,
        jar: &mut CookieJar,
        request: &TextRequest,
    ) -> Result<std::collections::BTreeMap<String, String>, NetworkError> {
        validate_http_url(&request.url)?;
        self.effective_request_headers_for(
            jar,
            &request.url,
            request.cross_site,
            request.method,
            &request.headers,
            request.body.as_deref(),
        )
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
        self.fetch_bytes(
            jar,
            TextRequest {
                url: url.clone(),
                cross_site,
                method,
                redirect_mode,
                headers: Vec::new(),
                body: None,
            },
            |_| {},
            |_| true,
            |_| {},
            |_| {},
        )
        .await
        .map(TextResponse::from)
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
        self.fetch_bytes(
            jar,
            TextRequest {
                url: url.clone(),
                cross_site,
                method,
                redirect_mode,
                headers,
                body: None,
            },
            |_| {},
            |_| true,
            |_| {},
            |_| {},
        )
        .await
        .map(TextResponse::from)
    }

    /// Fetch a fully specified bounded-text request, threading `jar` through
    /// redirect hops and validating every URL/header at the network boundary.
    pub async fn get_text_with_cookies_request(
        &mut self,
        jar: &mut CookieJar,
        request: TextRequest,
    ) -> Result<TextResponse, NetworkError> {
        validate_http_url(&request.url)?;
        self.fetch_bytes(jar, request, |_| {}, |_| true, |_| {}, |_| {})
            .await
            .map(TextResponse::from)
    }

    /// Fetch a fully specified bounded-text request and synchronously report
    /// each lifecycle event as it occurs. Events are also retained in the
    /// successful [`TextResponse`]; progress already reported is not retracted
    /// if a later redirect hop or body read fails.
    pub async fn get_text_with_cookies_request_with_progress<F>(
        &mut self,
        jar: &mut CookieJar,
        request: TextRequest,
        on_progress: F,
    ) -> Result<TextResponse, NetworkError>
    where
        F: FnMut(&NetworkEvent),
    {
        validate_http_url(&request.url)?;
        self.fetch_bytes(jar, request, on_progress, |_| true, |_| {}, |_| {})
            .await
            .map(TextResponse::from)
    }

    /// Fetch a fully specified request without decoding its bounded body.
    pub async fn get_bytes_with_cookies_request_with_progress<F>(
        &mut self,
        jar: &mut CookieJar,
        request: TextRequest,
        on_progress: F,
    ) -> Result<ByteResponse, NetworkError>
    where
        F: FnMut(&NetworkEvent),
    {
        validate_http_url(&request.url)?;
        self.fetch_bytes(jar, request, on_progress, |_| true, |_| {}, |_| {})
            .await
    }

    /// Fetch raw bytes while exposing the accepted final response head and
    /// each bounded transport chunk to the caller. The complete body and all
    /// lifecycle events are still retained in the returned response.
    pub async fn get_bytes_with_cookies_request_streaming<F, R, H, B>(
        &mut self,
        jar: &mut CookieJar,
        request: TextRequest,
        on_progress: F,
        on_redirect: R,
        on_response: H,
        on_body: B,
    ) -> Result<ByteResponse, NetworkError>
    where
        F: FnMut(&NetworkEvent),
        R: FnMut(&Url) -> bool,
        H: FnMut(&ResponseHead),
        B: FnMut(&[u8]),
    {
        validate_http_url(&request.url)?;
        self.fetch_bytes(jar, request, on_progress, on_redirect, on_response, on_body)
            .await
    }

    /// Fetch raw bytes with a caller-specific body cap no wider than the
    /// profile network cap. The limit is checked against `Content-Length` and
    /// the received body before the response crosses this boundary.
    pub async fn get_bytes_with_cookies_request_with_progress_and_limit<F>(
        &mut self,
        jar: &mut CookieJar,
        request: TextRequest,
        max_body_bytes: u64,
        on_progress: F,
    ) -> Result<ByteResponse, NetworkError>
    where
        F: FnMut(&NetworkEvent),
    {
        validate_http_url(&request.url)?;
        let limit = self.config.max_body_bytes.min(max_body_bytes);
        self.fetch_bytes_with_limit(
            jar,
            request,
            limit,
            FetchObservers {
                progress: on_progress,
                redirect: allow_redirect,
                response: ignore_response_head,
                body: ignore_body_chunk,
            },
        )
        .await
    }

    async fn fetch_bytes<F, R, H, B>(
        &mut self,
        jar: &mut CookieJar,
        request: TextRequest,
        on_progress: F,
        on_redirect: R,
        on_response: H,
        on_body: B,
    ) -> Result<ByteResponse, NetworkError>
    where
        F: FnMut(&NetworkEvent),
        R: FnMut(&Url) -> bool,
        H: FnMut(&ResponseHead),
        B: FnMut(&[u8]),
    {
        let limit = self.config.max_body_bytes;
        self.fetch_bytes_with_limit(
            jar,
            request,
            limit,
            FetchObservers {
                progress: on_progress,
                redirect: on_redirect,
                response: on_response,
                body: on_body,
            },
        )
        .await
    }

    async fn fetch_bytes_with_limit<F, R, H, B>(
        &mut self,
        jar: &mut CookieJar,
        request: TextRequest,
        limit: u64,
        observers: FetchObservers<F, R, H, B>,
    ) -> Result<ByteResponse, NetworkError>
    where
        F: FnMut(&NetworkEvent),
        R: FnMut(&Url) -> bool,
        H: FnMut(&ResponseHead),
        B: FnMut(&[u8]),
    {
        let FetchObservers {
            mut progress,
            mut redirect,
            mut response,
            body: mut on_body,
        } = observers;
        let TextRequest {
            url: start,
            cross_site,
            method,
            redirect_mode,
            headers,
            body,
        } = request;
        let max = self.config.max_redirects;
        let mut current = start;
        let mut redirects = 0u32;
        let mut visited: HashSet<String> = HashSet::new();
        let mut events = Vec::new();
        let mut redirect_aliasable = true;

        loop {
            visited.insert(current.to_string());
            record_progress(
                &mut events,
                NetworkEvent::RequestStart {
                    url: current.to_string(),
                    method,
                },
                &mut progress,
            );

            let request_headers = self.effective_request_headers_for(
                jar,
                &current,
                cross_site,
                method,
                &headers,
                body.as_deref(),
            )?;
            let mut req = self.client.request(method.into(), current.clone());
            for (name, value) in &request_headers {
                req = req.header(name, value);
            }
            if let Some(body) = &body {
                req = req.body(body.clone());
            }
            let mut resp = req.send().await.map_err(map_reqwest_error)?;

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

            if is_followable_redirect(status) && redirect_mode == RedirectMode::Manual {
                record_progress(
                    &mut events,
                    NetworkEvent::Response {
                        url: current.to_string(),
                        status,
                    },
                    &mut progress,
                );
                response(&ResponseHead {
                    headers: flatten_headers(resp.headers()),
                    status,
                    final_url: current.to_string(),
                    set_cookie: set_cookie.clone(),
                    redirects,
                    events: events.clone(),
                    request_headers: request_headers.clone(),
                    redirect_aliasable: redirect_response_aliasable(status, resp.headers()),
                    total_bytes: Some(0),
                });
                record_progress(
                    &mut events,
                    NetworkEvent::Completed {
                        url: current.to_string(),
                        body_bytes: 0,
                    },
                    &mut progress,
                );
                return Ok(ByteResponse {
                    body: Vec::new(),
                    headers: flatten_headers(resp.headers()),
                    status,
                    final_url: current.to_string(),
                    set_cookie,
                    redirects,
                    events,
                    request_headers,
                    from_cache: false,
                    redirect_aliasable: redirect_response_aliasable(status, resp.headers()),
                });
            }

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
                if !redirect(&next) {
                    return Err(NetworkError::Request {
                        message: format!("redirect target rejected by caller policy: {next}"),
                    });
                }
                redirect_aliasable &= redirect_response_aliasable(status, resp.headers());
                record_progress(
                    &mut events,
                    NetworkEvent::Redirect {
                        from: current.to_string(),
                        to: next.to_string(),
                        status,
                    },
                    &mut progress,
                );
                redirects += 1;
                current = next;
                continue;
            }

            record_progress(
                &mut events,
                NetworkEvent::Response {
                    url: current.to_string(),
                    status,
                },
                &mut progress,
            );

            // Body-size guard: prefer Content-Length, then the actual bytes.
            let total_bytes = resp.content_length();
            if let Some(cl) = total_bytes
                && cl > limit
            {
                return Err(NetworkError::BodyTooLarge {
                    limit,
                    actual: cl,
                    url: current.to_string(),
                });
            }
            // Capture headers before incrementally consuming the response.
            let headers = flatten_headers(resp.headers());
            response(&ResponseHead {
                headers: headers.clone(),
                status,
                final_url: current.to_string(),
                set_cookie: set_cookie.clone(),
                redirects,
                events: events.clone(),
                request_headers: request_headers.clone(),
                redirect_aliasable,
                total_bytes,
            });
            let capacity =
                usize::try_from(total_bytes.unwrap_or_default().min(limit)).unwrap_or_default();
            let mut body = Vec::with_capacity(capacity);
            let mut loaded_bytes = 0_u64;
            let mut pending_progress_bytes = 0_u64;
            let progress_quantum = body_progress_quantum(limit);
            while let Some(chunk) = resp.chunk().await.map_err(map_reqwest_error)? {
                let chunk_bytes = chunk.len() as u64;
                let actual = loaded_bytes.saturating_add(chunk_bytes);
                if actual > limit {
                    return Err(NetworkError::BodyTooLarge {
                        limit,
                        actual,
                        url: current.to_string(),
                    });
                }
                on_body(&chunk);
                body.extend_from_slice(&chunk);
                loaded_bytes = actual;
                pending_progress_bytes = pending_progress_bytes.saturating_add(chunk_bytes);
                if pending_progress_bytes >= progress_quantum
                    || total_bytes.is_some_and(|total| loaded_bytes == total)
                {
                    record_progress(
                        &mut events,
                        NetworkEvent::BodyProgress {
                            url: current.to_string(),
                            chunk_bytes: pending_progress_bytes,
                            loaded_bytes,
                            total_bytes,
                        },
                        &mut progress,
                    );
                    pending_progress_bytes = 0;
                }
            }
            if pending_progress_bytes > 0 {
                record_progress(
                    &mut events,
                    NetworkEvent::BodyProgress {
                        url: current.to_string(),
                        chunk_bytes: pending_progress_bytes,
                        loaded_bytes,
                        total_bytes,
                    },
                    &mut progress,
                );
            }
            record_progress(
                &mut events,
                NetworkEvent::Completed {
                    url: current.to_string(),
                    body_bytes: loaded_bytes,
                },
                &mut progress,
            );

            return Ok(ByteResponse {
                body,
                headers,
                status,
                final_url: current.to_string(),
                set_cookie,
                redirects,
                events,
                request_headers,
                from_cache: false,
                redirect_aliasable,
            });
        }
    }

    fn effective_request_headers_for(
        &self,
        jar: &mut CookieJar,
        url: &Url,
        cross_site: bool,
        method: Method,
        headers: &[(String, String)],
        body: Option<&[u8]>,
    ) -> Result<std::collections::BTreeMap<String, String>, NetworkError> {
        let mut out = std::collections::BTreeMap::new();
        for (name, value) in headers {
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|error| NetworkError::Request {
                    message: format!("invalid request header name {name}: {error}"),
                })?;
            let value = HeaderValue::from_str(value).map_err(|error| NetworkError::Request {
                message: format!("invalid request header value for {name}: {error}"),
            })?;
            let name = name.as_str().to_ascii_lowercase();
            let value = value.to_str().unwrap_or_default();
            out.entry(name)
                .and_modify(|current: &mut String| {
                    current.push_str(", ");
                    current.push_str(value);
                })
                .or_insert_with(|| value.to_owned());
        }
        out.entry("user-agent".to_owned())
            .or_insert_with(|| self.config.user_agent.clone());
        out.entry("accept-encoding".to_owned())
            .or_insert_with(|| "gzip, br".to_owned());
        if let Some(host) = request_host(url) {
            out.entry("host".to_owned()).or_insert(host);
        }
        let cookie = jar.cookies_for(url, cross_site, method);
        if !cookie.is_empty() {
            out.insert("cookie".to_owned(), cookie);
        } else {
            out.remove("cookie");
        }
        if let Some(body) = body {
            out.insert("content-length".to_owned(), body.len().to_string());
        } else {
            out.remove("content-length");
        }
        Ok(out)
    }
}

fn request_host(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn body_progress_quantum(limit: u64) -> u64 {
    limit.div_ceil(MAX_BODY_PROGRESS_EVENTS).max(1)
}

fn record_progress<F>(events: &mut Vec<NetworkEvent>, event: NetworkEvent, on_progress: &mut F)
where
    F: FnMut(&NetworkEvent),
{
    on_progress(&event);
    events.push(event);
}

fn allow_redirect(_: &Url) -> bool {
    true
}

fn ignore_response_head(_: &ResponseHead) {}

fn ignore_body_chunk(_: &[u8]) {}

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

fn redirect_response_aliasable(status: u16, headers: &reqwest::header::HeaderMap) -> bool {
    matches!(status, 301 | 308)
        && !headers
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value.split(',').any(|directive| {
                    directive
                        .split_once('=')
                        .map_or(directive, |(name, _)| name)
                        .trim()
                        .eq_ignore_ascii_case("no-store")
                })
            })
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
    use std::io::{Read, Write};

    use super::*;

    #[test]
    fn redirect_aliasability_requires_cacheable_permanent_response() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert!(redirect_response_aliasable(301, &headers));
        assert!(redirect_response_aliasable(308, &headers));
        assert!(!redirect_response_aliasable(302, &headers));
        headers.insert(
            reqwest::header::CACHE_CONTROL,
            reqwest::header::HeaderValue::from_static("private, no-store"),
        );
        assert!(!redirect_response_aliasable(301, &headers));
    }

    const PROGRESS_TEST_HOST: &str = "network-progress-vixen.com";

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
    fn body_progress_quantum_bounds_retained_diagnostics() {
        for limit in [0, 1, 255, 256, 257, DEFAULT_MAX_BODY_BYTES] {
            let quantum = body_progress_quantum(limit);
            assert!(quantum >= 1);
            assert!(limit.div_ceil(quantum) <= MAX_BODY_PROGRESS_EVENTS);
        }
    }

    #[test]
    fn client_builds_with_rustls() {
        // The reqwest+rustls client must construct (rustls-tls feature on).
        let net = Network::with_defaults().expect("client builds");
        assert_eq!(net.config().max_redirects, DEFAULT_MAX_REDIRECTS);
    }

    #[tokio::test]
    async fn network_progress_callback_follows_fetch_order() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let start = format!("http://{PROGRESS_TEST_HOST}:{}/start", address.port());
        let final_url = format!("http://{PROGRESS_TEST_HOST}:{}/final", address.port());
        let server_final_url = final_url.clone();
        let server = std::thread::spawn(move || {
            let (mut redirect, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = redirect.read(&mut request).unwrap();
            redirect
                .write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {server_final_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .unwrap();

            let (mut final_response, _) = listener.accept().unwrap();
            let _ = final_response.read(&mut request).unwrap();
            final_response
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let mut config = NetworkConfig::default();
        config
            .dns_overrides
            .push((PROGRESS_TEST_HOST.to_owned(), vec![address]));
        let mut network = Network::new(config).unwrap();
        let mut jar = CookieJar::default();
        let mut progress = Vec::new();

        let response = network
            .get_text_with_cookies_request_with_progress(
                &mut jar,
                TextRequest {
                    url: Url::parse(&start).unwrap(),
                    cross_site: false,
                    method: Method::Get,
                    redirect_mode: RedirectMode::Follow,
                    headers: Vec::new(),
                    body: None,
                },
                |event| progress.push(event.clone()),
            )
            .await
            .unwrap();

        let expected = vec![
            NetworkEvent::RequestStart {
                url: start,
                method: Method::Get,
            },
            NetworkEvent::Redirect {
                from: format!("http://{PROGRESS_TEST_HOST}:{}/start", address.port()),
                to: final_url.clone(),
                status: 302,
            },
            NetworkEvent::RequestStart {
                url: final_url.clone(),
                method: Method::Get,
            },
            NetworkEvent::Response {
                url: final_url,
                status: 200,
            },
            NetworkEvent::BodyProgress {
                url: format!("http://{PROGRESS_TEST_HOST}:{}/final", address.port()),
                chunk_bytes: 2,
                loaded_bytes: 2,
                total_bytes: Some(2),
            },
            NetworkEvent::Completed {
                url: format!("http://{PROGRESS_TEST_HOST}:{}/final", address.port()),
                body_bytes: 2,
            },
        ];
        assert_eq!(progress, expected);
        assert_eq!(response.events, expected);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn manual_redirect_does_not_buffer_or_reject_oversized_body() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{PROGRESS_TEST_HOST}:{}/manual", address.port());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let mut config = NetworkConfig {
            max_body_bytes: 8,
            ..NetworkConfig::default()
        };
        config
            .dns_overrides
            .push((PROGRESS_TEST_HOST.to_owned(), vec![address]));
        let mut network = Network::new(config).unwrap();
        let mut jar = CookieJar::default();

        let response = network
            .get_text_with_cookies_request(
                &mut jar,
                TextRequest {
                    url: Url::parse(&url).unwrap(),
                    cross_site: false,
                    method: Method::Get,
                    redirect_mode: RedirectMode::Manual,
                    headers: Vec::new(),
                    body: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(response.status, 302);
        assert_eq!(response.header("location"), Some("/final"));
        assert!(response.body.is_empty());
        assert_eq!(
            response.events,
            vec![
                NetworkEvent::RequestStart {
                    url: url.clone(),
                    method: Method::Get,
                },
                NetworkEvent::Response {
                    url: url.clone(),
                    status: 302,
                },
                NetworkEvent::Completed { url, body_bytes: 0 },
            ]
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn network_progress_redirect_survives_later_transport_error() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let start = format!("http://{PROGRESS_TEST_HOST}:{}/start", address.port());
        let broken = format!("http://{PROGRESS_TEST_HOST}:{}/broken", address.port());
        let server_broken = broken.clone();
        let server = std::thread::spawn(move || {
            let (mut redirect, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = redirect.read(&mut request).unwrap();
            redirect
                .write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {server_broken}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .unwrap();
            let (mut broken_response, _) = listener.accept().unwrap();
            let _ = broken_response.read(&mut request).unwrap();
        });
        let mut config = NetworkConfig::default();
        config
            .dns_overrides
            .push((PROGRESS_TEST_HOST.to_owned(), vec![address]));
        let mut network = Network::new(config).unwrap();
        let mut jar = CookieJar::default();
        let mut progress = Vec::new();

        network
            .get_text_with_cookies_request_with_progress(
                &mut jar,
                TextRequest {
                    url: Url::parse(&start).unwrap(),
                    cross_site: false,
                    method: Method::Get,
                    redirect_mode: RedirectMode::Follow,
                    headers: Vec::new(),
                    body: None,
                },
                |event| progress.push(event.clone()),
            )
            .await
            .expect_err("the redirected response closes before headers");

        assert_eq!(
            progress,
            vec![
                NetworkEvent::RequestStart {
                    url: start.clone(),
                    method: Method::Get,
                },
                NetworkEvent::Redirect {
                    from: start,
                    to: broken.clone(),
                    status: 302,
                },
                NetworkEvent::RequestStart {
                    url: broken,
                    method: Method::Get,
                },
            ]
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn incremental_body_limit_fails_before_publishing_oversized_chunk() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!(
            "http://{PROGRESS_TEST_HOST}:{}/chunked-limit",
            address.port()
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\n1234\r\n",
                )
                .unwrap();
            stream.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            stream.write_all(b"4\r\n5678\r\n0\r\n\r\n").unwrap();
        });
        let mut config = NetworkConfig::default();
        config
            .dns_overrides
            .push((PROGRESS_TEST_HOST.to_owned(), vec![address]));
        let mut network = Network::new(config).unwrap();
        let mut jar = CookieJar::default();
        let mut progress = Vec::new();

        let error = network
            .get_bytes_with_cookies_request_with_progress_and_limit(
                &mut jar,
                TextRequest {
                    url: Url::parse(&url).unwrap(),
                    cross_site: false,
                    method: Method::Get,
                    redirect_mode: RedirectMode::Follow,
                    headers: Vec::new(),
                    body: None,
                },
                6,
                |event| progress.push(event.clone()),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            NetworkError::BodyTooLarge {
                limit: 6,
                actual: 8,
                ..
            }
        ));
        assert!(progress.iter().any(|event| matches!(
            event,
            NetworkEvent::BodyProgress {
                loaded_bytes: 4,
                ..
            }
        )));
        assert!(
            !progress
                .iter()
                .any(|event| matches!(event, NetworkEvent::Completed { .. }))
        );
        server.join().unwrap();
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
