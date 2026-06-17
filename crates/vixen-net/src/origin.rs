//! Web origin (scheme/host/port tuple) — the partitioning key for storage
//! and cookies (docs/ARCHITECTURE.md "App ID and profile paths",
//! docs/SPEC.md "Cookie contract").
//!
//! An origin is the tuple `(scheme, host, port)` per RFC 6454. Opaque /
//! special origins (e.g. `null` on `data:` URLs) are represented by
//! [`Origin::opaque`], which never partitions with anything.

use std::fmt;
use url::Url;

/// A web origin. Cheap to clone; used as a storage partition key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
    opaque: bool,
}

impl Origin {
    /// Derive the origin of `url` per RFC 6454. Returns an opaque origin for
    /// schemes that are not tuple origins (e.g. `data:`, `file:`).
    pub fn from_url(url: &Url) -> Self {
        let scheme = url.scheme();
        match (url.host_str(), url.port_or_known_default()) {
            (Some(host), port)
                if scheme == "http" || scheme == "https" || scheme == "ws" || scheme == "wss" =>
            {
                Self {
                    scheme: scheme.to_owned(),
                    host: host.to_owned(),
                    port,
                    opaque: false,
                }
            }
            _ => Self::opaque(),
        }
    }

    /// An opaque ("null") origin that never matches any other.
    pub fn opaque() -> Self {
        Self {
            scheme: String::new(),
            host: String::new(),
            port: None,
            opaque: true,
        }
    }

    pub fn is_opaque(&self) -> bool {
        self.opaque
    }

    /// Stable string key for per-origin partitioning. Opaque origins share
    /// the sentinel `"opaque"` so they are isolated from tuple origins.
    pub fn partition_key(&self) -> String {
        if self.opaque {
            return "opaque".to_owned();
        }
        match self.port {
            Some(p) => format!("{}://{}:{}", self.scheme, self.host, p),
            None => format!("{}://{}", self.scheme, self.host),
        }
    }

    /// `https`/`wss` origins are "secure"; everything else is not. Drives
    /// the `Secure`-cookie gate and mixed-content reasoning.
    pub fn is_secure(&self) -> bool {
        matches!(self.scheme.as_str(), "https" | "wss")
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> Option<u16> {
        self.port
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.partition_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuple_origin_from_https_url() {
        let o = Origin::from_url(&Url::parse("https://example.com:8443/a/b").unwrap());
        assert_eq!(o.partition_key(), "https://example.com:8443");
        assert!(o.is_secure());
        assert!(!o.is_opaque());
    }

    #[test]
    fn default_port_is_resolved() {
        let o = Origin::from_url(&Url::parse("https://example.com/").unwrap());
        assert_eq!(o.port(), Some(443));
        let o = Origin::from_url(&Url::parse("http://example.com/").unwrap());
        assert_eq!(o.port(), Some(80));
    }

    #[test]
    fn opaque_for_data_and_file() {
        assert!(Origin::from_url(&Url::parse("data:text/plain,hi").unwrap()).is_opaque());
        assert!(Origin::from_url(&Url::parse("file:///etc/passwd").unwrap()).is_opaque());
    }

    #[test]
    fn opaque_never_matches_tuple() {
        let a = Origin::opaque();
        let b = Origin::from_url(&Url::parse("https://a.test/").unwrap());
        assert_ne!(a, b);
        assert_eq!(a.partition_key(), "opaque");
    }

    #[test]
    fn partitioning_is_origin_scoped() {
        // Same host, different scheme → different partition (origin = tuple).
        let a = Origin::from_url(&Url::parse("http://a.test/").unwrap());
        let b = Origin::from_url(&Url::parse("https://a.test/").unwrap());
        assert_ne!(a.partition_key(), b.partition_key());
    }
}
