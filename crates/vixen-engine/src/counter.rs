//! CSS counters — CSS2 § 12.4 (counter scoping/reset/increment) + CSS Lists 3
//! § 5 (`counter()` / `counters()` resolution). The scope-and-value half of
//! the CSS counters/list-marker surface; complements [`crate::list_marker`]
//! (the value → marker-text half) and the layout layer that drives the
//! traversal.
//!
//! What lives here:
//! - [`CounterOp`] + [`CounterOpKind`] — parse `counter-reset` /
//!   `counter-increment` / `counter-set` declaration values into the ordered
//!   `(name, value)` ops the cascade emits (with the per-kind default value:
//!   `0` for reset/set, `1` for increment).
//! - [`CounterValue`] / [`Scope`] — the pure value model: a counter is a named
//!   `i64` scoped to a subtree; scopes nest (outermost first, innermost last).
//! - [`resolve_counter`] — `counter(name)` resolution: the innermost in-scope
//!   value, or `None` (→ empty marker per CSS Lists 3 § 5) if not in scope.
//! - [`resolve_counters`] — `counters(name, delim)` resolution: every in-scope
//!   value joined outermost→innermost with `delim` (e.g. `"1.1"`).
//! - [`render_counter`] — `counter(name, style)` end-to-end: resolve + render
//!   via [`crate::list_marker`].
//!
//! What does *not* live here:
//! - The DOM traversal that pushes/pops scopes and applies reset/increment/set
//!   in document order (Phase 4 layout owns that; this module is the pure
//!   resolution primitive given the already-walked scope stack).
//! - `display: list-item` and the `::marker` box (Phase 4 layout).
//! - `@counter-style` user-defined styles (deferred; v1 ships the § 6.1 set).
//!
//! ## Scoping (CSS2 § 12.4.3)
//!
//! A counter is *created* by `counter-reset` on an element and *scoped* to
//! that element's subtree (its descendants, up to but not including the next
//! reset of the same name). Nested resets create nested scopes — the
//! resolution functions below take a stack of scopes, **outermost first,
//! innermost last**, so `resolve_counter` reads the last entry and
//! `resolve_counters` walks all of them.
//!
//! Reference: <https://www.w3.org/TR/css-lists-3/#counter-functions>.
//! CSS2 § 12.4: <https://www.w3.org/TR/CSS2/generate.html#counters>.

#![forbid(unsafe_code)]

use crate::list_marker::ListStyleType;

// ---------------------------------------------------------------------------
// Counter op parsing (counter-reset / counter-increment / counter-set)
// ---------------------------------------------------------------------------

/// Which counter-property a [`CounterOp`] came from. The only thing that
/// differs across the three is the default value applied when the author omits
/// the integer (CSS2 § 12.4: reset/set default to `0`, increment to `1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CounterOpKind {
    /// `counter-reset` — create/reset a counter to a value (default `0`).
    Reset,
    /// `counter-increment` — add a delta (default `1`).
    Increment,
    /// `counter-set` (CSS Lists 3 § 8.1) — set a counter to a value
    /// (default `0`), without creating a new scope.
    Set,
}

impl CounterOpKind {
    /// The default value when the author omits the integer.
    pub const fn default_value(self) -> i64 {
        match self {
            CounterOpKind::Reset | CounterOpKind::Set => 0,
            CounterOpKind::Increment => 1,
        }
    }
}

/// A single `(name, value)` op parsed from a `counter-*` declaration. The
/// cascade/layout layer walks the DOM applying these in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterOp {
    /// The counter name (a CSS `<custom-ident>`).
    pub name: String,
    /// The reset value / set value / increment delta.
    pub value: i64,
}

/// Parse error from [`parse_counter_ops`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CounterError {
    /// The declaration was empty (other than the `none` keyword, which is a
    /// valid no-op and yields an empty op list).
    #[error("counter declaration is empty")]
    Empty,
    /// A bare integer appeared with no preceding name, or two integers in a
    /// row.
    #[error("unexpected integer {0} without a preceding counter name")]
    UnexpectedInteger(String),
    /// An identifier that isn't a valid CSS `<custom-ident>` (or `none` in a
    /// non-leading position).
    #[error("invalid counter name: {0:?}")]
    InvalidName(String),
    /// An integer that doesn't parse as a signed `i64`.
    #[error("invalid integer: {0}")]
    InvalidInteger(String),
}

/// Parse a `counter-reset` / `counter-increment` / `counter-set` declaration
/// value into ordered ops. Tokens are ASCII-whitespace-separated `<custom-
/// ident>` optionally followed by one `<integer>`; an ident without a
/// following integer takes the [`CounterOpKind::default_value`].
///
/// `none` (case-insensitive, the whole declaration) is the explicit no-op and
/// yields an empty op list. Leading/trailing whitespace is trimmed.
///
/// ```
/// # use vixen_engine::counter::{CounterOp, CounterOpKind, parse_counter_ops};
/// let ops = parse_counter_ops("chapter 2 section", CounterOpKind::Reset).unwrap();
/// assert_eq!(ops, vec![
///     CounterOp { name: "chapter".into(), value: 2 },
///     CounterOp { name: "section".into(), value: 0 }, // reset default
/// ]);
/// ```
pub fn parse_counter_ops(
    declaration: &str,
    kind: CounterOpKind,
) -> Result<Vec<CounterOp>, CounterError> {
    let trimmed = declaration.trim();
    if trimmed.is_empty() {
        return Err(CounterError::Empty);
    }
    // `none` is the explicit no-op (CSS2 § 12.4: "the keyword 'none' … no
    // counters are reset").
    if trimmed.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }

    let tokens: Vec<&str> = trimmed.split_ascii_whitespace().collect();
    let mut ops = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if parse_integer(tok).is_ok() {
            // A bare integer with no preceding name.
            return Err(CounterError::UnexpectedInteger(tok.to_owned()));
        }
        if !is_custom_ident(tok) {
            return Err(CounterError::InvalidName(tok.to_owned()));
        }
        let name = tok.to_owned();
        // Peek: is the next token an integer?
        let mut value = kind.default_value();
        i += 1;
        if i < tokens.len()
            && let Ok(num) = parse_integer(tokens[i])
        {
            value = num;
            i += 1;
        }
        ops.push(CounterOp { name, value });
    }
    debug_assert!(i == tokens.len());
    Ok(ops)
}

/// Parse a CSS `<integer>` (optional `+`/`-` sign, then ASCII digits) into an
/// `i64`, saturating on overflow. Returns `Err` for non-integers so the caller
/// can distinguish "this token is an integer" from "this token is a name".
fn parse_integer(tok: &str) -> Result<i64, ()> {
    let bytes = tok.as_bytes();
    if bytes.is_empty() {
        return Err(());
    }
    let mut start = 0;
    if bytes[0] == b'+' || bytes[0] == b'-' {
        start = 1;
    }
    if start >= bytes.len() {
        return Err(()); // sign with no digits
    }
    if !bytes[start..].iter().all(|&b| b.is_ascii_digit()) {
        return Err(());
    }
    // i64::from_str handles saturating overflow via the saturating path on
    // `parse`; use a manual parse so we can saturate consistently.
    match tok.parse::<i64>() {
        Ok(v) => Ok(v),
        Err(_) => {
            // Overflow: clamp to the appropriate edge based on sign.
            Ok(if tok.starts_with('-') {
                i64::MIN
            } else {
                i64::MAX
            })
        }
    }
}

/// Minimal CSS `<custom-ident>` validation (CSS Values 4 § 5.2). Permissive on
/// purpose: a sequence of letters/digits/`_`/`-`/non-ASCII, where the first
/// char is not a digit, not a `-` followed by a digit or a second `-`, and the
/// whole token is not the reserved `none`. (The leading-position `none` is
/// handled by the caller; `none` appearing as a name mid-list is rejected
/// here.)
fn is_custom_ident(tok: &str) -> bool {
    if tok.is_empty() || tok.eq_ignore_ascii_case("none") {
        return false;
    }
    let mut chars = tok.chars().peekable();
    let first = chars.next().unwrap();
    // First-char rules.
    if first.is_ascii_digit() {
        return false;
    }
    if first == '-' {
        match chars.peek() {
            Some(c) if c.is_ascii_digit() => return false,
            Some('-') => return false,
            None => return false,
            _ => {}
        }
    }
    // A leading bare `-` (then end) is invalid; a leading `-` then a valid
    // identifier char is fine (handled above). Otherwise the first char must
    // be a letter, `_`, `-`, or non-ASCII.
    if !(first == '-' || first == '_' || first.is_ascii_alphabetic() || !first.is_ascii()) {
        return false;
    }
    chars.all(|c| c == '_' || c == '-' || c.is_ascii_alphanumeric() || !c.is_ascii())
}

// ---------------------------------------------------------------------------
// Scope stack + resolution (CSS2 § 12.4.3)
// ---------------------------------------------------------------------------

/// One counter value at one nesting level: a `(name, value)` pair. A scope is
/// the set of counters established by a single `counter-reset` (one element).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterValue {
    /// The counter name.
    pub name: String,
    /// The counter's value at this scope.
    pub value: i64,
}

/// One nesting level of counters — the counters established on a single
/// element by its `counter-reset` declaration (after increments from
/// descendants have been applied). Stored outermost-first in a scope stack.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    counters: Vec<CounterValue>,
}

impl Scope {
    /// An empty scope (no counters established on this level).
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a scope from the given `(name, value)` pairs.
    pub fn from_pairs<I: IntoIterator<Item = (String, i64)>>(pairs: I) -> Self {
        Self {
            counters: pairs
                .into_iter()
                .map(|(name, value)| CounterValue { name, value })
                .collect(),
        }
    }

    /// The value of `name` at this scope, if present.
    pub fn get(&self, name: &str) -> Option<i64> {
        self.counters
            .iter()
            .rev()
            .find(|c| c.name == name)
            .map(|c| c.value)
    }

    /// Iterate this scope's `(name, value)` pairs in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, i64)> {
        self.counters.iter().map(|c| (c.name.as_str(), c.value))
    }

    /// Whether this scope establishes any counters.
    pub fn is_empty(&self) -> bool {
        self.counters.is_empty()
    }
}

impl From<Vec<(String, i64)>> for Scope {
    fn from(pairs: Vec<(String, i64)>) -> Self {
        Self::from_pairs(pairs)
    }
}

/// `counter(name)` resolution (CSS Lists 3 § 5): the value of the innermost
/// in-scope counter of that name, or `None` if none is in scope. `None`
/// signals "render the empty string" per § 5.
///
/// `scopes` is ordered outermost-first, innermost-last (the natural document
/// order).
pub fn resolve_counter(scopes: &[Scope], name: &str) -> Option<i64> {
    scopes.iter().rev().find_map(|s| s.get(name))
}

/// `counters(name, delim)` resolution (CSS Lists 3 § 5): every in-scope value
/// of `name`, joined outermost→innermost with `delim`. Returns the empty
/// string when no counter of that name is in scope.
///
/// E.g. nested lists: `counters("item", ".")` → `"1.1"`, `"1.2"`, `"2.1"`.
pub fn resolve_counters(scopes: &[Scope], name: &str, delim: &str) -> String {
    let mut parts: Vec<i64> = Vec::new();
    for scope in scopes {
        if let Some(v) = scope.get(name) {
            parts.push(v);
        }
    }
    parts
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(delim)
}

/// `counter(name, style)` end-to-end: resolve + render via
/// [`crate::list_marker`]. Returns `None` if `name` is not in scope (→ the
/// content layer renders the empty string). `style` defaults to
/// [`ListStyleType::Decimal`] when omitted (CSS2 § 12.2).
///
/// ```
/// # use vixen_engine::counter::{render_counter, Scope};
/// # use vixen_engine::list_marker::ListStyleType;
/// let scopes = vec![Scope::from_pairs([("chapter".to_string(), 7)])];
/// assert_eq!(
///     render_counter(&scopes, "chapter", ListStyleType::UpperRoman),
///     Some("VII".to_owned())
/// );
/// // Out-of-scope counter → None (empty marker).
/// assert_eq!(render_counter(&scopes, "missing", ListStyleType::Decimal), None);
/// ```
pub fn render_counter(scopes: &[Scope], name: &str, style: ListStyleType) -> Option<String> {
    let value = resolve_counter(scopes, name)?;
    style.render(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse: reset ---------------------------------------------------

    #[test]
    fn reset_default_is_zero() {
        let ops = parse_counter_ops("section", CounterOpKind::Reset).unwrap();
        assert_eq!(
            ops,
            vec![CounterOp {
                name: "section".into(),
                value: 0
            }]
        );
    }

    #[test]
    fn reset_with_explicit_value() {
        let ops = parse_counter_ops("chapter 2", CounterOpKind::Reset).unwrap();
        assert_eq!(
            ops,
            vec![CounterOp {
                name: "chapter".into(),
                value: 2
            }]
        );
    }

    #[test]
    fn reset_multiple_mixed() {
        let ops = parse_counter_ops("chapter 2 section", CounterOpKind::Reset).unwrap();
        assert_eq!(
            ops,
            vec![
                CounterOp {
                    name: "chapter".into(),
                    value: 2
                },
                CounterOp {
                    name: "section".into(),
                    value: 0
                },
            ]
        );
    }

    #[test]
    fn reset_negative_value() {
        let ops = parse_counter_ops("c -1", CounterOpKind::Reset).unwrap();
        assert_eq!(
            ops,
            vec![CounterOp {
                name: "c".into(),
                value: -1
            }]
        );
    }

    #[test]
    fn reset_signed_positive() {
        let ops = parse_counter_ops("c +5", CounterOpKind::Reset).unwrap();
        assert_eq!(
            ops,
            vec![CounterOp {
                name: "c".into(),
                value: 5
            }]
        );
    }

    // --- parse: increment default differs -------------------------------

    #[test]
    fn increment_default_is_one() {
        let ops = parse_counter_ops("item", CounterOpKind::Increment).unwrap();
        assert_eq!(
            ops,
            vec![CounterOp {
                name: "item".into(),
                value: 1
            }]
        );
    }

    #[test]
    fn set_default_is_zero() {
        let ops = parse_counter_ops("item", CounterOpKind::Set).unwrap();
        assert_eq!(
            ops,
            vec![CounterOp {
                name: "item".into(),
                value: 0
            }]
        );
    }

    #[test]
    fn kind_default_values() {
        assert_eq!(CounterOpKind::Reset.default_value(), 0);
        assert_eq!(CounterOpKind::Set.default_value(), 0);
        assert_eq!(CounterOpKind::Increment.default_value(), 1);
    }

    // --- parse: none keyword + edge cases -------------------------------

    #[test]
    fn none_keyword_is_no_op() {
        assert!(
            parse_counter_ops("none", CounterOpKind::Reset)
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_counter_ops("NONE", CounterOpKind::Reset)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn empty_declaration_errors() {
        assert_eq!(
            parse_counter_ops("   ", CounterOpKind::Reset),
            Err(CounterError::Empty)
        );
    }

    #[test]
    fn leading_integer_errors() {
        assert!(matches!(
            parse_counter_ops("5 item", CounterOpKind::Reset),
            Err(CounterError::UnexpectedInteger(_))
        ));
    }

    #[test]
    fn two_integers_in_a_row_errors() {
        // "item 5 6" — the second integer has no preceding name.
        assert!(matches!(
            parse_counter_ops("item 5 6", CounterOpKind::Reset),
            Err(CounterError::UnexpectedInteger(_))
        ));
    }

    #[test]
    fn invalid_name_errors() {
        // A digit-leading token isn't a valid <custom-ident>.
        assert!(matches!(
            parse_counter_ops("1bad", CounterOpKind::Reset),
            Err(CounterError::InvalidName(_))
        ));
    }

    #[test]
    fn none_as_mid_list_name_rejected() {
        assert!(matches!(
            parse_counter_ops("item none", CounterOpKind::Reset),
            Err(CounterError::InvalidName(_))
        ));
    }

    #[test]
    fn leading_hyphen_ident_accepted() {
        let ops = parse_counter_ops("-foo 1", CounterOpKind::Reset).unwrap();
        assert_eq!(
            ops,
            vec![CounterOp {
                name: "-foo".into(),
                value: 1
            }]
        );
    }

    #[test]
    fn double_hyphen_name_rejected() {
        // `--custom` is reserved for CSS variables, not a counter name.
        assert!(matches!(
            parse_counter_ops("--x", CounterOpKind::Reset),
            Err(CounterError::InvalidName(_))
        ));
    }

    #[test]
    fn integer_overflow_saturates() {
        let ops = parse_counter_ops("c 99999999999999999999", CounterOpKind::Reset).unwrap();
        assert_eq!(
            ops,
            vec![CounterOp {
                name: "c".into(),
                value: i64::MAX
            }]
        );
    }

    // --- resolve_counter (innermost) ------------------------------------

    fn scopes_nested() -> Vec<Scope> {
        // Outer establishes "item"=1, an inner establishes "item"=2, and a
        // third scope establishes a different counter.
        vec![
            Scope::from_pairs([("item".to_string(), 1)]),
            Scope::from_pairs([("item".to_string(), 2)]),
            Scope::from_pairs([("other".to_string(), 9)]),
        ]
    }

    #[test]
    fn resolve_counter_returns_innermost() {
        let s = scopes_nested();
        assert_eq!(resolve_counter(&s, "item"), Some(2));
    }

    #[test]
    fn resolve_counter_missing_returns_none() {
        let s = scopes_nested();
        assert_eq!(resolve_counter(&s, "nope"), None);
    }

    #[test]
    fn resolve_counter_empty_scope_stack() {
        assert_eq!(resolve_counter(&[], "item"), None);
    }

    // --- resolve_counters (joined) --------------------------------------

    #[test]
    fn resolve_counters_joins_outermost_to_innermost() {
        let s = scopes_nested();
        assert_eq!(resolve_counters(&s, "item", "."), "1.2");
    }

    #[test]
    fn resolve_counters_skips_scopes_without_the_name() {
        let s = scopes_nested();
        // "other" only lives in the innermost scope → just "9".
        assert_eq!(resolve_counters(&s, "other", "."), "9");
    }

    #[test]
    fn resolve_counters_custom_delimiter() {
        let s = scopes_nested();
        assert_eq!(resolve_counters(&s, "item", "-"), "1-2");
    }

    #[test]
    fn resolve_counters_missing_is_empty_string() {
        let s = scopes_nested();
        assert_eq!(resolve_counters(&s, "nope", "."), "");
    }

    #[test]
    fn resolve_counters_three_levels() {
        let s = vec![
            Scope::from_pairs([("item".to_string(), 1)]),
            Scope::from_pairs([("item".to_string(), 3)]),
            Scope::from_pairs([("item".to_string(), 2)]),
        ];
        assert_eq!(resolve_counters(&s, "item", "."), "1.3.2");
    }

    // --- render_counter (compose with list_marker) ----------------------

    #[test]
    fn render_counter_decimal_default_style() {
        let s = vec![Scope::from_pairs([("n".to_string(), 42)])];
        assert_eq!(
            render_counter(&s, "n", ListStyleType::Decimal),
            Some("42".to_owned())
        );
    }

    #[test]
    fn render_counter_roman_style() {
        let s = vec![Scope::from_pairs([("chapter".to_string(), 14)])];
        assert_eq!(
            render_counter(&s, "chapter", ListStyleType::LowerRoman),
            Some("xiv".to_owned())
        );
    }

    #[test]
    fn render_counter_out_of_scope_is_none() {
        let s = vec![Scope::from_pairs([("n".to_string(), 1)])];
        assert_eq!(render_counter(&s, "missing", ListStyleType::Decimal), None);
    }

    // --- Scope helpers --------------------------------------------------

    #[test]
    fn scope_get_and_iter() {
        let s = Scope::from_pairs([("a".to_string(), 1), ("b".to_string(), 2)]);
        assert_eq!(s.get("a"), Some(1));
        assert_eq!(s.get("b"), Some(2));
        assert_eq!(s.get("c"), None);
        let collected: Vec<_> = s.iter().collect();
        assert_eq!(collected, vec![("a", 1), ("b", 2)]);
    }

    #[test]
    fn scope_duplicate_name_last_wins() {
        // CSS doesn't forbid declaring the same counter twice on one element;
        // resolution takes the last (matching § 12.4's "if the same counter is
        // reset more than once, only the last reset takes effect").
        let s = Scope::from_pairs([("n".to_string(), 1), ("n".to_string(), 5)]);
        assert_eq!(s.get("n"), Some(5));
    }

    #[test]
    fn empty_scope() {
        let s = Scope::new();
        assert!(s.is_empty());
        assert_eq!(s.get("n"), None);
    }
}
