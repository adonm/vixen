//! Permissions Policy 1 § 3 — pure logic for the `Permissions-Policy` HTTP
//! response header and the `<iframe allow>` attribute. Phase 7 security prep;
//! the complement to [`crate::sec_fetch`] (which describes *requests*) and
//! [`crate::sandboxing`] (which describes *iframe capability revocation*):
//! this module describes *feature gating* — the per-origin allowlists that
//! gate `geolocation`, `camera`, `microphone`, `autoplay`, `fullscreen`, …
//!
//! What lives here:
//! - [`Allowlist`] — the per-feature source list (`*` / `self` / `src` / an
//!   explicit origin set / the empty `()` deny-all).
//! - [`PermissionsPolicy`] — the parsed header (a `feature → Allowlist` map).
//! - [`parse_permissions_policy`] — the § 3.3 structured-header parser
//!   (`Permissions-Policy: geolocation=(self "https://partner.test"), camera=()`).
//! - [`parse_allow_attribute`] — the § 5.2 `<iframe allow>` attribute parser
//!   (`allow="geolocation; camera 'src'"`).
//! - [`PermissionsPolicy::allows`] — the § 4 evaluation: is `feature` enabled
//!   for `target` given the policy + the embedder origin?
//!
//! What does *not` live here:
//! - The actual feature enforcement (the host-hook layer consults [`allows`]
//!   before exposing `navigator.geolocation` etc.; Phase 6 wires it).
//! - The full Permissions Policy feature registry (v1.0 treats feature names
//!   as opaque strings; the canonical feature-name set lives in the WHATWG
//!   registry and grows independently).
//! - The `allowlist` *inheritance* rules for nested frames (a § 4.3 detail
//!   that composes [`PermissionsPolicy::allows`] across the frame chain;
//!   lands with the navigation pipeline).
//!
//! ## Grammar (§ 3.3 structured-field form)
//!
//! ```text
//! Permissions-Policy = sf-item ( "," sf-item )*
//! sf-item             = token [ "=" ( "*" / "(" source-list ")" ) ]
//! source              = "self" | "src" | origin-string
//! ```
//!
//! Reference: <https://www.w3.org/TR/permissions-policy-1/>.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use crate::origin::Origin;

// ---------------------------------------------------------------------------
// Allowlist — the per-feature source list
// ---------------------------------------------------------------------------

/// A feature's allowlist (Permissions Policy 1 § 3.3). The four § 3.3 forms:
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Allowlist {
    /// `feature=*` — the feature is allowed for every origin.
    Everyone,
    /// `feature=self` — only the embedder origin. The single-token `self`
    /// form (`camera=self`) desugars to this.
    Self_,
    /// `feature=src` — only the frame's own origin (the § 3.3 `src` keyword;
    /// only meaningful in the `<iframe allow>` attribute context).
    Src,
    /// `feature=(self "https://a.test" "https://b.test")` — the feature is
    /// allowed for the embedder origin and the listed origins.
    Origins(Vec<Origin>),
    /// `feature=()` — the feature is disabled for every origin (deny-all).
    /// This is distinct from omitting the feature, which leaves it at the
    /// § 3.3 default (allowed for self).
    None,
}

impl Allowlist {
    /// Evaluate the allowlist against a target origin (the origin requesting
    /// the feature) given the embedder origin (the origin that set the
    /// policy). The § 4 decision:
    /// - [`Allowlist::Everyone`] ⇒ always allowed.
    /// - [`Allowlist::None`] ⇒ always denied.
    /// - [`Allowlist::Self_`] ⇒ allowed iff target == embedder.
    /// - [`Allowlist::Src`] ⇒ allowed iff target == the frame's own src
    ///   origin (passed in as `embedder` here for the top-level case; the
    ///   frame-chain variant composes across nested embedders).
    /// - [`Allowlist::Origins`] ⇒ allowed iff target == embedder or target is
    ///   in the explicit origin list.
    pub fn allows(&self, embedder: &Origin, target: &Origin) -> bool {
        match self {
            Allowlist::Everyone => true,
            Allowlist::None => false,
            Allowlist::Self_ | Allowlist::Src => origin_eq(embedder, target),
            Allowlist::Origins(list) => {
                origin_eq(embedder, target) || list.iter().any(|o| origin_eq(o, target))
            }
        }
    }
}

/// Origin equality ignoring the `opaque` flag's interaction with tuple origins
/// (opaque origins never equal tuple origins).
fn origin_eq(a: &Origin, b: &Origin) -> bool {
    if a.is_opaque() || b.is_opaque() {
        return false;
    }
    a.scheme() == b.scheme() && a.host() == b.host() && a.port() == b.port()
}

// ---------------------------------------------------------------------------
// PermissionsPolicy — the parsed header / allow attribute
// ---------------------------------------------------------------------------

/// The parsed Permissions Policy (§ 3.3): a `feature → Allowlist` map.
/// Features not present in the map are at the § 3.3 default (allowed for the
/// embedder origin); use [`PermissionsPolicy::allows`] to apply that default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionsPolicy {
    features: BTreeMap<String, Allowlist>,
}

impl PermissionsPolicy {
    /// An empty policy: every feature is at its § 3.3 default (allowed for
    /// self).
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the allowlist for a feature, if the policy addressed it.
    pub fn get(&self, feature: &str) -> Option<&Allowlist> {
        self.features.get(feature)
    }

    /// The number of features this policy addresses.
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Is the policy empty?
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// The § 4 evaluation: is `feature` allowed for `target` given the policy
    /// and the embedder origin? Features not addressed by the policy default
    /// to the embedder origin only (the § 3.3 default).
    pub fn allows(&self, feature: &str, embedder: &Origin, target: &Origin) -> bool {
        match self.features.get(feature) {
            Some(list) => list.allows(embedder, target),
            // § 3.3 default: feature not present ⇒ allowed for self only.
            None => origin_eq(embedder, target),
        }
    }

    /// Insert / override a feature's allowlist (used by the host-hook layer
    /// to merge the response header with `<iframe allow>` inheritance).
    pub fn set(&mut self, feature: impl Into<String>, list: Allowlist) {
        self.features.insert(feature.into(), list);
    }
}

// ---------------------------------------------------------------------------
// Structured-header parser (§ 3.3)
// ---------------------------------------------------------------------------

/// Parse the `Permissions-Policy` response header (§ 3.3 structured-field
/// form). Tolerant of whitespace; malformed items are skipped (the spec's
/// "parse error ⇒ item dropped" rule). Returns the feature map (empty if the
/// whole header is unparseable).
///
/// ```
/// # use vixen_net::permissions_policy::{parse_permissions_policy, Allowlist};
/// let p = parse_permissions_policy("geolocation=(self \"https://partner.test\"), camera=()");
/// assert!(matches!(p.get("camera"), Some(Allowlist::None)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn parse_permissions_policy(header: &str) -> PermissionsPolicy {
    let mut policy = PermissionsPolicy::new();
    for item in split_top_level(header, ',') {
        if let Some((feature, list)) = parse_item(&item) {
            policy.set(feature, list);
        }
    }
    policy
}

/// Parse the `<iframe allow>` attribute (§ 5.2). The attribute is
/// semicolon-separated (not comma); each item is `feature` or
/// `feature = ( source* )` or `feature = "origin"`. The single-source form
/// (no parens) is also accepted: `allow="camera 'self'"` ≡ `camera=(self)`.
pub fn parse_allow_attribute(attr: &str) -> PermissionsPolicy {
    let mut policy = PermissionsPolicy::new();
    for item in split_top_level(attr, ';') {
        if let Some((feature, list)) = parse_item(&item) {
            policy.set(feature, list);
        }
    }
    policy
}

/// Parse a single `feature` or `feature = <rhs>` item. The bare `feature`
/// form (no `=`) means `feature=*` per § 5.2 (the iframe-allow shorthand for
/// "enable for everyone"); in the response header it's "enable for self".
fn parse_item(item: &str) -> Option<(String, Allowlist)> {
    let item = item.trim();
    if item.is_empty() {
        return None;
    }
    let (feature, rhs) = match item.split_once('=') {
        Some((f, r)) => (f.trim().to_owned(), r.trim()),
        None => {
            // No `=`. Two sub-cases per § 5.2:
            //  - bare feature name (`geolocation`) ⇒ Everyone (the shorthand).
            //  - `feature 'source'` (`camera 'self'`) ⇒ feature with the bare
            //    space-separated source list as the RHS. Split on the first
            //    run of whitespace.
            match item.split_once(char::is_whitespace) {
                Some((f, rest)) => (f.trim().to_owned(), rest.trim()),
                None => (item.to_owned(), ""),
            }
        }
    };
    if feature.is_empty() {
        return None;
    }
    // Feature names are ASCII tokens; reject anything with whitespace inside.
    if feature.chars().any(|c| c.is_ascii_whitespace()) {
        return None;
    }
    let list = if rhs.is_empty() {
        // Bare form. In both contexts this means "enable for everyone" per
        // § 5.2; the response-header default (self) is only reached via an
        // explicit `(self)`.
        Allowlist::Everyone
    } else {
        parse_rhs(rhs)?
    };
    Some((feature, list))
}

/// Parse the right-hand side of `feature = rhs`: `*`, `( sources )`, or a
/// single bare keyword/origin.
fn parse_rhs(rhs: &str) -> Option<Allowlist> {
    let r = rhs.trim();
    match r {
        "*" => return Some(Allowlist::Everyone),
        "()" => return Some(Allowlist::None),
        _ => {}
    }
    if let Some(inner) = strip_parens(r) {
        return Some(parse_source_list(inner));
    }
    // Bare single source: `feature=self` / `feature="https://a.test"`.
    Some(parse_source_list(r))
}

/// Parse a space-separated source list (`self src "https://a.test"`).
fn parse_source_list(inner: &str) -> Allowlist {
    let mut origins: Vec<Origin> = Vec::new();
    let mut has_self = false;
    let mut has_src = false;
    for token in inner.split_whitespace() {
        // Both the structured-header form (`"https://a.test"`) and the iframe
        // shorthand (`'self'`) quote tokens; strip either quote style.
        let token = token.trim_matches(|c| c == '"' || c == '\'');
        match token {
            "self" => has_self = true,
            "src" => has_src = true,
            origin_str => {
                if let Ok(o) = parse_origin_token(origin_str) {
                    origins.push(o);
                }
                // Unparseable origin tokens are silently dropped per the
                // § 3.3 "invalid item ⇒ drop" rule.
            }
        }
    }
    // De-sugar: `self` alone ⇒ Self_; `src` alone ⇒ Src; a mix folds `self`
    // into the origin list (the embedder is always implicitly in an Origins
    // list that also has `self`).
    let only_self = has_self && !has_src && origins.is_empty();
    let only_src = !has_self && has_src && origins.is_empty();
    let only_origins = !has_self && !has_src && !origins.is_empty();
    if only_self {
        return Allowlist::Self_;
    }
    if only_src {
        return Allowlist::Src;
    }
    if only_origins {
        return Allowlist::Origins(origins);
    }
    // Mixed: fold `self` into the origins list (the embedder origin will be
    // compared against target too). `src` is preserved as an extra embedder
    // the frame-chain layer would substitute; at this layer it's dropped
    // (§ 4.3 composes it at the frame boundary).
    if origins.is_empty() && (has_self || has_src) {
        // Only keywords, no origins: keep the keyword form.
        if has_self {
            return Allowlist::Self_;
        }
        return Allowlist::Src;
    }
    Allowlist::Origins(origins)
}

/// Parse an origin string from a quoted permission-policy source. Accepts the
/// `scheme://host[:port]` form; rejects anything malformed.
fn parse_origin_token(s: &str) -> Result<Origin, ()> {
    let url = url::Url::parse(s).map_err(|_| ())?;
    let origin = Origin::from_url(&url);
    if origin.is_opaque() {
        return Err(());
    }
    Ok(origin)
}

/// Strip a balanced `(...)` wrapper, returning the inner content. Returns
/// `None` if `s` doesn't start with `(` / end with `)` or the parens are
/// unbalanced.
fn strip_parens(s: &str) -> Option<&str> {
    let s = s.trim();
    if !s.starts_with('(') || !s.ends_with(')') {
        return None;
    }
    // Verify balance (no nested parens expected in § 3.3, but be defensive).
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && i != s.len() - 1 {
                    return None; // closing paren before end ⇒ extra content.
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    Some(&s[1..s.len() - 1])
}

/// Split on a top-level separator character, respecting `(...)` nesting and
/// quoted strings.
fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut in_quote = false;
    let mut current = String::new();
    for c in s.chars() {
        match c {
            '"' if !in_quote => {
                in_quote = true;
                current.push(c);
            }
            '"' if in_quote => {
                in_quote = false;
                current.push(c);
            }
            '(' if !in_quote => {
                depth += 1;
                current.push(c);
            }
            ')' if !in_quote && depth > 0 => {
                depth -= 1;
                current.push(c);
            }
            c if c == sep && depth == 0 && !in_quote => {
                out.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn origin(u: &str) -> Origin {
        Origin::from_url(&Url::parse(u).unwrap())
    }

    fn embedder() -> Origin {
        origin("https://embedder.test/")
    }

    // --- Structured-header parsing (§ 3.3) ------------------------------

    #[test]
    fn star_allows_everyone() {
        let p = parse_permissions_policy("geolocation=*");
        assert_eq!(p.get("geolocation"), Some(&Allowlist::Everyone));
        assert!(p.allows("geolocation", &embedder(), &origin("https://evil.test/")));
    }

    #[test]
    fn empty_parens_denies_all() {
        let p = parse_permissions_policy("camera=()");
        assert_eq!(p.get("camera"), Some(&Allowlist::None));
        assert!(!p.allows("camera", &embedder(), &embedder()));
        assert!(!p.allows("camera", &embedder(), &origin("https://evil.test/")));
    }

    #[test]
    fn self_only_allows_embedder_only() {
        let p = parse_permissions_policy("microphone=(self)");
        assert_eq!(p.get("microphone"), Some(&Allowlist::Self_));
        assert!(p.allows("microphone", &embedder(), &embedder()));
        assert!(!p.allows("microphone", &embedder(), &origin("https://evil.test/")));
    }

    #[test]
    fn origins_list_includes_embedder_and_listed() {
        let p = parse_permissions_policy("geolocation=(self \"https://partner.test\")");
        assert!(p.allows("geolocation", &embedder(), &embedder()));
        assert!(p.allows("geolocation", &embedder(), &origin("https://partner.test/")));
        assert!(!p.allows("geolocation", &embedder(), &origin("https://evil.test/")));
    }

    #[test]
    fn multiple_features_in_one_header() {
        let p = parse_permissions_policy(
            "geolocation=(self), camera=(), microphone=*, fullscreen=(\"https://app.test\")",
        );
        assert_eq!(p.len(), 4);
        assert!(matches!(p.get("camera"), Some(Allowlist::None)));
        assert!(matches!(p.get("microphone"), Some(Allowlist::Everyone)));
        assert!(matches!(p.get("fullscreen"), Some(Allowlist::Origins(_))));
    }

    #[test]
    fn feature_not_in_policy_defaults_to_self() {
        let p = parse_permissions_policy("camera=()");
        // `geolocation` not addressed ⇒ default self.
        assert!(p.allows("geolocation", &embedder(), &embedder()));
        assert!(!p.allows("geolocation", &embedder(), &origin("https://evil.test/")));
    }

    #[test]
    fn empty_policy_allows_self_for_everything() {
        let p = PermissionsPolicy::new();
        assert!(p.allows("anything", &embedder(), &embedder()));
        assert!(!p.allows("anything", &embedder(), &origin("https://evil.test/")));
    }

    // --- Malformed / tolerant parsing ----------------------------------

    #[test]
    fn malformed_item_dropped() {
        let p = parse_permissions_policy("camera=(), =*, geolocation=(self)");
        // The middle item (`=*`, no feature name) is dropped; the two valid
        // ones survive.
        assert_eq!(p.len(), 2);
        assert!(p.get("camera").is_some());
        assert!(p.get("geolocation").is_some());
    }

    #[test]
    fn unparseable_origin_token_dropped() {
        let p = parse_permissions_policy("geolocation=(self \"not a url\")");
        // The unparseable origin is dropped; only `self` survives.
        assert!(matches!(p.get("geolocation"), Some(Allowlist::Self_)));
    }

    #[test]
    fn whitespace_tolerant() {
        let p = parse_permissions_policy("  camera = ( self ,  \"https://a.test\" )  ");
        assert!(matches!(p.get("camera"), Some(Allowlist::Origins(_))));
    }

    #[test]
    fn quoted_and_unquoted_self_equivalent() {
        let a = parse_permissions_policy("camera=(self)");
        let b = parse_permissions_policy("camera=(\"self\")");
        assert_eq!(a.get("camera"), b.get("camera"));
    }

    // --- <iframe allow> attribute --------------------------------------

    #[test]
    fn allow_attribute_semicolon_separated() {
        let p = parse_allow_attribute("geolocation; camera 'src'; microphone=(self)");
        assert_eq!(p.len(), 3);
        // Bare `geolocation` ⇒ Everyone per § 5.2 shorthand.
        assert!(matches!(p.get("geolocation"), Some(Allowlist::Everyone)));
        assert!(matches!(p.get("microphone"), Some(Allowlist::Self_)));
    }

    #[test]
    fn allow_attribute_single_source_shorthand() {
        // `camera 'self'` ≡ `camera=(self)`.
        let p = parse_allow_attribute("camera 'self'");
        assert!(matches!(p.get("camera"), Some(Allowlist::Self_)));
    }

    // --- Allowlist evaluation edge cases --------------------------------

    #[test]
    fn origins_list_with_explicit_embedder_origin() {
        // `(https://embedder.test)` without `self` still allows the embedder
        // because the embedder origin matches a listed origin.
        let p = parse_permissions_policy("geolocation=(\"https://embedder.test\")");
        assert!(p.allows("geolocation", &embedder(), &embedder()));
    }

    #[test]
    fn opaque_embedder_never_allowed_via_self() {
        let p = parse_permissions_policy("camera=(self)");
        let opaque = Origin::opaque();
        assert!(!p.allows("camera", &opaque, &opaque));
    }

    #[test]
    fn port_distinction_honoured() {
        let p = parse_permissions_policy("camera=(self)");
        let e = origin("https://embedder.test:443/");
        let t = origin("https://embedder.test:8443/");
        assert!(!p.allows("camera", &e, &t));
    }

    // --- Internal helpers ----------------------------------------------

    #[test]
    fn strip_parens_balanced() {
        assert_eq!(strip_parens("(self)"), Some("self"));
        assert_eq!(strip_parens("(self src)"), Some("self src"));
        assert_eq!(strip_parens("self"), None);
        assert_eq!(strip_parens("(unbalanced"), None);
    }

    #[test]
    fn split_respects_parens_and_quotes() {
        let parts = split_top_level("a=(1, 2), b=\"x,y\"", ',');
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("a=(1, 2)"));
    }

    #[test]
    fn registrable_origins_list_round_trip() {
        let p = parse_permissions_policy(
            "fullscreen=(self \"https://a.test\" \"https://b.test:8080\")",
        );
        let list = match p.get("fullscreen") {
            Some(Allowlist::Origins(o)) => o,
            other => panic!("expected Origins, got {other:?}"),
        };
        assert_eq!(list.len(), 2);
        assert!(p.allows("fullscreen", &embedder(), &origin("https://b.test:8080/")));
    }
}
