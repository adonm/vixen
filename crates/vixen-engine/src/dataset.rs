//! HTML5 `HTMLElement.dataset` (DOMStringMap) bidirectional mapping — Phase 6
//! DOM prep (pure logic called out by `docs/PLAN.md` "Testing strategy" as a
//! Rust-unit-test surface). Implements the WHATWG HTML § 3.2.6.9 algorithm for
//! converting between `data-*` attribute names and dataset property names, so
//! the Phase 6 host-hook layer has one source of truth for the rule.
//!
//! What lives here:
//! - [`attribute_to_property`] — deserialization: `data-foo-bar` → `fooBar`.
//!   Returns `None` for attributes that are not exposed (anti-collision rule:
//!   a hyphen followed by an ASCII uppercase letter in the suffix).
//! - [`property_to_attribute`] — serialization: `fooBar` → `data-foo-bar`.
//! - [`collect_dataset`] — walk an attribute list (sorted, deduped) into the
//!   `(property, value)` pairs the dataset object exposes.
//!
//! What does *not* live here:
//! - The DOM hookup (the host-hook layer in Phase 6 owns the live
//!   `DOMStringMap` reflection; this module is the pure conversion it reduces
//!   to).
//! - XML namespacing (the v1.0 surface is HTML; XML documents have a separate
//!   spec path that is out of scope).
//!
//! The anti-collision rule (WHATWG HTML § 3.2.6.9 "If name contains a U+002D
//! HYPHEN-MINUS character followed by an ASCII lowercase letter, then
//! continue" applies to *deserialization* when the attribute name has not been
//! canonicalised by the HTML parser). Concretely: the HTML parser always
//! lowercases attribute names, so `data-foo-bar` is the canonical form and
//! deserialises to `fooBar`. An attribute set via `setAttribute` (which does
//! not lowercase) like `data-foo-Bar` would collide with the canonical
//! `data-foo-bar` form; the spec resolves the collision by hiding the
//! non-canonical attribute from `dataset`. We model that as `None`.
//!
//! Reference: <https://html.spec.whatwg.org/multipage/dom.html#attr-data-*>,
//! § 3.2.6.9 "Embedding custom non-visible data with the `data-*` attributes".

#![forbid(unsafe_code)]

/// The custom-data-attribute prefix. WHATWG HTML § 3.2.6.9: an attribute is a
/// custom data attribute iff its name starts with this string and has at least
/// one more character.
pub const DATA_PREFIX: &str = "data-";

/// Deserialise a custom data attribute name into its dataset property name
/// (WHATWG HTML § 3.2.6.9). Returns `None` when the attribute is not exposed
/// via `dataset`:
/// - Name is shorter than `data-` + one char (e.g. `data-`, `dat`, ...).
/// - Name does not start with `data-`.
/// - Suffix contains a hyphen immediately followed by an ASCII uppercase
///   letter (the anti-collision rule; only reachable via `setAttribute`).
///
/// Otherwise: for each `U+002D HYPHEN-MINUS` in the suffix that is followed by
/// an ASCII lowercase letter, remove the hyphen and uppercase the letter;
/// every other character (including other hyphens) passes through verbatim.
///
/// ```
/// # use vixen_engine::dataset::attribute_to_property;
/// assert_eq!(attribute_to_property("data-foo-bar"), Some("fooBar".to_owned()));
/// assert_eq!(attribute_to_property("data-x"),       Some("x".to_owned()));
/// assert_eq!(attribute_to_property("data-foo--bar"),Some("foo-Bar".to_owned()));
/// assert_eq!(attribute_to_property("data--foo"),    Some("Foo".to_owned()));
/// assert_eq!(attribute_to_property("data-foo-Bar"), None); // anti-collision
/// assert_eq!(attribute_to_property("data-"),        None); // no suffix char
/// assert_eq!(attribute_to_property("class"),        None); // not a data-* attr
/// ```
pub fn attribute_to_property(name: &str) -> Option<String> {
    // Step 1: must start with `data-` and have at least one suffix char.
    let suffix = name.strip_prefix(DATA_PREFIX)?;
    if suffix.is_empty() {
        return None;
    }

    // Step 2: anti-collision. If any hyphen is followed by an ASCII uppercase
    // letter, the attribute is not exposed. (Only reachable when the HTML
    // parser didn't lowercase the name — i.e. setAttribute.)
    let bytes = suffix.as_bytes();
    for window in bytes.windows(2) {
        if window[0] == b'-' && window[1].is_ascii_uppercase() {
            return None;
        }
    }

    // Step 3: rewrite hyphen-then-lowercase pairs into the uppercase letter,
    // leaving other hyphens (and other characters) verbatim.
    let mut out = String::with_capacity(suffix.len());
    let mut chars = suffix.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '-' {
            match chars.peek() {
                Some(&next) if next.is_ascii_lowercase() => {
                    chars.next();
                    out.push(next.to_ascii_uppercase());
                }
                _ => out.push('-'),
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Serialise a dataset property name back into its `data-*` attribute name
/// (WHATWG HTML § 3.2.6.9 "the rules for serialising"). For each ASCII
/// uppercase letter in the property name, insert a hyphen before it and
/// lowercase it; then prepend `data-`.
///
/// This is the inverse of [`attribute_to_property`] for every property name
/// the parser would produce. Property names containing characters other than
/// `[A-Za-z0-9_-]` are rejected with [`DatasetError::InvalidPropertyName`]
/// (HTML forbids them in `data-*` attributes — the names must be
/// XML-compatible, and JS object keys containing other punctuation are an
/// authoring error).
///
/// ```
/// # use vixen_engine::dataset::property_to_attribute;
/// assert_eq!(property_to_attribute("fooBar").unwrap(),    "data-foo-bar");
/// assert_eq!(property_to_attribute("foo-bar").unwrap(),   "data-foo-bar");
/// assert_eq!(property_to_attribute("x").unwrap(),         "data-x");
/// assert_eq!(property_to_attribute("foo-Bar").unwrap(),   "data-foo--bar");
/// ```
pub fn property_to_attribute(property: &str) -> Result<String, DatasetError> {
    if property.is_empty() {
        return Err(DatasetError::EmptyPropertyName);
    }
    let mut name = String::with_capacity(DATA_PREFIX.len() + property.len() + 4);
    name.push_str(DATA_PREFIX);
    for c in property.chars() {
        if c.is_ascii_uppercase() {
            name.push('-');
            name.push(c.to_ascii_lowercase());
        } else if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == ':' {
            name.push(c);
        } else {
            return Err(DatasetError::InvalidPropertyName(c));
        }
    }
    Ok(name)
}

/// Collect the `(property, value)` pairs a `data-*` attribute list exposes via
/// `dataset`. Attributes are processed in document order; if two attributes
/// map to the same property name (only possible via the non-canonical form
/// the parser would have lowercased), the first one wins (browsers keep the
/// first by attribute order).
///
/// Each input is `(attribute_name, attribute_value)`; non-`data-*` attributes
/// are filtered out. The returned vector preserves document order, which is
/// the order JS `Object.keys(element.dataset)` returns them in.
pub fn collect_dataset<I, K, V>(attrs: I) -> Vec<(String, V)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
{
    let mut out: Vec<(String, V)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (name, value) in attrs {
        let Some(prop) = attribute_to_property(name.as_ref()) else {
            continue;
        };
        if seen.contains(&prop) {
            // First-in-wins for collisions (matches Chrome/Firefox).
            continue;
        }
        seen.push(prop.clone());
        out.push((prop, value));
    }
    out
}

/// Serialisation failure for [`property_to_attribute`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DatasetError {
    /// Property names cannot be empty (`dataset[""] = x` is a no-op per spec).
    #[error("dataset property name cannot be empty")]
    EmptyPropertyName,
    /// Property names must be XML-name-compatible (ASCII alphanumerics plus
    /// `-_.:`); other characters are rejected.
    #[error("invalid character {0:?} in dataset property name")]
    InvalidPropertyName(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- attribute_to_property: canonical forms ---------------------------

    #[test]
    fn simple_data_attr_strips_prefix() {
        assert_eq!(attribute_to_property("data-foo"), Some("foo".to_owned()));
        assert_eq!(attribute_to_property("data-x"), Some("x".to_owned()));
        assert_eq!(attribute_to_property("data-a1"), Some("a1".to_owned()));
    }

    #[test]
    fn single_hyphen_uppercases_following_letter() {
        // The canonical HTML form (parser-lowercased): data-foo-bar → fooBar.
        assert_eq!(
            attribute_to_property("data-foo-bar"),
            Some("fooBar".to_owned())
        );
        assert_eq!(
            attribute_to_property("data-first-name"),
            Some("firstName".to_owned())
        );
    }

    #[test]
    fn multiple_hyphens_each_uppercase() {
        assert_eq!(
            attribute_to_property("data-foo-bar-baz"),
            Some("fooBarBaz".to_owned())
        );
        // `data-ng-model` → `ngModel` (Angular's idiom).
        assert_eq!(
            attribute_to_property("data-ng-model"),
            Some("ngModel".to_owned())
        );
    }

    #[test]
    fn trailing_hyphen_kept_verbatim() {
        // data-foo- → `foo-` (no following char to uppercase).
        assert_eq!(attribute_to_property("data-foo-"), Some("foo-".to_owned()));
    }

    #[test]
    fn leading_hyphen_uppercases_following_letter() {
        // data--foo → `Foo`: the first hyphen (after data-) is followed by
        // lowercase `f`, so the rewrite removes it and uppercases the letter.
        // The result is `Foo`, not `-foo` — there is no surviving hyphen.
        assert_eq!(attribute_to_property("data--foo"), Some("Foo".to_owned()));
    }

    #[test]
    fn double_hyphen_before_letter_keeps_first() {
        // data-foo--bar → `foo-Bar`: the inner `--` collapses to `-Bar`
        // (first `-` is followed by `-`, kept; second `-` is followed by `b`,
        // uppercased).
        assert_eq!(
            attribute_to_property("data-foo--bar"),
            Some("foo-Bar".to_owned())
        );
    }

    #[test]
    fn hyphen_followed_by_digit_is_kept() {
        // data-foo-1 → `foo-1` (digit is not lowercase, hyphen kept).
        assert_eq!(
            attribute_to_property("data-foo-1"),
            Some("foo-1".to_owned())
        );
        // data-foo-1bar → `foo-1bar` (digit follows hyphen, hyphen kept;
        // `b` is NOT preceded by the kept hyphen because the rewrite consumes
        // only hyphen+lowercase pairs).
        assert_eq!(
            attribute_to_property("data-foo-1bar"),
            Some("foo-1bar".to_owned())
        );
    }

    // --- attribute_to_property: rejection rules ---------------------------

    #[test]
    fn rejects_data_prefix_only() {
        // `data-` with no suffix char is not a custom data attribute.
        assert_eq!(attribute_to_property("data-"), None);
    }

    #[test]
    fn rejects_non_data_attributes() {
        assert_eq!(attribute_to_property("class"), None);
        assert_eq!(attribute_to_property("id"), None);
        assert_eq!(attribute_to_property("dat-foo"), None); // missing `a-`
        assert_eq!(attribute_to_property("data"), None);
        assert_eq!(attribute_to_property(""), None);
    }

    #[test]
    fn rejects_uppercase_after_hyphen_anti_collision() {
        // data-foo-Bar: the `-B` pair triggers the anti-collision rule
        // (only reachable via setAttribute; the HTML parser would have
        // lowercased to data-foo-bar which IS exposed).
        assert_eq!(attribute_to_property("data-foo-Bar"), None);
    }

    #[test]
    fn bare_uppercase_without_hyphen_is_exposed() {
        // data-X: no hyphen ⇒ no anti-collision risk. The bare uppercase
        // passes through unchanged. (A serialized property name always has
        // a hyphen before each uppercase, so data-X cannot collide with
        // any canonical attribute name.)
        assert_eq!(attribute_to_property("data-X"), Some("X".to_owned()));
        assert_eq!(
            attribute_to_property("data-fooBar"),
            Some("fooBar".to_owned())
        );
    }

    // --- property_to_attribute -------------------------------------------

    #[test]
    fn simple_property_prepends_data_prefix() {
        assert_eq!(property_to_attribute("foo").unwrap(), "data-foo");
        assert_eq!(property_to_attribute("x").unwrap(), "data-x");
    }

    #[test]
    fn uppercase_letter_inserts_hyphen_and_lowercases() {
        assert_eq!(property_to_attribute("fooBar").unwrap(), "data-foo-bar");
        assert_eq!(
            property_to_attribute("firstName").unwrap(),
            "data-first-name"
        );
        assert_eq!(
            property_to_attribute("fooBarBaz").unwrap(),
            "data-foo-bar-baz"
        );
    }

    #[test]
    fn existing_hyphen_passes_through() {
        // dataset["foo-bar"] serialises to the same attribute as dataset["fooBar"].
        assert_eq!(property_to_attribute("foo-bar").unwrap(), "data-foo-bar");
    }

    #[test]
    fn property_with_uppercase_after_hyphen_double_inserts() {
        // dataset["foo-Bar"]: `B` → `-b`, so `foo--bar`. Round-trips back.
        assert_eq!(property_to_attribute("foo-Bar").unwrap(), "data-foo--bar");
    }

    #[test]
    fn leading_hyphen_in_property() {
        assert_eq!(property_to_attribute("-foo").unwrap(), "data--foo");
    }

    #[test]
    fn digits_and_underscores_pass_through() {
        assert_eq!(property_to_attribute("foo_1").unwrap(), "data-foo_1");
        assert_eq!(property_to_attribute("foo1Bar").unwrap(), "data-foo1-bar");
    }

    #[test]
    fn rejects_empty_property_name() {
        assert!(matches!(
            property_to_attribute(""),
            Err(DatasetError::EmptyPropertyName)
        ));
    }

    #[test]
    fn rejects_property_name_with_invalid_chars() {
        // Spaces and other punctuation aren't valid in `data-*` names.
        assert!(property_to_attribute("foo bar").is_err());
        assert!(property_to_attribute("foo/bar").is_err());
        assert!(property_to_attribute("foo=bar").is_err());
    }

    // --- Round-trip -------------------------------------------------------

    #[test]
    fn round_trip_canonical_form() {
        // For every name the HTML parser produces, deserialise(serialise(x))==x.
        for parser_output in [
            "data-foo",
            "data-foo-bar",
            "data-first-name",
            "data-ng-model",
            "data-foo-bar-baz",
            "data-a1",
            "data-foo-1bar",
        ] {
            let prop = attribute_to_property(parser_output)
                .unwrap_or_else(|| panic!("failed to deserialise {parser_output}"));
            let attr = property_to_attribute(&prop)
                .unwrap_or_else(|e| panic!("failed to serialise {prop:?}: {e}"));
            assert_eq!(attr, parser_output, "round trip via {prop:?}");
        }
    }

    // --- collect_dataset -------------------------------------------------

    #[test]
    fn collect_filters_non_data_and_preserves_order() {
        let attrs = vec![
            ("class".to_string(), "main".to_string()),
            ("data-foo".to_string(), "1".to_string()),
            ("id".to_string(), "el".to_string()),
            ("data-bar-baz".to_string(), "2".to_string()),
        ];
        let out = collect_dataset(attrs);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], ("foo".to_string(), "1".to_string()));
        assert_eq!(out[1], ("barBaz".to_string(), "2".to_string()));
    }

    #[test]
    fn collect_first_wins_on_collision() {
        // data-foo-bar and data-fooBar both deserialise to "fooBar"; the first
        // wins (matches Chrome/Firefox — they keep the first attribute by
        // order, the second is hidden).
        let attrs = vec![
            ("data-foo-bar".to_string(), "from-dash".to_string()),
            ("data-fooBar".to_string(), "from-camel".to_string()),
        ];
        let out = collect_dataset(attrs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], ("fooBar".to_string(), "from-dash".to_string()));
    }

    #[test]
    fn collect_skips_unexposed() {
        // data-foo-Bar is rejected by the anti-collision rule.
        let attrs = vec![
            ("data-foo-Bar".to_string(), "hidden".to_string()),
            ("data-ok".to_string(), "visible".to_string()),
        ];
        let out = collect_dataset(attrs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], ("ok".to_string(), "visible".to_string()));
    }

    #[test]
    fn collect_empty_input() {
        let out: Vec<(String, String)> = collect_dataset(Vec::<(String, String)>::new());
        assert!(out.is_empty());
    }
}
