//! WHATWG URL `URLSearchParams` — Phase 6 host-bindings prep (docs/PLAN.md
//! Phase 6 step 5 "Network: … URL, URLSearchParams"). Implements the URL
//! Standard § 5.2 "application/x-www-form-urlencoded string parser" + § 5.3
//! "… serializer" plus the [`UrlSearchParams`] object's mutating surface, so
//! the Phase 6 JS host hook reflects `new URLSearchParams()` / `.get` /
//! `.append` / `.toString()` against one source of truth.
//!
//! What lives here:
//! - [`UrlSearchParams`] — the mutable ordered `(name, value)` list the
//!   `URLSearchParams` JS object mirrors, with `get`/`get_all`/`has`/`append`/
//!   `set`/`delete`/`sort`/`entries`/`keys`/`values` mirroring the WebIDL.
//! - [`parse`] — URL Standard § 5.2.4 byte parser: strip a leading `?`,
//!   split on `&`, split each on first `=`, `+` → SPACE, percent-decode as
//!   UTF-8 with U+FFFD for ill-formed sequences.
//! - [`serialize`] — URL Standard § 5.3.4 byte serializer: SPACE → `+`,
//!   percent-encode every byte but the URL-safe set (`* - . _ 0-9 A-Z a-z`),
//!   uppercase hex, pairs joined with `&`, name/value joined with `=`.
//!
//! What does *not* live here:
//! - URL parsing / mutation of the `URL` object's `query` field. The URL
//!   Standard binds a `URLSearchParams` instance to its `URL`'s query (the
//!   `.searchParams` accessor). That bidirectional wiring is the Phase 6 DOM
//!   layer; this module is the query-string ↔ pair-list arithmetic.
//! - Legacy encoding. Inputs are `&str` / `&[u8]` already; the WHATWG parser
//!   operates on bytes and decodes them as UTF-8 unconditionally (the modern
//!   web is UTF-8). The host hook handles any pre-decoding if ever needed.
//!
//! ## Why a separate module from `form_submission`
//!
//! [`form_submission`](crate::form_submission) is the HTML form *submission*
//! path: it walks a DOM entry list (with the `submitter` button excluded) and
//! encodes per the active `enctype`. The byte-level percent-encoding rules
//! are the same, but the `URLSearchParams` *object* is a different surface:
//! it parses an existing query string, exposes a mutable list, and
//! round-trips. Sharing the percent-encode alphabet would couple two
//! independent specs; the duplication is intentional and small.
//!
//! Reference: <https://url.spec.whatwg.org/#urlsearchparams>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// The mutable `URLSearchParams` surface (URL Standard § 5.4). An ordered
/// list of `(name, value)` pairs; methods mirror the WebIDL one-for-one so
/// the JS host hook is a thin wrapper.
///
/// Construct with [`UrlSearchParams::parse`] (or `Default` for an empty
/// instance), mutate with `append`/`set`/`delete`, and serialise with
/// [`UrlSearchParams::serialize`] (mirrors `URLSearchParams.prototype.toString`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UrlSearchParams {
    pairs: Vec<(String, String)>,
}

impl UrlSearchParams {
    /// Parse a query string (URL Standard § 5.2.4). A leading `?` is stripped
    /// if present; `&`-separated tuples each split on the first `=`.
    ///
    /// ```
    /// # use vixen_engine::url_search_params::UrlSearchParams;
    /// let p = UrlSearchParams::parse("?a=1&b=two");
    /// assert_eq!(p.get("a"), Some("1".to_string()));
    /// assert_eq!(p.get("b"), Some("two".to_string()));
    /// ```
    pub fn parse(input: &str) -> Self {
        Self {
            pairs: parse(input),
        }
    }

    /// Construct an empty `URLSearchParams`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The first value for `name`, or `None` (mirrors `URLSearchParams.prototype.get`).
    pub fn get(&self, name: &str) -> Option<String> {
        self.pairs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }

    /// Every value for `name`, in insertion order (`URLSearchParams.prototype.getAll`).
    pub fn get_all(&self, name: &str) -> Vec<String> {
        self.pairs
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// Whether any pair has `name` (`URLSearchParams.prototype.has(name)`).
    pub fn has(&self, name: &str) -> bool {
        self.pairs.iter().any(|(n, _)| n == name)
    }

    /// Whether the `(name, value)` pair exists (`URLSearchParams.prototype.has(name, value)`
    /// — the two-argument overload added to the spec later).
    pub fn has_pair(&self, name: &str, value: &str) -> bool {
        self.pairs.iter().any(|(n, v)| n == name && v == value)
    }

    /// Append `(name, value)` to the end (`URLSearchParams.prototype.append`).
    pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.pairs.push((name.into(), value.into()));
    }

    /// Remove every pair whose name matches, then append one
    /// (`URLSearchParams.prototype.set`). Preserves the position of the first
    /// removed occurrence, matching the spec ordering.
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        let mut inserted = false;
        let mut next: Vec<(String, String)> = Vec::with_capacity(self.pairs.len());
        for (n, v) in self.pairs.drain(..) {
            if n != name {
                next.push((n, v));
            } else if !inserted {
                // Replace the first occurrence in-place; drop the rest.
                next.push((name.to_string(), value.clone()));
                inserted = true;
            }
        }
        if !inserted {
            next.push((name.to_string(), value));
        }
        self.pairs = next;
    }

    /// Remove every pair whose name matches (`URLSearchParams.prototype.delete(name)`).
    pub fn delete(&mut self, name: &str) {
        self.pairs.retain(|(n, _)| n != name);
    }

    /// Remove every `(name, value)` pair
    /// (`URLSearchParams.prototype.delete(name, value)` — the two-argument
    /// overload added later).
    pub fn delete_pair(&mut self, name: &str, value: &str) {
        self.pairs.retain(|(n, v)| !(n == name && v == value));
    }

    /// Sort by name in code-unit order, stable for equal names
    /// (`URLSearchParams.prototype.sort`). Equal names keep their relative
    /// order, matching the spec's "sort in ascending order, with the pairs
    /// being compared by their names".
    pub fn sort(&mut self) {
        self.pairs.sort_by(|a, b| a.0.cmp(&b.0));
    }

    /// Number of pairs (mirrors `URLSearchParams.prototype.size`).
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Serialise back to a query string (without the leading `?`), per
    /// URL Standard § 5.3.4 (`URLSearchParams.prototype.toString`).
    ///
    /// ```
    /// # use vixen_engine::url_search_params::UrlSearchParams;
    /// let mut p = UrlSearchParams::parse("a=1&b=two");
    /// p.set("b", "three");
    /// assert_eq!(p.serialize(), "a=1&b=three");
    /// ```
    pub fn serialize(&self) -> String {
        serialize(self.pairs.iter().map(|(n, v)| (n.as_str(), v.as_str())))
    }

    /// Borrowing iterator over `(name, value)` pairs, in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.pairs.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }

    /// Owning iterator over `(name, value)` pairs (`URLSearchParams`'s
    /// `entries()` / the default iterator).
    pub fn entries(self) -> impl Iterator<Item = (String, String)> {
        self.pairs.into_iter()
    }

    /// Owning iterator over just names (`keys()`).
    pub fn keys(self) -> impl Iterator<Item = String> {
        self.pairs.into_iter().map(|(n, _)| n)
    }

    /// Owning iterator over just values (`values()`).
    pub fn values(self) -> impl Iterator<Item = String> {
        self.pairs.into_iter().map(|(_, v)| v)
    }
}

// ---------------------------------------------------------------------------
// Parser (URL Standard § 5.2.4)
// ---------------------------------------------------------------------------

/// Parse an `application/x-www-form-urlencoded` string into an ordered list
/// of `(name, value)` pairs (URL Standard § 5.2.4). Used by
/// [`UrlSearchParams::parse`]; exposed standalone so callers can keep the raw
/// list (e.g. for one-shot query-string inspection without owning a struct).
///
/// The leading `?`, if present, is stripped. Empty tuples (an `&` with nothing
/// between) are dropped, matching the spec ("if bytes is the empty byte
/// sequence, continue"). A tuple without `=` gets an empty value.
pub fn parse(input: &str) -> Vec<(String, String)> {
    // Strip a single leading `?` (URL Standard § 5.2.4 step 1, applied to the
    // "value" passed by the URL query constructor).
    let input = input.strip_prefix('?').unwrap_or(input);
    let mut out = Vec::new();
    for tuple in input.split('&') {
        if tuple.is_empty() {
            continue;
        }
        let (name, value) = match tuple.split_once('=') {
            Some((n, v)) => (n, v),
            None => (tuple, ""),
        };
        out.push((percent_decode_tf8(name), percent_decode_tf8(value)));
    }
    out
}

/// Percent-decode a byte slice as UTF-8 (URL Standard's "percent-decode" +
/// "UTF-8 decode without BOM"), applying the form-urlencoded parser's
/// `+` → SPACE rule to the RAW input bytes — before decoding — so an encoded
/// plus `%2B` (byte `0x2B` produced by the decode loop) survives untouched.
/// Doing the replacement after decoding would corrupt `%2B` into a space;
/// URL Standard § 5.2.4 specifies the swap happens while scanning the input.
///
/// Ill-formed `%XX` runs and non-UTF-8 bytes produce U+FFFD, matching the
/// WHATWG "UTF-8 decode" replacement rules (one U+FFFD per maximal ill-formed
/// subpart — `from_utf8_lossy` produces the same observable count).
fn percent_decode_tf8(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                decoded.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        // URL Standard § 5.2.4: a literal `+` in the RAW input becomes SPACE.
        // Done here — on the un-decoded byte — so `%2B` decoded above is left
        // alone. (A post-decode sweep would wrongly turn that `0x2B` into a
        // space and break `?k=a%2Bb` → `"a+b"`.)
        decoded.push(if b == b'+' { b' ' } else { b });
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Serializer (URL Standard § 5.3.4)
// ---------------------------------------------------------------------------

/// Serialise an ordered `(name, value)` list back to the
/// `application/x-www-form-urlencoded` string (URL Standard § 5.3.4). The
/// output has no leading `?`; the caller prepends it when writing to a URL.
///
/// Encoding rules (same byte set as `form_submission::encode_urlencoded`):
/// - SPACE → `+`.
/// - The URL-safe set `* - . _ 0-9 A-Z a-z` passes through unchanged.
/// - Every other byte becomes `%XX` with uppercase hex.
/// - Pairs joined with `&`; name/value joined with `=`.
///
/// Empty input serialises to the empty string. An empty *value* still emits
/// `name=` (so round-tripping preserves presence).
pub fn serialize<K, V, I>(pairs: I) -> String
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut out = String::new();
    let mut first = true;
    for (name, value) in pairs {
        if !first {
            out.push('&');
        }
        first = false;
        urlencoded_byte_encode(name.as_ref(), &mut out);
        out.push('=');
        urlencoded_byte_encode(value.as_ref(), &mut out);
    }
    out
}

/// The URL Standard "application/x-www-form-urlencoded" byte serializer.
/// Operates on UTF-8 bytes (`&str`); SPACE → `+`; the URL-safe set passes
/// through; every other byte is `%XX` uppercase.
fn urlencoded_byte_encode(input: &str, out: &mut String) {
    for &b in input.as_bytes() {
        match b {
            b' ' => out.push('+'),
            b'*' | b'-' | b'.' | b'_' => out.push(b as char),
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => out.push(b as char),
            _ => {
                out.push('%');
                push_upper_hex(b, out);
            }
        }
    }
}

fn push_upper_hex(b: u8, out: &mut String) {
    fn hex_digit(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            10..=15 => (b'A' + (n - 10)) as char,
            _ => unreachable!("hex digit out of range"),
        }
    }
    out.push(hex_digit(b >> 4));
    out.push(hex_digit(b & 0x0F));
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse ----------------------------------------------------------

    #[test]
    fn parse_strips_leading_question_mark() {
        assert_eq!(parse("?a=1"), vec![("a".into(), "1".into())]);
        assert_eq!(parse("a=1"), vec![("a".into(), "1".into())]);
        // Only one leading `?` is stripped.
        assert_eq!(parse("??a=1"), vec![("?a".into(), "1".into())]);
    }

    #[test]
    fn parse_multiple_pairs() {
        let p = parse("a=1&b=2&c=3");
        assert_eq!(
            p,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("c".into(), "3".into()),
            ]
        );
    }

    #[test]
    fn parse_value_without_equals_is_empty_string() {
        // URL Standard § 5.2.4: a tuple with no `=` gets an empty value.
        let p = parse("flag&key=val");
        assert_eq!(p[0], ("flag".to_string(), String::new()));
        assert_eq!(p[1], ("key".to_string(), "val".into()));
    }

    #[test]
    fn parse_split_on_first_equals_only() {
        // A value containing `=` keeps it (split_once, not split).
        let p = parse("eq=a=b");
        assert_eq!(p[0], ("eq".to_string(), "a=b".into()));
    }

    #[test]
    fn parse_plus_becomes_space() {
        assert_eq!(parse("q=hello+world")[0].1, "hello world");
    }

    #[test]
    fn parse_encoded_plus_survives_literal_plus_becomes_space() {
        // Regression: URL Standard § 5.2.4 applies `+` → SPACE to the RAW
        // input bytes BEFORE percent-decoding. So `%2B` (encoded plus) must
        // decode to `+` and survive, while a literal `+` becomes a space.
        assert_eq!(parse("k=%2B")[0].1, "+"); // encoded plus survives
        assert_eq!(parse("k=a+b")[0].1, "a b"); // literal plus → space
    }

    #[test]
    fn parse_encoded_plus_round_trips_through_serialize() {
        // `serialize` emits `%2B` for a `+`; re-parsing must yield `+` back,
        // not a space (the round-trip property that motivated the fix).
        let p = UrlSearchParams::parse("k=%2B");
        assert_eq!(p.serialize(), "k=%2B");
        assert_eq!(
            UrlSearchParams::parse(&p.serialize()).get("k"),
            Some("+".to_string())
        );
    }

    #[test]
    fn parse_percent_decoding() {
        assert_eq!(parse("email=a%40b.example")[0].1, "a@b.example");
        // Lowercase hex accepted on input (browsers accept both).
        assert_eq!(parse("k=%2f")[0].1, "/");
    }

    #[test]
    fn parse_percent_decoding_uppercase_hex() {
        assert_eq!(parse("k=%2F")[0].1, "/");
    }

    #[test]
    fn parse_utf8_multibyte_percent_decoded() {
        // é = U+00E9 = 0xC3 0xA9 in UTF-8.
        assert_eq!(parse("k=%C3%A9")[0].1, "é");
    }

    #[test]
    fn parse_invalid_percent_kept_literally() {
        // `%` not followed by two hex digits is preserved as `%`.
        assert_eq!(parse("k=a%")[0].1, "a%");
        assert_eq!(parse("k=%zz")[0].1, "%zz");
        assert_eq!(parse("k=%2")[0].1, "%2");
    }

    #[test]
    fn parse_invalid_utf8_replacement_char() {
        // 0xFF is not a valid UTF-8 byte sequence → U+FFFD (one per maximal
        // ill-formed subpart). `from_utf8_lossy` matches the WHATWG count.
        let v = &parse(&format!("k={}%FF", "%FF"))[0].1;
        assert!(v.contains('\u{FFFD}'), "got {v:?}");
    }

    #[test]
    fn parse_empty_tuples_dropped() {
        // `&&a=1&&` → only `a=1`.
        assert_eq!(
            parse("&&a=1&&b=2&&"),
            vec![("a".into(), "1".into()), ("b".into(), "2".into()),]
        );
    }

    #[test]
    fn parse_empty_input_yields_empty_list() {
        assert!(parse("").is_empty());
        assert!(parse("?").is_empty());
    }

    // --- serialize ------------------------------------------------------

    #[test]
    fn serialize_round_trip() {
        let pairs = [("a", "1"), ("b", "two")];
        assert_eq!(serialize(pairs), "a=1&b=two");
    }

    #[test]
    fn serialize_space_becomes_plus() {
        assert_eq!(serialize([("q", "hello world")]), "q=hello+world");
    }

    #[test]
    fn serialize_at_becomes_percent_40() {
        assert_eq!(serialize([("email", "a@b.test")]), "email=a%40b.test");
    }

    #[test]
    fn serialize_uppercase_hex() {
        assert_eq!(serialize([("k", "é")]), "k=%C3%A9");
    }

    #[test]
    fn serialize_url_safe_chars_pass_through() {
        assert_eq!(serialize([("k", "a*b-c.d_e")]), "k=a*b-c.d_e");
    }

    #[test]
    fn serialize_empty_value_emits_equals() {
        assert_eq!(serialize([("k", "")]), "k=");
    }

    #[test]
    fn serialize_empty_input_returns_empty_string() {
        let empty: [(&str, &str); 0] = [];
        assert_eq!(serialize(empty), "");
    }

    #[test]
    fn serialize_empty_name_emits_equals() {
        // Empty names are legal in URLSearchParams (`=value`).
        assert_eq!(serialize([("", "v")]), "=v");
    }

    // --- UrlSearchParams object surface --------------------------------

    #[test]
    fn get_returns_first_value() {
        let p = UrlSearchParams::parse("a=1&a=2&b=3");
        assert_eq!(p.get("a"), Some("1".to_string()));
        assert_eq!(p.get("b"), Some("3".to_string()));
        assert_eq!(p.get("missing"), None);
    }

    #[test]
    fn get_all_returns_every_value_in_order() {
        let p = UrlSearchParams::parse("a=1&b=2&a=3&a=4");
        assert_eq!(p.get_all("a"), vec!["1", "3", "4"]);
        assert_eq!(p.get_all("b"), vec!["2"]);
        assert!(p.get_all("z").is_empty());
    }

    #[test]
    fn has_and_has_pair() {
        let p = UrlSearchParams::parse("a=1&b=2");
        assert!(p.has("a"));
        assert!(!p.has("z"));
        assert!(p.has_pair("a", "1"));
        assert!(!p.has_pair("a", "2")); // name exists, value doesn't match
    }

    #[test]
    fn append_adds_to_end() {
        let mut p = UrlSearchParams::parse("a=1");
        p.append("a", "2");
        assert_eq!(p.get_all("a"), vec!["1", "2"]);
    }

    #[test]
    fn set_replaces_first_and_drops_rest_preserving_position() {
        let mut p = UrlSearchParams::parse("a=1&b=2&a=3&c=4");
        p.set("a", "new");
        // The first `a` slot is replaced; later `a`s dropped; order kept.
        assert_eq!(p.get_all("a"), vec!["new"]);
        let pairs: Vec<_> = p
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("a".into(), "new".into()),
                ("b".into(), "2".into()),
                ("c".into(), "4".into()),
            ]
        );
    }

    #[test]
    fn set_when_absent_appends_to_end() {
        let mut p = UrlSearchParams::parse("a=1");
        p.set("b", "2");
        assert_eq!(p.iter().count(), 2);
        assert_eq!(p.get("b"), Some("2".to_string()));
    }

    #[test]
    fn delete_removes_all_with_name() {
        let mut p = UrlSearchParams::parse("a=1&b=2&a=3");
        p.delete("a");
        assert!(!p.has("a"));
        assert!(p.has("b"));
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn delete_pair_removes_only_matching() {
        let mut p = UrlSearchParams::parse("a=1&a=2&a=3");
        p.delete_pair("a", "2");
        assert_eq!(p.get_all("a"), vec!["1", "3"]);
    }

    #[test]
    fn sort_orders_by_name_stably() {
        let mut p = UrlSearchParams::parse("b=1&a=1&b=2&a=2&c=1");
        p.sort();
        let names: Vec<_> = p.iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["a", "a", "b", "b", "c"]);
        // Stable: the first `a` (value "1") precedes the second (value "2").
        let a_vals: Vec<_> = p.get_all("a");
        assert_eq!(a_vals, vec!["1", "2"]);
    }

    #[test]
    fn size_reports_pair_count() {
        let p = UrlSearchParams::parse("a=1&b=2&c=3");
        assert_eq!(p.len(), 3);
        assert!(!p.is_empty());
        let empty = UrlSearchParams::new();
        assert!(empty.is_empty());
    }

    #[test]
    fn serialize_via_method_matches_free_function() {
        let p = UrlSearchParams::parse("name=Ada Lovelace&email=a@b.example");
        let s1 = p.serialize();
        let s2 = serialize(p.iter());
        assert_eq!(s1, s2);
        assert_eq!(s1, "name=Ada+Lovelace&email=a%40b.example");
    }

    #[test]
    fn full_round_trip_preserves_pairs() {
        // parse → serialize → parse yields the same pairs.
        let original = "a=1&b=hello+world&c=%40";
        let p = UrlSearchParams::parse(original);
        let reserialized = p.serialize();
        let p2 = UrlSearchParams::parse(&reserialized);
        let left: Vec<_> = p
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect();
        let right: Vec<_> = p2
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect();
        assert_eq!(left, right);
    }

    #[test]
    fn entries_keys_values_iterators() {
        let p = UrlSearchParams::parse("a=1&b=2");
        assert_eq!(
            p.clone().entries().collect::<Vec<_>>(),
            vec![("a".into(), "1".into()), ("b".into(), "2".into()),]
        );
        let p2 = UrlSearchParams::parse("a=1&b=2");
        assert_eq!(p2.keys().collect::<Vec<_>>(), vec!["a", "b"]);
        let p3 = UrlSearchParams::parse("a=1&b=2");
        assert_eq!(p3.values().collect::<Vec<_>>(), vec!["1", "2"]);
    }

    #[test]
    fn default_is_empty() {
        let p = UrlSearchParams::default();
        assert!(p.is_empty());
        assert_eq!(p.serialize(), "");
    }

    // --- Real-world URL query -------------------------------------------

    #[test]
    fn typical_search_query() {
        // A search-engine query string with mixed content.
        let p = UrlSearchParams::parse("?q=rust+web+browser&page=2&safe=active");
        assert_eq!(p.get("q"), Some("rust web browser".to_string()));
        assert_eq!(p.get("page"), Some("2".to_string()));
        assert_eq!(p.get("safe"), Some("active".to_string()));
        assert_eq!(p.len(), 3);
    }
}
