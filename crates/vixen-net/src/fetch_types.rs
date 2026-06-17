//! Request/response types shared by the networking layer
//! (docs/ARCHITECTURE.md "Data flow per navigation"). Kept free of
//! `reqwest` types so the seam stays implementation-agnostic.

use std::collections::BTreeMap;

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

/// The decoded result of a `get_text` fetch (the navigation data-flow entry
/// in docs/ARCHITECTURE.md). Headers are lower-cased and multi-valued
/// headers collapsed into one entry per name.
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
