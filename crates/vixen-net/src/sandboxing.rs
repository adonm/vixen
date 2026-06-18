//! HTML `<iframe sandbox>` flag parsing — Phase 7 security prep (pure logic
//! at a trust boundary documented in `docs/ARCHITECTURE.md`). Implements the
//! WHATWG HTML § 4.8.5 sandbox-flag parser + the security-relevant
//! predicates the fetch / script / navigation layers consult when loading
//! framed content.
//!
//! What lives here:
//! - [`SandboxFlags`] — the bitfield of every § 4.8.5 `allow-*` flag.
//! - [`parse_sandbox`] — the attribute-value parser (tokenised on ASCII
//!   whitespace, case-insensitive, unknown flags ignored, empty ⇒ most
//!   restrictive).
//! - [`SandboxFlags::allows_*`] — per-capability predicates the layers
//!   consult (`scripts`, `same_origin`, `forms`, `popups`, top-nav family,
//!   `modals`, `pointer_lock`, `presentation`, `orientation_lock`,
//!   `downloads`, `storage_access`).
//! - [`SandboxFlags::implies_unique_origin`] — the § 4.8.5 opaque-origin rule
//!   (`allow-same-origin` absent ⇒ the framed document gets an opaque
//!   origin, isolating its storage/cookies from the embedder).
//! - [`SandboxFlags::is_dangerous_scripts_plus_same_origin`] — the famous
//!   "if both `allow-scripts` and `allow-same-origin` are present, the
//!   sandbox is escapable" warning predicate.
//!
//! What does *not* live here:
//! - The actual sandbox enforcement (the script-layer CSP gating, the
//!   navigation guard, the opaque-origin injection — those consult this
//!   module's predicates and live in `vixen-core::script` / the navigation
//!   pipeline).
//! - The `allowpopups` ↔ `target=_blank` plumbing (a runtime decision the
//!   navigation layer makes given [`SandboxFlags::allows_popups`]).
//! - The `sandbox` attribute reflection on the HTMLIFrameElement host hook
//!   (Phase 6 host-hook layer owns the live reflection; this module is the
//!   pure value it reduces to).
//!
//! ## WHATWG § 4.8.5 semantics
//!
//! The `sandbox` attribute is a set of ASCII-whitespace-separated tokens. The
//! rules:
//! - An empty set (the `sandbox` attribute present but valueless, or
//!   `sandbox=""`) ⇒ every capability is revoked. This is the "most
//!   restrictive" form.
//! - A non-empty set ⇒ the named capabilities are re-enabled; everything
//!   else stays revoked.
//! - Tokens are ASCII case-insensitive (`Allow-Scripts` == `allow-scripts`).
//! - Unknown tokens are ignored (no parse error).
//! - Order is irrelevant (set semantics).
//!
//! The `allow-scripts` + `allow-same-origin` warning: per the spec, "when
//! the embedded document has the same origin as the embedding page, the
//! sandbox is ineffectual" because the framed script can walk up to its
//! parent and remove the sandbox attribute. [`is_dangerous_scripts_plus_same_origin`]
//! surfaces this so the shell can flag it as a `EngineDiagnostic`.
//!
//! Reference: <https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-sandbox>
//! § 4.8.5 "the `sandbox` attribute".

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// SandboxFlags
// ---------------------------------------------------------------------------

/// The set of parsed `sandbox` flags. One bit per WHATWG HTML § 4.8.5
/// `allow-*` keyword. The default (empty attribute) is `SandboxFlags::empty()`
/// — every flag cleared, every capability revoked (the most-restrictive form).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SandboxFlags(u32);

// Bit assignments — names mirror the WHATWG § 4.8.5 keywords verbatim.
impl SandboxFlags {
    const ALLOW_FORMS: u32 = 1 << 0;
    const ALLOW_MODALS: u32 = 1 << 1;
    const ALLOW_ORIENTATION_LOCK: u32 = 1 << 2;
    const ALLOW_POINTER_LOCK: u32 = 1 << 3;
    const ALLOW_POPUPS: u32 = 1 << 4;
    const ALLOW_POPUPS_TO_ESCAPE_SANDBOX: u32 = 1 << 5;
    const ALLOW_PRESENTATION: u32 = 1 << 6;
    const ALLOW_SAME_ORIGIN: u32 = 1 << 7;
    const ALLOW_SCRIPTS: u32 = 1 << 8;
    const ALLOW_TOP_NAVIGATION: u32 = 1 << 9;
    const ALLOW_TOP_NAVIGATION_BY_USER_ACTIVATION: u32 = 1 << 10;
    const ALLOW_TOP_NAVIGATION_TO_CUSTOM_PROTOCOLS: u32 = 1 << 11;
    const ALLOW_DOWNLOADS: u32 = 1 << 12;
    const ALLOW_STORAGE_ACCESS_BY_USER_ACTIVATION: u32 = 1 << 13;
    /// WHATWG § 4.8.5.38 — the "download without user activation" escape
    /// hatch. Only emitted by very recent specs; the flag name mirrors the
    /// spec prose ("unsafe downloads").
    const ALLOW_UNSAFE_DOWNLOADS: u32 = 1 << 14;

    /// All flags cleared — the most-restrictive sandbox (every capability
    /// revoked). This is the result of parsing `sandbox=""`.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// `true` iff no flag is set (the most-restrictive form).
    pub fn is_most_restrictive(self) -> bool {
        self.0 == 0
    }

    /// Bitwise union (`self | other`).
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Bitwise intersection (`self & other`).
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// `true` iff any flag is set in both.
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    // --- per-capability predicates (WHATWG § 4.8.5 step "parse a sandboxing
    //     directive") -----------------------------------------------

    /// `allow-forms` — re-enables form submission. Without it, the framed
    /// document's forms are inert (submit is a no-op).
    pub fn allows_forms(self) -> bool {
        self.0 & Self::ALLOW_FORMS != 0
    }

    /// `allow-modals` — re-enables `window.alert()`, `confirm()`, `prompt()`.
    pub fn allows_modals(self) -> bool {
        self.0 & Self::ALLOW_MODALS != 0
    }

    /// `allow-orientation-lock` — re-enables `screen.orientation.lock()`.
    pub fn allows_orientation_lock(self) -> bool {
        self.0 & Self::ALLOW_ORIENTATION_LOCK != 0
    }

    /// `allow-pointer-lock` — re-enables `Element.requestPointerLock()`.
    pub fn allows_pointer_lock(self) -> bool {
        self.0 & Self::ALLOW_POINTER_LOCK != 0
    }

    /// `allow-popups` — re-enables `window.open()`, `target=_blank`. Without
    /// it, popups are blocked.
    pub fn allows_popups(self) -> bool {
        self.0 & Self::ALLOW_POPUPS != 0
    }

    /// `allow-popups-to-escape-sandbox` — popups opened by the framed document
    /// inherit *no* sandboxing (they get a fresh, unrestricted browsing
    /// context). Without it, popups inherit the parent's sandbox.
    pub fn allows_popups_to_escape_sandbox(self) -> bool {
        self.0 & Self::ALLOW_POPUPS_TO_ESCAPE_SANDBOX != 0
    }

    /// `allow-presentation` — re-enables `PresentationRequest`.
    pub fn allows_presentation(self) -> bool {
        self.0 & Self::ALLOW_PRESENTATION != 0
    }

    /// `allow-same-origin` — treats the framed document as its real origin
    /// (not an opaque one). Without it, [`implies_unique_origin`] ⇒ `true`.
    pub fn allows_same_origin(self) -> bool {
        self.0 & Self::ALLOW_SAME_ORIGIN != 0
    }

    /// `allow-scripts` — re-enables JS execution. Without it, the framed
    /// document's `<script>` is inert.
    pub fn allows_scripts(self) -> bool {
        self.0 & Self::ALLOW_SCRIPTS != 0
    }

    /// `allow-top-navigation` — the framed document can navigate its
    /// top-level browsing context unconditionally.
    pub fn allows_top_navigation(self) -> bool {
        self.0 & Self::ALLOW_TOP_NAVIGATION != 0
    }

    /// `allow-top-navigation-by-user-activation` — the framed document can
    /// navigate the top-level context iff a user-gesture is present.
    pub fn allows_top_navigation_by_user_activation(self) -> bool {
        self.0 & Self::ALLOW_TOP_NAVIGATION_BY_USER_ACTIVATION != 0
    }

    /// `allow-top-navigation-to-custom-protocols` — the framed document can
    /// navigate the top-level context to an external protocol handler
    /// (`mailto:`, `tel:`, …).
    pub fn allows_top_navigation_to_custom_protocols(self) -> bool {
        self.0 & Self::ALLOW_TOP_NAVIGATION_TO_CUSTOM_PROTOCOLS != 0
    }

    /// `allow-downloads` — re-enables downloads triggered by `<a download>`
    /// or programmatic fetch + blob URL downloads.
    pub fn allows_downloads(self) -> bool {
        self.0 & Self::ALLOW_DOWNLOADS != 0
    }

    /// `allow-storage-access-by-user-activation` — re-enables
    /// `document.requestStorageAccess()` (the Storage Access API).
    pub fn allows_storage_access_by_user_activation(self) -> bool {
        self.0 & Self::ALLOW_STORAGE_ACCESS_BY_USER_ACTIVATION != 0
    }

    /// `allow-unsafe-downloads` — the recent escape hatch for downloads
    /// without user activation. Only used by very recent specs.
    pub fn allows_unsafe_downloads(self) -> bool {
        self.0 & Self::ALLOW_UNSAFE_DOWNLOADS != 0
    }

    // --- Derived security predicates --------------------------------

    /// WHATWG § 4.8.5 "if the allow-same-origin flag is not set, the
    /// document gets an opaque origin" ⇒ storage/cookies are partitioned
    /// away from the embedder. Returns `true` when the framed document's
    /// origin will be opaque (i.e. `allow-same-origin` is absent).
    pub fn implies_unique_origin(self) -> bool {
        !self.allows_same_origin()
    }

    /// WHATWG § 4.8.5 "Potentially exploitable combination". `true` when
    /// both `allow-scripts` and `allow-same-origin` are set — the framed
    /// document can script out of the sandbox (it can reach the parent's
    /// DOM, find its own iframe element, and remove the `sandbox` attribute).
    /// The spec mandates a console warning; this predicate surfaces it.
    pub fn is_dangerous_scripts_plus_same_origin(self) -> bool {
        self.allows_scripts() && self.allows_same_origin()
    }

    /// `true` when *any* top-navigation capability is granted. The
    /// navigation layer consults this before checking the per-flag
    /// user-activation rule.
    pub fn allows_any_top_navigation(self) -> bool {
        self.allows_top_navigation()
            || self.allows_top_navigation_by_user_activation()
            || self.allows_top_navigation_to_custom_protocols()
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse the `sandbox` attribute value (WHATWG § 4.8.5 + § 2.7.3). Tokens
/// are split on ASCII whitespace, compared case-insensitively against the
/// `allow-*` keyword set, and OR'd into the flag bitfield. Unknown tokens
/// are ignored (no parse error). An empty value ⇒ [`SandboxFlags::empty`]
/// (the most-restrictive form).
///
/// The single HTML-parser edge case: the valueless `sandbox` attribute
/// (`<iframe sandbox>`) is serialised by the HTML parser as the empty string,
/// which [`parse_sandbox`] handles correctly (`empty()` ⇒ most restrictive).
pub fn parse_sandbox(attribute_value: &str) -> SandboxFlags {
    let mut flags = SandboxFlags::empty();
    for token in attribute_value.split_ascii_whitespace() {
        // The WHATWG parsing rule: ASCII case-insensitive match.
        match token.to_ascii_lowercase().as_str() {
            "allow-forms" => flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_FORMS)),
            "allow-modals" => flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_MODALS)),
            "allow-orientation-lock" => {
                flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_ORIENTATION_LOCK))
            }
            "allow-pointer-lock" => {
                flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_POINTER_LOCK))
            }
            "allow-popups" => flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_POPUPS)),
            "allow-popups-to-escape-sandbox" => {
                flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_POPUPS_TO_ESCAPE_SANDBOX))
            }
            "allow-presentation" => {
                flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_PRESENTATION))
            }
            "allow-same-origin" => {
                flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_SAME_ORIGIN))
            }
            "allow-scripts" => flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_SCRIPTS)),
            "allow-top-navigation" => {
                flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_TOP_NAVIGATION))
            }
            "allow-top-navigation-by-user-activation" => {
                flags = flags.union(SandboxFlags(
                    SandboxFlags::ALLOW_TOP_NAVIGATION_BY_USER_ACTIVATION,
                ))
            }
            "allow-top-navigation-to-custom-protocols" => {
                flags = flags.union(SandboxFlags(
                    SandboxFlags::ALLOW_TOP_NAVIGATION_TO_CUSTOM_PROTOCOLS,
                ))
            }
            "allow-downloads" => flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_DOWNLOADS)),
            "allow-storage-access-by-user-activation" => {
                flags = flags.union(SandboxFlags(
                    SandboxFlags::ALLOW_STORAGE_ACCESS_BY_USER_ACTIVATION,
                ))
            }
            "allow-unsafe-downloads" => {
                flags = flags.union(SandboxFlags(SandboxFlags::ALLOW_UNSAFE_DOWNLOADS))
            }
            _ => {} // unknown tokens ignored per WHATWG.
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Parser: empty / most-restrictive ------------------------------

    #[test]
    fn empty_value_is_most_restrictive() {
        // `<iframe sandbox>` or `<iframe sandbox="">` ⇒ all flags cleared.
        let f = parse_sandbox("");
        assert_eq!(f, SandboxFlags::empty());
        assert!(f.is_most_restrictive());
        assert!(!f.allows_scripts());
        assert!(!f.allows_forms());
        assert!(!f.allows_same_origin());
    }

    #[test]
    fn whitespace_only_is_most_restrictive() {
        let f = parse_sandbox("   \t\n  ");
        assert!(f.is_most_restrictive());
    }

    // --- Parser: single flags -----------------------------------------

    #[test]
    fn parse_allow_scripts() {
        let f = parse_sandbox("allow-scripts");
        assert!(f.allows_scripts());
        assert!(!f.allows_forms());
        assert!(f.implies_unique_origin()); // allow-same-origin absent
    }

    #[test]
    fn parse_allow_same_origin() {
        let f = parse_sandbox("allow-same-origin");
        assert!(f.allows_same_origin());
        assert!(!f.implies_unique_origin());
        assert!(!f.allows_scripts());
    }

    #[test]
    fn parse_allow_forms() {
        assert!(parse_sandbox("allow-forms").allows_forms());
    }

    #[test]
    fn parse_allow_popups() {
        assert!(parse_sandbox("allow-popups").allows_popups());
    }

    #[test]
    fn parse_allow_modals() {
        assert!(parse_sandbox("allow-modals").allows_modals());
    }

    #[test]
    fn parse_allow_pointer_lock() {
        assert!(parse_sandbox("allow-pointer-lock").allows_pointer_lock());
    }

    #[test]
    fn parse_allow_presentation() {
        assert!(parse_sandbox("allow-presentation").allows_presentation());
    }

    #[test]
    fn parse_allow_orientation_lock() {
        assert!(parse_sandbox("allow-orientation-lock").allows_orientation_lock());
    }

    #[test]
    fn parse_allow_top_navigation() {
        let f = parse_sandbox("allow-top-navigation");
        assert!(f.allows_top_navigation());
        assert!(f.allows_any_top_navigation());
    }

    #[test]
    fn parse_allow_top_navigation_by_user_activation() {
        let f = parse_sandbox("allow-top-navigation-by-user-activation");
        assert!(f.allows_top_navigation_by_user_activation());
        assert!(f.allows_any_top_navigation());
    }

    #[test]
    fn parse_allow_top_navigation_to_custom_protocols() {
        let f = parse_sandbox("allow-top-navigation-to-custom-protocols");
        assert!(f.allows_top_navigation_to_custom_protocols());
        assert!(f.allows_any_top_navigation());
    }

    #[test]
    fn parse_allow_popups_to_escape_sandbox() {
        assert!(parse_sandbox("allow-popups-to-escape-sandbox").allows_popups_to_escape_sandbox());
    }

    #[test]
    fn parse_allow_downloads() {
        assert!(parse_sandbox("allow-downloads").allows_downloads());
    }

    #[test]
    fn parse_allow_storage_access_by_user_activation() {
        assert!(
            parse_sandbox("allow-storage-access-by-user-activation")
                .allows_storage_access_by_user_activation()
        );
    }

    #[test]
    fn parse_allow_unsafe_downloads() {
        assert!(parse_sandbox("allow-unsafe-downloads").allows_unsafe_downloads());
    }

    // --- Parser: case-insensitivity -----------------------------------

    #[test]
    fn parse_case_insensitive() {
        // WHATWG: tokens are ASCII case-insensitive. Real-world pages emit
        // mixed-case (legacy Microsoft CMSes, etc.).
        assert!(parse_sandbox("Allow-Scripts").allows_scripts());
        assert!(parse_sandbox("ALLOW-SCRIPTS").allows_scripts());
        assert!(parse_sandbox("allow-scripts ALLOW-FORMS").allows_forms());
    }

    // --- Parser: multiple flags ---------------------------------------

    #[test]
    fn parse_multiple_flags_in_any_order() {
        let a = parse_sandbox("allow-scripts allow-forms allow-same-origin");
        let b = parse_sandbox("allow-same-origin allow-forms allow-scripts");
        assert_eq!(a, b);
        assert!(a.allows_scripts());
        assert!(a.allows_forms());
        assert!(a.allows_same_origin());
    }

    #[test]
    fn parse_whitespace_collapses() {
        // Tab/newline-separated tokens parse identically to space-separated.
        let a = parse_sandbox("allow-scripts allow-forms");
        let b = parse_sandbox("allow-scripts\tallow-forms");
        let c = parse_sandbox("  allow-scripts\nallow-forms  ");
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    // --- Parser: unknown tokens ignored -------------------------------

    #[test]
    fn parse_ignores_unknown_tokens() {
        let f = parse_sandbox("allow-scripts made-up-flag allow-forms");
        assert!(f.allows_scripts());
        assert!(f.allows_forms());
    }

    #[test]
    fn parse_ignores_typos() {
        // `allow-script` (missing s) is NOT `allow-scripts`.
        let f = parse_sandbox("allow-script");
        assert!(!f.allows_scripts());
    }

    // --- Derived predicates -------------------------------------------

    #[test]
    fn implies_unique_origin_unless_allow_same_origin() {
        // The default (empty sandbox) ⇒ opaque origin.
        assert!(parse_sandbox("").implies_unique_origin());
        // allow-scripts alone ⇒ still opaque.
        assert!(parse_sandbox("allow-scripts").implies_unique_origin());
        // allow-same-origin ⇒ real origin.
        assert!(!parse_sandbox("allow-same-origin").implies_unique_origin());
        // Both ⇒ real origin AND scripts ⇒ dangerous.
        let f = parse_sandbox("allow-scripts allow-same-origin");
        assert!(!f.implies_unique_origin());
        assert!(f.is_dangerous_scripts_plus_same_origin());
    }

    #[test]
    fn dangerous_combination_only_when_both_set() {
        assert!(!parse_sandbox("").is_dangerous_scripts_plus_same_origin());
        assert!(!parse_sandbox("allow-scripts").is_dangerous_scripts_plus_same_origin());
        assert!(!parse_sandbox("allow-same-origin").is_dangerous_scripts_plus_same_origin());
        assert!(
            parse_sandbox("allow-scripts allow-same-origin")
                .is_dangerous_scripts_plus_same_origin()
        );
    }

    #[test]
    fn allows_any_top_navigation_only_when_a_top_nav_flag_set() {
        assert!(!parse_sandbox("").allows_any_top_navigation());
        assert!(parse_sandbox("allow-top-navigation").allows_any_top_navigation());
        assert!(
            parse_sandbox("allow-top-navigation-by-user-activation").allows_any_top_navigation()
        );
        assert!(
            parse_sandbox("allow-top-navigation-to-custom-protocols").allows_any_top_navigation()
        );
        // A non-top-nav flag doesn't grant top-nav.
        assert!(!parse_sandbox("allow-scripts").allows_any_top_navigation());
    }

    // --- Bit ops -------------------------------------------------------

    #[test]
    fn union_combines_flags() {
        let a = parse_sandbox("allow-scripts");
        let b = parse_sandbox("allow-forms");
        let combined = a.union(b);
        assert!(combined.allows_scripts());
        assert!(combined.allows_forms());
    }

    #[test]
    fn intersection_keeps_only_shared_flags() {
        let a = parse_sandbox("allow-scripts allow-forms");
        let b = parse_sandbox("allow-forms allow-same-origin");
        let shared = a.intersection(b);
        assert!(shared.allows_forms());
        assert!(!shared.allows_scripts());
        assert!(!shared.allows_same_origin());
    }

    #[test]
    fn intersects_tests_for_any_shared() {
        let a = parse_sandbox("allow-scripts");
        let b = parse_sandbox("allow-scripts allow-forms");
        let c = parse_sandbox("allow-forms");
        assert!(a.intersects(b));
        assert!(!a.intersects(c));
    }

    #[test]
    fn empty_flags_identity() {
        // empty | x == x; empty & x == empty.
        let x = parse_sandbox("allow-scripts");
        assert_eq!(SandboxFlags::empty().union(x), x);
        assert_eq!(SandboxFlags::empty().intersection(x), SandboxFlags::empty());
        assert!(!SandboxFlags::empty().intersects(x));
    }

    // --- Round-trip via parse + predicate ------------------------------

    #[test]
    fn minimal_sandbox_for_user_content() {
        // The typical embed: allow-scripts only (origin stays opaque so the
        // framed page can't read parent cookies/storage).
        let f = parse_sandbox("allow-scripts");
        assert!(f.allows_scripts());
        assert!(f.implies_unique_origin());
        assert!(!f.is_dangerous_scripts_plus_same_origin());
        assert!(!f.allows_any_top_navigation()); // can't navigate parent
        assert!(!f.allows_popups()); // can't open popups
    }

    #[test]
    fn youtube_embed_pattern() {
        // The typical YouTube embed: scripts + popups + same-origin +
        // presentation (this is the documented YouTube iframe pattern).
        let f = parse_sandbox("allow-scripts allow-same-origin allow-popups allow-presentation");
        assert!(f.allows_scripts());
        assert!(f.allows_same_origin());
        assert!(f.allows_popups());
        assert!(f.allows_presentation());
        // This is the "dangerous" combination (which YouTube accepts as the
        // trade-off for the embedded player).
        assert!(f.is_dangerous_scripts_plus_same_origin());
    }
}
