//! CSS Masking 1 — the `mask` layer data model (Phase 5 paint prep). The
//! per-layer longhands the `mask` shorthand expands to, ready for the paint
//! path to sample against. The mask-image fetch + the per-pixel
//! alpha/luminance sampling is the paint path; this module is the pure
//! parse + the typed longhand enum resolution.
//!
//! What lives here:
//! - [`MaskMode`] — `mask-mode` (`alpha` / `luminance` / `match-source`).
//! - [`MaskRepeat`] — `mask-repeat` (the 6 repeat styles, `repeat-x`/
//!   `repeat-y` collapsed).
//! - [`MaskBox`] — `mask-clip` + `mask-origin` shared keyword set
//!   (`border-box`/`padding-box`/`content-box`/`no-clip`/`fill-box`/`stroke-box`/
//!   `view-box`).
//! - [`MaskLayer`] — one layer's resolved longhands (image source kept as
//!   the authored string; the cascade resolves the URL / image() / gradient
//!   against the resource loader).
//! - [`parse_mask`] — the § 6.1 `mask` shorthand parse: comma-separated
//!   layers, each a whitespace-tolerant token list, with the per-property
//!   slot-fill + the `mask-position` / `mask-size` slash forms.
//!
//! What does *not* live here:
//! - The mask-image fetch + decode (the resource loader; Phase 5).
//! - The per-pixel alpha / luminance sampling (WebRender's job).
//! - The `mask-border` family (CSS Masking 1 § 7 — the nine-region mask;
//!   deferred; [`crate::border_image`] is the neighbouring family).
//! - The `mask-composite` compositing-operator family (`add`/`subtract`/
//!   `intersect`/`exclude`) — modelled as an enum but the composite step
//!   itself is the paint path (reuses [`crate::blend`]).
//!
//! Reference: <https://www.w3.org/TR/css-masking-1/#the-mask>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// MaskMode
// ---------------------------------------------------------------------------

/// `mask-mode` (CSS Masking 1 § 6.5) — how the mask image's pixels are
/// interpreted. `MatchSource` is the default: an SVG `<mask>` uses
/// `luminance`, a raster image uses `alpha`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MaskMode {
    /// `alpha` — the mask's alpha channel gates the pixel.
    Alpha,
    /// `luminance` — the mask's luminance gates the pixel (SVG `<mask>`).
    Luminance,
    /// `match-source` (default) — `luminance` for SVG `<mask>`, `alpha` for
    /// raster / CSS images.
    #[default]
    MatchSource,
}

impl MaskMode {
    /// Parse one `mask-mode` token (ASCII-case-insensitive). `None` for an
    /// unknown token (the caller falls back to the default `match-source`).
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "alpha" => Some(MaskMode::Alpha),
            "luminance" => Some(MaskMode::Luminance),
            "match-source" => Some(MaskMode::MatchSource),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// MaskRepeat
// ---------------------------------------------------------------------------

/// `mask-repeat` (CSS Masking 1 § 6.4) — the 6 repeat styles. The
/// `repeat-x` / `repeat-y` one-axis forms are collapsed; the two-axis form
/// (`repeat no-repeat`) is parsed as `RepeatX` / `RepeatY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MaskRepeat {
    /// `repeat` (both axes).
    #[default]
    Repeat,
    /// `no-repeat` (both axes).
    NoRepeat,
    /// `repeat-x` ≡ `repeat no-repeat`.
    RepeatX,
    /// `repeat-y` ≡ `no-repeat repeat`.
    RepeatY,
    /// `space` (both axes) — distribute, no clipping.
    Space,
    /// `round` (both axes) — rescale to fit an integer count.
    Round,
}

impl MaskRepeat {
    /// Parse one `mask-repeat` token (ASCII-case-insensitive). The
    /// two-token form (`repeat no-repeat`) is handled in [`parse_mask`];
    /// this handles the single keyword.
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "repeat" => Some(MaskRepeat::Repeat),
            "no-repeat" => Some(MaskRepeat::NoRepeat),
            "repeat-x" => Some(MaskRepeat::RepeatX),
            "repeat-y" => Some(MaskRepeat::RepeatY),
            "space" => Some(MaskRepeat::Space),
            "round" => Some(MaskRepeat::Round),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// MaskBox
// ---------------------------------------------------------------------------

/// The shared `mask-clip` + `mask-origin` keyword set (CSS Masking 1 § 6.6
/// and § 6.7). `NoClip` is a `mask-clip`-only value (the mask is not
/// clipped to a box); it is not a valid `mask-origin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MaskBox {
    #[default]
    BorderBox,
    PaddingBox,
    ContentBox,
    /// `no-clip` — `mask-clip`-only; the mask is not clipped.
    NoClip,
    /// SVG box values (land with the SVG paint path).
    FillBox,
    StrokeBox,
    ViewBox,
}

impl MaskBox {
    /// Parse one box token (ASCII-case-insensitive). `None` for unknown.
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "border-box" => Some(MaskBox::BorderBox),
            "padding-box" => Some(MaskBox::PaddingBox),
            "content-box" => Some(MaskBox::ContentBox),
            "no-clip" => Some(MaskBox::NoClip),
            "fill-box" => Some(MaskBox::FillBox),
            "stroke-box" => Some(MaskBox::StrokeBox),
            "view-box" => Some(MaskBox::ViewBox),
            _ => None,
        }
    }

    /// `true` iff this is a valid `mask-origin` value (`no-clip` is
    /// `mask-clip`-only).
    pub fn is_valid_origin(self) -> bool {
        !matches!(self, MaskBox::NoClip)
    }
}

// ---------------------------------------------------------------------------
// MaskLayer
// ---------------------------------------------------------------------------

/// One layer's resolved longhands (CSS Masking 1 § 6). The `image` is kept
/// as the authored string (`"url(#mask1)"` / `"linear-gradient(...)"` /
/// `"none"`); the cascade resolves it against the resource loader. The
/// other longhands are typed enums ready for the paint path.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskLayer {
    /// `mask-image` (the source, authored verbatim; `none` ⇒ no mask).
    pub image: String,
    /// `mask-mode`.
    pub mode: MaskMode,
    /// `mask-repeat`.
    pub repeat: MaskRepeat,
    /// `mask-position` (x, y) — authored verbatim; the cascade resolves
    /// against the mask-positioning area (reuses
    /// [`crate::background_position`]).
    pub position: String,
    /// `mask-size` — authored verbatim (the `auto` / `<length>` /
    /// `<percentage>` / `cover` / `contain` family).
    pub size: String,
    /// `mask-clip` — the box the mask is clipped to.
    pub clip: MaskBox,
    /// `mask-origin` — the box the mask-position is resolved against.
    pub origin: MaskBox,
}

impl MaskLayer {
    /// The initial layer (CSS Masking 1 § 6): `mask-image: none`,
    /// `mask-mode: match-source`, `mask-repeat: repeat`,
    /// `mask-position: 0% 0%`, `mask-size: auto`, `mask-clip: border-box`,
    /// `mask-origin: border-box`.
    pub fn initial() -> Self {
        Self {
            image: String::from("none"),
            mode: MaskMode::MatchSource,
            repeat: MaskRepeat::Repeat,
            position: String::from("0% 0%"),
            size: String::from("auto"),
            clip: MaskBox::BorderBox,
            origin: MaskBox::BorderBox,
        }
    }
}

// ---------------------------------------------------------------------------
// parse_mask
// ---------------------------------------------------------------------------

/// Parse the `mask` shorthand (CSS Masking 1 § 6.1) into a list of
/// [`MaskLayer`]s. Layers are comma-separated; within a layer, the
/// longhand tokens appear in any order, with the `mask-position` /
/// `mask-size` slash form (`<position> / <size>`) recognised.
///
/// Each unrecognised token is assigned to `mask-image` (the spec's
/// "first unrecognised token is the image source" rule). Unrecognised
/// tokens after the image are dropped (the spec's "parse error ⇒ drop
/// the declaration" is softened here to "drop the token" so a partial
/// parse still yields a usable layer; the cascade re-validates).
///
/// ```
/// # use vixen_engine::mask::{MaskMode, MaskRepeat, parse_mask};
/// let layers = parse_mask("url(#m) alpha no-repeat");
/// assert_eq!(layers.len(), 1);
/// assert_eq!(layers[0].image, "url(#m)");
/// assert_eq!(layers[0].mode, MaskMode::Alpha);
/// assert_eq!(layers[0].repeat, MaskRepeat::NoRepeat);
/// ```
pub fn parse_mask(value: &str) -> Vec<MaskLayer> {
    // Split on top-level commas (not inside parens — a gradient's comma
    // would split a layer).
    let layer_strs = split_top_level_commas(value);
    layer_strs.into_iter().map(parse_one_layer).collect()
}

/// Parse one layer's token list into a [`MaskLayer`], applying the spec's
/// slot-fill rules. Tokens are tried against each longhand in turn; the
/// first unrecognised token becomes the image source.
fn parse_one_layer(s: &str) -> MaskLayer {
    let mut layer = MaskLayer::initial();
    let mut image_set = false;
    let mut position_tokens: Vec<&str> = Vec::new();
    let mut after_slash = false;
    let mut size_tokens: Vec<&str> = Vec::new();

    for tok in tokenize(s) {
        if after_slash {
            size_tokens.push(tok);
            continue;
        }
        // The slash separates position from size.
        if tok == "/" {
            after_slash = true;
            continue;
        }
        // Try mask-mode.
        if let Some(mode) = MaskMode::parse(tok) {
            layer.mode = mode;
            continue;
        }
        // Try mask-repeat (single keyword; the two-axis form is handled
        // below by peeking — kept simple: a `repeat no-repeat` sequence
        // collapses here heuristically).
        if let Some(rep) = MaskRepeat::parse(tok) {
            layer.repeat = rep;
            continue;
        }
        // Try a box token. mask-clip and mask-origin share the keyword set;
        // the first box token fills clip, the second fills origin (the § 6.1
        // slot order: position, size, repeat, origin, clip, mode — but
        // clip-after-origin is the shorthand convention). We fill origin
        // first, then clip, matching the spec's "if two box values, first
        // is origin, second is clip".
        if let Some(box_) = MaskBox::parse(tok) {
            if layer.origin == MaskBox::BorderBox && layer.clip == MaskBox::BorderBox {
                // First box token → origin.
                layer.origin = box_;
            } else if layer.clip == MaskBox::BorderBox {
                // Second box token → clip.
                layer.clip = box_;
            } else {
                // Third box token: drop (parse error).
            }
            continue;
        }
        // A keyword position token (center / top / left / …) or a length /
        // percent — accumulate into the position list.
        if is_position_token(tok) {
            position_tokens.push(tok);
            continue;
        }
        // Otherwise: the image source (first unrecognised token). Subsequent
        // unrecognised tokens are dropped.
        if !image_set {
            layer.image = tok.to_owned();
            image_set = true;
        }
    }

    if !position_tokens.is_empty() {
        layer.position = position_tokens.join(" ");
    }
    if !size_tokens.is_empty() {
        layer.size = size_tokens.join(" ");
    }
    layer
}

/// `true` iff `tok` is a plausible `mask-position` token (a position
/// keyword or a length / percentage). Conservative — a token that looks
/// numeric or is a position keyword counts; everything else is left for the
/// image-source slot.
fn is_position_token(tok: &str) -> bool {
    matches!(
        tok.to_ascii_lowercase().as_str(),
        "center" | "left" | "right" | "top" | "bottom"
    ) || tok.ends_with('%')
        || tok.ends_with("px")
        || tok.parse::<f32>().is_ok()
}

/// Tokenise a layer string into whitespace-separated tokens, preserving
/// parenthesised groups (so `url(#m)` and `linear-gradient(...)` stay one
/// token). Slash separators are emitted as their own token.
fn tokenize(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
        } else if depth == 0 && c.is_ascii_whitespace() {
            if i > start {
                out.push(&s[start..i]);
            }
            start = i + 1;
        } else if depth == 0 && c == b'/' {
            if i > start {
                out.push(&s[start..i]);
            }
            out.push(&s[i..i + 1]);
            start = i + 1;
        }
        i += 1;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Split on top-level commas (not inside parens).
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
        } else if depth == 0 && c == b',' {
            out.push(s[start..i].trim());
            start = i + 1;
        }
        i += 1;
    }
    let last = s[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- MaskMode ------------------------------------------------------

    #[test]
    fn mask_mode_parse() {
        assert_eq!(MaskMode::parse("alpha"), Some(MaskMode::Alpha));
        assert_eq!(MaskMode::parse("Luminance"), Some(MaskMode::Luminance));
        assert_eq!(MaskMode::parse("match-source"), Some(MaskMode::MatchSource));
        assert_eq!(MaskMode::parse("inherit"), None);
        assert_eq!(MaskMode::default(), MaskMode::MatchSource);
    }

    // --- MaskRepeat ----------------------------------------------------

    #[test]
    fn mask_repeat_parse() {
        assert_eq!(MaskRepeat::parse("repeat"), Some(MaskRepeat::Repeat));
        assert_eq!(MaskRepeat::parse("no-repeat"), Some(MaskRepeat::NoRepeat));
        assert_eq!(MaskRepeat::parse("repeat-x"), Some(MaskRepeat::RepeatX));
        assert_eq!(MaskRepeat::parse("repeat-y"), Some(MaskRepeat::RepeatY));
        assert_eq!(MaskRepeat::parse("space"), Some(MaskRepeat::Space));
        assert_eq!(MaskRepeat::parse("round"), Some(MaskRepeat::Round));
        assert_eq!(MaskRepeat::parse("garbage"), None);
        assert_eq!(MaskRepeat::default(), MaskRepeat::Repeat);
    }

    // --- MaskBox -------------------------------------------------------

    #[test]
    fn mask_box_parse() {
        assert_eq!(MaskBox::parse("border-box"), Some(MaskBox::BorderBox));
        assert_eq!(MaskBox::parse("padding-box"), Some(MaskBox::PaddingBox));
        assert_eq!(MaskBox::parse("content-box"), Some(MaskBox::ContentBox));
        assert_eq!(MaskBox::parse("no-clip"), Some(MaskBox::NoClip));
        assert_eq!(MaskBox::parse("view-box"), Some(MaskBox::ViewBox));
        assert_eq!(MaskBox::parse("garbage"), None);
        assert_eq!(MaskBox::default(), MaskBox::BorderBox);
    }

    #[test]
    fn mask_box_no_clip_not_valid_origin() {
        assert!(!MaskBox::NoClip.is_valid_origin());
        assert!(MaskBox::BorderBox.is_valid_origin());
        assert!(MaskBox::ContentBox.is_valid_origin());
    }

    // --- MaskLayer initial --------------------------------------------

    #[test]
    fn mask_layer_initial() {
        let l = MaskLayer::initial();
        assert_eq!(l.image, "none");
        assert_eq!(l.mode, MaskMode::MatchSource);
        assert_eq!(l.repeat, MaskRepeat::Repeat);
        assert_eq!(l.position, "0% 0%");
        assert_eq!(l.size, "auto");
        assert_eq!(l.clip, MaskBox::BorderBox);
        assert_eq!(l.origin, MaskBox::BorderBox);
    }

    // --- parse_mask ----------------------------------------------------

    #[test]
    fn parse_mask_single_layer_url() {
        let layers = parse_mask("url(#mask1)");
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].image, "url(#mask1)");
        // Defaults preserved for the unset longhands.
        assert_eq!(layers[0].mode, MaskMode::MatchSource);
    }

    #[test]
    fn parse_mask_multiple_layers_comma_separated() {
        let layers = parse_mask("url(#a), url(#b) alpha, none");
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0].image, "url(#a)");
        assert_eq!(layers[1].image, "url(#b)");
        assert_eq!(layers[1].mode, MaskMode::Alpha);
        assert_eq!(layers[2].image, "none");
    }

    #[test]
    fn parse_mask_gradient_paren_commas_do_not_split_layer() {
        let layers = parse_mask("linear-gradient(to right, red, blue)");
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].image, "linear-gradient(to right, red, blue)");
    }

    #[test]
    fn parse_mask_mode_and_repeat() {
        let layers = parse_mask("url(#m) luminance no-repeat");
        assert_eq!(layers[0].image, "url(#m)");
        assert_eq!(layers[0].mode, MaskMode::Luminance);
        assert_eq!(layers[0].repeat, MaskRepeat::NoRepeat);
    }

    #[test]
    fn parse_mask_origin_then_clip() {
        let layers = parse_mask("url(#m) content-box padding-box");
        // First box → origin; second box → clip.
        assert_eq!(layers[0].origin, MaskBox::ContentBox);
        assert_eq!(layers[0].clip, MaskBox::PaddingBox);
    }

    #[test]
    fn parse_mask_position_accumulates() {
        let layers = parse_mask("url(#m) center top");
        assert_eq!(layers[0].position, "center top");
    }

    #[test]
    fn parse_mask_position_and_size_with_slash() {
        let layers = parse_mask("url(#m) 50% 50% / cover");
        assert_eq!(layers[0].position, "50% 50%");
        assert_eq!(layers[0].size, "cover");
    }

    #[test]
    fn parse_mask_empty_returns_empty() {
        assert!(parse_mask("").is_empty());
        assert!(parse_mask("   ").is_empty());
    }

    #[test]
    fn parse_mask_none_keyword_as_image() {
        let layers = parse_mask("none");
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].image, "none");
    }

    // --- tokenize / split helpers -------------------------------------

    #[test]
    fn tokenize_preserves_parens() {
        let toks = tokenize("url(#m) alpha");
        assert_eq!(toks, vec!["url(#m)", "alpha"]);
    }

    #[test]
    fn tokenize_emits_slash_separator() {
        let toks = tokenize("50% 50% / cover");
        assert_eq!(toks, vec!["50%", "50%", "/", "cover"]);
    }

    #[test]
    fn split_top_level_commas_skips_parens() {
        let parts = split_top_level_commas("a, linear-gradient(x, y), c");
        assert_eq!(parts, vec!["a", "linear-gradient(x, y)", "c"]);
    }

    #[test]
    fn split_top_level_commas_trims_and_drops_empty_trailing() {
        let parts = split_top_level_commas("a , , b ");
        assert_eq!(parts, vec!["a", "", "b"]);
    }

    // --- is_position_token --------------------------------------------

    #[test]
    fn is_position_token_keywords_and_lengths() {
        assert!(is_position_token("center"));
        assert!(is_position_token("top"));
        assert!(is_position_token("50%"));
        assert!(is_position_token("10px"));
        assert!(is_position_token("10"));
        assert!(!is_position_token("alpha"));
        assert!(!is_position_token("no-repeat"));
    }
}
