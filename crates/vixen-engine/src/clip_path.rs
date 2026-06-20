//! CSS `clip-path` basic shapes — Phase 5 paint prep (pure logic). The
//! `<basic-shape>` family the `clip-path` property reduces to, plus the
//! per-pixel point-in-shape test the paint path clips against. Composes
//! with [`crate::border_radius`] (the `inset() round <radius>` form) and
//! [`crate::background_position`] (the `at <position>` resolution).
//!
//! What lives here:
//! - [`ClipPath`] — the typed basic-shape family: [`ClipPath::Inset`] /
//!   [`ClipPath::Circle`] / [`ClipPath::Ellipse`] / [`ClipPath::Polygon`] /
//!   [`ClipPath::None`]. Carries the authored coordinates (px / percent /
//!   keyword) so the caller resolves them against the reference box once.
//! - [`GeometryBox`] — the `<geometry-box>` reference the shape is resolved
//!   against (`border-box` default for HTML; `fill-box`/`stroke-box`/`view-box`
//!   for SVG).
//! - [`Coord`] — a single `at <position>` coordinate (px / percent / `center`
//!   / `top`/`bottom`/`left`/`right`), with [`Coord::resolve`] against a box
//!   extent.
//! - [`parse_clip_path`] — the basic-shape parse (case-insensitive function
//!   name, parenthesised argument list, whitespace/comma tolerance).
//! - [`ClipPath::contains`] — the point-in-shape test: `true` iff `point`
//!   lies inside the shape resolved against `box`. The paint path calls this
//!   per pixel to decide clip in/out.
//!
//! What does *not* live here:
//! - The `path(<svg>)` basic shape (SVG path parsing — deferred; the four
//!   geometric shapes cover the common HTML surface).
//! - Anti-aliased edge coverage (WebRender's job; this is the boolean
//!   in/out test).
//! - The reference-box selection from the cascade (`border-box` /
//!   `content-box` &c. resolve to a rect in the layout layer; the caller
//!   passes the resolved rect to [`ClipPath::contains`]).
//!
//! Reference: <https://www.w3.org/TR/css-masking-1/#the-clip-path>,
//! basic-shape grammar <https://www.w3.org/TR/css-shapes-1/#basic-shape-functions>.

#![forbid(unsafe_code)]

use crate::border_radius::{BorderRadius, CornerRadius};

// ---------------------------------------------------------------------------
// GeometryBox
// ---------------------------------------------------------------------------

/// The `<geometry-box>` reference a `clip-path` basic shape resolves against
/// (CSS Masking 1 § 5.1). `border-box` is the default for HTML; the SVG box
/// values (`fill-box` / `stroke-box` / `view-box`) land with the SVG paint
/// path. The caller maps the selected box to a concrete [`Rect`] before
/// calling [`ClipPath::contains`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GeometryBox {
    #[default]
    BorderBox,
    PaddingBox,
    ContentBox,
    MarginBox,
    FillBox,
    StrokeBox,
    ViewBox,
}

impl GeometryBox {
    /// Parse one `<geometry-box>` token (ASCII-case-insensitive). Returns
    /// `None` for an unknown token (the caller falls back to the default
    /// `border-box`).
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "border-box" => Some(GeometryBox::BorderBox),
            "padding-box" => Some(GeometryBox::PaddingBox),
            "content-box" => Some(GeometryBox::ContentBox),
            "margin-box" => Some(GeometryBox::MarginBox),
            "fill-box" => Some(GeometryBox::FillBox),
            "stroke-box" => Some(GeometryBox::StrokeBox),
            "view-box" => Some(GeometryBox::ViewBox),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Coord + Rect
// ---------------------------------------------------------------------------

/// A 2D axis-aligned rectangle (the reference box the caller resolves).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// Construct from `(x, y, width, height)`.
    pub const fn xywh(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
    /// The right edge.
    pub fn right(self) -> f32 {
        self.x + self.width
    }
    /// The bottom edge.
    pub fn bottom(self) -> f32 {
        self.y + self.height
    }
    /// The centre point.
    pub fn center(self) -> (f32, f32) {
        (self.x + self.width * 0.5, self.y + self.height * 0.5)
    }
}

/// One coordinate of a basic-shape position. CSS Shapes 1 § 6: a position
/// coordinate is a length, a percentage, or one of the keywords
/// `center` / `left`/`right` (x) / `top`/`bottom` (y). Resolved against the
/// reference-box extent via [`Coord::resolve`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Coord {
    /// A definite pixel length.
    Px(f32),
    /// A percentage of the reference-box extent (0.0 = top/left, 1.0 =
    /// bottom/right).
    Percent(f32),
    /// `center` — the 50% point.
    Center,
    /// `left` / `top` — the 0% point.
    Start,
    /// `right` / `bottom` — the 100% point.
    End,
}

impl Coord {
    /// Resolve this coordinate against a reference-box extent (`origin` =
    /// the box's top/left for that axis, `extent` = the box's width/height).
    /// A [`Coord::Px`] is absolute; a percentage / keyword is resolved
    /// against `extent`.
    pub fn resolve(self, origin: f32, extent: f32) -> f32 {
        match self {
            Coord::Px(v) => origin + v,
            Coord::Percent(p) => origin + extent * p,
            Coord::Center => origin + extent * 0.5,
            Coord::Start => origin,
            Coord::End => origin + extent,
        }
    }
}

// ---------------------------------------------------------------------------
// ClipPath
// ---------------------------------------------------------------------------

/// The `clip-path` basic shape (CSS Masking 1 § 5 + CSS Shapes 1 § 6). The
/// `None` variant is both the initial value and the `clip-path: none`
/// authored form (no clipping).
#[derive(Debug, Clone, PartialEq)]
pub enum ClipPath {
    /// `clip-path: none` — no clipping (the initial value).
    None,
    /// `inset(<offsets> round <radius>)` — a rectangle inset from the
    /// reference box by `top`/`right`/`bottom`/`left`, with optional corner
    /// rounding via [`BorderRadius`] (resolved against the inset rect).
    Inset {
        top: f32,
        right: f32,
        bottom: f32,
        left: f32,
        radius: BorderRadius,
    },
    /// `circle(<radius> at <position>)` — a circle. `radius` is the radius
    /// (`f32::INFINITY` for the `closest-side` default that fills the box);
    /// `cx`/`cy` are the centre coordinates.
    Circle { radius: f32, cx: Coord, cy: Coord },
    /// `ellipse(<rx> <ry> at <position>)` — an ellipse.
    Ellipse {
        rx: f32,
        ry: f32,
        cx: Coord,
        cy: Coord,
    },
    /// `polygon(<fill-rule>, <points>)` — a polygon. `points` is the vertex
    /// list (x, y pairs); `fill_rule` is `nonzero` (default) or `evenodd`.
    Polygon {
        points: Vec<(f32, f32)>,
        fill_rule: FillRule,
    },
}

/// The polygon fill rule (SVG § 8.4 / CSS Shapes 1 § 6.2). `NonZero` is the
/// default; `EvenOdd` is the `polygon(evenodd, …)` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FillRule {
    /// `nonzero` (default) — a point is inside if the winding number is
    /// non-zero.
    #[default]
    NonZero,
    /// `evenodd` — a point is inside if a ray crosses an odd number of
    /// edges.
    EvenOdd,
}

impl FillRule {
    /// Parse one fill-rule token (ASCII-case-insensitive). `None` for an
    /// unknown token (the caller falls back to the default `nonzero`).
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "nonzero" => Some(FillRule::NonZero),
            "evenodd" => Some(FillRule::EvenOdd),
            _ => None,
        }
    }
}

impl ClipPath {
    /// `true` iff `point` lies inside this shape resolved against `box`. The
    /// paint path calls this per pixel; the shape's percentage / keyword
    /// coordinates resolve against `box` here.
    ///
    /// [`ClipPath::None`] always returns `true` (no clipping).
    pub fn contains(&self, point: (f32, f32), box_: Rect) -> bool {
        let (px, py) = point;
        match self {
            ClipPath::None => true,
            ClipPath::Inset {
                top,
                right,
                bottom,
                left,
                radius,
            } => {
                // The inset rect.
                let r = Rect::xywh(
                    box_.x + left,
                    box_.y + top,
                    box_.width - left - right,
                    box_.height - top - bottom,
                );
                if px < r.x || px > r.right() || py < r.y || py > r.bottom() {
                    return false;
                }
                // Corner rounding: if the point is in a corner region, apply
                // the quarter-ellipse test.
                let tl = radius.top_left;
                let tr = radius.top_right;
                let br = radius.bottom_right;
                let bl = radius.bottom_left;
                // Top-left corner.
                if px < r.x + tl.h && py < r.y + tl.v {
                    return ellipse_quarter_contains(px, py, r.x + tl.h, r.y + tl.v, tl.h, tl.v);
                }
                // Top-right corner.
                if px > r.right() - tr.h && py < r.y + tr.v {
                    return ellipse_quarter_contains(
                        px,
                        py,
                        r.right() - tr.h,
                        r.y + tr.v,
                        tr.h,
                        tr.v,
                    );
                }
                // Bottom-right corner.
                if px > r.right() - br.h && py > r.bottom() - br.v {
                    return ellipse_quarter_contains(
                        px,
                        py,
                        r.right() - br.h,
                        r.bottom() - br.v,
                        br.h,
                        br.v,
                    );
                }
                // Bottom-left corner.
                if px < r.x + bl.h && py > r.bottom() - bl.v {
                    return ellipse_quarter_contains(
                        px,
                        py,
                        r.x + bl.h,
                        r.bottom() - bl.v,
                        bl.h,
                        bl.v,
                    );
                }
                true
            }
            ClipPath::Circle { radius, cx, cy } => {
                let cxr = cx.resolve(box_.x, box_.width);
                let cyr = cy.resolve(box_.y, box_.height);
                let dx = px - cxr;
                let dy = py - cyr;
                dx * dx + dy * dy <= radius * radius
            }
            ClipPath::Ellipse { rx, ry, cx, cy } => {
                let cxr = cx.resolve(box_.x, box_.width);
                let cyr = cy.resolve(box_.y, box_.height);
                let dx = (px - cxr) / rx;
                let dy = (py - cyr) / ry;
                dx * dx + dy * dy <= 1.0
            }
            ClipPath::Polygon { points, fill_rule } => {
                if points.len() < 3 {
                    return false;
                }
                match fill_rule {
                    FillRule::NonZero => point_in_polygon_nonzero(px, py, points),
                    FillRule::EvenOdd => point_in_polygon_evenodd(px, py, points),
                }
            }
        }
    }
}

/// Test whether `(px, py)` lies inside the quarter-ellipse centred at
/// `(cx, cy)` with radii `(rx, ry)`. The point is in the corner region
/// (outside the centre), so inside iff the point is within the ellipse.
fn ellipse_quarter_contains(px: f32, py: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> bool {
    if rx <= 0.0 || ry <= 0.0 {
        return true; // zero radius ⇒ sharp corner ⇒ always inside the region
    }
    let dx = (px - cx) / rx;
    let dy = (py - cy) / ry;
    dx * dx + dy * dy <= 1.0
}

// ---------------------------------------------------------------------------
// Polygon point-in-shape (SVG § 8.4 winding rules)
// ---------------------------------------------------------------------------

/// Even-odd rule: a ray from the point crosses an odd number of edges.
fn point_in_polygon_evenodd(px: f32, py: f32, points: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let n = points.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        if (yi > py) != (yj > py) {
            let x_cross = xi + (py - yi) / (yj - yi) * (xj - xi);
            if px < x_cross {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Non-zero rule: sum the signed edge crossings; inside iff the winding
/// number is non-zero.
fn point_in_polygon_nonzero(px: f32, py: f32, points: &[(f32, f32)]) -> bool {
    let mut winding = 0i32;
    let n = points.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        if (yi <= py) != (yj <= py) {
            // Direction of the crossing: up (yi<yj) or down.
            let x_cross = xi + (py - yi) / (yj - yi) * (xj - xi);
            if px < x_cross {
                if yi < yj {
                    winding += 1;
                } else {
                    winding -= 1;
                }
            }
        }
        j = i;
    }
    winding != 0
}

// ---------------------------------------------------------------------------
// parse_clip_path
// ---------------------------------------------------------------------------

/// Parse an authored `clip-path` value into a [`ClipPath`]. Recognises the
/// four basic shapes (`inset()` / `circle()` / `ellipse()` / `polygon()`)
/// and `none`; the `path()` form and the `<geometry-box>` suffix are
/// deferred (returns `None` for `path()`; a geometry-box suffix is parsed
/// but discarded here — the caller selects the reference box). Returns
/// `None` for an unrecognised value.
pub fn parse_clip_path(value: &str) -> Option<ClipPath> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("none") {
        return Some(ClipPath::None);
    }
    // Function form: `name(args)`.
    let (name, args) = split_function(v)?;
    match name.to_ascii_lowercase().as_str() {
        "inset" => parse_inset(args).map(|(t, r, b, l, radius)| ClipPath::Inset {
            top: t,
            right: r,
            bottom: b,
            left: l,
            radius,
        }),
        "circle" => parse_circle(args),
        "ellipse" => parse_ellipse(args),
        "polygon" => parse_polygon(args),
        _ => None, // path() and unknown forms are deferred.
    }
}

/// Split `name(args)` into the name and the inner-args string.
fn split_function(v: &str) -> Option<(&str, &str)> {
    let open = v.find('(')?;
    let close = v.rfind(')')?;
    if close < open {
        return None;
    }
    let name = v[..open].trim();
    let args = &v[open + 1..close];
    Some((name, args))
}

/// Parse the `inset(<offsets> [round <radius>])` argument list. Offsets are
/// 1–4 px/percent values (TRBL expansion per CSS Values 4 § 7); the `round
/// <radius>` is an optional `border-radius` shorthand.
fn parse_inset(args: &str) -> Option<(f32, f32, f32, f32, BorderRadius)> {
    // Split on ` round ` (case-insensitive) into offsets + radius parts.
    let lower = args.to_ascii_lowercase();
    let (offsets_part, radius_part) = match lower.find(" round ") {
        Some(idx) => (&args[..idx], Some(&args[idx + 7..])),
        None => (args, None),
    };
    let offsets: Vec<f32> = offsets_part
        .split_ascii_whitespace()
        .map(parse_length_or_percent)
        .collect::<Option<Vec<_>>>()?;
    let (top, right, bottom, left) = expand_trbl(&offsets)?;
    let radius = match radius_part {
        Some(r) => parse_border_radius(r).unwrap_or(BorderRadius::ZERO),
        None => BorderRadius::ZERO,
    };
    Some((top, right, bottom, left, radius))
}

/// Parse `circle([<radius>] [at <position>])`. The default radius is the
/// distance from the centre to the farthest corner (modelled as
/// `f32::INFINITY` here — the caller resolves the closest-side / farthest-
/// corner keyword against the box; the contains test treats infinity as
/// "always inside", which is wrong for farthest-corner but the caller
/// supplies the resolved radius for real geometry).
fn parse_circle(args: &str) -> Option<ClipPath> {
    let lower = args.to_ascii_lowercase();
    let (radius_part, pos_part) = match lower.find(" at ") {
        Some(idx) => (&args[..idx], Some(&args[idx + 4..])),
        None => (args, None),
    };
    let radius = if radius_part.trim().is_empty() {
        f32::INFINITY
    } else {
        // Accept a plain length/percent; the `closest-side`/`farthest-corner`
        // keywords are resolved by the caller (deferred).
        parse_length_or_percent(radius_part.trim()).unwrap_or(f32::INFINITY)
    };
    let (cx, cy) = match pos_part {
        Some(p) => parse_position(p).unwrap_or((Coord::Center, Coord::Center)),
        None => (Coord::Center, Coord::Center),
    };
    Some(ClipPath::Circle { radius, cx, cy })
}

/// Parse `ellipse([<rx> <ry>] [at <position>])`.
fn parse_ellipse(args: &str) -> Option<ClipPath> {
    let lower = args.to_ascii_lowercase();
    let (radii_part, pos_part) = match lower.find(" at ") {
        Some(idx) => (&args[..idx], Some(&args[idx + 4..])),
        None => (args, None),
    };
    let (rx, ry) = if radii_part.trim().is_empty() {
        (f32::INFINITY, f32::INFINITY)
    } else {
        let mut it = radii_part.split_ascii_whitespace();
        let rx = parse_length_or_percent(it.next()?)?;
        let ry = parse_length_or_percent(it.next().unwrap_or("0"))?;
        (rx, ry)
    };
    let (cx, cy) = match pos_part {
        Some(p) => parse_position(p).unwrap_or((Coord::Center, Coord::Center)),
        None => (Coord::Center, Coord::Center),
    };
    Some(ClipPath::Ellipse { rx, ry, cx, cy })
}

/// Parse `polygon([<fill-rule>,] <points>)`. Points are whitespace- or
/// comma-separated `x y` pairs; the optional fill-rule is a leading
/// `nonzero`/`evenodd` token followed by a comma.
fn parse_polygon(args: &str) -> Option<ClipPath> {
    let mut fill_rule = FillRule::NonZero;
    let mut rest = args;
    // Leading `<fill-rule>,`?
    let lower = args.to_ascii_lowercase();
    if lower.starts_with("nonzero") || lower.starts_with("evenodd") {
        let comma = args.find(',')?;
        let rule_tok = args[..comma].trim();
        fill_rule = FillRule::parse(rule_tok)?;
        rest = &args[comma + 1..];
    }
    let mut points = Vec::new();
    let mut last_x: Option<f32> = None;
    for tok in rest.split(|c: char| c.is_ascii_whitespace() || c == ',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let v = parse_length_or_percent(tok)?;
        match last_x {
            None => last_x = Some(v),
            Some(x) => {
                points.push((x, v));
                last_x = None;
            }
        }
    }
    if points.len() < 3 {
        return None;
    }
    Some(ClipPath::Polygon { points, fill_rule })
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse a `<length-percentage>` as a plain f32. `50%` → `50.0` (the raw
/// numeric prefix; the caller decides percent semantics — for positions we
/// divide by 100, for insets we treat the value verbatim); `10px` → `10.0`;
/// `10` → `10.0`. Returns `None` for a non-numeric token.
fn parse_length_or_percent(s: &str) -> Option<f32> {
    let s = s.trim();
    // Strip a trailing unit (`px` or `%`) and parse the leading number.
    let num_str = s
        .strip_suffix("px")
        .or_else(|| s.strip_suffix('%'))
        .unwrap_or(s)
        .trim();
    num_str.parse::<f32>().ok()
}

/// Expand a 1–4 value list to TRBL per CSS Values 4 § 7:
/// `a` → `a a a a`; `a b` → `a b a b`; `a b c` → `a b c b`;
/// `a b c d` → `a b c d`.
fn expand_trbl(values: &[f32]) -> Option<(f32, f32, f32, f32)> {
    match values {
        [a] => Some((*a, *a, *a, *a)),
        [a, b] => Some((*a, *b, *a, *b)),
        [a, b, c] => Some((*a, *b, *c, *b)),
        [a, b, c, d] => Some((*a, *b, *c, *d)),
        _ => None,
    }
}

/// Parse a `at <position>` pair into `(x, y)` coords. Tolerates the keyword
/// forms (`center`, `left`/`right` for x, `top`/`bottom` for y) and
/// length/percent values. The 1-value form (`at 50%`) sets x; y defaults to
/// `center`. This is a simplified position parse (the full 3-value form
/// `at top 50% right` lands with the cascade).
fn parse_position(s: &str) -> Option<(Coord, Coord)> {
    let toks: Vec<&str> = s.split_ascii_whitespace().collect();
    if toks.is_empty() {
        return None;
    }
    let parse_one = |t: &str| -> Coord {
        match t.to_ascii_lowercase().as_str() {
            "center" => Coord::Center,
            "left" | "top" => Coord::Start,
            "right" | "bottom" => Coord::End,
            _ => {
                // A percent → Coord::Percent (0.0–1.0); a px / bare number
                // → Coord::Px. `parse_length_or_percent` strips the unit
                // and returns the raw number.
                let is_percent = t.ends_with('%');
                match parse_length_or_percent(t) {
                    Some(v) if is_percent => Coord::Percent(v / 100.0),
                    Some(v) => Coord::Px(v),
                    None => Coord::Center,
                }
            }
        }
    };
    match toks.len() {
        1 => Some((parse_one(toks[0]), Coord::Center)),
        2 => Some((parse_one(toks[0]), parse_one(toks[1]))),
        _ => None,
    }
}

/// Parse a `border-radius` shorthand string into [`BorderRadius`]. The
/// 1–4 TRBL expansion + the optional `/` for the h/v split. Kept simple
/// (the full parser lives in the cascade); the inset `round` form is the
/// common case.
fn parse_border_radius(s: &str) -> Option<BorderRadius> {
    let (h_part, v_part) = match s.find('/') {
        Some(idx) => (&s[..idx], Some(&s[idx + 1..])),
        None => (s, None),
    };
    let h: Vec<f32> = h_part
        .split_ascii_whitespace()
        .map(parse_length_or_percent)
        .collect::<Option<Vec<_>>>()?;
    let v: Vec<f32> = match v_part {
        Some(p) => p
            .split_ascii_whitespace()
            .map(parse_length_or_percent)
            .collect::<Option<Vec<_>>>()?,
        None => h.clone(),
    };
    let (tl_h, tr_h, br_h, bl_h) = expand_trbl(&h)?;
    let (tl_v, tr_v, br_v, bl_v) = expand_trbl(&v)?;
    Some(BorderRadius {
        top_left: CornerRadius { h: tl_h, v: tl_v },
        top_right: CornerRadius { h: tr_h, v: tr_v },
        bottom_right: CornerRadius { h: br_h, v: br_v },
        bottom_left: CornerRadius { h: bl_h, v: bl_v },
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn box_100() -> Rect {
        Rect::xywh(0.0, 0.0, 100.0, 100.0)
    }

    // --- GeometryBox / FillRule ---------------------------------------

    #[test]
    fn geometry_box_parse() {
        assert_eq!(
            GeometryBox::parse("border-box"),
            Some(GeometryBox::BorderBox)
        );
        assert_eq!(
            GeometryBox::parse("Content-Box"),
            Some(GeometryBox::ContentBox)
        );
        assert_eq!(GeometryBox::parse("view-box"), Some(GeometryBox::ViewBox));
        assert_eq!(GeometryBox::parse("garbage"), None);
    }

    #[test]
    fn geometry_box_default_is_border_box() {
        assert_eq!(GeometryBox::default(), GeometryBox::BorderBox);
    }

    #[test]
    fn fill_rule_parse() {
        assert_eq!(FillRule::parse("nonzero"), Some(FillRule::NonZero));
        assert_eq!(FillRule::parse("evenodd"), Some(FillRule::EvenOdd));
        assert_eq!(FillRule::parse("inherit"), None);
        assert_eq!(FillRule::default(), FillRule::NonZero);
    }

    // --- Coord::resolve -----------------------------------------------

    #[test]
    fn coord_resolve_variants() {
        let origin = 10.0;
        let extent = 80.0;
        assert_eq!(Coord::Px(20.0).resolve(origin, extent), 30.0);
        assert_eq!(Coord::Percent(0.5).resolve(origin, extent), 50.0);
        assert_eq!(Coord::Center.resolve(origin, extent), 50.0);
        assert_eq!(Coord::Start.resolve(origin, extent), 10.0);
        assert_eq!(Coord::End.resolve(origin, extent), 90.0);
    }

    // --- None ---------------------------------------------------------

    #[test]
    fn none_always_contains() {
        let clip = parse_clip_path("none").unwrap();
        assert!(clip.contains((0.0, 0.0), box_100()));
        assert!(clip.contains((1000.0, 1000.0), box_100()));
    }

    // --- Inset --------------------------------------------------------

    #[test]
    fn parse_inset_simple() {
        let clip = parse_clip_path("inset(10px)").unwrap();
        assert!(matches!(
            clip,
            ClipPath::Inset {
                top: 10.0,
                right: 10.0,
                bottom: 10.0,
                left: 10.0,
                ..
            }
        ));
    }

    #[test]
    fn parse_inset_trbl_expansion() {
        let clip = parse_clip_path("inset(10px 20px 30px 40px)").unwrap();
        if let ClipPath::Inset {
            top,
            right,
            bottom,
            left,
            ..
        } = clip
        {
            assert_eq!((top, right, bottom, left), (10.0, 20.0, 30.0, 40.0));
        } else {
            panic!("expected Inset");
        }
    }

    #[test]
    fn inset_contains_interior_not_exterior() {
        let clip = parse_clip_path("inset(10px)").unwrap();
        let b = box_100();
        // Interior point (inset rect is 10..90).
        assert!(clip.contains((50.0, 50.0), b));
        // Just inside the inset edge.
        assert!(clip.contains((11.0, 50.0), b));
        // Outside the inset rect (in the 10px margin).
        assert!(!clip.contains((5.0, 50.0), b));
        // Outside the box.
        assert!(!clip.contains((95.0, 50.0), b));
    }

    #[test]
    fn inset_with_radius_clips_corner() {
        let clip = parse_clip_path("inset(0 round 20px)").unwrap();
        let b = box_100();
        // Centre is inside.
        assert!(clip.contains((50.0, 50.0), b));
        // The corner (1,1) is outside the rounded corner.
        assert!(!clip.contains((1.0, 1.0), b));
        // Mid-edge (1, 50) is inside (no rounding there).
        assert!(clip.contains((1.0, 50.0), b));
    }

    // --- Circle -------------------------------------------------------

    #[test]
    fn parse_circle_default() {
        // circle() with no args → infinite radius, centred.
        let clip = parse_clip_path("circle()").unwrap();
        if let ClipPath::Circle { radius, cx, cy } = clip {
            assert!(radius.is_infinite());
            assert_eq!(cx, Coord::Center);
            assert_eq!(cy, Coord::Center);
        } else {
            panic!("expected Circle");
        }
    }

    #[test]
    fn parse_circle_radius_and_position() {
        let clip = parse_clip_path("circle(50px at 25% 75%)").unwrap();
        if let ClipPath::Circle { radius, cx, cy } = clip {
            assert_eq!(radius, 50.0);
            assert_eq!(cx, Coord::Percent(0.25));
            assert_eq!(cy, Coord::Percent(0.75));
        } else {
            panic!("expected Circle");
        }
    }

    #[test]
    fn circle_contains_within_radius() {
        let clip = parse_clip_path("circle(50px at center)").unwrap();
        let b = box_100();
        // Centre (50,50) is inside.
        assert!(clip.contains((50.0, 50.0), b));
        // 40px from centre is inside (40 < 50).
        assert!(clip.contains((90.0, 50.0), b));
        // 60px from centre is outside.
        assert!(!clip.contains((110.0, 50.0), b));
    }

    #[test]
    fn circle_resolves_position_against_box() {
        // circle at 0% 0% (top-left), radius 30.
        let clip = parse_clip_path("circle(30px at 0% 0%)").unwrap();
        let b = Rect::xywh(100.0, 100.0, 100.0, 100.0);
        // Centre is (100,100); a point 20px right is inside.
        assert!(clip.contains((120.0, 100.0), b));
        // A point 40px right is outside.
        assert!(!clip.contains((140.0, 100.0), b));
    }

    // --- Ellipse ------------------------------------------------------

    #[test]
    fn parse_ellipse() {
        let clip = parse_clip_path("ellipse(50px 25px at center)").unwrap();
        if let ClipPath::Ellipse { rx, ry, .. } = clip {
            assert_eq!(rx, 50.0);
            assert_eq!(ry, 25.0);
        } else {
            panic!("expected Ellipse");
        }
    }

    #[test]
    fn ellipse_contains_resolves_radii() {
        let clip = parse_clip_path("ellipse(50px 25px at center)").unwrap();
        let b = box_100();
        // Centre inside.
        assert!(clip.contains((50.0, 50.0), b));
        // 40px right (within rx 50) inside.
        assert!(clip.contains((90.0, 50.0), b));
        // 40px down (beyond ry 25) outside.
        assert!(!clip.contains((50.0, 90.0), b));
    }

    // --- Polygon ------------------------------------------------------

    #[test]
    fn parse_polygon_triangle() {
        let clip = parse_clip_path("polygon(0 0, 100 0, 50 100)").unwrap();
        if let ClipPath::Polygon { points, fill_rule } = clip {
            assert_eq!(points, vec![(0.0, 0.0), (100.0, 0.0), (50.0, 100.0)]);
            assert_eq!(fill_rule, FillRule::NonZero);
        } else {
            panic!("expected Polygon");
        }
    }

    #[test]
    fn parse_polygon_with_fill_rule() {
        let clip = parse_clip_path("polygon(evenodd, 0 0, 100 0, 100 100, 0 100)").unwrap();
        if let ClipPath::Polygon { fill_rule, .. } = clip {
            assert_eq!(fill_rule, FillRule::EvenOdd);
        } else {
            panic!("expected Polygon");
        }
    }

    #[test]
    fn polygon_contains_centroid() {
        let clip = parse_clip_path("polygon(0 0, 100 0, 100 100, 0 100)").unwrap();
        let b = box_100();
        // Square (0..100); centroid inside.
        assert!(clip.contains((50.0, 50.0), b));
        // Outside the square.
        assert!(!clip.contains((150.0, 50.0), b));
    }

    #[test]
    fn polygon_evenodd_hole() {
        // A square with a square hole (winding cancels in the hole under
        // even-odd).
        let clip = parse_clip_path(
            "polygon(evenodd, 0 0, 100 0, 100 100, 0 100, 25 25, 75 25, 75 75, 25 75)",
        )
        .unwrap();
        let b = box_100();
        // Outside the outer square.
        assert!(!clip.contains((-10.0, 50.0), b));
        // In the hole (50,50) — even-odd says outside.
        assert!(!clip.contains((50.0, 50.0), b));
        // On the ring (10, 10) — inside.
        assert!(clip.contains((10.0, 10.0), b));
    }

    #[test]
    fn polygon_nonzero_hole_filled() {
        // Same geometry under nonzero: the inner square is wound the same
        // direction as the outer, so the hole region has winding 2 (inside).
        let clip = parse_clip_path(
            "polygon(nonzero, 0 0, 100 0, 100 100, 0 100, 25 25, 75 25, 75 75, 25 75)",
        )
        .unwrap();
        let b = box_100();
        // The "hole" is filled under nonzero (winding 2).
        assert!(clip.contains((50.0, 50.0), b));
    }

    // --- parse_clip_path edge cases -----------------------------------

    #[test]
    fn parse_none_case_insensitive() {
        assert!(matches!(parse_clip_path("NONE"), Some(ClipPath::None)));
        assert!(matches!(parse_clip_path(" None "), Some(ClipPath::None)));
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert!(parse_clip_path("url(#clip)").is_none());
        assert!(parse_clip_path("path('M 0 0 L 100 100')").is_none());
        assert!(parse_clip_path("garbage").is_none());
    }

    #[test]
    fn parse_polygon_too_few_points_returns_none() {
        assert!(parse_clip_path("polygon(0 0, 50 50)").is_none());
    }

    // --- TRBL expansion -----------------------------------------------

    #[test]
    fn expand_trbl_variants() {
        assert_eq!(expand_trbl(&[10.0]), Some((10.0, 10.0, 10.0, 10.0)));
        assert_eq!(expand_trbl(&[10.0, 20.0]), Some((10.0, 20.0, 10.0, 20.0)));
        assert_eq!(
            expand_trbl(&[10.0, 20.0, 30.0]),
            Some((10.0, 20.0, 30.0, 20.0))
        );
        assert_eq!(
            expand_trbl(&[10.0, 20.0, 30.0, 40.0]),
            Some((10.0, 20.0, 30.0, 40.0))
        );
        assert_eq!(expand_trbl(&[]), None);
    }
}
