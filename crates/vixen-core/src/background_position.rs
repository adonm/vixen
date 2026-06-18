//! CSS `background-position` resolution — Phase 5 paint prep (pure logic
//! called out by `docs/PLAN.md` "Testing strategy" as a Rust-unit-test
//! surface). Implements CSS Backgrounds Level 3 § 3.6 (`background-position`)
//! and § 4.2 (the `<position>` resolution algorithm), so the paint path can
//! position the background image rect against any background positioning
//! area without waiting for the WebRender background plumbing.
//!
//! What lives here:
//! - [`BackgroundPosition`] — `(horizontal, vertical)` axis components, each
//!   the `(fraction, offset_px)` pair the § 4.2 algorithm resolves to.
//! - [`AxisComponent`] — one axis: `fraction ∈ [0,1]` of
//!   `(container_size − image_size)`, plus a signed px offset. Keywords +
//!   bare lengths + `%` reduce to this.
//! - [`BackgroundPosition::resolve`] — apply § 4.2 to concrete px sizes,
//!   returning `(x, y)` for paint.
//! - [`parse_background_position`] — the `<bg-position>` grammar (1/2/3/4
//!   value forms, keyword + length + percentage mix, the keyword/offset swap
//!   rule for the 3/4-value form).
//!
//! What does *not* live here:
//! - Multi-background list resolution (`background-position: a, b` is a list
//!   over the background layer list; this module resolves one layer, the
//!   caller iterates). Same pattern as `box-shadow`.
//! - The `background-origin` interaction (border-box / padding-box /
//!   content-box) — that selects the *container* size; this module just takes
//!   the resolved container + image px values. The caller hands them in.
//! - `calc()` (the cascade-resolved surface feeds definite px). The parser
//!   handles plain lengths and percentages only.
//!
//! ## The resolution formula (CSS Backgrounds 3 § 4.2)
//!
//! Each axis resolves to `(container_size − image_size) * fraction + offset`,
//! where the fraction comes from the keyword or percentage and the offset is
//! the length component (signed relative to the keyword's edge — `right 10px`
//! ⇒ offset `−10`, so the image's right edge is 10px *inside* the container's
//! right edge):
//!
//! ```text
//! X = (container_w − image_w) * fraction_x + offset_x
//! Y = (container_h − image_h) * fraction_y + offset_y
//! ```
//!
//! `left`/`top` ⇒ fraction 0, `right`/`bottom` ⇒ fraction 1, `center` ⇒
//! fraction 0.5, `<percentage>` ⇒ that fraction. A bare `<length>` ⇒ fraction
//! 0 with that length as the offset (it's `left <length>` / `top <length>`
//! shorthand). The four-value form's offsets are measured *from* the named
//! edge, so `right 10px` ⇒ fraction 1, offset `−10` (the image's right edge
//! is 10px inside the container's right edge).
//!
//! Reference: <https://www.w3.org/TR/css-backgrounds-3/#background-position>,
//! § 4.2 "<position>" (<https://www.w3.org/TR/css-backgrounds-3/#position>).

#![forbid(unsafe_code)]

/// One axis of a resolved `<bg-position>`: a fraction of the
/// (container_size − image_size) plus a signed px offset from that anchor.
/// This is the form the § 4.2 resolution algorithm reduces every keyword /
/// length / percentage combination to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisComponent {
    /// Fraction of `(container_size − image_size)`: `0.0` for `left`/`top`,
    /// `1.0` for `right`/`bottom`, `0.5` for `center`, the literal value for
    /// bare `<percentage>` (`25%` ⇒ `0.25`).
    pub fraction: f32,
    /// The signed px offset. `left 10px` ⇒ `+10`; `right 10px` ⇒ `−10` (image
    /// right edge 10px inside the container right edge). Bare `<length>`
    /// ⇒ `+length` (the start anchor with that offset).
    pub offset: f32,
}

impl AxisComponent {
    /// `left` / `top` — image start at container start.
    pub const START: AxisComponent = AxisComponent {
        fraction: 0.0,
        offset: 0.0,
    };
    /// `right` / `bottom` — image end at container end.
    pub const END: AxisComponent = AxisComponent {
        fraction: 1.0,
        offset: 0.0,
    };
    /// `center` — image centre at container centre.
    pub const CENTER: AxisComponent = AxisComponent {
        fraction: 0.5,
        offset: 0.0,
    };

    /// A bare percentage (`25%` ⇒ fraction 0.25, no offset).
    pub const fn percent(p: f32) -> Self {
        Self {
            fraction: p,
            offset: 0.0,
        }
    }

    /// A bare length (`10px` ⇒ fraction 0, offset +10).
    pub const fn length(l: f32) -> Self {
        Self {
            fraction: 0.0,
            offset: l,
        }
    }

    /// Apply the § 4.2 resolution formula for one axis given concrete px
    /// sizes: `(container − image) * fraction + offset`.
    pub fn resolve(self, container_size: f32, image_size: f32) -> f32 {
        (container_size - image_size) * self.fraction + self.offset
    }
}

/// A `background-position` value, horizontal + vertical components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackgroundPosition {
    pub horizontal: AxisComponent,
    pub vertical: AxisComponent,
}

impl BackgroundPosition {
    /// `0% 0%` (the initial value).
    pub const INITIAL: BackgroundPosition = BackgroundPosition {
        horizontal: AxisComponent::START,
        vertical: AxisComponent::START,
    };
    /// `center center` — image centred in both axes.
    pub const CENTER: BackgroundPosition = BackgroundPosition {
        horizontal: AxisComponent::CENTER,
        vertical: AxisComponent::CENTER,
    };

    /// Resolve to a concrete `(x, y)` paint position given the background
    /// positioning area size and the background image size (both in px).
    pub fn resolve(
        self,
        container_w: f32,
        container_h: f32,
        image_w: f32,
        image_h: f32,
    ) -> (f32, f32) {
        let x = self.horizontal.resolve(container_w, image_w);
        let y = self.vertical.resolve(container_h, image_h);
        (x, y)
    }
}

// ---------------------------------------------------------------------------
// Parser (host-hook + reftest helper)
// ---------------------------------------------------------------------------

/// Parse a `background-position` value (one layer; the multi-layer comma form
/// is split + iterated by the caller — same shape as `box-shadow`).
///
/// Accepts the 1/2/3/4-value forms, keyword / length / percentage mix, and
/// the § 4.2 swap rule (a vertical keyword as the first of two values swaps
/// the implicit axis assignment). Length args carry an optional `px` suffix;
/// percentages are `<number>%`.
pub fn parse_background_position(input: &str) -> Result<BackgroundPosition, BgPositionParseError> {
    let tokens: Vec<&str> = input.split_ascii_whitespace().collect();
    match tokens.as_slice() {
        [] => Err(BgPositionParseError::Empty),
        [one] => parse_one_value(one),
        [a, b] => parse_two_values(a, b),
        [a, b, c] => parse_three_values(a, b, c),
        [a, b, c, d] => parse_four_values(a, b, c, d),
        _ => Err(BgPositionParseError::TooManyValues),
    }
}

/// 1-value form: keyword applies to its axis; the other axis defaults to
/// `center`. A bare length/percentage applies horizontally; vertical defaults
/// to `center` (CSS Backgrounds 3 § 4.2 prose: "if only one value is
/// specified, the second value is assumed to be center").
fn parse_one_value(v: &str) -> Result<BackgroundPosition, BgPositionParseError> {
    if is_horizontal_keyword(v) {
        // `left` / `right` ⇒ horizontal, vertical = center.
        let kw = parse_keyword(v).unwrap();
        Ok(BackgroundPosition {
            horizontal: AxisComponent {
                fraction: keyword_fraction(kw),
                offset: 0.0,
            },
            vertical: AxisComponent::CENTER,
        })
    } else if is_vertical_keyword(v) {
        // `top` / `bottom` ⇒ vertical, horizontal = center.
        let kw = parse_keyword(v).unwrap();
        Ok(BackgroundPosition {
            horizontal: AxisComponent::CENTER,
            vertical: AxisComponent {
                fraction: keyword_fraction(kw),
                offset: 0.0,
            },
        })
    } else if v.eq_ignore_ascii_case("center") {
        Ok(BackgroundPosition::CENTER)
    } else {
        // Bare length or percentage ⇒ horizontal, vertical = center.
        let c = parse_length_or_percent(v)?;
        Ok(BackgroundPosition {
            horizontal: c,
            vertical: AxisComponent::CENTER,
        })
    }
}

/// 2-value form: first applies horizontally, second vertically — *unless*
/// the first is a vertical keyword (`top`/`bottom`), in which case the spec
/// (§ 4.2) requires the axes be swapped (this is the historical
/// `top right` ⇒ `right top` rewrite).
fn parse_two_values(a: &str, b: &str) -> Result<BackgroundPosition, BgPositionParseError> {
    // Reject the ambiguous keyword pairs (two horizontal or two vertical).
    if is_horizontal_keyword(a) && is_horizontal_keyword(b)
        || is_vertical_keyword(a) && is_vertical_keyword(b)
    {
        return Err(BgPositionParseError::AmbiguousAxes(
            a.to_owned(),
            b.to_owned(),
        ));
    }
    // Swap if `a` is vertical (per § 4.2: a vertical keyword as the first
    // value implies the axes are listed in vertical-first order).
    let (first, second) = if is_vertical_keyword(a) {
        (b, a)
    } else {
        (a, b)
    };
    let horizontal = value_to_horizontal(first)?;
    let vertical = value_to_vertical(second)?;
    Ok(BackgroundPosition {
        horizontal,
        vertical,
    })
}

/// 3-value form: `[ center | [ left | right ] <length-percentage>? ] &&
/// [ center | [ top | bottom ] <length-percentage>? ]` reduced to one axis
/// being a keyword+offset pair and the other being the implicit center.
fn parse_three_values(
    a: &str,
    b: &str,
    c: &str,
) -> Result<BackgroundPosition, BgPositionParseError> {
    if is_horizontal_keyword(a) {
        // horizontal: a [b], vertical: center.
        // `b` is the length/percent offset for the horizontal keyword.
        let kw = parse_keyword(a).unwrap();
        let off = parse_keyword_offset(b, kw)?;
        let vertical = parse_value_or_keyword(c, Axis::Vertical)?;
        Ok(BackgroundPosition {
            horizontal: AxisComponent {
                fraction: keyword_fraction(kw),
                offset: off,
            },
            vertical,
        })
    } else if is_vertical_keyword(a) {
        // vertical: a [b], horizontal: center.
        let kw = parse_keyword(a).unwrap();
        let off = parse_keyword_offset(b, kw)?;
        let horizontal = parse_value_or_keyword(c, Axis::Horizontal)?;
        Ok(BackgroundPosition {
            horizontal,
            vertical: AxisComponent {
                fraction: keyword_fraction(kw),
                offset: off,
            },
        })
    } else {
        Err(BgPositionParseError::InvalidThreeValueForm(
            a.to_owned(),
            b.to_owned(),
            c.to_owned(),
        ))
    }
}

/// 4-value form: `[ center | [ left | right ] <length-percentage>? ] &&
/// [ center | [ top | bottom ] <length-percentage>? ]` — one
/// keyword+offset per axis. The two axes may appear in either order
/// (horizontal-first is conventional; vertical-first is accepted).
fn parse_four_values(
    a: &str,
    b: &str,
    c: &str,
    d: &str,
) -> Result<BackgroundPosition, BgPositionParseError> {
    let first_axis = if is_horizontal_keyword(a) {
        Axis::Horizontal
    } else if is_vertical_keyword(a) {
        Axis::Vertical
    } else {
        return Err(BgPositionParseError::ExpectedKeyword(a.to_owned()));
    };
    // c must be a keyword for the opposite axis.
    let second_axis = if is_horizontal_keyword(c) {
        Axis::Horizontal
    } else if is_vertical_keyword(c) {
        Axis::Vertical
    } else {
        return Err(BgPositionParseError::ExpectedKeyword(c.to_owned()));
    };
    if first_axis == second_axis {
        return Err(BgPositionParseError::AmbiguousAxes(
            a.to_owned(),
            c.to_owned(),
        ));
    }
    let kw_a = parse_keyword(a).unwrap();
    let kw_c = parse_keyword(c).unwrap();
    let off_a = parse_keyword_offset(b, kw_a)?;
    let off_c = parse_keyword_offset(d, kw_c)?;
    let component_a = AxisComponent {
        fraction: keyword_fraction(kw_a),
        offset: off_a,
    };
    let component_c = AxisComponent {
        fraction: keyword_fraction(kw_c),
        offset: off_c,
    };
    Ok(if first_axis == Axis::Horizontal {
        BackgroundPosition {
            horizontal: component_a,
            vertical: component_c,
        }
    } else {
        BackgroundPosition {
            horizontal: component_c,
            vertical: component_a,
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keyword {
    Start,
    Center,
    End,
}

fn parse_keyword(s: &str) -> Option<Keyword> {
    match s.to_ascii_lowercase().as_str() {
        "left" | "top" => Some(Keyword::Start),
        "right" | "bottom" => Some(Keyword::End),
        "center" => Some(Keyword::Center),
        _ => None,
    }
}

fn is_horizontal_keyword(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "left" | "right")
}

fn is_vertical_keyword(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "top" | "bottom")
}

fn keyword_fraction(kw: Keyword) -> f32 {
    match kw {
        Keyword::Start => 0.0,
        Keyword::Center => 0.5,
        Keyword::End => 1.0,
    }
}

/// Build an [`AxisComponent`] from a bare keyword (`left` ⇒ 0%+0, `right`
/// ⇒ 100%+0, `center` ⇒ 50%+0).
fn component_from_keyword(kw: Keyword, _unused: f32) -> AxisComponent {
    AxisComponent {
        fraction: keyword_fraction(kw),
        offset: 0.0,
    }
}

/// Parse a value that may be a keyword or a length/percentage. `axis`
/// disambiguates the keyword (e.g. `left` is valid only on the horizontal
/// axis).
fn parse_value_or_keyword(s: &str, axis: Axis) -> Result<AxisComponent, BgPositionParseError> {
    if let Some(kw) = parse_keyword(s) {
        let is_horizontal = matches!(s.to_ascii_lowercase().as_str(), "left" | "right");
        let is_vertical = matches!(s.to_ascii_lowercase().as_str(), "top" | "bottom");
        let kw_axis_valid = match axis {
            Axis::Horizontal => is_horizontal || kw == Keyword::Center,
            Axis::Vertical => is_vertical || kw == Keyword::Center,
        };
        if !kw_axis_valid {
            return Err(BgPositionParseError::WrongAxisKeyword(s.to_owned()));
        }
        return Ok(AxisComponent {
            fraction: keyword_fraction(kw),
            offset: 0.0,
        });
    }
    parse_length_or_percent(s)
}

/// First-of-two parser: the horizontal slot may be a horizontal keyword,
/// `center`, or a length/percentage.
fn value_to_horizontal(s: &str) -> Result<AxisComponent, BgPositionParseError> {
    if let Some(kw) = parse_keyword(s) {
        if is_vertical_keyword(s) {
            return Err(BgPositionParseError::WrongAxisKeyword(s.to_owned()));
        }
        return Ok(component_from_keyword(kw, 0.0));
    }
    parse_length_or_percent(s)
}

/// Second-of-two parser: the vertical slot may be a vertical keyword,
/// `center`, or a length/percentage.
fn value_to_vertical(s: &str) -> Result<AxisComponent, BgPositionParseError> {
    if let Some(kw) = parse_keyword(s) {
        if is_horizontal_keyword(s) {
            return Err(BgPositionParseError::WrongAxisKeyword(s.to_owned()));
        }
        return Ok(component_from_keyword(kw, 0.0));
    }
    parse_length_or_percent(s)
}

/// Parse a bare `<length-percentage>` token into the standard
/// `(fraction, offset)` form. Bare `<length>` ⇒ fraction 0 + offset; bare
/// `<percentage>` ⇒ fraction (0..1) + offset 0. Used by the 1-value and
/// 2-value forms (where there's no keyword anchor to offset from).
fn parse_length_or_percent(s: &str) -> Result<AxisComponent, BgPositionParseError> {
    let s = s.trim();
    if let Some(percent_str) = s.strip_suffix('%') {
        let p: f32 = percent_str
            .trim()
            .parse()
            .map_err(|_| BgPositionParseError::BadLength(s.to_owned()))?;
        return Ok(AxisComponent {
            fraction: p / 100.0,
            offset: 0.0,
        });
    }
    let stripped = s.strip_suffix("px").unwrap_or(s);
    let v: f32 = stripped
        .parse()
        .map_err(|_| BgPositionParseError::BadLength(s.to_owned()))?;
    Ok(AxisComponent {
        fraction: 0.0,
        offset: v,
    })
}

/// Parse a `<length>` offset for a keyword anchor (the 3-value / 4-value form
/// `right 10px top 5px`). `anchor` selects the sign convention: a `Start`
/// keyword anchor keeps the offset positive (image shifts right/down from
/// start); an `End` keyword anchor negates it (image shifts left/up from end,
/// so the image's far edge moves *inside* the container's far edge).
///
/// Per CSS Backgrounds 3 § 4.2, the keyword-offset position accepts
/// `<length>` only — a `<percentage>` here is rejected with
/// [`BgPositionParseError::PercentOffsetIllegal`].
fn parse_keyword_offset(s: &str, anchor: Keyword) -> Result<f32, BgPositionParseError> {
    let trimmed = s.trim();
    if trimmed.ends_with('%') {
        return Err(BgPositionParseError::PercentOffsetIllegal(
            trimmed.to_owned(),
        ));
    }
    let stripped = trimmed.strip_suffix("px").unwrap_or(trimmed);
    let v: f32 = stripped
        .parse()
        .map_err(|_| BgPositionParseError::BadLength(trimmed.to_owned()))?;
    // For End anchors (`right 10px`), the offset is subtracted (image moves
    // 10px inside the container's right edge).
    Ok(match anchor {
        Keyword::Start => v,
        // End and Center are handled as "offset from the far edge", negated.
        Keyword::End | Keyword::Center => -v,
    })
}

/// Parse error for [`parse_background_position`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BgPositionParseError {
    #[error("empty background-position")]
    Empty,
    #[error("too many values (max 4)")]
    TooManyValues,
    #[error("both values are on the same axis: {0:?} {1:?}")]
    AmbiguousAxes(String, String),
    #[error("invalid 3-value form: {0:?} {1:?} {2:?}")]
    InvalidThreeValueForm(String, String, String),
    #[error("expected a keyword (left/right/top/bottom), got {0:?}")]
    ExpectedKeyword(String),
    #[error("keyword {0:?} is on the wrong axis")]
    WrongAxisKeyword(String),
    #[error("invalid length/percentage: {0:?}")]
    BadLength(String),
    #[error("percentages are not allowed as keyword offsets: {0:?}")]
    PercentOffsetIllegal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    // --- AxisComponent.resolve (the formula) ----------------------------

    #[test]
    fn start_resolves_to_zero_when_image_fits() {
        // container 100, image 50, fraction 0 (start) ⇒ X = 0.
        let x = AxisComponent::START.resolve(100.0, 50.0);
        assert!(approx(x, 0.0));
    }

    #[test]
    fn end_puts_image_far_edge_at_container_far_edge() {
        // fraction 1, container 100, image 50 ⇒ X = 100 - 50 = 50.
        let x = AxisComponent::END.resolve(100.0, 50.0);
        assert!(approx(x, 50.0));
    }

    #[test]
    fn center_centres_image() {
        // fraction 0.5, container 100, image 50 ⇒ X = (100-50)*0.5 = 25.
        let x = AxisComponent::CENTER.resolve(100.0, 50.0);
        assert!(approx(x, 25.0));
    }

    #[test]
    fn percent_resolves_against_difference() {
        // 25% of (100 - 50) = 12.5.
        let x = AxisComponent::percent(0.25).resolve(100.0, 50.0);
        assert!(approx(x, 12.5));
    }

    #[test]
    fn length_resolves_to_offset_from_zero() {
        // bare length: fraction 0, offset 10 ⇒ X = 10 regardless of sizes.
        let x = AxisComponent::length(10.0).resolve(100.0, 50.0);
        assert!(approx(x, 10.0));
    }

    #[test]
    fn percent_with_equal_sizes_is_zero() {
        // If image == container, (c-i)=0, so any percentage resolves to 0.
        let x = AxisComponent::END.resolve(100.0, 100.0);
        assert!(approx(x, 0.0));
        let x = AxisComponent::percent(0.75).resolve(100.0, 100.0);
        assert!(approx(x, 0.0));
    }

    #[test]
    fn negative_offset_shifts_left() {
        // `right 10px` ⇒ fraction 1, offset -10. With container 100, image
        // 50: X = (100-50) - 10 = 40.
        let x = AxisComponent {
            fraction: 1.0,
            offset: -10.0,
        }
        .resolve(100.0, 50.0);
        assert!(approx(x, 40.0));
    }

    // --- BackgroundPosition.resolve -------------------------------------

    #[test]
    fn initial_resolves_to_top_left() {
        let (x, y) = BackgroundPosition::INITIAL.resolve(100.0, 100.0, 30.0, 30.0);
        assert!(approx(x, 0.0));
        assert!(approx(y, 0.0));
    }

    #[test]
    fn center_centres_in_both_axes() {
        let (x, y) = BackgroundPosition::CENTER.resolve(100.0, 100.0, 30.0, 40.0);
        assert!(approx(x, 35.0)); // (100-30)*0.5
        assert!(approx(y, 30.0)); // (100-40)*0.5
    }

    // --- Parser: 1-value -----------------------------------------------

    #[test]
    fn parse_left_horizontal_center_vertical() {
        let p = parse_background_position("left").unwrap();
        assert_eq!(p.horizontal, AxisComponent::START);
        assert_eq!(p.vertical, AxisComponent::CENTER);
    }

    #[test]
    fn parse_top_vertical_center_horizontal() {
        let p = parse_background_position("top").unwrap();
        assert_eq!(p.horizontal, AxisComponent::CENTER);
        assert_eq!(p.vertical, AxisComponent::START);
    }

    #[test]
    fn parse_right_bottom_single_value() {
        let p = parse_background_position("right").unwrap();
        assert_eq!(p.horizontal, AxisComponent::END);
        assert_eq!(p.vertical, AxisComponent::CENTER);
        let p = parse_background_position("bottom").unwrap();
        assert_eq!(p.horizontal, AxisComponent::CENTER);
        assert_eq!(p.vertical, AxisComponent::END);
    }

    #[test]
    fn parse_center_alone_is_center_center() {
        let p = parse_background_position("center").unwrap();
        assert_eq!(p, BackgroundPosition::CENTER);
    }

    #[test]
    fn parse_single_length_horizontal_vertical_center() {
        let p = parse_background_position("10px").unwrap();
        assert_eq!(p.horizontal, AxisComponent::length(10.0));
        assert_eq!(p.vertical, AxisComponent::CENTER);
    }

    #[test]
    fn parse_single_percent_horizontal_vertical_center() {
        let p = parse_background_position("25%").unwrap();
        assert_eq!(p.horizontal, AxisComponent::percent(0.25));
        assert_eq!(p.vertical, AxisComponent::CENTER);
    }

    // --- Parser: 2-value -----------------------------------------------

    #[test]
    fn parse_two_keywords_assign_axes() {
        let p = parse_background_position("left top").unwrap();
        assert_eq!(p.horizontal, AxisComponent::START);
        assert_eq!(p.vertical, AxisComponent::START);
        let p = parse_background_position("right bottom").unwrap();
        assert_eq!(p.horizontal, AxisComponent::END);
        assert_eq!(p.vertical, AxisComponent::END);
    }

    #[test]
    fn parse_swapped_keyword_order() {
        // `top right` is equivalent to `right top` per § 4.2.
        let p = parse_background_position("top right").unwrap();
        assert_eq!(p.horizontal, AxisComponent::END);
        assert_eq!(p.vertical, AxisComponent::START);
    }

    #[test]
    fn parse_two_lengths_or_percents() {
        let p = parse_background_position("10px 20px").unwrap();
        assert_eq!(p.horizontal, AxisComponent::length(10.0));
        assert_eq!(p.vertical, AxisComponent::length(20.0));
        let p = parse_background_position("25% 50%").unwrap();
        assert_eq!(p.horizontal, AxisComponent::percent(0.25));
        assert_eq!(p.vertical, AxisComponent::percent(0.5));
    }

    #[test]
    fn parse_mixed_keyword_and_length() {
        let p = parse_background_position("left 10px").unwrap();
        assert_eq!(p.horizontal, AxisComponent::START);
        assert_eq!(p.vertical, AxisComponent::length(10.0));
        let p = parse_background_position("10px center").unwrap();
        assert_eq!(p.horizontal, AxisComponent::length(10.0));
        assert_eq!(p.vertical, AxisComponent::CENTER);
    }

    #[test]
    fn parse_rejects_same_axis_keyword_pair() {
        // `left right` is ambiguous (both horizontal).
        assert!(parse_background_position("left right").is_err());
        assert!(parse_background_position("top bottom").is_err());
    }

    // --- Parser: 3-value -----------------------------------------------

    #[test]
    fn parse_three_horizontal_offset_vertical_center() {
        // `right 10px center`: horizontal = END - 10, vertical = center.
        let p = parse_background_position("right 10px center").unwrap();
        assert_eq!(
            p.horizontal,
            AxisComponent {
                fraction: 1.0,
                offset: -10.0
            }
        );
        assert_eq!(p.vertical, AxisComponent::CENTER);
    }

    #[test]
    fn parse_three_vertical_offset_horizontal_center() {
        // `top 5px center`: vertical = START + 5, horizontal = center.
        let p = parse_background_position("top 5px center").unwrap();
        assert_eq!(
            p.vertical,
            AxisComponent {
                fraction: 0.0,
                offset: 5.0
            }
        );
        assert_eq!(p.horizontal, AxisComponent::CENTER);
    }

    #[test]
    fn parse_three_rejects_percent_offset() {
        // § 4.2: keyword offsets must be lengths, not percentages.
        assert!(parse_background_position("right 10% center").is_err());
    }

    // --- Parser: 4-value -----------------------------------------------

    #[test]
    fn parse_four_horizontal_then_vertical_offset() {
        // `right 10px top 5px` ⇒ h = END - 10, v = START + 5.
        let p = parse_background_position("right 10px top 5px").unwrap();
        assert_eq!(
            p.horizontal,
            AxisComponent {
                fraction: 1.0,
                offset: -10.0
            }
        );
        assert_eq!(
            p.vertical,
            AxisComponent {
                fraction: 0.0,
                offset: 5.0
            }
        );
    }

    #[test]
    fn parse_four_vertical_then_horizontal_offset() {
        // `top 5px right 10px` (swapped order) ⇒ same as above.
        let p = parse_background_position("top 5px right 10px").unwrap();
        assert_eq!(
            p.horizontal,
            AxisComponent {
                fraction: 1.0,
                offset: -10.0
            }
        );
        assert_eq!(
            p.vertical,
            AxisComponent {
                fraction: 0.0,
                offset: 5.0
            }
        );
    }

    #[test]
    fn parse_four_rejects_same_axis_pair() {
        // `right 10px left 5px` is ambiguous.
        assert!(parse_background_position("right 10px left 5px").is_err());
    }

    // --- Parser: errors ------------------------------------------------

    #[test]
    fn parse_empty_errors() {
        assert!(matches!(
            parse_background_position(""),
            Err(BgPositionParseError::Empty)
        ));
    }

    #[test]
    fn parse_too_many_values_errors() {
        assert!(matches!(
            parse_background_position("left top right bottom center"),
            Err(BgPositionParseError::TooManyValues)
        ));
    }

    // --- Round trip: parse + resolve -----------------------------------

    #[test]
    fn parse_then_resolve_left_top() {
        let p = parse_background_position("left top").unwrap();
        let (x, y) = p.resolve(100.0, 100.0, 30.0, 30.0);
        assert!(approx(x, 0.0));
        assert!(approx(y, 0.0));
    }

    #[test]
    fn parse_then_resolve_50_percent() {
        let p = parse_background_position("50% 50%").unwrap();
        let (x, y) = p.resolve(100.0, 100.0, 30.0, 30.0);
        assert!(approx(x, 35.0));
        assert!(approx(y, 35.0));
    }

    #[test]
    fn parse_then_resolve_four_value() {
        // right 10px top 5px with container 100x100, image 30x30:
        // X = (100-30)*1 - 10 = 60; Y = 0 + 5 = 5.
        let p = parse_background_position("right 10px top 5px").unwrap();
        let (x, y) = p.resolve(100.0, 100.0, 30.0, 30.0);
        assert!(approx(x, 60.0), "x={x}");
        assert!(approx(y, 5.0), "y={y}");
    }

    #[test]
    fn parse_then_resolve_25_percent_center() {
        // 25% center, container 100x100, image 30x30:
        // X = (100-30)*0.25 = 17.5; Y = (100-30)*0.5 = 35.
        let p = parse_background_position("25%").unwrap();
        let (x, y) = p.resolve(100.0, 100.0, 30.0, 30.0);
        assert!(approx(x, 17.5));
        assert!(approx(y, 35.0));
    }
}
