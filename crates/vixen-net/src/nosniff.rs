//! `X-Content-Type-Options: nosniff` — Phase 7 security prep. The response
//! header that suppresses MIME sniffing and (for the two "nosniff-blocked"
//! destinations) blocks the subresource if its declared MIME is not the
//! expected type. The fetch layer consults this before executing a script or
//! applying a stylesheet.
//!
//! What lives here:
//! - [`is_nosniff`] — the § 1 header parse (case-insensitive single token).
//! - [`is_javascript_mime`] — Fetch § 3.7 "JavaScript MIME type" predicate
//!   (the 16-entry list the script destination requires).
//! - [`Destination`] — the fetch § 3.1.7 request destination, collapsed to
//!   the two nosniff-relevant categories (`Script` / `Style` / `Other`).
//! - [`enforce`] — the § 2 enforcement: for a `Script` destination, block if
//!   the MIME is not a JavaScript MIME type; for a `Style` destination, block
//!   if the MIME is not `text/css`; otherwise allow.
//! - [`NosniffOutcome`] — `Allow` / `Block(reason)` for the fetch layer's
//!   error reporting.
//!
//! What does *not* live here:
//! - The actual MIME sniffing algorithm (Fetch § 8). Vixen does not sniff by
//!   default (the declared MIME wins); `nosniff` only tightens the script /
//!   style MIME check. The sniff algorithm itself is deferred.
//! - The `CORB` / `ORB` (cross-origin read blocking) algorithms, which build
//!   on the nosniff outcome but consult the `Sec-Fetch-*` headers too
//!   (Phase 7, [`crate::sec_fetch`]).
//!
//! ## Trust boundary
//!
//! Without `nosniff`, a server can label an `<img src>` response as
//! `text/html` and, if the browser sniffed it as a script, a latent XSS
//! surface opens. With `nosniff`, the browser refuses to execute a
//! `<script>` whose MIME is not a JavaScript MIME type — closing the
//! confusion attack for the two destinations where it matters (script,
//! style). Other destinations are unaffected (the spec intentionally limits
//! `nosniff`'s scope).
//!
//! Reference: <https://fetch.spec.whatwg.org/#x-content-type-options-header>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Header parse
// ---------------------------------------------------------------------------

/// `true` iff the `X-Content-Type-Options` header value is `nosniff`
/// (case-insensitive). Any other value (including the empty string, the
/// absent header, and the historical `nosniff; …` parameterised form) does
/// **not** enable nosniff — the header is a bare token per Fetch § 3.2.
///
/// ```
/// # use vixen_net::nosniff::is_nosniff;
/// assert!(is_nosniff("nosniff"));
/// assert!(is_nosniff("NoSniff"));
/// assert!(!is_nosniff(""));
/// assert!(!is_nosniff("nosniff; foo=bar")); // parameterised form is rejected
/// ```
pub fn is_nosniff(header_value: &str) -> bool {
    header_value.trim().eq_ignore_ascii_case("nosniff")
}

// ---------------------------------------------------------------------------
// JavaScript MIME type (Fetch § 3.7)
// ---------------------------------------------------------------------------

/// `true` iff `mime` is one of the Fetch § 3.7 "JavaScript MIME type"s the
/// `<script>` / worker destinations accept. Comparison is on the MIME
/// essence (`type/subtype`, ASCII-case-insensitive, parameters stripped).
/// The caller passes the response's `Content-Type` MIME essence (the
/// [`crate::mime`] parser's `essence()` is the canonical source).
///
/// The 16 entries (Fetch § 3.7 + the historical Netscape/JScript variants):
/// `application/ecmascript`, `application/javascript`,
/// `application/x-ecmascript`, `application/x-javascript`,
/// `text/ecmascript`, `text/javascript`, `text/javascript1.0` … `1.5`,
/// `text/jscript`, `text/livescript`, `text/x-ecmascript`,
/// `text/x-javascript`.
pub fn is_javascript_mime(mime: &str) -> bool {
    matches!(
        mime.trim().to_ascii_lowercase().as_str(),
        "application/ecmascript"
            | "application/javascript"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "text/javascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
    )
}

// ---------------------------------------------------------------------------
// Destination
// ---------------------------------------------------------------------------

/// The fetch destination, collapsed to the two nosniff-relevant categories
/// (Fetch § 3.1.7). `Script` covers `<script>`, `Worker` / `SharedWorker` /
/// `ServiceWorker` script requests, and `import()` script-like requests;
/// `Style` covers `<link rel=stylesheet>`; everything else is `Other` and
/// unaffected by nosniff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Destination {
    /// `<script>`, classic / module workers, `import()`.
    Script,
    /// `<link rel="stylesheet">`.
    Style,
    /// Everything else (`<img>`, `<iframe>`, `fetch()`, …). nosniff does
    /// not consult these.
    #[default]
    Other,
}

impl Destination {
    /// Map a Fetch § 3.1.7 request-destination token to the collapsed
    /// category. Unknown tokens map to [`Destination::Other`].
    pub fn from_fetch_destination(token: &str) -> Self {
        match token {
            "script" | "worker" | "sharedworker" | "serviceworker" | "audioworklet"
            | "paintworklet" => Destination::Script,
            "style" => Destination::Style,
            _ => Destination::Other,
        }
    }
}

// ---------------------------------------------------------------------------
// Enforcement
// ---------------------------------------------------------------------------

/// Why a response was blocked (or allowed) by [`enforce`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NosniffOutcome {
    /// The response is allowed — either nosniff was not set, or the MIME
    /// matches the destination's requirement.
    Allow,
    /// Blocked: the destination is `Script` but the MIME is not a JavaScript
    /// MIME type. Carries the offending MIME essence for the console error.
    BlockScript(String),
    /// Blocked: the destination is `Style` but the MIME is not `text/css`.
    /// Carries the offending MIME essence.
    BlockStyle(String),
}

/// Enforce `X-Content-Type-Options: nosniff` per Fetch § 2. `nosniff_set` is
/// [`is_nosniff`] over the response header; `mime_essence` is the response's
/// `Content-Type` MIME essence (`type/subtype`, lowercased); `dest` is the
/// request destination collapsed via [`Destination::from_fetch_destination`].
///
/// Returns [`NosniffOutcome::Allow`] when nosniff is not set or the MIME
/// matches; the corresponding `Block*` variant when nosniff is set and the
/// MIME does not match the destination's requirement. The fetch layer
/// surfaces the block as a network error (the response body is discarded).
///
/// ```
/// # use vixen_net::nosniff::{Destination, NosniffOutcome, enforce};
/// // nosniff not set → always allow.
/// assert_eq!(enforce(false, "text/plain", Destination::Script), NosniffOutcome::Allow);
/// // nosniff set + script dest + non-JS MIME → block.
/// assert_eq!(enforce(true, "text/plain", Destination::Script), NosniffOutcome::BlockScript("text/plain".into()));
/// // nosniff set + script dest + JS MIME → allow.
/// assert_eq!(enforce(true, "text/javascript", Destination::Script), NosniffOutcome::Allow);
/// ```
pub fn enforce(nosniff_set: bool, mime_essence: &str, dest: Destination) -> NosniffOutcome {
    if !nosniff_set {
        return NosniffOutcome::Allow;
    }
    match dest {
        Destination::Script => {
            if is_javascript_mime(mime_essence) {
                NosniffOutcome::Allow
            } else {
                NosniffOutcome::BlockScript(mime_essence.to_owned())
            }
        }
        Destination::Style => {
            if mime_essence.trim().eq_ignore_ascii_case("text/css") {
                NosniffOutcome::Allow
            } else {
                NosniffOutcome::BlockStyle(mime_essence.to_owned())
            }
        }
        Destination::Other => NosniffOutcome::Allow,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_nosniff ----------------------------------------------------

    #[test]
    fn nosniff_canonical() {
        assert!(is_nosniff("nosniff"));
    }

    #[test]
    fn nosniff_case_insensitive() {
        assert!(is_nosniff("NoSniff"));
        assert!(is_nosniff("NOSNIFF"));
        assert!(is_nosniff(" nosniff "));
    }

    #[test]
    fn nosniff_rejects_other_values() {
        assert!(!is_nosniff(""));
        assert!(!is_nosniff("no-sniff"));
        assert!(!is_nosniff("true"));
        // The parameterised historical form is rejected — the header is a
        // bare token.
        assert!(!is_nosniff("nosniff; foo=bar"));
    }

    // --- is_javascript_mime -------------------------------------------

    #[test]
    fn js_mime_canonical() {
        assert!(is_javascript_mime("text/javascript"));
        assert!(is_javascript_mime("application/javascript"));
        assert!(is_javascript_mime("application/ecmascript"));
    }

    #[test]
    fn js_mime_historical_variants() {
        assert!(is_javascript_mime("text/javascript1.5"));
        assert!(is_javascript_mime("text/jscript"));
        assert!(is_javascript_mime("text/livescript"));
        assert!(is_javascript_mime("text/x-javascript"));
        assert!(is_javascript_mime("application/x-ecmascript"));
    }

    #[test]
    fn js_mime_case_insensitive_and_parameters_stripped_by_caller() {
        assert!(is_javascript_mime("TEXT/JAVASCRIPT"));
        // The caller is responsible for stripping parameters (this module
        // compares the essence); a parameter-laden value does not match.
        assert!(!is_javascript_mime("text/javascript; charset=utf-8"));
    }

    #[test]
    fn js_mime_rejects_non_js() {
        assert!(!is_javascript_mime("text/plain"));
        assert!(!is_javascript_mime("application/json"));
        assert!(!is_javascript_mime("text/css"));
        assert!(!is_javascript_mime("application/wasm"));
        assert!(!is_javascript_mime(""));
    }

    // --- Destination ---------------------------------------------------

    #[test]
    fn destination_collapse() {
        assert_eq!(
            Destination::from_fetch_destination("script"),
            Destination::Script
        );
        assert_eq!(
            Destination::from_fetch_destination("worker"),
            Destination::Script
        );
        assert_eq!(
            Destination::from_fetch_destination("serviceworker"),
            Destination::Script
        );
        assert_eq!(
            Destination::from_fetch_destination("style"),
            Destination::Style
        );
        assert_eq!(
            Destination::from_fetch_destination("image"),
            Destination::Other
        );
        assert_eq!(
            Destination::from_fetch_destination("document"),
            Destination::Other
        );
        assert_eq!(Destination::from_fetch_destination(""), Destination::Other);
    }

    // --- enforce -------------------------------------------------------

    #[test]
    fn enforce_allows_when_nosniff_unset() {
        // Even a non-JS MIME is allowed without nosniff.
        assert_eq!(
            enforce(false, "text/plain", Destination::Script),
            NosniffOutcome::Allow
        );
        assert_eq!(
            enforce(false, "text/javascript", Destination::Script),
            NosniffOutcome::Allow
        );
        assert_eq!(
            enforce(false, "text/html", Destination::Style),
            NosniffOutcome::Allow
        );
    }

    #[test]
    fn enforce_blocks_script_with_non_js_mime() {
        assert_eq!(
            enforce(true, "text/html", Destination::Script),
            NosniffOutcome::BlockScript("text/html".into())
        );
        assert_eq!(
            enforce(true, "image/png", Destination::Script),
            NosniffOutcome::BlockScript("image/png".into())
        );
    }

    #[test]
    fn enforce_allows_script_with_js_mime() {
        assert_eq!(
            enforce(true, "text/javascript", Destination::Script),
            NosniffOutcome::Allow
        );
        assert_eq!(
            enforce(true, "application/javascript", Destination::Script),
            NosniffOutcome::Allow
        );
    }

    #[test]
    fn enforce_blocks_style_with_non_css_mime() {
        assert_eq!(
            enforce(true, "text/plain", Destination::Style),
            NosniffOutcome::BlockStyle("text/plain".into())
        );
        assert_eq!(
            enforce(true, "text/html", Destination::Style),
            NosniffOutcome::BlockStyle("text/html".into())
        );
    }

    #[test]
    fn enforce_allows_style_with_text_css() {
        assert_eq!(
            enforce(true, "text/css", Destination::Style),
            NosniffOutcome::Allow
        );
        // Case-insensitive.
        assert_eq!(
            enforce(true, "TEXT/CSS", Destination::Style),
            NosniffOutcome::Allow
        );
    }

    #[test]
    fn enforce_other_destination_always_allowed() {
        // nosniff only affects script + style.
        assert_eq!(
            enforce(true, "text/html", Destination::Other),
            NosniffOutcome::Allow
        );
        assert_eq!(
            enforce(true, "application/octet-stream", Destination::Other),
            NosniffOutcome::Allow
        );
    }

    #[test]
    fn enforce_worker_collapses_to_script() {
        // The fetch destination "worker" collapses to Script and is blocked
        // under nosniff if the MIME is not a JS MIME type.
        assert!(matches!(
            enforce(
                true,
                "text/plain",
                Destination::from_fetch_destination("worker")
            ),
            NosniffOutcome::BlockScript(_)
        ));
    }
}
