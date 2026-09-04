//! HTML `DOMTokenList` (`element.classList`, `<link>.relList`, `<iframe>`) —
//! Phase 6 DOM host-bindings prep (pure logic called out by `docs/PLAN.md`
//! "Testing strategy" as a Rust-unit-test surface). Implements the
//! WHATWG HTML § 4.6.4 `DOMTokenList` algorithm + the § 2.7.3 "common parser
//! idioms" (`split on ASCII whitespace`, drop duplicates, preserve order, the
//! `validate a token` rules), so the Phase 6 host-hook layer has one source
//! of truth for every `DOMTokenList`-bearing attribute surface.
//!
//! What lives here:
//! - [`DomTokenList`] — the ordered, de-duplicated token vector + the full
//!   mutating surface (`add`/`remove`/`toggle`/`replace`/`contains`/`item`).
//! - [`parse_token_set`] — the WHATWG § 2.7.3 "ordered set of unique
//!   space-separated tokens" parser.
//! - [`serialize_token_set`] — the inverse (space-joined serializer).
//! - [`validate_token`] — the WHATWG § 2.7.3 `validate a token` predicate
//!   (reject empty + reject any ASCII whitespace).
//!
//! What does *not* live here:
//! - The live DOM reflection (the host-hook layer owns the bidirectional
//!   attribute ↔ `DOMStringMap` reflection; this module is the pure value it
//!   reduces to). Mutating operations build a fresh [`DomTokenList`]; the
//!   host-hook layer writes its [`DomTokenList::serialize`] back to the
//!   attribute and dispatches a `MutationObserver` record.
//! - The `relList.supportedTokens()` set (only `relList` carries one; the
//!   generic `DOMTokenList` does not). For `relList` the host-hook layer
//!   consults [`SupportedTokens`] at the `supports()` boundary.
//! - `MutationObserver` records (the DOM event layer's job).
//!
//! ## WHATWG § 2.7.3 parsing rules
//!
//! The "ordered set of tokens" parser:
//! 1. Split on every ASCII-whitespace code point (space, tab, LF, CR, FF).
//! 2. Drop empty strings (consecutive whitespace doesn't produce empties).
//! 3. Drop duplicates, preserving first-occurrence order.
//!
//! The "validate a token" predicate:
//! 1. Empty token ⇒ `SyntaxError`.
//! 2. Token containing any ASCII-whitespace code point ⇒ `InvalidCharacterError`.
//!
//! `add` / `remove` / `toggle` / `replace` / `contains` all `validate` first;
//! a single bad token aborts the whole call (WHATWG § 4.6.4 step "if any of
//! them fail validation, throw" — atomicity is the easy-to-get-wrong part).
//!
//! Reference: <https://html.spec.whatwg.org/multipage/common-dom-interfaces.html>
//! § 2.7.3 (parsing) + § 4.6.4 (`DOMTokenList` interface).

#![forbid(unsafe_code)]

/// The ASCII whitespace code points the WHATWG § 2.7.3 token-set parser
/// splits on. Matches the HTML "ASCII whitespace" definition (space, tab,
/// LF, CR, FF — *not* the Unicode White_Space set; this matters for author
/// CSS that uses NBSP as a separator, which the spec doesn't break on).
const ASCII_WHITESPACE: &[char] = &[' ', '\t', '\n', '\r', '\x0c'];

// ---------------------------------------------------------------------------
// Parser + serializer + validator
// ---------------------------------------------------------------------------

/// Parse the WHATWG § 2.7.3 "ordered set of unique space-separated tokens"
/// from an attribute value (or any string). Whitespace runs collapse; order
/// is first-occurrence; duplicates are dropped.
///
/// ```
/// # use vixen_engine::class_list::parse_token_set;
/// assert_eq!(parse_token_set("a b a c"), vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
/// assert_eq!(parse_token_set("  \ta\rb\n c "), vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
/// assert!(parse_token_set("").is_empty());
/// ```
pub fn parse_token_set(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in input.split(ASCII_WHITESPACE) {
        if token.is_empty() {
            continue;
        }
        if !out.iter().any(|t| t == token) {
            out.push(token.to_owned());
        }
    }
    out
}

/// Serialize an ordered token set back to the space-separated attribute form
/// (WHATWG § 2.7.3 "serialize" + § 4.6.4 `DOMTokenList.value`). Tokens are
/// joined by a single U+0020. Empty list ⇒ empty string.
pub fn serialize_token_set<I, S>(tokens: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut buf = String::new();
    for (i, t) in tokens.into_iter().enumerate() {
        if i > 0 {
            buf.push(' ');
        }
        buf.push_str(t.as_ref());
    }
    buf
}

/// The WHATWG § 2.7.3 `validate a token` predicate. Empty ⇒ `SyntaxError`;
/// ASCII-whitespace-bearing ⇒ `InvalidCharacterError`. Returns `Ok(())` for
/// any other string (case is preserved; tokens are case-sensitive).
pub fn validate_token(token: &str) -> Result<(), TokenListError> {
    if token.is_empty() {
        return Err(TokenListError::EmptyToken);
    }
    if token.contains(ASCII_WHITESPACE) {
        return Err(TokenListError::WhitespaceInToken {
            token: token.to_owned(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DomTokenList value
// ---------------------------------------------------------------------------

/// A WHATWG HTML § 4.6.4 `DOMTokenList` value. The ordered, de-duplicated
/// token vector backing `element.classList`, `<link>.relList`, etc. Mutating
/// operations are atomic per the spec ("validate all, then mutate all").
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DomTokenList {
    tokens: Vec<String>,
}

impl DomTokenList {
    /// Construct from an attribute value (the § 2.7.3 parser runs).
    pub fn parse(attribute_value: &str) -> Self {
        Self {
            tokens: parse_token_set(attribute_value),
        }
    }

    /// Construct from an already-parsed token set (no copy through the string
    /// form). Useful for the host-hook layer when it already has the token vec.
    pub fn from_tokens(tokens: Vec<String>) -> Self {
        // Re-run the de-dup + order-preserving pass defensively (the caller
        // might have duplicates).
        let mut seen: Vec<String> = Vec::with_capacity(tokens.len());
        for t in tokens {
            if !seen.contains(&t) {
                seen.push(t);
            }
        }
        Self { tokens: seen }
    }

    /// An empty list (`element.classList` on an element with no `class`
    /// attribute, or with `class=""`).
    pub fn empty() -> Self {
        Self { tokens: Vec::new() }
    }

    /// The `length` property.
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// `length === 0`.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// The `item(index)` property. Returns `None` for out-of-range (the JS
    /// surface returns `null` for those).
    pub fn item(&self, index: usize) -> Option<&str> {
        self.tokens.get(index).map(String::as_str)
    }

    /// The `contains(token)` property. Validates the token first (per the
    /// spec, `contains` throws on empty / whitespace-bearing input).
    pub fn contains(&self, token: &str) -> Result<bool, TokenListError> {
        validate_token(token)?;
        Ok(self.tokens.iter().any(|t| t == token))
    }

    /// The `add(...tokens)` method. Validates every token first (atomicity:
    /// any invalid token ⇒ no mutation ⇒ the error surfaces). Tokens already
    /// present are no-ops (matches spec — `add` is idempotent).
    pub fn add<I, S>(&mut self, tokens: I) -> Result<(), TokenListError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Materialise + validate first (WHATWG § 4.6.4: "If any of the
        // validated tokens fail, throw before mutating").
        let collected: Vec<String> = tokens
            .into_iter()
            .map(|t| {
                let s = t.as_ref();
                validate_token(s).map(|()| s.to_owned())
            })
            .collect::<Result<_, _>>()?;
        for t in collected {
            if !self.tokens.contains(&t) {
                self.tokens.push(t);
            }
        }
        Ok(())
    }

    /// The `remove(...tokens)` method. Validates every token first. Removing
    /// an absent token is a no-op (matches spec).
    pub fn remove<I, S>(&mut self, tokens: I) -> Result<(), TokenListError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let collected: Vec<String> = tokens
            .into_iter()
            .map(|t| {
                let s = t.as_ref();
                validate_token(s).map(|()| s.to_owned())
            })
            .collect::<Result<_, _>>()?;
        // Remove every matching token, preserving order of the survivors.
        self.tokens.retain(|t| !collected.contains(t));
        Ok(())
    }

    /// The `toggle(token, force?)` method. Returns `true` if the token is
    /// now present, `false` if absent.
    ///
    /// - `force == None` (the JS `toggle(token)` form): if present, remove
    ///   and return `false`; if absent, add and return `true`.
    /// - `force == Some(true)`: behave like `add(token)`, return `true`.
    /// - `force == Some(false)`: behave like `remove(token)`, return `false`.
    pub fn toggle(&mut self, token: &str, force: Option<bool>) -> Result<bool, TokenListError> {
        validate_token(token)?;
        let present = self.tokens.iter().any(|t| t == token);
        let after_add = match force {
            None => !present,
            Some(true) => true,
            Some(false) => false,
        };
        if after_add && !present {
            self.tokens.push(token.to_owned());
        } else if !after_add && present {
            self.tokens.retain(|t| t != token);
        }
        Ok(after_add)
    }

    /// The `replace(old, new)` method. Returns `true` if `old` was present
    /// (and got replaced in-place, preserving order); `false` if `old` was
    /// absent (the list is unchanged).
    pub fn replace(&mut self, old: &str, new: &str) -> Result<bool, TokenListError> {
        validate_token(old)?;
        validate_token(new)?;
        let Some(idx) = self.tokens.iter().position(|t| t == old) else {
            return Ok(false);
        };
        // WHATWG § 4.6.4 `replace`: if `new` is already present, the result
        // is the removal of `old` (no duplicate insertion). The returned
        // value is still `true` (old was found).
        if old == new {
            return Ok(true);
        }
        if let Some(other) = self.tokens.iter().position(|t| t == new) {
            // `new` already present ⇒ drop `old`; preserve `new`'s position.
            self.tokens.remove(idx);
            // `other` may have shifted if it was after `idx`.
            let _ = other; // documentation-only; no rebinding needed.
            return Ok(true);
        }
        self.tokens[idx] = new.to_owned();
        Ok(true)
    }

    /// Iterate over the tokens in document order. JS `for...of` / `entries()`
    /// / `forEach()` use this.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.tokens.iter().map(String::as_str)
    }

    /// The `value` / `toString()` property. The serializer round-trips back
    /// to the space-separated attribute form.
    pub fn serialize(&self) -> String {
        serialize_token_set(&self.tokens)
    }

    /// Borrow the underlying token vector (read-only). The host-hook layer
    /// uses this for snapshot extraction + the WPT `selector-match` check.
    pub fn as_vec(&self) -> &[String] {
        &self.tokens
    }
}

/// The optional "supported tokens" set some `DOMTokenList`s carry (only
/// `<link>.relList` per WHATWG). Used by `relList.supports(token)`. The
/// generic `DOMTokenList` has no supported-tokens set; only `relList` does,
/// and `add`/`replace` ignore it (they accept any token) — `supports` is the
/// only surface that consults it.
#[derive(Debug, Clone, Default)]
pub struct SupportedTokens {
    set: Vec<String>,
}

impl SupportedTokens {
    /// The link-types supported tokens (the WHATWG § 4.6.5 LinkTypes table).
    /// Construct from the spec table; the host-hook layer keeps the table in
    /// sync with `vixen-net::referrer_policy` / `mixed_content` etc.
    pub fn new<I, S>(tokens: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let set = tokens.into_iter().map(Into::into).collect();
        Self { set }
    }

    /// `relList.supports(token)` (WHATWG § 4.6.5 step 2). Case-insensitive
    /// comparison: link types are ASCII case-insensitive.
    pub fn supports(&self, token: &str) -> bool {
        let lower = token.to_ascii_lowercase();
        self.set.iter().any(|t| t.eq_ignore_ascii_case(&lower))
    }
}

/// Why a `DOMTokenList` operation failed. Maps 1:1 to the WHATWG § 4.6.4
/// `DOMException` types the JS surface throws.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenListError {
    /// `SyntaxError` — `add`/`remove`/`toggle`/`replace`/`contains` called
    /// with the empty string.
    #[error("token cannot be the empty string (SyntaxError)")]
    EmptyToken,
    /// `InvalidCharacterError` — token contained an ASCII-whitespace code
    /// point.
    #[error("token {token:?} contains ASCII whitespace (InvalidCharacterError)")]
    WhitespaceInToken { token: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_token_set -----------------------------------------------

    #[test]
    fn parse_simple_space_separated() {
        assert_eq!(
            parse_token_set("a b c"),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }

    #[test]
    fn parse_drops_duplicates_preserving_first_position() {
        assert_eq!(
            parse_token_set("a b a c b"),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }

    #[test]
    fn parse_collapses_whitespace_runs() {
        // Tab, CR, LF, FF are all separators.
        assert_eq!(
            parse_token_set("a\tb\nc\rd\x0ce"),
            vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned(),
                "e".to_owned()
            ]
        );
        assert_eq!(
            parse_token_set("  a   b  "),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    #[test]
    fn parse_empty_and_whitespace_only() {
        assert!(parse_token_set("").is_empty());
        assert!(parse_token_set("   \t\n  ").is_empty());
    }

    #[test]
    fn parse_case_sensitive() {
        // WHATWG: tokens are case-sensitive (class="Foo" ≠ class="foo").
        let v = parse_token_set("Foo foo FOO");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], "Foo");
        assert_eq!(v[1], "foo");
        assert_eq!(v[2], "FOO");
    }

    #[test]
    fn parse_keeps_non_ascii_whitespace_separator_chars() {
        // U+00A0 NBSP is NOT ASCII whitespace per WHATWG, so it's part of a
        // token (not a separator). Same for vertical tab? Actually FF is in
        // the list, but VT (\x0b) is not — keep the test on NBSP.
        let v = parse_token_set("a\u{00A0}b");
        assert_eq!(v, vec!["a\u{00A0}b".to_owned()]);
    }

    // --- serialize_token_set -------------------------------------------

    #[test]
    fn serialize_round_trips_parse() {
        for original in ["a b c", "  a   b  ", "x", ""] {
            let parsed = parse_token_set(original);
            let serialized = serialize_token_set(&parsed);
            let reparsed = parse_token_set(&serialized);
            assert_eq!(parsed, reparsed, "round trip {original:?}");
        }
    }

    #[test]
    fn serialize_empty_is_empty_string() {
        let empty: Vec<String> = Vec::new();
        assert_eq!(serialize_token_set(&empty), "");
    }

    #[test]
    fn serialize_single_token_no_trailing_space() {
        let v = vec!["only".to_owned()];
        assert_eq!(serialize_token_set(&v), "only");
    }

    // --- validate_token ------------------------------------------------

    #[test]
    fn validate_rejects_empty() {
        assert_eq!(validate_token(""), Err(TokenListError::EmptyToken));
    }

    #[test]
    fn validate_rejects_whitespace_inside() {
        for bad in ["a b", "a\tb", "a\nb", "a\rb", "a\x0cb"] {
            let r = validate_token(bad);
            assert!(
                matches!(r, Err(TokenListError::WhitespaceInToken { .. })),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn validate_accepts_normal_token() {
        assert!(validate_token("foo").is_ok());
        assert!(validate_token("Foo-Bar_123").is_ok());
        assert!(validate_token("a\u{00A0}b").is_ok()); // NBSP ok
    }

    // --- DomTokenList: read surface ------------------------------------

    #[test]
    fn parse_and_query() {
        let list = DomTokenList::parse("foo bar baz");
        assert_eq!(list.len(), 3);
        assert!(!list.is_empty());
        assert_eq!(list.item(0), Some("foo"));
        assert_eq!(list.item(2), Some("baz"));
        assert_eq!(list.item(3), None); // out of range
        assert!(list.contains("bar").unwrap());
        assert!(!list.contains("qux").unwrap());
    }

    #[test]
    fn contains_validates_first() {
        let list = DomTokenList::parse("foo");
        assert_eq!(list.contains(""), Err(TokenListError::EmptyToken));
        assert_eq!(
            list.contains("a b"),
            Err(TokenListError::WhitespaceInToken {
                token: "a b".to_owned()
            })
        );
    }

    #[test]
    fn empty_list() {
        let list = DomTokenList::empty();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
        assert_eq!(list.serialize(), "");
    }

    // --- add -----------------------------------------------------------

    #[test]
    fn add_new_tokens() {
        let mut list = DomTokenList::parse("foo");
        list.add(["bar", "baz"]).unwrap();
        assert_eq!(list.serialize(), "foo bar baz");
    }

    #[test]
    fn add_is_idempotent() {
        let mut list = DomTokenList::parse("foo bar");
        list.add(["bar"]).unwrap();
        assert_eq!(list.serialize(), "foo bar");
    }

    #[test]
    fn add_atomic_on_invalid_token() {
        let mut list = DomTokenList::parse("foo");
        // The empty token aborts the whole add; nothing is mutated.
        let err = list.add(["bar", "", "baz"]).unwrap_err();
        assert_eq!(err, TokenListError::EmptyToken);
        assert_eq!(list.serialize(), "foo");
    }

    #[test]
    fn add_atomic_on_whitespace_token() {
        let mut list = DomTokenList::parse("foo");
        let err = list.add(["bar", "a b"]).unwrap_err();
        assert!(matches!(err, TokenListError::WhitespaceInToken { .. }));
        assert_eq!(list.serialize(), "foo");
    }

    // --- remove --------------------------------------------------------

    #[test]
    fn remove_existing_preserves_order() {
        let mut list = DomTokenList::parse("foo bar baz");
        list.remove(["bar"]).unwrap();
        assert_eq!(list.serialize(), "foo baz");
    }

    #[test]
    fn remove_multiple() {
        let mut list = DomTokenList::parse("a b c d e");
        list.remove(["b", "d"]).unwrap();
        assert_eq!(list.serialize(), "a c e");
    }

    #[test]
    fn remove_absent_is_noop() {
        let mut list = DomTokenList::parse("foo");
        list.remove(["bar"]).unwrap();
        assert_eq!(list.serialize(), "foo");
    }

    #[test]
    fn remove_validates_first() {
        let mut list = DomTokenList::parse("foo");
        assert!(list.remove([""]).is_err());
        assert_eq!(list.serialize(), "foo");
    }

    // --- toggle --------------------------------------------------------

    #[test]
    fn toggle_adds_if_absent() {
        let mut list = DomTokenList::parse("foo");
        assert!(list.toggle("bar", None).unwrap());
        assert_eq!(list.serialize(), "foo bar");
    }

    #[test]
    fn toggle_removes_if_present() {
        let mut list = DomTokenList::parse("foo bar");
        assert!(!list.toggle("bar", None).unwrap());
        assert_eq!(list.serialize(), "foo");
    }

    #[test]
    fn toggle_force_true_keeps_present_token() {
        let mut list = DomTokenList::parse("foo");
        assert!(list.toggle("foo", Some(true)).unwrap());
        assert!(list.contains("foo").unwrap());
        assert_eq!(list.serialize(), "foo");
    }

    #[test]
    fn toggle_force_false_keeps_absent_token_absent() {
        let mut list = DomTokenList::parse("foo");
        assert!(!list.toggle("bar", Some(false)).unwrap());
        assert!(!list.contains("bar").unwrap());
        assert_eq!(list.serialize(), "foo");
    }

    #[test]
    fn toggle_validates_first() {
        let mut list = DomTokenList::parse("foo");
        assert!(list.toggle("", None).is_err());
        assert!(list.toggle("a b", None).is_err());
        assert_eq!(list.serialize(), "foo");
    }

    // --- replace -------------------------------------------------------

    #[test]
    fn replace_in_place_preserves_position() {
        let mut list = DomTokenList::parse("foo bar baz");
        assert!(list.replace("bar", "qux").unwrap());
        assert_eq!(list.serialize(), "foo qux baz");
    }

    #[test]
    fn replace_returns_false_when_old_absent() {
        let mut list = DomTokenList::parse("foo bar");
        assert!(!list.replace("missing", "qux").unwrap());
        assert_eq!(list.serialize(), "foo bar");
    }

    #[test]
    fn replace_with_same_token_returns_true_unchanged() {
        let mut list = DomTokenList::parse("foo");
        assert!(list.replace("foo", "foo").unwrap());
        assert_eq!(list.serialize(), "foo");
    }

    #[test]
    fn replace_with_existing_new_drops_old() {
        // replace("bar", "foo") on "bar foo" ⇒ "foo" (no duplicate inserted).
        let mut list = DomTokenList::parse("bar foo");
        assert!(list.replace("bar", "foo").unwrap());
        assert_eq!(list.serialize(), "foo");
    }

    #[test]
    fn replace_validates_both() {
        let mut list = DomTokenList::parse("foo");
        assert!(list.replace("", "bar").is_err());
        assert!(list.replace("foo", "").is_err());
        assert!(list.replace("foo", "a b").is_err());
        assert_eq!(list.serialize(), "foo");
    }

    // --- iter + as_vec -------------------------------------------------

    #[test]
    fn iter_yields_in_order() {
        let list = DomTokenList::parse("c b a");
        let collected: Vec<&str> = list.iter().collect();
        assert_eq!(collected, vec!["c", "b", "a"]);
    }

    #[test]
    fn as_vec_backing_is_snapshot_stable() {
        let list = DomTokenList::parse("foo bar");
        let snap = list.as_vec();
        assert_eq!(snap, &["foo".to_string(), "bar".to_string()]);
    }

    // --- from_tokens ---------------------------------------------------

    #[test]
    fn from_tokens_dedups() {
        let list = DomTokenList::from_tokens(vec![
            "a".to_owned(),
            "b".to_owned(),
            "a".to_owned(),
            "c".to_owned(),
        ]);
        assert_eq!(list.serialize(), "a b c");
    }

    // --- SupportedTokens ----------------------------------------------

    #[test]
    fn supports_is_case_insensitive() {
        // The WHATWG § 4.6.5 LinkTypes table is ASCII case-insensitive.
        let s = SupportedTokens::new(["stylesheet", "preload", "icon"]);
        assert!(s.supports("stylesheet"));
        assert!(s.supports("StyleSheet"));
        assert!(s.supports("PRELOAD"));
        assert!(!s.supports("pingback"));
    }

    #[test]
    fn supports_empty_is_false() {
        let s = SupportedTokens::new(["stylesheet"]);
        // The empty token isn't in the supported set.
        assert!(!s.supports(""));
    }

    // --- Mutation atomicity across multiple ops ------------------------

    #[test]
    fn add_then_remove_then_toggle_round_trip() {
        let mut list = DomTokenList::empty();
        list.add(["a", "b", "c"]).unwrap();
        list.remove(["b"]).unwrap();
        list.toggle("d", None).unwrap();
        list.toggle("a", None).unwrap(); // removes a
        assert_eq!(list.serialize(), "c d");
    }
}
