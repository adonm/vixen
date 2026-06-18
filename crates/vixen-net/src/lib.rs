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
//! - [`referrer_policy::resolve_referrer`] — Fetch § 4.3.7 `Referer`
//!   resolution (Phase 7 prep).
//! - [`strict_transport_security::parse_strict_transport_security`] — RFC
//!   6795 HSTS parsing (Phase 7 prep).
//! - [`mixed_content::classify`] — W3C Mixed Content L1 § 3 verdict the
//!   fetch layer consults for every subresource out of an HTTPS context
//!   (Phase 7 prep).
//! - [`sandboxing::parse_sandbox`] — WHATWG HTML § 4.8.5 `<iframe sandbox>`
//!   flag parser the script/navigation/storage layers consult when loading
//!   framed content (Phase 7 prep).

#![forbid(unsafe_code)]

pub mod cookie;
pub mod cors;
pub mod csp;
pub mod fetch_types;
pub mod http_helpers;
pub mod mixed_content;
pub mod network;
pub mod origin;
pub mod permissions;
pub mod referrer_policy;
pub mod sandboxing;
pub mod strict_transport_security;
pub mod url_policy;

pub use cookie::{Cookie, CookieError, CookieJar, MAX_COOKIES, SameSite};
pub use cors::{
    CORS_FORBIDDEN_RESPONSE_HEADERS, CORS_SAFELISTED_RESPONSE_HEADERS, CorsCheckOutcome,
    CorsCredentialsMode, CorsError, CorsResponseHeaders, cors_check, cors_filtered_headers,
};
pub use csp::{ContentSecurityPolicy, CspPolicy, HashAlg, HostSource, Source};
pub use fetch_types::{Method, TextResponse};
pub use mixed_content::{MixedContentVerdict, ResourceType, classify as classify_mixed_content};
pub use network::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_REDIRECTS, Network, NetworkConfig, NetworkError,
};
pub use origin::Origin;
pub use permissions::{PermissionKind, PermissionState, PermissionStore};
pub use referrer_policy::{ReferrerPolicy, ReferrerValue, parse_referrer_policy, resolve_referrer};
pub use sandboxing::{SandboxFlags, parse_sandbox};
pub use strict_transport_security::{HstsDirective, HstsEntry, parse_strict_transport_security};
pub use url_policy::{UrlPolicyError, is_private_host, validate_http_url};
