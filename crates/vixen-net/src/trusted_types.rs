//! W3C Trusted Types — the `require-trusted-types-for` + `trusted-types`
//! CSP directive boundary the DOM injection-sink host hooks (`.innerHTML`,
//! `eval()`, `document.write()`, `script.src = …`, &c.) consult before
//! accepting a string (Phase 7 prep). Pure over the two CSP directives + a
//! `value_is_trusted` flag; the JS `TrustedTypePolicy` factory + the
//! `default` policy's `createHTML`/`createScript`/`createScriptURL`
//! sanitiser invocation stay in the host hook.
//!
//! What lives here:
//! - [`TrustedTypeKind`] — the three Trusted\* value kinds (`TrustedHTML` /
//!   `TrustedScript` / `TrustedScriptURL`).
//! - [`AllowedNames`] — the `trusted-types` directive's policy-name set
//!   (`None` / `Explicit(list)` / `Wildcard`).
//! - [`TrustedTypesPolicyNames`] — the parsed `trusted-types` directive:
//!   the allowed names + the `allow-duplicates` flag.
//! - [`RequireFor`] — the parsed `require-trusted-types-for` directive (the
//!   `'script'` sink-group flag — the only group in v1, covering every TT
//!   sink).
//! - [`parse_trusted_types`] / [`parse_require_trusted_types_for`] — the
//!   two directive parsers.
//! - [`policy_creation_allowed`] — the § 3.2.3 `trustedTypes.createPolicy`
//!   gate (the allowed-names match + the duplicate-name block).
//! - [`evaluate_sink`] — the § 3.3.5 injection-sink decision: a Trusted\*
//!   value ⇒ `Allow`; a string at a TT-requiring sink with a `default`
//!   policy ⇒ `ApplyDefaultPolicy`; a string at a TT-requiring sink with
//!   no default ⇒ `Block`; a string at a non-TT-requiring sink ⇒ `Allow`.
//!
//! What does *not* live here:
//! - The `TrustedTypePolicy` factory + the `createHTML`/`createScript`/
//!   `createScriptURL` sanitiser functions — the host hook's JS surface;
//!   this module is the pure policy boundary.
//! - The `default` policy's actual sanitisation — the host hook runs the
//!   author-supplied `default` policy; [`evaluate_sink`] only decides
//!   whether to invoke it ([`TrustedTypesOutcome::ApplyDefaultPolicy`]).
//! - The CSP directive-splitting (the host hook splits the `Content-
//!   Security-Policy` header into directives + passes this module the two
//!   TT directive values).
//! - The reporting API (`require-trusted-types-for` violations + the
//!   `trusted-types` violation reports) — the host hook's reporting surface.
//!
//! ## The sink decision
//!
//! ```text
//! value is a Trusted* object                  ⇒ Allow
//! sink is not TT-required (no require-for)    ⇒ Allow (the string is used)
//! TT-required + a `default` policy present    ⇒ ApplyDefaultPolicy
//! TT-required + no `default` policy           ⇒ Block (a TypeError)
//! ```
//!
//! Reference: <https://w3.org/TR/trusted-types/>,
//! CSP integration <https://w3.org/TR/CSP3/#trusted-types>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// TrustedTypeKind + AllowedNames
// ---------------------------------------------------------------------------

/// The three Trusted\* value kinds (W3C TT § 2.2). Each injection sink
/// accepts one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustedTypeKind {
    /// `TrustedHTML` — `.innerHTML` / `.outerHTML` / `insertAdjacentHTML` /
    /// `document.write`.
    Html,
    /// `TrustedScript` — `eval()` / `setTimeout(string)` / the inline event
    /// handlers.
    Script,
    /// `TrustedScriptURL` — `script.src` / `Worker`'s script URL.
    ScriptUrl,
}

impl TrustedTypeKind {
    /// The serialised kind name (lowercase).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Script => "script",
            Self::ScriptUrl => "script-url",
        }
    }
}

/// The `trusted-types` directive's allowed policy-name set (W3C TT § 3.4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowedNames {
    /// `'none'` (or an empty directive) — no policies may be created.
    None,
    /// An explicit policy-name list — only those names may be created.
    Explicit(Vec<String>),
    /// `*` (the wildcard) — any policy name may be created.
    Wildcard,
}

/// The parsed `trusted-types` CSP directive: the allowed policy-name set +
/// the `allow-duplicates` flag (W3C TT § 3.4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedTypesPolicyNames {
    /// The allowed policy-name set.
    pub allowed: AllowedNames,
    /// `true` iff the `allow-duplicates` keyword was present (a policy name
    /// may be created more than once; otherwise a duplicate name is a
    /// `TypeError` per § 3.2.3).
    pub allow_duplicates: bool,
}

/// The parsed `require-trusted-types-for` CSP directive (W3C TT § 3.4.4).
/// The `'script'` sink-group is the only one in v1; it covers every TT
/// injection sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RequireFor {
    /// `true` iff `require-trusted-types-for 'script'` is present (every
    /// TT sink requires a Trusted\* value or the default policy).
    pub script: bool,
}

// ---------------------------------------------------------------------------
// parse_trusted_types + parse_require_trusted_types_for
// ---------------------------------------------------------------------------

/// Parse the `trusted-types` CSP directive value (§ 3.4.3). A space-
/// separated list of policy names + the `allow-duplicates` keyword + the
/// `*` wildcard. `'none'` ⇒ no policies allowed (and wins over a
/// non-empty list per the § 3.4.3 "no other source-expression" rule).
/// Names are case-sensitive (policy names are identifiers).
pub fn parse_trusted_types(value: &str) -> TrustedTypesPolicyNames {
    let mut names: Vec<String> = vec![];
    let mut allow_duplicates = false;
    let mut wildcard = false;
    let mut has_none = false;
    for tok in value.split_ascii_whitespace() {
        match tok {
            "'none'" => has_none = true,
            "allow-duplicates" => allow_duplicates = true,
            "*" => wildcard = true,
            _ => names.push(tok.to_string()),
        }
    }
    let allowed = if has_none {
        AllowedNames::None
    } else if wildcard {
        AllowedNames::Wildcard
    } else if names.is_empty() {
        AllowedNames::None
    } else {
        AllowedNames::Explicit(names)
    };
    TrustedTypesPolicyNames {
        allowed,
        allow_duplicates,
    }
}

/// Parse the `require-trusted-types-for` CSP directive value (§ 3.4.4).
/// The `'script'` sink-group is the only one in v1; `script` is set iff it
/// is present (the keyword is matched case-insensitively + the optional
/// surrounding single-quotes are stripped).
pub fn parse_require_trusted_types_for(value: &str) -> RequireFor {
    let mut script = false;
    for tok in value.split_ascii_whitespace() {
        let bare = tok.trim_matches('\'');
        if bare.eq_ignore_ascii_case("script") {
            script = true;
        }
    }
    RequireFor { script }
}

// ---------------------------------------------------------------------------
// policy_creation_allowed + evaluate_sink
// ---------------------------------------------------------------------------

/// The § 3.2.3 `trustedTypes.createPolicy(name)` gate: `true` iff a policy
/// named `name` may be created given the allowed-name set + the
/// `allow-duplicates` flag. `existing_count` is the number of policies
/// already created with that name (a duplicate is blocked unless
/// `allow-duplicates` was set). An empty name is always rejected.
pub fn policy_creation_allowed(
    names: &TrustedTypesPolicyNames,
    name: &str,
    existing_count: usize,
) -> bool {
    if name.is_empty() {
        return false;
    }
    let name_ok = match &names.allowed {
        AllowedNames::None => false,
        AllowedNames::Wildcard => true,
        AllowedNames::Explicit(list) => list.iter().any(|n| n == name),
    };
    if !name_ok {
        return false;
    }
    if existing_count > 0 && !names.allow_duplicates {
        return false;
    }
    true
}

/// The injection-sink decision (§ 3.3.5). `value_is_trusted` is `true` when
/// the value is a `TrustedHTML` / `TrustedScript` / `TrustedScriptURL`
/// object (the host hook's type tag); `has_default_policy` is `true` when a
/// policy named `default` has been created (the § 3.3.5.4 default-policy
/// fallback).
pub fn evaluate_sink(
    require: &RequireFor,
    has_default_policy: bool,
    value_is_trusted: bool,
) -> TrustedTypesOutcome {
    if value_is_trusted {
        return TrustedTypesOutcome::Allow;
    }
    if !require.script {
        // The sink is not TT-required — the string is used as-is.
        return TrustedTypesOutcome::Allow;
    }
    // TT-required + a string value.
    if has_default_policy {
        TrustedTypesOutcome::ApplyDefaultPolicy
    } else {
        TrustedTypesOutcome::Block
    }
}

/// The injection-sink decision outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustedTypesOutcome {
    /// The value is accepted (it's a Trusted\* object, or the sink is not
    /// TT-required).
    Allow,
    /// The value is blocked (a string at a TT-requiring sink with no
    /// `default` policy — the host hook throws a `TypeError`).
    Block,
    /// The `default` policy is invoked to convert the string into a
    /// Trusted\* value (the § 3.3.5.4 fallback). The host hook runs the
    /// author-supplied `default` policy's `createHTML`/`createScript`/
    /// `createScriptURL`.
    ApplyDefaultPolicy,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_trusted_types -----------------------------------------

    #[test]
    fn parse_trusted_types_explicit_list() {
        let p = parse_trusted_types("one two three");
        assert_eq!(
            p.allowed,
            AllowedNames::Explicit(vec!["one".into(), "two".into(), "three".into()])
        );
        assert!(!p.allow_duplicates);
    }

    #[test]
    fn parse_trusted_types_wildcard() {
        let p = parse_trusted_types("*");
        assert_eq!(p.allowed, AllowedNames::Wildcard);
    }

    #[test]
    fn parse_trusted_types_none() {
        assert_eq!(parse_trusted_types("'none'").allowed, AllowedNames::None);
        assert_eq!(
            parse_trusted_types("").allowed,
            AllowedNames::None,
            "empty ⇒ None"
        );
    }

    #[test]
    fn parse_trusted_types_none_wins_over_list() {
        // `'none'` plus other tokens ⇒ the § 3.4.3 "no other source-
        // expression" rule ⇒ None.
        let p = parse_trusted_types("'none' foo");
        assert_eq!(p.allowed, AllowedNames::None);
    }

    #[test]
    fn parse_trusted_types_allow_duplicates() {
        let p = parse_trusted_types("foo allow-duplicates");
        assert!(p.allow_duplicates);
        assert_eq!(p.allowed, AllowedNames::Explicit(vec!["foo".into()]));
    }

    #[test]
    fn parse_trusted_types_names_are_case_sensitive() {
        let p = parse_trusted_types("MyPolicy");
        assert_eq!(p.allowed, AllowedNames::Explicit(vec!["MyPolicy".into()]));
    }

    // --- parse_require_trusted_types_for -----------------------------

    #[test]
    fn parse_require_for_script() {
        assert!(parse_require_trusted_types_for("'script'").script);
        assert!(
            parse_require_trusted_types_for("script").script,
            "unquoted tolerated"
        );
        assert!(
            parse_require_trusted_types_for("'SCRIPT'").script,
            "case-insensitive"
        );
    }

    #[test]
    fn parse_require_for_absent() {
        assert!(!parse_require_trusted_types_for("").script);
        assert!(!parse_require_trusted_types_for("other").script);
    }

    // --- policy_creation_allowed -------------------------------------

    #[test]
    fn create_policy_explicit_list_allows_listed_name() {
        let p = parse_trusted_types("foo bar");
        assert!(policy_creation_allowed(&p, "foo", 0));
        assert!(
            !policy_creation_allowed(&p, "baz", 0),
            "unlisted name rejected"
        );
    }

    #[test]
    fn create_policy_wildcard_allows_any() {
        let p = parse_trusted_types("*");
        assert!(policy_creation_allowed(&p, "anything", 0));
    }

    #[test]
    fn create_policy_none_rejects_all() {
        let p = parse_trusted_types("'none'");
        assert!(!policy_creation_allowed(&p, "foo", 0));
    }

    #[test]
    fn create_policy_duplicate_blocked_without_allow_duplicates() {
        let p = parse_trusted_types("foo");
        assert!(policy_creation_allowed(&p, "foo", 0));
        assert!(!policy_creation_allowed(&p, "foo", 1), "duplicate rejected");
    }

    #[test]
    fn create_policy_duplicate_allowed_with_allow_duplicates() {
        let p = parse_trusted_types("foo allow-duplicates");
        assert!(policy_creation_allowed(&p, "foo", 1));
        assert!(policy_creation_allowed(&p, "foo", 5));
    }

    #[test]
    fn create_policy_empty_name_rejected() {
        let p = parse_trusted_types("*");
        assert!(!policy_creation_allowed(&p, "", 0));
    }

    // --- evaluate_sink -----------------------------------------------

    #[test]
    fn trusted_value_always_allowed() {
        let require = RequireFor { script: true };
        assert_eq!(
            evaluate_sink(&require, false, true),
            TrustedTypesOutcome::Allow
        );
    }

    #[test]
    fn string_at_non_required_sink_allowed() {
        let require = RequireFor { script: false };
        assert_eq!(
            evaluate_sink(&require, false, false),
            TrustedTypesOutcome::Allow,
            "no require-for ⇒ string used as-is"
        );
    }

    #[test]
    fn string_at_required_sink_without_default_blocked() {
        let require = RequireFor { script: true };
        assert_eq!(
            evaluate_sink(&require, false, false),
            TrustedTypesOutcome::Block
        );
    }

    #[test]
    fn string_at_required_sink_with_default_applies_default() {
        let require = RequireFor { script: true };
        assert_eq!(
            evaluate_sink(&require, true, false),
            TrustedTypesOutcome::ApplyDefaultPolicy
        );
    }

    #[test]
    fn default_policy_does_not_override_trusted_value() {
        let require = RequireFor { script: true };
        // A Trusted* value is allowed directly even when a default policy
        // exists (no sanitiser round-trip needed).
        assert_eq!(
            evaluate_sink(&require, true, true),
            TrustedTypesOutcome::Allow
        );
    }

    // --- TrustedTypeKind ---------------------------------------------

    #[test]
    fn kind_as_str() {
        assert_eq!(TrustedTypeKind::Html.as_str(), "html");
        assert_eq!(TrustedTypeKind::Script.as_str(), "script");
        assert_eq!(TrustedTypeKind::ScriptUrl.as_str(), "script-url");
    }
}
