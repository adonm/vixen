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
//! - [`sec_fetch::classify_site`] + [`sec_fetch::SecFetchHeaders`] — Fetch
//!   § 3.1 `Sec-Fetch-*` request-metadata parsing + the § 3.2.4 site
//!   relationship classifier the fetch layer consults for the Cross-Origin
//!   gates (Phase 7 prep).
//! - [`permissions_policy::parse_permissions_policy`] — Permissions Policy 1
//!   § 3.3 `Permissions-Policy` header + `<iframe allow>` parser the host
//!   hooks consult before exposing `navigator.geolocation` &c. (Phase 7 prep).
//! - [`coop::parse_coop`] — HTML § 7.8 `Cross-Origin-Opener-Policy` header
//!   parser; together with [`coep::parse_coep`] gates the cross-origin
//!   isolation the high-resolution timers require (Phase 7 prep).
//! - [`coep::parse_coep`] — Fetch § 3.2 `Cross-Origin-Embedder-Policy`
//!   header parser + the [`coep::is_cross_origin_isolated`] gate
//!   `performance.now()` coarsening and `SharedArrayBuffer` exposure consult
//!   (Phase 7 prep).
//! - [`integrity::parse_integrity`] + [`integrity::verify`] — W3C SRI
//!   `<script integrity>` / `<link integrity>` metadata parse + the
//!   constant-time hash verify the fetch layer consults before executing a
//!   subresource (Phase 7 prep).
//! - [`nosniff::enforce`] — Fetch § 2 `X-Content-Type-Options: nosniff`
//!   enforcement (the script / style MIME block) the fetch layer consults
//!   before executing a script or applying a stylesheet (Phase 7 prep).
//! - [`corp::parse_corp`] + [`corp::coep_corp_gate`] — Fetch § 4.5.3
//!   `Cross-Origin-Resource-Policy` parse + the combined COEP + CORP gate
//!   the fetch layer consults before applying a no-cors subresource
//!   response into a COEP-hardened document (Phase 7 prep).
//! - [`trusted_types::parse_trusted_types`] +
//!   [`trusted_types::parse_require_trusted_types_for`] +
//!   [`trusted_types::evaluate_sink`] — W3C Trusted Types `trusted-types` +
//!   `require-trusted-types-for` CSP directive parse + the injection-sink
//!   decision the DOM sink host hooks (`.innerHTML`, `eval`, &c.) consult
//!   before accepting a string (Phase 7 prep).

#![forbid(unsafe_code)]

pub mod coep;
pub mod cookie;
pub mod coop;
pub mod corp;
pub mod cors;
pub mod csp;
pub mod fetch_types;
pub mod http_helpers;
pub mod integrity;
pub mod mixed_content;
pub mod network;
pub mod nosniff;
pub mod origin;
pub mod permissions;
pub mod permissions_policy;
pub mod referrer_policy;
pub mod sandboxing;
pub mod sec_fetch;
pub mod strict_transport_security;
pub mod trusted_types;
pub mod url_policy;
pub mod websocket;

pub use coep::{Coep, is_cross_origin_isolated, parse_coep};
pub use cookie::{Cookie, CookieError, CookieJar, CookieSnapshot, MAX_COOKIES, SameSite};
pub use coop::{Coop, parse_coop};
pub use corp::{
    CoepCorpOutcome, Corp, CorpOutcome, check_corp, coep_corp_gate, is_same_site, parse_corp,
};
pub use cors::{
    CORS_FORBIDDEN_RESPONSE_HEADERS, CORS_SAFELISTED_RESPONSE_HEADERS, CorsCheckOutcome,
    CorsCredentialsMode, CorsError, CorsResponseHeaders, cors_check, cors_filtered_headers,
};
pub use csp::{ContentSecurityPolicy, CspPolicy, HashAlg, HostSource, Source};
pub use fetch_types::{Method, NetworkEvent, RedirectMode, TextResponse};
pub use integrity::{
    HashAlgorithm, IntegrityOutcome, IntensityItem, parse_integrity, verify as verify_integrity,
};
pub use mixed_content::{MixedContentVerdict, ResourceType, classify as classify_mixed_content};
pub use network::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_REDIRECTS, Network, NetworkConfig, NetworkError,
};
pub use nosniff::{Destination, NosniffOutcome, enforce as enforce_nosniff, is_nosniff};
pub use origin::Origin;
pub use permissions::{PermissionKind, PermissionState, PermissionStore};
pub use permissions_policy::{
    Allowlist, PermissionsPolicy, parse_allow_attribute, parse_permissions_policy,
};
pub use referrer_policy::{ReferrerPolicy, ReferrerValue, parse_referrer_policy, resolve_referrer};
pub use sandboxing::{SandboxFlags, parse_sandbox};
pub use sec_fetch::{
    SecFetchDest, SecFetchHeaders, SecFetchMode, SecFetchSite, SecFetchUser, classify_site,
};
pub use strict_transport_security::{HstsDirective, HstsEntry, parse_strict_transport_security};
pub use trusted_types::{
    AllowedNames, RequireFor, TrustedTypeKind, TrustedTypesOutcome, TrustedTypesPolicyNames,
    evaluate_sink, parse_require_trusted_types_for, parse_trusted_types, policy_creation_allowed,
};
pub use url_policy::{UrlPolicyError, is_private_host, validate_http_url};
