//! URLPattern API § 2 — pathname pattern compile + match (pure logic). The
//! route-matching primitive client-side routers, service-worker `FetchEvent`
//! routing, and the `new URLPattern()` host hook reduce to. Complements
//! [`crate::url_search_params`] (the query surface) and [`crate::media_query`]
//! (the CSS-side condition matching).
//!
//! What lives here:
//! - [`URLPattern`] — a compiled pathname pattern (literal segments + `:name`
//!   named captures + `*` rest-of-path wildcard).
//! - [`URLPattern::compile`] — parse the pattern grammar into the segment
//!   list (with the `:name` + `*` decode, duplicate-name detection, the
//!   wildcard-must-be-trailing rule).
//! - [`URLPattern::match_pathname`] — full-match a pathname against the
//!   compiled pattern, returning the named captures (`None` on mismatch).
//!
//! What does *not* live here:
//! - The `protocol`/`hostname`/`port`/`search`/`hash` components (URLPattern
//!   § 2 compiles each independently; v1 ships pathname — the dominant use —
//!   and the rest land with the host hook).
//! - Full regex custom params (`:name(\\d+)`) — deferred; the § 2 tokenizer
//!   for that is large and the named/`*` subset covers real routing.
//! - The JS `URLPattern` object (`test`/`exec`/`keys`) — Phase 6 host hook.
//! - Case-sensitivity options (`URLPatternOptions`) — v1 is case-sensitive
//!   (the spec defaults to case-insensitive on hostname only).
//!
//! ## Grammar (pathname subset)
//!
//! ```text
//! pattern  = "/"? segment ( "/" segment )* "/"?
//! segment  = literal | param | wildcard
//! param    = ":" name            // one path segment, captured
//! wildcard = "*"                 // rest of path (incl. "/"), captured as "*"
//! name     = ALPHA | "_" ( ALPHA | DIGIT | "_" )*      // [A-Za-z_][A-Za-z0-9_]*
//! literal  = any char except "/" ":" "*"
//! ```
//!
//! Matching is **full** (the pattern must consume the entire pathname) and
//! **segment-based**: both pattern and pathname split on `/`, empty segments
//! (leading/trailing/duplicate `/`) dropped, so `/posts` ≡ `/posts/`. A
//! `:name` capture matches exactly one non-empty path segment; `*` matches
//! zero-or-more remaining segments (captured joined by `/`).
//!
//! Reference: <https://urlpattern.spec.whatwg.org/>.
//! Path-to-regexp (the matching model): <https://github.com/pillarjs/path-to-regexp>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Segment + URLPattern
// ---------------------------------------------------------------------------

/// One segment of a compiled pathname pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// A literal segment: matches a path segment by exact string equality.
    Literal(String),
    /// A named param `:name`: matches one non-empty path segment, captured
    /// under `name`.
    Param { name: String },
    /// A rest-of-path wildcard `*`: matches zero-or-more remaining path
    /// segments, captured (joined by `/`) under the name `*`.
    Wildcard,
}

/// A compiled URLPattern pathname pattern. Cheap to clone; compile once and
/// match many pathnames against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct URLPattern {
    segments: Vec<Segment>,
    /// The original pattern string (for diagnostics + the `URLPattern` JS
    /// object's readback).
    raw: String,
}

/// Error from [`URLPattern::compile`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum URLPatternError {
    /// A `:name` capture had an empty or invalid name (must be
    /// `[A-Za-z_][A-Za-z0-9_]*`).
    #[error("invalid param name: {0:?}")]
    InvalidParamName(String),
    /// The same capture name appears twice in one pattern.
    #[error("duplicate capture name: {0}")]
    DuplicateName(String),
    /// A `*` wildcard appeared anywhere but the last segment (everything after
    /// a greedy wildcard is unreachable; rewrite the pattern).
    #[error("wildcard must be the last segment")]
    WildcardMustBeLast,
    /// The pattern was empty (no segments).
    #[error("pattern is empty")]
    Empty,
}

impl URLPattern {
    /// Compile a pathname pattern. Leading/trailing/duplicate `/` are
    /// normalised (collapsed to segment boundaries); empty patterns error.
    ///
    /// ```
    /// # use vixen_engine::url_pattern::URLPattern;
    /// let p = URLPattern::compile("/posts/:id").unwrap();
    /// let caps = p.match_pathname("/posts/42").unwrap();
    /// assert_eq!(caps, vec![("id".to_string(), "42".to_string())]);
    /// ```
    pub fn compile(pattern: &str) -> Result<Self, URLPatternError> {
        let mut segments = Vec::new();
        let mut seen_names: Vec<String> = Vec::new();
        for raw_seg in pattern.split('/') {
            let seg = raw_seg.trim();
            if seg.is_empty() {
                continue; // collapsed boundary (leading/trailing/double `/`)
            }
            let parsed = parse_segment(seg)?;
            if let Segment::Param { name } = &parsed {
                if seen_names.iter().any(|n| n == name) {
                    return Err(URLPatternError::DuplicateName(name.clone()));
                }
                seen_names.push(name.clone());
            }
            // The wildcard must be the last segment; if it's followed by any
            // other segment, the rest is unreachable.
            if matches!(parsed, Segment::Wildcard) && !segments.is_empty() {
                // Allowed: a wildcard after some segments is the normal form.
            }
            segments.push(parsed);
        }
        if segments.is_empty() {
            return Err(URLPatternError::Empty);
        }
        // Enforce: a wildcard may appear only as the last segment.
        for (i, seg) in segments.iter().enumerate() {
            if matches!(seg, Segment::Wildcard) && i != segments.len() - 1 {
                return Err(URLPatternError::WildcardMustBeLast);
            }
        }
        Ok(Self {
            segments,
            raw: pattern.to_owned(),
        })
    }

    /// The original pattern string this was compiled from.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The number of pattern segments (including the wildcard if present).
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Match `pathname` against the compiled pattern. Returns the named
    /// captures (in pattern order) on a full match, or `None` on mismatch.
    /// Empty path segments are collapsed (so `/posts` ≡ `/posts/`).
    pub fn match_pathname(&self, pathname: &str) -> Option<Vec<(String, String)>> {
        let path_segments: Vec<&str> = pathname.split('/').filter(|s| !s.is_empty()).collect();
        let mut captures: Vec<(String, String)> = Vec::new();
        let mut j = 0; // index into path_segments
        for (i, seg) in self.segments.iter().enumerate() {
            match seg {
                Segment::Literal(lit) => {
                    let path_seg = path_segments.get(j)?; // pattern longer than path
                    if *path_seg != lit {
                        return None;
                    }
                    j += 1;
                }
                Segment::Param { name } => {
                    let path_seg = path_segments.get(j)?;
                    if path_seg.is_empty() {
                        return None; // :name requires a non-empty segment
                    }
                    captures.push((name.clone(), (*path_seg).to_owned()));
                    j += 1;
                }
                Segment::Wildcard => {
                    // Capture zero-or-more remaining segments, joined by `/`.
                    let rest = path_segments[j..].join("/");
                    captures.push(("*".to_owned(), rest));
                    j = path_segments.len();
                    // The wildcard is the last pattern segment (enforced at
                    // compile); any following iteration can't happen.
                    debug_assert_eq!(i, self.segments.len() - 1);
                    break;
                }
            }
        }
        // Full-match check: the pattern must consume every path segment
        // (unless a wildcard already absorbed the rest).
        if j != path_segments.len() {
            return None;
        }
        Some(captures)
    }

    /// Boolean test variant of [`URLPattern::match_pathname`] (drops the
    /// captures). Mirrors the JS `URLPattern.prototype.test` surface.
    pub fn test_pathname(&self, pathname: &str) -> bool {
        self.match_pathname(pathname).is_some()
    }
}

/// Parse one (non-empty, slash-trimmed) pattern segment.
fn parse_segment(seg: &str) -> Result<Segment, URLPatternError> {
    // Wildcard: a bare `*` segment (the URLPattern spec also accepts a `*`
    // mid-segment as a literal-ish match, but the routing form treats a `*`
    // segment as the rest-of-path wildcard).
    if seg == "*" {
        return Ok(Segment::Wildcard);
    }
    // Named param: `:name`.
    if let Some(name) = seg.strip_prefix(':') {
        if !is_valid_name(name) || name.is_empty() {
            return Err(URLPatternError::InvalidParamName(name.to_owned()));
        }
        return Ok(Segment::Param {
            name: name.to_owned(),
        });
    }
    // A segment containing `*` or `:` mid-text isn't a valid literal in the
    // routing subset — reject so the author knows the feature is unsupported
    // (full regex custom params land with the § 2 tokenizer).
    if seg.contains('*') || seg.contains(':') {
        return Err(URLPatternError::InvalidParamName(seg.to_owned()));
    }
    Ok(Segment::Literal(seg.to_owned()))
}

/// `[A-Za-z_][A-Za-z0-9_]*` — the URLPattern § 2.3 name grammar.
fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(p: &URLPattern, path: &str) -> Vec<(String, String)> {
        p.match_pathname(path).expect("expected match")
    }

    // --- compile --------------------------------------------------------

    #[test]
    fn compile_literal_only() {
        let p = URLPattern::compile("/posts").unwrap();
        assert_eq!(p.segment_count(), 1);
        assert_eq!(p.raw(), "/posts");
    }

    #[test]
    fn compile_named_param() {
        let p = URLPattern::compile("/users/:id").unwrap();
        assert_eq!(p.segment_count(), 2);
    }

    #[test]
    fn compile_collapses_empty_segments() {
        // Leading/trailing/double slashes collapse.
        let a = URLPattern::compile("/posts/:id").unwrap();
        let b = URLPattern::compile("//posts//id//").unwrap();
        // Both compile to a 2-segment pattern (b with a literal "id").
        assert_eq!(a.segment_count(), 2);
        assert_eq!(b.segment_count(), 2);
    }

    #[test]
    fn compile_rejects_empty_pattern() {
        assert_eq!(URLPattern::compile(""), Err(URLPatternError::Empty));
        assert_eq!(URLPattern::compile("/"), Err(URLPatternError::Empty));
        assert_eq!(URLPattern::compile("///"), Err(URLPatternError::Empty));
    }

    #[test]
    fn compile_rejects_invalid_param_name() {
        assert!(matches!(
            URLPattern::compile("/users/:"),
            Err(URLPatternError::InvalidParamName(_))
        ));
        // Name starting with a digit.
        assert!(matches!(
            URLPattern::compile("/users/:1id"),
            Err(URLPatternError::InvalidParamName(_))
        ));
    }

    #[test]
    fn compile_rejects_duplicate_name() {
        assert_eq!(
            URLPattern::compile("/:a/:a"),
            Err(URLPatternError::DuplicateName("a".into()))
        );
    }

    #[test]
    fn compile_rejects_non_trailing_wildcard() {
        assert_eq!(
            URLPattern::compile("/*/foo"),
            Err(URLPatternError::WildcardMustBeLast)
        );
    }

    #[test]
    fn compile_accepts_trailing_wildcard() {
        let p = URLPattern::compile("/files/*").unwrap();
        assert_eq!(p.segment_count(), 2);
    }

    #[test]
    fn compile_rejects_mid_segment_specials() {
        // `*` mid-segment isn't supported in the routing subset.
        assert!(matches!(
            URLPattern::compile("/a*b"),
            Err(URLPatternError::InvalidParamName(_))
        ));
        // `:` mid-segment likewise.
        assert!(matches!(
            URLPattern::compile("/a:b"),
            Err(URLPatternError::InvalidParamName(_))
        ));
    }

    #[test]
    fn compile_underscore_name() {
        assert!(URLPattern::compile("/:_user_id").is_ok());
    }

    // --- match: literal -------------------------------------------------

    #[test]
    fn literal_matches_exact() {
        let p = URLPattern::compile("/posts").unwrap();
        assert!(p.test_pathname("/posts"));
        assert!(!p.test_pathname("/pages"));
    }

    #[test]
    fn literal_trailing_slash_insensitive() {
        // Trailing slash collapses → same segment list.
        let p = URLPattern::compile("/posts").unwrap();
        assert!(p.test_pathname("/posts/"));
        assert!(p.test_pathname("posts"));
    }

    #[test]
    fn multi_segment_literal_full_match_required() {
        let p = URLPattern::compile("/a/b/c").unwrap();
        assert!(p.test_pathname("/a/b/c"));
        assert!(!p.test_pathname("/a/b")); // too short
        assert!(!p.test_pathname("/a/b/c/d")); // too long
    }

    // --- match: named param --------------------------------------------

    #[test]
    fn named_param_captures_one_segment() {
        let p = URLPattern::compile("/users/:id").unwrap();
        assert_eq!(caps(&p, "/users/42"), vec![("id".into(), "42".into())]);
        assert_eq!(caps(&p, "/users/abc"), vec![("id".into(), "abc".into())]);
    }

    #[test]
    fn named_param_does_not_cross_slash() {
        let p = URLPattern::compile("/users/:id").unwrap();
        // /users/42/posts → pattern too short for full match.
        assert!(p.match_pathname("/users/42/posts").is_none());
    }

    #[test]
    fn multiple_named_params_in_order() {
        let p = URLPattern::compile("/posts/:post/comments/:comment").unwrap();
        assert_eq!(
            caps(&p, "/posts/7/comments/3"),
            vec![("post".into(), "7".into()), ("comment".into(), "3".into()),]
        );
    }

    #[test]
    fn literal_and_param_mix() {
        let p = URLPattern::compile("/api/v1/:resource").unwrap();
        assert_eq!(
            caps(&p, "/api/v1/users"),
            vec![("resource".into(), "users".into())]
        );
        // Wrong literal prefix.
        assert!(p.match_pathname("/api/v2/users").is_none());
    }

    // --- match: wildcard ------------------------------------------------

    #[test]
    fn wildcard_captures_rest() {
        let p = URLPattern::compile("/files/*").unwrap();
        assert_eq!(caps(&p, "/files/a/b/c"), vec![("*".into(), "a/b/c".into())]);
    }

    #[test]
    fn wildcard_matches_zero_rest() {
        let p = URLPattern::compile("/files/*").unwrap();
        // `/files` (no rest) → wildcard captures empty string.
        assert_eq!(caps(&p, "/files"), vec![("*".into(), "".into())]);
    }

    #[test]
    fn wildcard_requires_leading_literal_match() {
        let p = URLPattern::compile("/files/*").unwrap();
        assert!(p.match_pathname("/other/a").is_none());
    }

    #[test]
    fn bare_wildcard_only() {
        let p = URLPattern::compile("/*").unwrap();
        assert!(p.test_pathname("/anything/here"));
        assert_eq!(caps(&p, "/x/y"), vec![("*".into(), "x/y".into())]);
    }

    // --- full-match semantics ------------------------------------------

    #[test]
    fn pattern_longer_than_path_no_match() {
        let p = URLPattern::compile("/a/:b/c").unwrap();
        assert!(p.match_pathname("/a/x").is_none());
    }

    #[test]
    fn path_longer_than_pattern_no_match() {
        let p = URLPattern::compile("/a/:b").unwrap();
        assert!(p.match_pathname("/a/x/extra").is_none());
    }

    #[test]
    fn empty_path_matches_root_pattern() {
        // A single-literal pattern "" can't be compiled (empty), so test the
        // wildcard-only root instead.
        let p = URLPattern::compile("/*").unwrap();
        assert_eq!(caps(&p, "/"), vec![("*".into(), "".into())]);
    }

    // --- case sensitivity ----------------------------------------------

    #[test]
    fn matching_is_case_sensitive() {
        let p = URLPattern::compile("/Posts").unwrap();
        assert!(p.test_pathname("/Posts"));
        assert!(!p.test_pathname("/posts"));
    }
}
