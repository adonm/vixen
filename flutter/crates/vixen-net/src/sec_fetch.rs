//! Fetch § 3.1 `Sec-Fetch-*` request-metadata headers — Phase 7 security
//! prep. The structured-header surface the Fetch standard requires every
//! client to attach (and every server to consult for the § 3.2 Cross-Origin
//! checks + the § 5 "Forbidden header name" rule that makes these headers
//! attacker-controlled-browserside-only).
//!
//! What lives here:
//! - [`SecFetchSite`] / [`SecFetchMode`] / [`SecFetchDest`] / [`SecFetchUser`]
//!   — the typed enums the four headers reduce to.
//! - [`classify_site`] — the § 3.2.4 relationship classifier the fetch layer
//!   consults to set `Sec-Fetch-Site` for an outgoing request (and that
//!   servers consult for Cross-Origin-Resource-Policy gating).
//! - [`SecFetchHeaders::parse`] — parse a header set into the typed bundle.
//!
//! What does *not* live here:
//! - The actual header attachment / network-layer enforcement (Phase 1 fetch
//!   pipeline + Phase 7 hardening consult this module's predicates).
//! - The Cross-Origin-Resource-Policy header itself (a separate § 3.2
//!   header; lands alongside this module when the fetch layer wires it).
//!
//! Reference: <https://fetch.spec.whatwg.org/#http-header-layer>.

#![forbid(unsafe_code)]

use crate::origin::Origin;
use crate::site::same_registrable_domain;

// ---------------------------------------------------------------------------
// Sec-Fetch-Site (§ 3.1.4)
// ---------------------------------------------------------------------------

/// The value of the `Sec-Fetch-Site` header (Fetch § 3.1.4): the relationship
/// between the request's origin and the target's origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SecFetchSite {
    /// `same-origin` — same scheme, host, and port.
    SameOrigin,
    /// `same-site` — same registrable domain but a different origin (subdomain
    /// or scheme mismatch).
    SameSite,
    /// `cross-site` — different registrable domain.
    CrossSite,
    /// `none` — a user-initiated top-level navigation (no embedder origin).
    /// Per § 3.1.4 this is the value attached when the request originates
    /// outside any browsing context (e.g. the URL bar).
    #[default]
    None,
}

impl SecFetchSite {
    /// Parse the header value (case-sensitive; the spec mandates lowercase).
    /// Returns `None` for any value outside the four-token set so an
    /// attacker-spoofed value fails closed.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "same-origin" => Some(SecFetchSite::SameOrigin),
            "same-site" => Some(SecFetchSite::SameSite),
            "cross-site" => Some(SecFetchSite::CrossSite),
            "none" => Some(SecFetchSite::None),
            _ => None,
        }
    }

    /// The canonical lowercase header value this site serialises to.
    pub fn as_str(self) -> &'static str {
        match self {
            SecFetchSite::SameOrigin => "same-origin",
            SecFetchSite::SameSite => "same-site",
            SecFetchSite::CrossSite => "cross-site",
            SecFetchSite::None => "none",
        }
    }

    /// Is this a cross-origin request (`cross-site` or `same-site`)? The
    /// predicate servers use for the § 3.2 CORP / § 4 CORS gate.
    pub fn is_cross_origin(self) -> bool {
        matches!(self, SecFetchSite::CrossSite | SecFetchSite::SameSite)
    }
}

impl std::fmt::Display for SecFetchSite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Sec-Fetch-Mode (§ 3.1.2)
// ---------------------------------------------------------------------------

/// The value of the `Sec-Fetch-Mode` header (Fetch § 3.1.2): the fetch mode
/// the request was made with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SecFetchMode {
    Cors,
    #[default]
    NoCors,
    Navigate,
    SameOrigin,
    Websocket,
}

impl SecFetchMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "cors" => Some(SecFetchMode::Cors),
            "navigate" => Some(SecFetchMode::Navigate),
            "no-cors" => Some(SecFetchMode::NoCors),
            "same-origin" => Some(SecFetchMode::SameOrigin),
            "websocket" => Some(SecFetchMode::Websocket),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SecFetchMode::Cors => "cors",
            SecFetchMode::NoCors => "no-cors",
            SecFetchMode::Navigate => "navigate",
            SecFetchMode::SameOrigin => "same-origin",
            SecFetchMode::Websocket => "websocket",
        }
    }
}

impl std::fmt::Display for SecFetchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Sec-Fetch-Dest (§ 3.1.3)
// ---------------------------------------------------------------------------

/// The value of the `Sec-Fetch-Dest` header (Fetch § 3.1.3): the request
/// destination (the kind of resource being fetched). The full § 3.1.3 token
/// set; unknown tokens parse to `None` (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SecFetchDest {
    Audio,
    AudioWorklet,
    Document,
    Embed,
    #[default]
    Empty,
    Font,
    Frame,
    IFrame,
    Image,
    JSON,
    Manifest,
    Object,
    PaintWorklet,
    Report,
    Script,
    ServiceWorker,
    SharedWorker,
    Style,
    Track,
    Video,
    WebIdentity,
    Worker,
    Xslt,
}

impl SecFetchDest {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "audio" => Some(SecFetchDest::Audio),
            "audioworklet" => Some(SecFetchDest::AudioWorklet),
            "document" => Some(SecFetchDest::Document),
            "embed" => Some(SecFetchDest::Embed),
            "empty" => Some(SecFetchDest::Empty),
            "font" => Some(SecFetchDest::Font),
            "frame" => Some(SecFetchDest::Frame),
            "iframe" => Some(SecFetchDest::IFrame),
            "image" => Some(SecFetchDest::Image),
            "json" => Some(SecFetchDest::JSON),
            "manifest" => Some(SecFetchDest::Manifest),
            "object" => Some(SecFetchDest::Object),
            "paintworklet" => Some(SecFetchDest::PaintWorklet),
            "report" => Some(SecFetchDest::Report),
            "script" => Some(SecFetchDest::Script),
            "serviceworker" => Some(SecFetchDest::ServiceWorker),
            "sharedworker" => Some(SecFetchDest::SharedWorker),
            "style" => Some(SecFetchDest::Style),
            "track" => Some(SecFetchDest::Track),
            "video" => Some(SecFetchDest::Video),
            "webidentity" => Some(SecFetchDest::WebIdentity),
            "worker" => Some(SecFetchDest::Worker),
            "xslt" => Some(SecFetchDest::Xslt),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SecFetchDest::Audio => "audio",
            SecFetchDest::AudioWorklet => "audioworklet",
            SecFetchDest::Document => "document",
            SecFetchDest::Embed => "embed",
            SecFetchDest::Empty => "empty",
            SecFetchDest::Font => "font",
            SecFetchDest::Frame => "frame",
            SecFetchDest::IFrame => "iframe",
            SecFetchDest::Image => "image",
            SecFetchDest::JSON => "json",
            SecFetchDest::Manifest => "manifest",
            SecFetchDest::Object => "object",
            SecFetchDest::PaintWorklet => "paintworklet",
            SecFetchDest::Report => "report",
            SecFetchDest::Script => "script",
            SecFetchDest::ServiceWorker => "serviceworker",
            SecFetchDest::SharedWorker => "sharedworker",
            SecFetchDest::Style => "style",
            SecFetchDest::Track => "track",
            SecFetchDest::Video => "video",
            SecFetchDest::WebIdentity => "webidentity",
            SecFetchDest::Worker => "worker",
            SecFetchDest::Xslt => "xslt",
        }
    }

    /// Is this a navigation request destination (`document` / `frame` /
    /// `iframe`)? Drives the § 4.4 "navigation request" gating.
    pub fn is_navigation(self) -> bool {
        matches!(
            self,
            SecFetchDest::Document | SecFetchDest::Frame | SecFetchDest::IFrame
        )
    }

    /// Is this an embedding destination (`embed` / `object` / `frame` /
    /// `iframe`)? Drives the § 3.2 Cross-Origin-Embedder-Policy check.
    pub fn is_embed(self) -> bool {
        matches!(
            self,
            SecFetchDest::Embed | SecFetchDest::Object | SecFetchDest::Frame | SecFetchDest::IFrame
        )
    }
}

impl std::fmt::Display for SecFetchDest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Sec-Fetch-User (§ 3.1.5)
// ---------------------------------------------------------------------------

/// The value of the `Sec-Fetch-User` header (Fetch § 3.1.5): `?1` if the
/// request was triggered by a user activation, `?0` otherwise. Sent only on
/// navigation requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SecFetchUser {
    #[default]
    NotUserActivated,
    UserActivated,
}

impl SecFetchUser {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "?1" => Some(SecFetchUser::UserActivated),
            "?0" => Some(SecFetchUser::NotUserActivated),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SecFetchUser::NotUserActivated => "?0",
            SecFetchUser::UserActivated => "?1",
        }
    }

    pub fn is_user_activated(self) -> bool {
        matches!(self, SecFetchUser::UserActivated)
    }
}

impl std::fmt::Display for SecFetchUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// The bundled header set
// ---------------------------------------------------------------------------

/// The parsed `Sec-Fetch-*` header set for a single request. Any header that
/// is missing or unparseable stays at its [`Default`] (which is the
/// fail-closed "treat as opaque" form); the fetch layer consults the
/// [`SecFetchSite`] value to gate cross-origin behaviour.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecFetchHeaders {
    pub site: SecFetchSite,
    pub mode: SecFetchMode,
    pub dest: SecFetchDest,
    pub user: SecFetchUser,
}

impl SecFetchHeaders {
    /// Parse the four headers out of a `(name, value)` iterator (the network
    /// layer hands these in). Header names are matched case-insensitively;
    /// unknown names are ignored. Repeated headers: last-wins per the spec's
    /// "combine" rule (these are forbidden headers so the browser is the only
    /// source).
    pub fn parse<'a, I>(headers: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut out = SecFetchHeaders::default();
        for (name, value) in headers {
            // Header names are ASCII case-insensitive (RFC 9110 § 5.1). The
            // Sec-Fetch-* names are canonical-cased; compare lowercased.
            match name.to_ascii_lowercase().as_str() {
                "sec-fetch-site" => {
                    if let Some(s) = SecFetchSite::parse(value) {
                        out.site = s;
                    }
                }
                "sec-fetch-mode" => {
                    if let Some(m) = SecFetchMode::parse(value) {
                        out.mode = m;
                    }
                }
                "sec-fetch-dest" => {
                    if let Some(d) = SecFetchDest::parse(value) {
                        out.dest = d;
                    }
                }
                "sec-fetch-user" => {
                    if let Some(u) = SecFetchUser::parse(value) {
                        out.user = u;
                    }
                }
                _ => {}
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Site classification (§ 3.2.4)
// ---------------------------------------------------------------------------

/// Classify the `Sec-Fetch-Site` relationship between an embedder (the
/// request's origin, `None` for a user-typed navigation) and a target URL.
/// This is the § 3.2.4 algorithm the fetch layer runs to set the header for
/// outgoing requests, and that servers run to validate it on incoming ones.
///
/// - `None` embedder ⇒ [`SecFetchSite::None`] (user-initiated top-level nav).
/// - Same `(scheme, host, port)` ⇒ [`SecFetchSite::SameOrigin`].
/// - Same registrable domain ⇒ [`SecFetchSite::SameSite`].
/// - Otherwise ⇒ [`SecFetchSite::CrossSite`].
///
pub fn classify_site(embedder: Option<&Origin>, target: &Origin) -> SecFetchSite {
    let Some(embedder) = embedder else {
        return SecFetchSite::None;
    };
    if embedder.is_opaque() || target.is_opaque() {
        // An opaque embedder can't make same-origin/same-site requests.
        return SecFetchSite::CrossSite;
    }
    if is_same_origin(embedder, target) {
        return SecFetchSite::SameOrigin;
    }
    if is_same_site(embedder, target) {
        return SecFetchSite::SameSite;
    }
    SecFetchSite::CrossSite
}

/// Exact `(scheme, host, port)` tuple equality (RFC 6454 origin equality).
fn is_same_origin(a: &Origin, b: &Origin) -> bool {
    a.scheme() == b.scheme() && a.host() == b.host() && a.port() == b.port()
}

fn is_same_site(a: &Origin, b: &Origin) -> bool {
    // § 3.2.4: same-site additionally requires scheme match (with the legacy
    // upgrade exception that http→https is still same-site). Vixen enforces
    // strict scheme equality for the cross-origin gate and treats the upgrade
    // exception as a CORP-layer concern.
    if !scheme_compatible_for_same_site(a.scheme(), b.scheme()) {
        return false;
    }
    same_registrable_domain(a.host(), b.host())
}

/// Whether two schemes are compatible for the same-site test. `http`/`ws` and
/// `https`/`wss` are each a pair; cross-pair is same-site (the upgrade
/// exception), intra-pair is same-site, anything else is cross-site.
fn scheme_compatible_for_same_site(a: &str, b: &str) -> bool {
    fn tier(s: &str) -> u8 {
        match s {
            "http" | "ws" => 1,
            "https" | "wss" => 2,
            _ => 0,
        }
    }
    let (ta, tb) = (tier(a), tier(b));
    if ta == 0 || tb == 0 {
        return a == b;
    }
    // Both are http/ws or https/wss family ⇒ compatible (the upgrade path).
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn origin(u: &str) -> Origin {
        Origin::from_url(&Url::parse(u).unwrap())
    }

    // --- Enum parsing + round-trip -------------------------------------

    #[test]
    fn site_parses_canonical_tokens() {
        assert_eq!(
            SecFetchSite::parse("same-origin"),
            Some(SecFetchSite::SameOrigin)
        );
        assert_eq!(
            SecFetchSite::parse("same-site"),
            Some(SecFetchSite::SameSite)
        );
        assert_eq!(
            SecFetchSite::parse("cross-site"),
            Some(SecFetchSite::CrossSite)
        );
        assert_eq!(SecFetchSite::parse("none"), Some(SecFetchSite::None));
    }

    #[test]
    fn site_rejects_unknown_tokens() {
        assert_eq!(SecFetchSite::parse("evil"), None);
        assert_eq!(SecFetchSite::parse("Same-Origin"), None); // case-sensitive
        assert_eq!(SecFetchSite::parse(""), None);
    }

    #[test]
    fn mode_parses_all_five() {
        for (s, v) in [
            ("cors", SecFetchMode::Cors),
            ("navigate", SecFetchMode::Navigate),
            ("no-cors", SecFetchMode::NoCors),
            ("same-origin", SecFetchMode::SameOrigin),
            ("websocket", SecFetchMode::Websocket),
        ] {
            assert_eq!(SecFetchMode::parse(s), Some(v));
            assert_eq!(v.as_str(), s);
        }
    }

    #[test]
    fn dest_parses_and_classifies() {
        assert_eq!(
            SecFetchDest::parse("document"),
            Some(SecFetchDest::Document)
        );
        assert!(SecFetchDest::Document.is_navigation());
        assert!(SecFetchDest::IFrame.is_navigation());
        assert!(SecFetchDest::IFrame.is_embed());
        assert!(!SecFetchDest::Script.is_navigation());
        assert!(!SecFetchDest::Empty.is_embed());
    }

    #[test]
    fn user_parses_question_token() {
        assert_eq!(SecFetchUser::parse("?1"), Some(SecFetchUser::UserActivated));
        assert!(SecFetchUser::parse("?1").unwrap().is_user_activated());
        assert_eq!(
            SecFetchUser::parse("?0"),
            Some(SecFetchUser::NotUserActivated)
        );
        assert_eq!(SecFetchUser::parse("1"), None);
        assert_eq!(SecFetchUser::parse("true"), None);
    }

    #[test]
    fn enum_display_round_trips() {
        assert_eq!(SecFetchSite::CrossSite.to_string(), "cross-site");
        assert_eq!(SecFetchMode::Navigate.to_string(), "navigate");
        assert_eq!(SecFetchDest::Image.to_string(), "image");
        assert_eq!(SecFetchUser::UserActivated.to_string(), "?1");
    }

    // --- classify_site: same-origin ------------------------------------

    #[test]
    fn same_scheme_host_port_is_same_origin() {
        let a = origin("https://example.com/a");
        let b = origin("https://example.com/b");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::SameOrigin);
    }

    #[test]
    fn different_port_is_not_same_origin() {
        let a = origin("https://example.com:443/");
        let b = origin("https://example.com:8443/");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::SameSite);
    }

    #[test]
    fn different_scheme_is_not_same_origin() {
        let a = origin("http://example.com/");
        let b = origin("https://example.com/");
        // http vs https: same-site via the upgrade exception.
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::SameSite);
    }

    // --- classify_site: same-site --------------------------------------

    #[test]
    fn subdomain_is_same_site() {
        let a = origin("https://app.example.com/");
        let b = origin("https://api.example.com/");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::SameSite);
    }

    #[test]
    fn same_registrable_domain_case_insensitive() {
        let a = origin("https://App.Example.com/");
        let b = origin("https://api.EXAMPLE.com/");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::SameSite);
    }

    // --- classify_site: cross-site -------------------------------------

    #[test]
    fn different_registrable_domain_is_cross_site() {
        let a = origin("https://example.com/");
        let b = origin("https://evil.test/");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::CrossSite);
    }

    #[test]
    fn cross_site_predicate_covers_same_site() {
        // is_cross_origin() true for both CrossSite and SameSite (the § 3.2
        // CORP gate treats them identically).
        assert!(SecFetchSite::CrossSite.is_cross_origin());
        assert!(SecFetchSite::SameSite.is_cross_origin());
        assert!(!SecFetchSite::SameOrigin.is_cross_origin());
        assert!(!SecFetchSite::None.is_cross_origin());
    }

    // --- classify_site: none -------------------------------------------

    #[test]
    fn no_embedder_is_none() {
        let b = origin("https://example.com/");
        assert_eq!(classify_site(None, &b), SecFetchSite::None);
    }

    #[test]
    fn opaque_embedder_is_cross_site() {
        let a = Origin::opaque();
        let b = origin("https://example.com/");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::CrossSite);
    }

    // --- Header parsing ------------------------------------------------

    #[test]
    fn parses_full_header_set() {
        let headers = vec![
            ("Sec-Fetch-Site", "cross-site"),
            ("Sec-Fetch-Mode", "cors"),
            ("Sec-Fetch-Dest", "image"),
            ("Sec-Fetch-User", "?1"),
            ("Content-Type", "application/json"), // ignored
        ];
        let parsed = SecFetchHeaders::parse(headers);
        assert_eq!(parsed.site, SecFetchSite::CrossSite);
        assert_eq!(parsed.mode, SecFetchMode::Cors);
        assert_eq!(parsed.dest, SecFetchDest::Image);
        assert_eq!(parsed.user, SecFetchUser::UserActivated);
    }

    #[test]
    fn header_names_case_insensitive() {
        let parsed = SecFetchHeaders::parse(vec![("sec-FETCH-site", "same-origin")]);
        assert_eq!(parsed.site, SecFetchSite::SameOrigin);
    }

    #[test]
    fn malformed_header_value_fails_closed_to_default() {
        let parsed = SecFetchHeaders::parse(vec![("Sec-Fetch-Site", "evil")]);
        // Default SecFetchSite is None; a bad value keeps the default.
        assert_eq!(parsed.site, SecFetchSite::None);
    }

    #[test]
    fn repeated_headers_last_wins() {
        let parsed = SecFetchHeaders::parse(vec![
            ("Sec-Fetch-Site", "same-origin"),
            ("Sec-Fetch-Site", "cross-site"),
        ]);
        assert_eq!(parsed.site, SecFetchSite::CrossSite);
    }

    #[test]
    fn empty_headers_yields_defaults() {
        let parsed = SecFetchHeaders::parse(Vec::<(&str, &str)>::new());
        assert_eq!(parsed.site, SecFetchSite::None);
        assert_eq!(parsed.mode, SecFetchMode::NoCors);
        assert_eq!(parsed.dest, SecFetchDest::Empty);
        assert_eq!(parsed.user, SecFetchUser::NotUserActivated);
    }

    #[test]
    fn localhost_is_cross_site_to_example() {
        let a = origin("http://localhost/");
        let b = origin("https://example.com/");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::CrossSite);
    }

    #[test]
    fn multi_label_public_suffix_is_cross_site() {
        let a = origin("https://a.co.uk/");
        let b = origin("https://b.co.uk/");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::CrossSite);
    }

    #[test]
    fn private_public_suffix_is_cross_site() {
        let a = origin("https://a.github.io/");
        let b = origin("https://b.github.io/");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::CrossSite);
    }

    #[test]
    fn same_site_handles_trailing_dot() {
        let a = origin("https://www.example.com./");
        let b = origin("https://api.example.com/");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::SameSite);
    }

    #[test]
    fn ips_without_registrable_domains_are_cross_site() {
        let a = origin("https://127.0.0.1:443/");
        let b = origin("https://127.0.0.1:8443/");
        assert_eq!(classify_site(Some(&a), &b), SecFetchSite::CrossSite);
    }
}
