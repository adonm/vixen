//! Mixed content classification — Phase 7 security prep (docs/PLAN.md
//! Phase 7 + docs/SPEC.md "Security"). Implements the W3C Mixed Content
//! Level 1 § 3 "should fetching the request be blocked as mixed content?"
//! decision the network layer consults at every subresource fetch out of an
//! HTTPS page context.
//!
//! What lives here:
//! - [`ResourceType`] — the fetch-destination categories that map onto the
//!   W3C MC § 3.2 modal grouping (active = blockable, passive = auto-upgradable,
//!   navigation = allowed).
//! - [`MixedContentVerdict`] — the network-layer outcome: [`NotMixed`]
//!   (rules don't apply), [`Block`] (active mixed content), [`Upgrade`]
//!   (passive mixed content → rewrite to https).
//! - [`classify`] — the § 3 algorithm. Combines the context's trustworthiness,
//!   the request URL's trustworthiness (via [`referrer_policy::is_potentially_trustworthy`]),
//!   the resource type, and the CSP `block-all-mixed-content` directive.
//!
//! What does *not* live here:
//! - The actual URL rewrite for [`MixedContentVerdict::Upgrade`]. The fetch
//!   state machine does that (and reports a console warning); this module
//!   only names the verdict.
//! - CSP parsing. The `block_all_mixed_content` flag arrives already parsed
//!   from [`csp::ContentSecurityPolicy`]; this module treats it as an input.
//! - Origin/host-level checks beyond the scheme. Mixed content is purely a
//!   transport-security boundary: a `wss://` subresource on an `https://`
//!   page is fine regardless of host, a `ws://` one is mixed regardless.
//!
//! ## Trust boundary
//!
//! Mixed content is a passive-leak boundary: a single `http://` stylesheet
//! on an otherwise-`https://` page lets a network attacker read the page
//! contents (active content can fully compromise the page). The classifier
//! is consulted after [`url_policy`] + before the request leaves the process,
//! and fails closed ([`Block`]) on every active mixed fetch per W3C MC § 3.2
//! (modern browsers block rather than warn, since the warning UX was
//! ineffective).
//!
//! References:
//! - W3C Mixed Content L1 § 3 "Should fetching the request be blocked as
//!   mixed content?" (<https://www.w3.org/TR/mixed-content/#algorithms>).
//! - Fetch § 4.5 "potentially trustworthy origin".
//! - CSP Level 3 § `block-all-mixed-content` directive.

#![forbid(unsafe_code)]

use url::Url;

use crate::referrer_policy::is_potentially_trustworthy;

// ---------------------------------------------------------------------------
// Resource categorisation (W3C MC § 3.2)
// ---------------------------------------------------------------------------

/// The fetch-destination category of a subresource. Maps onto the W3C MC § 3.2
/// modal grouping that decides the verdict. Distinct from the fetch standard's
/// `RequestDestination` enum (which is finer-grained) — we collapse to the
/// three categories the mixed-content algorithm actually branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    // --- Active (blockable) ---------------------------------------------
    /// `<script src>`, `<link rel=modulepreload>`, `import()`, workers.
    Script,
    /// `<link rel=stylesheet>`, CSS `@import`.
    Stylesheet,
    /// `fetch()`, `XMLHttpRequest` — programmer-initiated subresource fetches.
    Fetch,
    /// `<iframe src>`, `<frame src>` — embedded browsing contexts.
    Iframe,
    /// `<object data>`, `<embed src>` — plugin content.
    Object,
    /// `<link rel=preload as=font>`, `@font-face src`.
    Font,
    /// `EventSource`, `WebSocket` — long-lived streaming connections.
    Stream,
    // --- Passive (auto-upgradable) --------------------------------------
    /// `<img src>`, `<picture><source srcset>`.
    Image,
    /// `<audio src>`, `<source>` inside `<audio>`/`<video>`.
    Audio,
    /// `<video src>`, `<video poster>`.
    Video,
    // --- Navigation -----------------------------------------------------
    /// Top-level document navigation (`<a href>`, `location.href = …`).
    /// Never blocked as mixed content (browsers may upgrade separately).
    Document,
}

impl ResourceType {
    /// Whether this category is "active" mixed content (W3C MC § 3.2: must
    /// be blocked when mixed). Active content can fully compromise the page
    /// — script execution, DOM access, network reads — so the spec mandates
    /// a hard block rather than an upgrade.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            ResourceType::Script
                | ResourceType::Stylesheet
                | ResourceType::Fetch
                | ResourceType::Iframe
                | ResourceType::Object
                | ResourceType::Font
                | ResourceType::Stream
        )
    }

    /// Whether this category is "passive" mixed content (W3C MC § 3.3:
    /// optionally blockable, auto-upgradable). Passive content can leak data
    /// (e.g. an `http://` image URL hints at what the user is viewing) but
    /// can't execute code in the page, so the modern browser response is to
    /// transparently upgrade `http://` → `https://`.
    pub fn is_passive(self) -> bool {
        matches!(
            self,
            ResourceType::Image | ResourceType::Audio | ResourceType::Video
        )
    }

    /// Whether this category is a top-level navigation (never blocked as
    /// mixed content per W3C MC § 3 — the user can always follow a link to
    /// an `http://` page; the browser may upgrade the top-level navigation
    /// separately via the UA's HTTPS-First policy).
    pub fn is_navigation(self) -> bool {
        matches!(self, ResourceType::Document)
    }
}

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

/// The network-layer outcome of [`classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixedContentVerdict {
    /// The mixed-content rules do not apply: the context is not a secure
    /// context (an `http://` page can load `http://` resources freely), or
    /// the request URL is itself transport-secure (`https`/`wss`/trustworthy).
    NotMixed,
    /// Block the fetch entirely. Active mixed content under an HTTPS context,
    /// or any mixed fetch when `block-all-mixed-content` is set.
    Block,
    /// Rewrite the URL scheme to `https` and re-fetch. Passive mixed content
    /// under an HTTPS context without the `block-all` directive.
    Upgrade,
}

// ---------------------------------------------------------------------------
// Classifier (W3C MC § 3)
// ---------------------------------------------------------------------------

/// Decide whether a subresource fetch out of a secure context is mixed
/// content, and what to do about it (W3C MC L1 § 3 "should fetching the
/// request be blocked as mixed content?").
///
/// - `context_trustworthy`: whether the page's own origin is a potentially
///   trustworthy (secure) context — typically `https`, `wss`, `file`, or
///   `localhost` per [`is_potentially_trustworthy`]. If `false`, the rules
///   don't apply and the verdict is [`NotMixed`].
/// - `request_url`: the subresource URL.
/// - `resource`: the subresource's category ([`ResourceType`]).
/// - `block_all_mixed_content`: whether the page's CSP carries the
///   `block-all-mixed-content` directive. When `true`, even passive mixed
///   content is blocked (not upgraded).
///
/// Decision tree (W3C MC § 3 + modern browser UX):
/// 1. Context not trustworthy → [`NotMixed`].
/// 2. Request URL already trustworthy → [`NotMixed`].
/// 3. `block-all-mixed-content` → [`Block`] (everything).
/// 4. Navigation → [`NotMixed`] (never blocked as mixed content).
/// 5. Passive resource → [`Upgrade`].
/// 6. Active resource → [`Block`].
pub fn classify(
    context_trustworthy: bool,
    request_url: &Url,
    resource: ResourceType,
    block_all_mixed_content: bool,
) -> MixedContentVerdict {
    // (1) The rules only apply when the context is secure.
    if !context_trustworthy {
        return MixedContentVerdict::NotMixed;
    }
    // (2) Already-secure subresources aren't mixed content.
    if is_potentially_trustworthy(request_url) {
        return MixedContentVerdict::NotMixed;
    }
    // (3) CSP `block-all-mixed-content` overrides everything (even upgrades).
    if block_all_mixed_content {
        return MixedContentVerdict::Block;
    }
    // (4) Top-level navigation is never blocked as mixed content.
    if resource.is_navigation() {
        return MixedContentVerdict::NotMixed;
    }
    // (5) Passive content auto-upgrades (modern browsers stopped allowing
    //     passive mixed content to load as-is).
    if resource.is_passive() {
        return MixedContentVerdict::Upgrade;
    }
    // (6) Active content is blocked.
    debug_assert!(
        resource.is_active(),
        "unhandled resource category fell through"
    );
    MixedContentVerdict::Block
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    // --- Categorisation -------------------------------------------------

    #[test]
    fn active_resource_set_matches_spec() {
        // Every active category is_active; none is_passive or is_navigation.
        for r in [
            ResourceType::Script,
            ResourceType::Stylesheet,
            ResourceType::Fetch,
            ResourceType::Iframe,
            ResourceType::Object,
            ResourceType::Font,
            ResourceType::Stream,
        ] {
            assert!(r.is_active(), "{r:?} should be active");
            assert!(!r.is_passive(), "{r:?} should not be passive");
            assert!(!r.is_navigation(), "{r:?} should not be navigation");
        }
    }

    #[test]
    fn passive_resource_set_matches_spec() {
        for r in [
            ResourceType::Image,
            ResourceType::Audio,
            ResourceType::Video,
        ] {
            assert!(r.is_passive(), "{r:?} should be passive");
            assert!(!r.is_active());
            assert!(!r.is_navigation());
        }
    }

    #[test]
    fn navigation_resource_is_document_only() {
        assert!(ResourceType::Document.is_navigation());
        assert!(!ResourceType::Document.is_active());
        assert!(!ResourceType::Document.is_passive());
    }

    #[test]
    fn every_variant_is_in_exactly_one_category() {
        // Defence against a future addition landing uncategorised.
        for r in [
            ResourceType::Script,
            ResourceType::Stylesheet,
            ResourceType::Fetch,
            ResourceType::Iframe,
            ResourceType::Object,
            ResourceType::Font,
            ResourceType::Stream,
            ResourceType::Image,
            ResourceType::Audio,
            ResourceType::Video,
            ResourceType::Document,
        ] {
            let cats = [r.is_active(), r.is_passive(), r.is_navigation()]
                .iter()
                .filter(|&&b| b)
                .count();
            assert_eq!(cats, 1, "{r:?} is in {cats} categories (expected 1)");
        }
    }

    // --- classify: context not secure -----------------------------------

    #[test]
    fn http_page_loading_http_is_not_mixed() {
        // The mixed-content rules only fire on a secure context. An http
        // page loading http resources is normal.
        let v = classify(
            false,
            &url("http://a.test/x.js"),
            ResourceType::Script,
            false,
        );
        assert_eq!(v, MixedContentVerdict::NotMixed);
    }

    #[test]
    fn http_page_loading_http_image_is_not_mixed() {
        let v = classify(
            false,
            &url("http://a.test/x.png"),
            ResourceType::Image,
            false,
        );
        assert_eq!(v, MixedContentVerdict::NotMixed);
    }

    // --- classify: request already secure -------------------------------

    #[test]
    fn https_page_loading_https_script_is_not_mixed() {
        let v = classify(
            true,
            &url("https://a.test/x.js"),
            ResourceType::Script,
            false,
        );
        assert_eq!(v, MixedContentVerdict::NotMixed);
    }

    #[test]
    fn https_page_loading_wss_websocket_is_not_mixed() {
        let v = classify(true, &url("wss://a.test/ws"), ResourceType::Stream, false);
        assert_eq!(v, MixedContentVerdict::NotMixed);
    }

    #[test]
    fn https_page_loading_localhost_http_is_not_mixed() {
        // localhost is treated as trustworthy (WPSA); not mixed.
        let v = classify(
            true,
            &url("http://localhost:8080/dev.js"),
            ResourceType::Script,
            false,
        );
        assert_eq!(v, MixedContentVerdict::NotMixed);
    }

    // --- classify: active mixed content → Block -------------------------

    #[test]
    fn https_page_loading_http_script_blocks() {
        let v = classify(
            true,
            &url("http://a.test/x.js"),
            ResourceType::Script,
            false,
        );
        assert_eq!(v, MixedContentVerdict::Block);
    }

    #[test]
    fn https_page_loading_http_stylesheet_blocks() {
        let v = classify(
            true,
            &url("http://a.test/x.css"),
            ResourceType::Stylesheet,
            false,
        );
        assert_eq!(v, MixedContentVerdict::Block);
    }

    #[test]
    fn https_page_loading_http_iframe_blocks() {
        let v = classify(
            true,
            &url("http://a.test/frame"),
            ResourceType::Iframe,
            false,
        );
        assert_eq!(v, MixedContentVerdict::Block);
    }

    #[test]
    fn https_page_loading_http_object_blocks() {
        let v = classify(true, &url("http://a.test/obj"), ResourceType::Object, false);
        assert_eq!(v, MixedContentVerdict::Block);
    }

    #[test]
    fn https_page_loading_http_font_blocks() {
        let v = classify(
            true,
            &url("http://a.test/f.woff"),
            ResourceType::Font,
            false,
        );
        assert_eq!(v, MixedContentVerdict::Block);
    }

    #[test]
    fn https_page_fetching_http_via_xhr_blocks() {
        let v = classify(true, &url("http://a.test/api"), ResourceType::Fetch, false);
        assert_eq!(v, MixedContentVerdict::Block);
    }

    #[test]
    fn https_page_opening_ws_websocket_blocks() {
        // ws:// (unlike wss://) is mixed content on an https page.
        let v = classify(true, &url("ws://a.test/ws"), ResourceType::Stream, false);
        assert_eq!(v, MixedContentVerdict::Block);
    }

    // --- classify: passive mixed content → Upgrade ----------------------

    #[test]
    fn https_page_loading_http_image_upgrades() {
        let v = classify(
            true,
            &url("http://a.test/x.png"),
            ResourceType::Image,
            false,
        );
        assert_eq!(v, MixedContentVerdict::Upgrade);
    }

    #[test]
    fn https_page_loading_http_audio_upgrades() {
        let v = classify(
            true,
            &url("http://a.test/x.mp3"),
            ResourceType::Audio,
            false,
        );
        assert_eq!(v, MixedContentVerdict::Upgrade);
    }

    #[test]
    fn https_page_loading_http_video_upgrades() {
        let v = classify(
            true,
            &url("http://a.test/x.mp4"),
            ResourceType::Video,
            false,
        );
        assert_eq!(v, MixedContentVerdict::Upgrade);
    }

    // --- classify: navigation never blocked ------------------------------

    #[test]
    fn https_page_navigating_to_http_is_not_blocked() {
        // Top-level navigation is exempt — the user can always follow a link
        // to http. (The UA's HTTPS-First mode is a separate upgrade path.)
        let v = classify(
            true,
            &url("http://a.test/page"),
            ResourceType::Document,
            false,
        );
        assert_eq!(v, MixedContentVerdict::NotMixed);
    }

    // --- classify: CSP block-all-mixed-content overrides ----------------

    #[test]
    fn block_all_directive_blocks_passive_content() {
        // Without the directive, an http image upgrades. With it, blocks.
        let v = classify(true, &url("http://a.test/x.png"), ResourceType::Image, true);
        assert_eq!(v, MixedContentVerdict::Block);
    }

    #[test]
    fn block_all_directive_blocks_active_content() {
        let v = classify(true, &url("http://a.test/x.js"), ResourceType::Script, true);
        assert_eq!(v, MixedContentVerdict::Block);
    }

    #[test]
    fn block_all_directive_does_not_apply_when_context_insecure() {
        // The directive is meaningless on an http page; rules short-circuit.
        let v = classify(
            false,
            &url("http://a.test/x.png"),
            ResourceType::Image,
            true,
        );
        assert_eq!(v, MixedContentVerdict::NotMixed);
    }

    #[test]
    fn block_all_directive_does_not_block_secure_request() {
        let v = classify(
            true,
            &url("https://a.test/x.png"),
            ResourceType::Image,
            true,
        );
        assert_eq!(v, MixedContentVerdict::NotMixed);
    }

    // --- Realistic HTTPS pages ------------------------------------------

    #[test]
    fn typical_https_page_with_one_mixed_resource() {
        // The pattern that breaks real sites: a mostly-https page with one
        // legacy http:// script tag.
        let v = classify(
            true,
            &url("http://legacy-analytics.example/tracker.js"),
            ResourceType::Script,
            false,
        );
        assert_eq!(v, MixedContentVerdict::Block);
        // Same resource upgraded to https would pass.
        let v = classify(
            true,
            &url("https://legacy-analytics.example/tracker.js"),
            ResourceType::Script,
            false,
        );
        assert_eq!(v, MixedContentVerdict::NotMixed);
    }

    #[test]
    fn https_page_with_legacy_http_image_upgrades() {
        let v = classify(
            true,
            &url("http://cdn.example/hero.jpg"),
            ResourceType::Image,
            false,
        );
        assert_eq!(v, MixedContentVerdict::Upgrade);
    }
}
