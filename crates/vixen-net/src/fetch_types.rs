//! Request/response types shared by the networking layer
//! (docs/ARCHITECTURE.md "Data flow per navigation"). Kept free of
//! `reqwest` types so the seam stays implementation-agnostic.

use std::collections::BTreeMap;

use url::Url;

/// HTTP methods Vixen initiates. `is_safe` follows RFC 7231 §4.2.1: safe
/// methods (GET/HEAD/OPTIONS) do not mutate server state and gate
/// `SameSite=Lax` cross-site cookie sending (docs/SPEC.md "Cookie
/// defaults").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Delete,
    Patch,
    Options,
}

/// Fetch redirect handling mode. `Follow` preserves Vixen's existing
/// navigation behavior; `Error` and `Manual` are used by script fetch options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectMode {
    Follow,
    Error,
    Manual,
}

/// One fully specified bounded-text network request.
///
/// Keeping request metadata together prevents method/body/header policy from
/// drifting as navigation, runtime fetch, and form submission converge on the
/// same network seam.
#[derive(Debug, Clone)]
pub struct TextRequest {
    pub url: Url,
    pub cross_site: bool,
    pub method: Method,
    pub redirect_mode: RedirectMode,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// Stable network lifecycle events emitted during a bounded text fetch and
/// retained by a completed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkEvent {
    RequestStart {
        url: String,
        method: Method,
    },
    Redirect {
        from: String,
        to: String,
        status: u16,
    },
    Response {
        url: String,
        status: u16,
    },
}

impl Method {
    /// RFC 7231 §4.2.1 safe methods.
    pub fn is_safe(self) -> bool {
        matches!(self, Method::Get | Method::Head | Method::Options)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Patch => "PATCH",
            Method::Options => "OPTIONS",
        }
    }
}

/// The raw result of a bounded fetch. Headers are lower-cased and multi-valued
/// headers collapsed into one entry per name.
#[derive(Debug, Clone)]
pub struct ByteResponse {
    /// Final response body. Bounded by the client's max body size.
    pub body: Vec<u8>,
    pub headers: BTreeMap<String, String>,
    pub status: u16,
    pub final_url: String,
    pub set_cookie: Vec<String>,
    pub redirects: u32,
    pub events: Vec<NetworkEvent>,
}

impl ByteResponse {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(|s| s.as_str())
    }

    /// Parsed `Content-Type` media type (before `;`).
    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
            .map(|v| v.split(';').next().unwrap_or("").trim())
    }
}

/// The decoded result of a `get_text` fetch (the navigation data-flow entry
/// in docs/ARCHITECTURE.md).
#[derive(Debug, Clone)]
pub struct TextResponse {
    /// Final response body (UTF-8 lossy). Bounded by the client's max body.
    pub body: String,
    /// Lower-cased header name → value(s). Multiple values joined by ", ".
    pub headers: BTreeMap<String, String>,
    /// Final HTTP status code after any redirects.
    pub status: u16,
    /// Final URL after redirects.
    pub final_url: String,
    /// `Set-Cookie` response headers, in receipt order. Fed to the cookie
    /// jar by the caller (`Network::get_text_with_cookies`).
    pub set_cookie: Vec<String>,
    /// Number of HTTP redirects followed to reach this response.
    pub redirects: u32,
    /// Stable network lifecycle events for automation/diagnostics.
    pub events: Vec<NetworkEvent>,
}

impl From<ByteResponse> for TextResponse {
    fn from(response: ByteResponse) -> Self {
        Self {
            body: String::from_utf8_lossy(&response.body).into_owned(),
            headers: response.headers,
            status: response.status,
            final_url: response.final_url,
            set_cookie: response.set_cookie,
            redirects: response.redirects,
            events: response.events,
        }
    }
}

impl TextResponse {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(|s| s.as_str())
    }

    /// Convenience: parsed `Content-Type` media type (before `;`).
    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
            .map(|v| v.split(';').next().unwrap_or("").trim())
    }
}
