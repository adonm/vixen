//! vixen-net — networking + security policy (docs/PLAN.md Phase 1 "crown
//! jewels").
//!
//! Pure-Rust, fail-closed subsystems. The public entry points are the
//! boundaries from docs/ARCHITECTURE.md "Trust boundaries":
//!
//! - [`url_policy::validate_http_url`] — SSRF / private-IP / reserved-TLD
//!   block at every fetch.
//! - [`cookie::CookieJar`] — RFC 6265 jar with Vixen defaults (Lax default
//!   SameSite, 512-entry FIFO cap, HttpOnly document-side rejection).
//! - [`csp::ContentSecurityPolicy`] — parser + enforcer for script-exec /
//!   fetch / plugin boundaries.
//! - [`network::Network`] — reqwest + rustls client, manual redirect
//!   following so URL policy + cookies re-apply at every hop.
//! - [`permissions::PermissionStore`] — per-origin permissions (Prompt ⇒
//!   denied, fail closed).
//! - [`origin::Origin`] — `(scheme, host, port)` partitioning key.

#![forbid(unsafe_code)]

pub mod cookie;
pub mod csp;
pub mod fetch_types;
pub mod http_helpers;
pub mod network;
pub mod origin;
pub mod permissions;
pub mod url_policy;

pub use cookie::{Cookie, CookieError, CookieJar, MAX_COOKIES, SameSite};
pub use csp::{ContentSecurityPolicy, CspPolicy, HashAlg, HostSource, Source};
pub use fetch_types::{Method, TextResponse};
pub use network::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_REDIRECTS, Network, NetworkConfig, NetworkError,
};
pub use origin::Origin;
pub use permissions::{PermissionKind, PermissionState, PermissionStore};
pub use url_policy::{UrlPolicyError, is_private_host, validate_http_url};
