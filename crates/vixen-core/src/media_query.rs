//! CSS Media Queries 4 evaluation — pure logic for the parenthesised
//! `<media-condition>` family that `<img sizes>` and `<source media>` (Phase 6
//! responsive-image selection) reduce against, plus the `MediaQueryList` and
//! `@media` Stylo boundary. The complement to [`crate::srcset`]: srcset parses
//! the candidate list, this module decides *which* candidate applies given the
//! viewport.
//!
//! What lives here:
//! - [`Viewport`] — the resolved media description the queries match against
//!   (size in CSS px, DPR, orientation, colour depth, hover/pointer,
//!   `prefers-color-scheme`, `prefers-reduced-motion`).
//! - [`MediaQuery`] — a parsed query (optional media-type prefix + a
//!   [`MediaCondition`] tree). [`MediaQuery::parse`] + [`MediaQuery::matches`].
//! - [`MediaCondition`] / [`MediaInParens`] / [`MediaFeature`] — the condition
//!   tree (§ 3 + § 4) with `and`/`or`/`not` over parenthesised features.
//!
//! What does *not* live here:
//! - The full CSS tokeniser (Stylo owns `@media` block parsing at the cascade
//!   boundary; this module is the value/projection layer the headless
//!   `--media` surface and the responsive-image selection consult).
//! - The `<mf-range>` two-value form `(600px <= width <= 800px)` — the § 4.3
//!   single-comparison forms cover every realistic `sizes` / `<source media>`
//!   authoring pattern; the two-value form is rare and Stylo handles it at the
//!   cascade boundary.
//! - Legacy `<general-enclosed>` (the spec's forward-compat escape hatch);
//!   matched as `false` so an unknown construct never accidentally fires.
//!
//! ## Grammar (CSS Media Queries 4 § 2 + § 3)
//!
//! ```text
//! <media-query>      = <media-condition>
//!                     | [ not | only ]? <media-type> [ and <media-condition> ]?
//! <media-condition>  = <media-not> | <media-and> | <media-or> | <media-in-parens>
//! <media-not>        = "not" <media-in-parens>
//! <media-and>        = <media-in-parens> ( "and" <media-in-parens> )+
//! <media-or>         = <media-in-parens> ( "or" <media-in-parens> )+
//! <media-in-parens>  = "(" <media-condition> ")" | <media-feature>
//! <media-feature>    = "(" <mf-name> [ ":" <mf-value> ]? ")"
//! ```
//!
//! Reference: <https://www.w3.org/TR/mediaqueries-4/>.

#![forbid(unsafe_code)]

use crate::length::{Length, LengthContext};
use crate::ratio::Ratio;
use crate::resolution::Resolution;

// ---------------------------------------------------------------------------
// Viewport — the media description queries match against
// ---------------------------------------------------------------------------

/// Screen orientation per CSS Media Queries 4 § 4.5. `Portrait` when the
/// viewport height ≥ width; `Landscape` otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Orientation {
    Portrait,
    Landscape,
}

/// The user's colour-scheme preference (CSS Color Adjust 1 § 4, surfaced via
/// the `prefers-color-scheme` media feature).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorScheme {
    /// The user has no preference (the spec's `no-preference` value, used when
    /// the host doesn't expose the OS setting).
    #[default]
    NoPreference,
    Light,
    Dark,
}

/// Coarse pointer type for the `pointer` media feature (CSS Media Queries 4
/// § 4.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PointerAccuracy {
    /// No pointing device at all.
    #[default]
    None,
    /// Fine pointer (mouse, trackpad).
    Fine,
    /// Coarse pointer (touch).
    Coarse,
}

/// The resolved viewport + environment description media queries match
/// against. Every field maps to at least one media feature; the responsive
/// selectors and the `MediaQueryList` host hook (Phase 6) both reduce against
/// one [`Viewport`].
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    /// Viewport width in CSS pixels.
    pub width_px: f64,
    /// Viewport height in CSS pixels.
    pub height_px: f64,
    /// Device pixel ratio (`resolution` / `min-resolution` reduce to this).
    pub dpr: f64,
    /// Screen orientation (derived from width/height unless overridden).
    pub orientation: Orientation,
    /// Colour depth in bits per component (`color` / `min-color` reduce to
    /// this). The browser default is `24`.
    pub color_bits: u32,
    /// Whether the user can hover over elements with the primary pointer
    /// (`hover` feature).
    pub hover: bool,
    /// Pointer accuracy (`pointer` feature).
    pub pointer: PointerAccuracy,
    /// User's colour-scheme preference (`prefers-color-scheme`).
    pub color_scheme: ColorScheme,
    /// Whether the user has requested reduced motion (`prefers-reduced-motion`
    /// feature).
    pub reduced_motion: bool,
}

impl Viewport {
    /// Construct a viewport with just dimensions + DPR; everything else gets
    /// sane browser defaults (24-bit colour, no hover override, light scheme,
    /// no reduced motion). Orientation is derived from width vs height.
    pub fn new(width_px: f64, height_px: f64, dpr: f64) -> Self {
        Self {
            width_px,
            height_px,
            dpr,
            orientation: if height_px >= width_px {
                Orientation::Portrait
            } else {
                Orientation::Landscape
            },
            color_bits: 24,
            hover: true,
            pointer: PointerAccuracy::Fine,
            color_scheme: ColorScheme::Light,
            reduced_motion: false,
        }
    }

    /// The aspect ratio of the viewport (`width / height`).
    pub fn aspect_ratio(self) -> f64 {
        if self.height_px == 0.0 {
            f64::INFINITY
        } else {
            self.width_px / self.height_px
        }
    }

    /// The [`LengthContext`] viewport-relative features resolve against.
    pub fn length_context(self) -> LengthContext {
        LengthContext {
            viewport_w: self.width_px.round().max(0.0) as u32,
            viewport_h: self.height_px.round().max(0.0) as u32,
            ..LengthContext::default()
        }
    }
}

impl Default for Viewport {
    fn default() -> Self {
        // Matches the headless/WPT-harness default in LengthContext (800x600).
        Self::new(800.0, 600.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// Feature values
// ---------------------------------------------------------------------------

/// A media-feature value: a length, a ratio, a resolution, an integer, or a
/// keyword (`landscape`, `dark`, `fine`, …). The parser dispatches on the
/// feature name (§ 4 "value type" column); this enum carries the resolved form.
#[derive(Debug, Clone, PartialEq)]
pub enum FeatureValue {
    /// A `<length>` value (`600px`, `40em`); resolved against the viewport.
    Length(Length),
    /// A `<ratio>` value (`16/9`).
    Ratio(Ratio),
    /// A `<resolution>` value (`2dppx`, `192dpi`).
    Resolution(Resolution),
    /// A bare integer (`24` for `color: 24`).
    Integer(i64),
    /// A bare keyword identifier (`landscape`, `dark`, `fine`, `reduce`).
    Keyword(String),
}

/// A single `<media-feature>` after the `min-`/`max-` prefix has been decoded
/// into a [`Range`] constraint. The § 4.3 range-mapping rule: `min-width` ≡
/// `width >=`, `max-width` ≡ `width <=`, and a bare feature name with a value
/// is `==`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// `min-*` — feature ≥ value.
    Min,
    /// `max-*` — feature ≤ value.
    Max,
    /// bare `*` — feature == value (or, for boolean features, feature "true").
    Exact,
}

/// A parsed `<media-feature>`: a name (`width`, `orientation`, …) plus an
/// optional value and a [`Range`] constraint derived from any `min-`/`max-`
/// prefix. A feature with `value == None` is the § 4.3 boolean form
/// (`(hover)`, `(inverted-colors)`).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFeature {
    /// The feature base name with any `min-`/`max-` prefix stripped.
    pub name: &'static str,
    /// The decoded range constraint.
    pub range: Range,
    /// The authored value, if any (`None` ⇒ boolean form).
    pub value: Option<FeatureValue>,
}

// ---------------------------------------------------------------------------
// The condition tree
// ---------------------------------------------------------------------------

/// A parenthesised operand inside a [`MediaCondition`]: either a nested
/// condition, or a single [`MediaFeature`].
#[derive(Debug, Clone, PartialEq)]
pub enum MediaInParens {
    Condition(Box<MediaCondition>),
    Feature(MediaFeature),
    /// `<general-enclosed>` — the spec's forward-compat escape hatch. Matches
    /// `false` so an unknown construct never accidentally fires (§ 3.2 last
    /// bullet: "User agents must evaluate … as `not all`").
    Unknown,
}

/// A `<media-condition>`: the `and`/`or`/`not` tree over [`MediaInParens`].
#[derive(Debug, Clone, PartialEq)]
pub enum MediaCondition {
    /// `not <in-parens>`.
    Not(MediaInParens),
    /// `<in-parens> and <in-parens> and …` (≥ 2 operands). All must match.
    And(Vec<MediaInParens>),
    /// `<in-parens> or <in-parens> or …` (≥ 2 operands). At least one matches.
    Or(Vec<MediaInParens>),
    /// A single parenthesised operand with no combinator.
    Single(MediaInParens),
}

impl MediaCondition {
    /// Evaluate the condition against a [`Viewport`].
    pub fn matches(&self, vp: &Viewport) -> bool {
        match self {
            MediaCondition::Not(p) => !p.matches(vp),
            MediaCondition::And(parts) => parts.iter().all(|p| p.matches(vp)),
            MediaCondition::Or(parts) => parts.iter().any(|p| p.matches(vp)),
            MediaCondition::Single(p) => p.matches(vp),
        }
    }
}

impl MediaInParens {
    /// Evaluate against a [`Viewport`].
    pub fn matches(&self, vp: &Viewport) -> bool {
        match self {
            MediaInParens::Condition(c) => c.matches(vp),
            MediaInParens::Feature(f) => f.matches(vp),
            // § 3.2: general-enclosed matches false.
            MediaInParens::Unknown => false,
        }
    }
}

impl MediaFeature {
    /// Evaluate against a [`Viewport`].
    pub fn matches(&self, vp: &Viewport) -> bool {
        // The § 4 "Media Features" table. Names are lowercased by the parser.
        match self.name {
            // --- Length-valued size features (§ 4.2, § 4.3) ---
            "width" => self.compare_length(vp.width_px, vp),
            "height" => self.compare_length(vp.height_px, vp),
            // --- Ratio-valued (§ 4.4) ---
            "aspect-ratio" => self.compare_ratio(vp.aspect_ratio()),
            // --- Orientation (§ 4.5) ---
            "orientation" => self.compare_keyword(match vp.orientation {
                Orientation::Portrait => "portrait",
                Orientation::Landscape => "landscape",
            }),
            // --- Resolution (§ 4.6) ---
            "resolution" => self.compare_resolution(vp.dpr),
            // --- Colour depth (§ 4.8). The § 4.3 boolean form `(color)` is
            // true iff the device has a non-zero colour depth. ---
            "color" => {
                if self.value.is_none() {
                    return vp.color_bits > 0;
                }
                self.compare_integer(vp.color_bits as i64)
            }
            // --- Interaction features (§ 4.10, § 4.11) ---
            "hover" => {
                if self.value.is_none() {
                    // § 4.3 boolean form `(hover)` — true iff hover is available.
                    return vp.hover;
                }
                let any = if vp.hover { "hover" } else { "none" };
                self.compare_keyword(any)
            }
            "pointer" => self.compare_keyword(match vp.pointer {
                PointerAccuracy::None => "none",
                PointerAccuracy::Fine => "fine",
                PointerAccuracy::Coarse => "coarse",
            }),
            // --- User preference (§ 5) ---
            "prefers-color-scheme" => self.compare_keyword(match vp.color_scheme {
                ColorScheme::NoPreference => "no-preference",
                ColorScheme::Light => "light",
                ColorScheme::Dark => "dark",
            }),
            "prefers-reduced-motion" => self.compare_keyword(if vp.reduced_motion {
                "reduce"
            } else {
                "no-preference"
            }),
            // Unknown feature ⇒ § 3.3 "evaluate to false".
            _ => false,
        }
    }

    fn compare_length(&self, viewport_px: f64, vp: &Viewport) -> bool {
        let Some(FeatureValue::Length(authored)) = &self.value else {
            return false;
        };
        let target = authored.to_px(&vp.length_context());
        match self.range {
            Range::Min => viewport_px >= target,
            Range::Max => viewport_px <= target,
            Range::Exact => (viewport_px - target).abs() < 1e-6,
        }
    }

    fn compare_ratio(&self, viewport_quotient: f64) -> bool {
        let Some(FeatureValue::Ratio(authored)) = &self.value else {
            return false;
        };
        let target = authored.quotient();
        match self.range {
            Range::Min => viewport_quotient >= target,
            Range::Max => viewport_quotient <= target,
            Range::Exact => (viewport_quotient - target).abs() < 1e-9,
        }
    }

    fn compare_resolution(&self, viewport_dppx: f64) -> bool {
        let Some(FeatureValue::Resolution(authored)) = &self.value else {
            return false;
        };
        let target = authored.to_dppx();
        match self.range {
            Range::Min => viewport_dppx >= target,
            Range::Max => viewport_dppx <= target,
            Range::Exact => (viewport_dppx - target).abs() < 1e-9,
        }
    }

    fn compare_integer(&self, viewport_value: i64) -> bool {
        let Some(FeatureValue::Integer(authored)) = self.value else {
            return false;
        };
        match self.range {
            Range::Min => viewport_value >= authored,
            Range::Max => viewport_value <= authored,
            Range::Exact => viewport_value == authored,
        }
    }

    fn compare_keyword(&self, viewport_kw: &str) -> bool {
        // For keyword features, only the Exact range makes sense; min-/max-
        // prefixes on a keyword feature are ignored per § 4 (the prefix is
        // only valid on numeric features).
        match &self.value {
            Some(FeatureValue::Keyword(authored)) => authored.eq_ignore_ascii_case(viewport_kw),
            // Boolean form `(hover)` — matches iff the feature is "on". For
            // the § 4 features that support boolean form, treat the authored
            // viewport state as the boolean.
            None => match self.name {
                "hover" => viewport_kw == "hover",
                _ => false,
            },
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Media type
// ---------------------------------------------------------------------------

/// The `<media-type>` of § 2. `All` is the implicit default when a query
/// starts with `(…)` rather than a bare type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MediaType {
    #[default]
    All,
    Screen,
    Print,
}

impl MediaType {
    /// Parse a `<media-type>` keyword (case-insensitive). Returns `None` for
    /// anything that isn't `all`/`screen`/`print` (so a feature name like
    /// `width` isn't mistaken for a media type).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Some(MediaType::All),
            "screen" => Some(MediaType::Screen),
            "print" => Some(MediaType::Print),
            _ => None,
        }
    }

    /// Whether the given viewport satisfies this media type. A web browser
    /// shell is always `Screen`/`All`; `Print` is only satisfied when the
    /// viewport is flagged as a print context (Vixen surfaces this via a
    /// dedicated `Viewport` flag in a later slice — for now print matches
    /// `false` against a screen viewport).
    pub fn matches(self, vp: &Viewport) -> bool {
        match self {
            MediaType::All | MediaType::Screen => true,
            MediaType::Print => {
                // TODO(Phase 6): consult a `print_mode` flag on Viewport.
                let _ = vp;
                false
            }
        }
    }
}

// ---------------------------------------------------------------------------
// MediaQuery — the top-level parsed form
// ---------------------------------------------------------------------------

/// A parsed CSS `<media-query>`. Either a bare condition, or a media type
/// (with optional `not`/`only` prefix) followed by an optional condition.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaQuery {
    /// `true` when the query was authored as `not <type>` — inverts the
    /// whole-query result (§ 2: "`not screen and (min-width: 600px)`" ≡
    /// `!screen || !(width >= 600px)`).
    pub negate: bool,
    /// The media type. `All` for a bare `(…)` query.
    pub media_type: MediaType,
    /// The trailing condition, if any.
    pub condition: Option<MediaCondition>,
}

impl MediaQuery {
    /// Parse a `<media-query>` string. Empty / whitespace-only input parses
    /// to a query that matches everything (`<media-type> = all`, no condition,
    /// no negation) — matching the § 2 "not all" production's inverse.
    pub fn parse(input: &str) -> Result<Self, MediaQueryError> {
        let tokens = tokenize(input)?;
        let mut p = Parser::new(&tokens);
        let q = p.parse_query()?;
        // Trailing tokens after the query ⇒ malformed.
        if !p.at_end() {
            return Err(MediaQueryError::TrailingTokens);
        }
        Ok(q)
    }

    /// Evaluate the query against a [`Viewport`].
    pub fn matches(&self, vp: &Viewport) -> bool {
        let type_ok = self.media_type.matches(vp);
        let cond_ok = self.condition.as_ref().is_none_or(|c| c.matches(vp));
        let result = type_ok && cond_ok;
        if self.negate { !result } else { result }
    }
}

/// Parse error for [`MediaQuery::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaQueryError {
    #[error("empty media query")]
    Empty,
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("expected ')' to close a parenthesised group")]
    UnclosedParen,
    #[error("expected ':' or ')' inside media feature")]
    MalformedFeature,
    #[error("trailing tokens after media query")]
    TrailingTokens,
    #[error("invalid media-feature value: {0:?}")]
    InvalidValue(String),
}

// ---------------------------------------------------------------------------
// Tokeniser
// ---------------------------------------------------------------------------

/// A flat token the recursive-descent parser consumes. Whitespace is
/// significant only between words (it separates `and`/`or`/`not` keywords from
/// operands) so the tokeniser records it as a token.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    LParen,
    RParen,
    Colon,
    /// A whitespace run (≥ 1 ASCII-whitespace char).
    Whitespace,
    /// Any run of non-structural, non-whitespace chars (`min-width`,
    /// `600px`, `landscape`, `and`, `16/9`, …).
    Word(String),
}

/// ASCII whitespace per CSS (§ 1.2 of Media Queries 4 inherits CSS 2.1).
fn is_css_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

fn tokenize(input: &str) -> Result<Vec<Tok>, MediaQueryError> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if is_css_ws(b) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_css_ws(bytes[i]) {
                i += 1;
            }
            // Collapse to a single whitespace token.
            let _ = start;
            out.push(Tok::Whitespace);
            continue;
        }
        match b {
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b':' => {
                out.push(Tok::Colon);
                i += 1;
            }
            _ => {
                // A word runs until whitespace or a structural char. The
                // `<ratio>` form `16/9` is a single word because `/` is not
                // structural here.
                let start = i;
                i += 1;
                while i < bytes.len()
                    && !is_css_ws(bytes[i])
                    && !matches!(bytes[i], b'(' | b')' | b':')
                {
                    i += 1;
                }
                let word = std::str::from_utf8(&bytes[start..i])
                    .map_err(|_| MediaQueryError::MalformedFeature)?
                    .to_owned();
                out.push(Tok::Word(word));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Recursive-descent parser
// ---------------------------------------------------------------------------

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(toks: &'a [Tok]) -> Self {
        Self { toks, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    fn peek(&self) -> Option<&'a Tok> {
        self.toks.get(self.pos)
    }

    fn skip_ws(&mut self) {
        while self.peek() == Some(&Tok::Whitespace) {
            self.pos += 1;
        }
    }

    /// If the next non-ws token is the keyword `kw` (case-insensitive),
    /// consume it and return `true`.
    fn consume_keyword(&mut self, kw: &str) -> bool {
        self.skip_ws();
        if let Some(Tok::Word(w)) = self.peek()
            && w.eq_ignore_ascii_case(kw)
        {
            self.pos += 1;
            return true;
        }
        false
    }

    fn parse_query(&mut self) -> Result<MediaQuery, MediaQueryError> {
        // Strip leading whitespace; empty input ⇒ the "matches everything"
        // query.
        self.skip_ws();
        if self.at_end() {
            return Ok(MediaQuery {
                negate: false,
                media_type: MediaType::All,
                condition: None,
            });
        }

        // Optional `not`/`only` prefix + `<media-type>`. The grammar ambiguity
        // (§ 2 vs § 3): `not` may either negate a `<media-type>` (`not screen`)
        // or introduce a `<media-condition>` (`not (…)`). Disambiguate by what
        // follows: a `<media-type>` word ⇒ type negation; a `(` or a
        // combinator ⇒ bare condition (unconsume `not`).
        let mut negate = false;
        let mut media_type = MediaType::All;
        let mut saw_type = false;
        let not_start = self.pos;
        if self.consume_keyword("not") {
            self.skip_ws();
            match self.peek() {
                Some(Tok::Word(w)) if MediaType::parse(w).is_some() => {
                    negate = true;
                    media_type = MediaType::parse(w).unwrap();
                    self.pos += 1;
                    saw_type = true;
                }
                // `not (` or `not <combinator>` ⇒ the `not` belongs to a bare
                // condition. Rewind to before `not` so parse_condition() sees
                // the whole thing.
                _ => {
                    self.pos = not_start;
                }
            }
        } else if self.consume_keyword("only") {
            // `only` is a legacy prefix; ignored for matching. Must be
            // followed by a media type.
            self.skip_ws();
            if let Some(Tok::Word(w)) = self.peek()
                && let Some(t) = MediaType::parse(w)
            {
                media_type = t;
                self.pos += 1;
                saw_type = true;
            }
        } else if let Some(Tok::Word(w)) = self.peek()
            && let Some(t) = MediaType::parse(w)
        {
            media_type = t;
            self.pos += 1;
            saw_type = true;
        }

        // After a media type: optional `and <media-condition>`.
        let condition = if saw_type {
            if self.consume_keyword("and") {
                Some(self.parse_condition_without_or()?)
            } else {
                None
            }
        } else {
            // No media type ⇒ the query is a bare `<media-condition>`.
            Some(self.parse_condition()?)
        };

        Ok(MediaQuery {
            negate,
            media_type,
            condition,
        })
    }

    /// `<media-condition>` — allows `or`.
    fn parse_condition(&mut self) -> Result<MediaCondition, MediaQueryError> {
        // `not <in-parens>`
        if self.consume_keyword("not") {
            let inner = self.parse_in_parens()?;
            return Ok(MediaCondition::Not(inner));
        }
        let first = self.parse_in_parens()?;
        // `and`-chain or `or`-chain or single.
        self.skip_ws();
        if self.peek_keyword() == Some("and") {
            let mut parts = vec![first];
            while self.consume_keyword("and") {
                parts.push(self.parse_in_parens()?);
            }
            return Ok(MediaCondition::And(parts));
        }
        if self.peek_keyword() == Some("or") {
            let mut parts = vec![first];
            while self.consume_keyword("or") {
                parts.push(self.parse_in_parens()?);
            }
            return Ok(MediaCondition::Or(parts));
        }
        Ok(MediaCondition::Single(first))
    }

    /// `<media-condition-without-or>` — forbids top-level `or` (only valid
    /// after a media type per § 2.5).
    fn parse_condition_without_or(&mut self) -> Result<MediaCondition, MediaQueryError> {
        if self.consume_keyword("not") {
            let inner = self.parse_in_parens()?;
            return Ok(MediaCondition::Not(inner));
        }
        let first = self.parse_in_parens()?;
        self.skip_ws();
        if self.peek_keyword() == Some("and") {
            let mut parts = vec![first];
            while self.consume_keyword("and") {
                parts.push(self.parse_in_parens()?);
            }
            return Ok(MediaCondition::And(parts));
        }
        Ok(MediaCondition::Single(first))
    }

    /// Peek the next non-ws token as a lowercased keyword, without consuming.
    fn peek_keyword(&mut self) -> Option<&str> {
        let saved = self.pos;
        self.skip_ws();
        let kw = match self.peek() {
            Some(Tok::Word(w)) => Some(w.as_str()),
            _ => None,
        };
        self.pos = saved;
        kw
    }

    /// `<media-in-parens>`: `( <media-condition> )` | `( <media-feature> )`.
    fn parse_in_parens(&mut self) -> Result<MediaInParens, MediaQueryError> {
        self.skip_ws();
        if self.peek() != Some(&Tok::LParen) {
            return Err(MediaQueryError::MalformedFeature);
        }
        self.pos += 1; // consume '('
        self.skip_ws();

        // If the first thing after '(' is another '(', it's a nested
        // condition (or a `not`/`and`/`or` combinator).
        if self.peek() == Some(&Tok::LParen) || self.peek_keyword_is_combinator() {
            let cond = self.parse_condition()?;
            self.skip_ws();
            if self.peek() != Some(&Tok::RParen) {
                return Err(MediaQueryError::UnclosedParen);
            }
            self.pos += 1;
            return Ok(MediaInParens::Condition(Box::new(cond)));
        }

        // Otherwise a media feature: `<word> [':' <value>]? ')'`.
        let feature = self.parse_feature()?;
        self.skip_ws();
        if self.peek() != Some(&Tok::RParen) {
            return Err(MediaQueryError::UnclosedParen);
        }
        self.pos += 1;
        Ok(MediaInParens::Feature(feature))
    }

    fn peek_keyword_is_combinator(&mut self) -> bool {
        matches!(self.peek_keyword(), Some("not") | Some("and") | Some("or"))
    }

    /// Parse the inside of a media-feature: `<name> [':' <value>]?`. The
    /// closing `)` is consumed by the caller.
    fn parse_feature(&mut self) -> Result<MediaFeature, MediaQueryError> {
        let name_tok = match self.peek() {
            Some(Tok::Word(w)) => w.clone(),
            _ => return Err(MediaQueryError::MalformedFeature),
        };
        self.pos += 1;
        // `min-`/`max-` prefix decode.
        let (base_name, range) = decode_range_prefix(&name_tok);
        let static_name =
            static_feature_name(base_name).ok_or(MediaQueryError::MalformedFeature)?;
        self.skip_ws();
        // Boolean form: `(hover)` — no value.
        if self.peek() == Some(&Tok::RParen) {
            return Ok(MediaFeature {
                name: static_name,
                range,
                value: None,
            });
        }
        if self.peek() != Some(&Tok::Colon) {
            return Err(MediaQueryError::MalformedFeature);
        }
        self.pos += 1; // consume ':'
        self.skip_ws();
        let value_tok = match self.peek() {
            Some(Tok::Word(w)) => w.clone(),
            _ => return Err(MediaQueryError::MalformedFeature),
        };
        self.pos += 1;
        let value = parse_feature_value(static_name, &value_tok)
            .ok_or_else(|| MediaQueryError::InvalidValue(value_tok.clone()))?;
        Ok(MediaFeature {
            name: static_name,
            range,
            value: Some(value),
        })
    }
}

/// Strip a `min-`/`max-` prefix, returning `(base_name, Range)`. A bare
/// feature name gets [`Range::Exact`]. The returned `&str` aliases the input
/// (not the lowercased temporary) so callers can hand it back as `&'static`.
fn decode_range_prefix(name: &str) -> (&str, Range) {
    // Compare case-insensitively, but slice the original so we don't return a
    // reference into a temporary `String`.
    if name
        .get(0..4)
        .is_some_and(|p| p.eq_ignore_ascii_case("min-"))
    {
        return (&name[4..], Range::Min);
    }
    if name
        .get(0..4)
        .is_some_and(|p| p.eq_ignore_ascii_case("max-"))
    {
        return (&name[4..], Range::Max);
    }
    (name, Range::Exact)
}

/// Map a feature base name to its canonical static-lifetime spelling. This
/// validates that the feature is known (unknown ⇒ `None` ⇒ parse error, which
/// the § 3.3 "unknown feature" rule reduces to a non-matching query).
fn static_feature_name(base: &str) -> Option<&'static str> {
    // Lowercase-compare against the known set.
    let known: &[&str] = &[
        "width",
        "height",
        "aspect-ratio",
        "orientation",
        "resolution",
        "color",
        "hover",
        "pointer",
        "prefers-color-scheme",
        "prefers-reduced-motion",
    ];
    known.iter().copied().find(|k| base.eq_ignore_ascii_case(k))
}

/// Parse a feature value given the (decoded) feature name. Returns `None` for
/// a value that doesn't fit the feature's type (e.g. `width: landscape`).
fn parse_feature_value(name: &str, raw: &str) -> Option<FeatureValue> {
    match name {
        // Length-valued features.
        "width" | "height" => Length::parse(raw).ok().map(FeatureValue::Length),
        // Ratio-valued features.
        "aspect-ratio" => Ratio::parse(raw).ok().map(FeatureValue::Ratio),
        // Resolution-valued features.
        "resolution" => Resolution::parse(raw).ok().map(FeatureValue::Resolution),
        // Integer-valued features.
        "color" => raw.parse::<i64>().ok().map(FeatureValue::Integer),
        // Keyword-valued features — accept any identifier; the comparator
        // checks the specific keyword.
        "orientation" | "hover" | "pointer" | "prefers-color-scheme" | "prefers-reduced-motion" => {
            // Validate it's a bare identifier (no digits/units). We accept
            // anything that doesn't look like a number/unit pair; the match
            // step rejects unknown keywords by failing to compare equal.
            if raw.is_empty() {
                return None;
            }
            Some(FeatureValue::Keyword(raw.to_ascii_lowercase()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(w: f64, h: f64) -> Viewport {
        Viewport::new(w, h, 1.0)
    }

    // --- Width / min-width / max-width ---------------------------------

    #[test]
    fn min_width_matches_at_boundary() {
        // § 4.3: `min-width: 600px` ≡ `width >= 600px`. The boundary is
        // inclusive (matches at exactly 600).
        let q = MediaQuery::parse("(min-width: 600px)").unwrap();
        assert!(q.matches(&vp(600.0, 400.0)));
        assert!(q.matches(&vp(800.0, 600.0)));
        assert!(!q.matches(&vp(599.0, 400.0)));
    }

    #[test]
    fn max_width_matches_at_boundary() {
        let q = MediaQuery::parse("(max-width: 799px)").unwrap();
        assert!(q.matches(&vp(799.0, 400.0)));
        assert!(q.matches(&vp(100.0, 100.0)));
        assert!(!q.matches(&vp(800.0, 600.0)));
    }

    #[test]
    fn bare_width_is_exact_equality() {
        let q = MediaQuery::parse("(width: 800px)").unwrap();
        assert!(q.matches(&vp(800.0, 600.0)));
        assert!(!q.matches(&vp(801.0, 600.0)));
    }

    #[test]
    fn height_feature_matches() {
        let q = MediaQuery::parse("(min-height: 600px)").unwrap();
        assert!(q.matches(&vp(800.0, 600.0)));
        assert!(!q.matches(&vp(800.0, 599.0)));
    }

    #[test]
    fn length_units_resolve_against_viewport() {
        // `em` resolves via the default 16px font.
        let q = MediaQuery::parse("(min-width: 37.5em)").unwrap();
        assert!(q.matches(&vp(600.0, 400.0))); // 37.5 * 16 = 600
        assert!(!q.matches(&vp(599.9, 400.0)));
    }

    // --- Orientation + ratio --------------------------------------------

    #[test]
    fn orientation_keyword_landscape() {
        let q = MediaQuery::parse("(orientation: landscape)").unwrap();
        assert!(q.matches(&vp(800.0, 600.0))); // w > h
        assert!(!q.matches(&vp(600.0, 800.0))); // h > w
    }

    #[test]
    fn orientation_keyword_portrait() {
        let q = MediaQuery::parse("(orientation: portrait)").unwrap();
        assert!(q.matches(&vp(600.0, 800.0)));
        assert!(!q.matches(&vp(800.0, 600.0)));
    }

    #[test]
    fn aspect_ratio_min() {
        let q = MediaQuery::parse("(min-aspect-ratio: 16/9)").unwrap();
        assert!(q.matches(&vp(1920.0, 1080.0))); // 16:9 exactly
        assert!(q.matches(&vp(2000.0, 1000.0))); // 2:1 > 16:9
        assert!(!q.matches(&vp(4.0, 3.0)));
    }

    #[test]
    fn aspect_ratio_single_number_form() {
        // § 4.4 of CSS Values 4: `2` ≡ `2/1`.
        let q = MediaQuery::parse("(min-aspect-ratio: 2)").unwrap();
        assert!(q.matches(&vp(2000.0, 1000.0)));
    }

    // --- Resolution + color ---------------------------------------------

    #[test]
    fn min_resolution_dppx() {
        let q = MediaQuery::parse("(min-resolution: 2dppx)").unwrap();
        assert!(q.matches(&Viewport::new(800.0, 600.0, 2.0)));
        assert!(!q.matches(&Viewport::new(800.0, 600.0, 1.5)));
    }

    #[test]
    fn resolution_x_alias() {
        // `x` is the alias for `dppx`.
        let q = MediaQuery::parse("(min-resolution: 2x)").unwrap();
        assert!(q.matches(&Viewport::new(800.0, 600.0, 2.0)));
    }

    #[test]
    fn color_feature_exact() {
        let q = MediaQuery::parse("(color)").unwrap();
        // Boolean form is true iff color depth > 0.
        assert!(q.matches(&vp(800.0, 600.0)));
    }

    #[test]
    fn min_color_feature() {
        let q = MediaQuery::parse("(min-color: 8)").unwrap();
        assert!(q.matches(&vp(800.0, 600.0))); // default 24 >= 8
    }

    // --- Media type + prefix -------------------------------------------

    #[test]
    fn bare_screen_matches_screen_viewport() {
        let q = MediaQuery::parse("screen").unwrap();
        assert!(q.matches(&vp(800.0, 600.0)));
    }

    #[test]
    fn print_does_not_match_screen_viewport() {
        let q = MediaQuery::parse("print").unwrap();
        assert!(!q.matches(&vp(800.0, 600.0)));
    }

    #[test]
    fn screen_and_condition() {
        let q = MediaQuery::parse("screen and (min-width: 600px)").unwrap();
        assert!(q.matches(&vp(800.0, 600.0)));
        assert!(!q.matches(&vp(500.0, 400.0)));
    }

    #[test]
    fn not_screen_inverts_type() {
        let q = MediaQuery::parse("not screen").unwrap();
        assert!(!q.matches(&vp(800.0, 600.0)));
        assert_eq!(q.media_type, MediaType::Screen);
        assert!(q.negate);
    }

    #[test]
    fn only_prefix_is_accepted() {
        let q = MediaQuery::parse("only screen and (max-width: 100px)").unwrap();
        assert!(!q.matches(&vp(800.0, 600.0)));
    }

    #[test]
    fn all_matches_everything() {
        let q = MediaQuery::parse("all").unwrap();
        assert!(q.matches(&vp(1.0, 1.0)));
    }

    // --- Combinators: and / or / not -----------------------------------

    #[test]
    fn and_combinator_requires_all() {
        let q = MediaQuery::parse("(min-width: 600px) and (max-width: 1200px)").unwrap();
        assert!(q.matches(&vp(800.0, 600.0)));
        assert!(!q.matches(&vp(500.0, 400.0)));
        assert!(!q.matches(&vp(1300.0, 400.0)));
    }

    #[test]
    fn or_combinator_requires_any() {
        let q = MediaQuery::parse("(max-width: 400px) or (min-width: 1000px)").unwrap();
        assert!(q.matches(&vp(300.0, 400.0)));
        assert!(q.matches(&vp(1200.0, 400.0)));
        assert!(!q.matches(&vp(800.0, 600.0)));
    }

    #[test]
    fn not_inverts_single_condition() {
        let q = MediaQuery::parse("not (min-width: 600px)").unwrap();
        assert!(!q.matches(&vp(800.0, 600.0)));
        assert!(q.matches(&vp(500.0, 400.0)));
    }

    #[test]
    fn nested_parens_group_condition() {
        let q = MediaQuery::parse("(not (min-width: 600px)) and (max-width: 800px)").unwrap();
        // 300px: `not (>=600)` = true, `<=800` = true ⇒ AND = true.
        assert!(q.matches(&vp(300.0, 400.0)));
        // 700px: `not (>=600)` = false ⇒ AND = false.
        assert!(!q.matches(&vp(700.0, 400.0)));
        // 500px: `not (>=600)` = true, `<=800` = true ⇒ AND = true.
        assert!(q.matches(&vp(500.0, 400.0)));
    }

    // --- User-preference + interaction features ------------------------

    #[test]
    fn prefers_color_scheme_dark() {
        let mut v = vp(800.0, 600.0);
        v.color_scheme = ColorScheme::Dark;
        let q = MediaQuery::parse("(prefers-color-scheme: dark)").unwrap();
        assert!(q.matches(&v));
        assert!(!q.matches(&vp(800.0, 600.0))); // default light
    }

    #[test]
    fn hover_boolean_form() {
        let q = MediaQuery::parse("(hover)").unwrap();
        assert!(q.matches(&vp(800.0, 600.0))); // default hover = true
        let mut v = vp(800.0, 600.0);
        v.hover = false;
        assert!(!q.matches(&v));
    }

    #[test]
    fn pointer_coarse() {
        let mut v = vp(800.0, 600.0);
        v.pointer = PointerAccuracy::Coarse;
        let q = MediaQuery::parse("(pointer: coarse)").unwrap();
        assert!(q.matches(&v));
        assert!(!q.matches(&vp(800.0, 600.0))); // default fine
    }

    #[test]
    fn prefers_reduced_motion_reduce() {
        let mut v = vp(800.0, 600.0);
        v.reduced_motion = true;
        let q = MediaQuery::parse("(prefers-reduced-motion: reduce)").unwrap();
        assert!(q.matches(&v));
        assert!(!q.matches(&vp(800.0, 600.0)));
    }

    // --- Unknown / malformed -------------------------------------------

    #[test]
    fn unknown_feature_never_matches() {
        let q = MediaQuery::parse("(made-up-feature: 42px)").unwrap_err();
        // Unknown features fail to parse (§ 3.3 ⇒ query is `not all`).
        let _ = q;
    }

    #[test]
    fn empty_query_matches_everything() {
        let q = MediaQuery::parse("").unwrap();
        assert!(q.matches(&vp(1.0, 1.0)));
        assert!(q.matches(&vp(99999.0, 99999.0)));
    }

    #[test]
    fn whitespace_only_query_matches_everything() {
        let q = MediaQuery::parse("   \t  ").unwrap();
        assert!(q.matches(&vp(1.0, 1.0)));
    }

    #[test]
    fn unclosed_paren_is_an_error() {
        assert!(matches!(
            MediaQuery::parse("(min-width: 600px"),
            Err(MediaQueryError::UnclosedParen)
        ));
    }

    #[test]
    fn trailing_tokens_is_an_error() {
        assert!(matches!(
            MediaQuery::parse("screen extra junk)"),
            Err(MediaQueryError::TrailingTokens) | Err(MediaQueryError::UnclosedParen)
        ));
    }

    // --- Orientation derivation ----------------------------------------

    #[test]
    fn orientation_derived_from_dimensions() {
        assert_eq!(vp(800.0, 600.0).orientation, Orientation::Landscape);
        assert_eq!(vp(600.0, 800.0).orientation, Orientation::Portrait);
        // Equal ⇒ portrait per the spec's `>=` rule (square is portrait).
        assert_eq!(vp(600.0, 600.0).orientation, Orientation::Portrait);
    }

    #[test]
    fn aspect_ratio_helper() {
        let v = vp(1600.0, 900.0);
        assert!((v.aspect_ratio() - 16.0 / 9.0).abs() < 1e-9);
    }
}
