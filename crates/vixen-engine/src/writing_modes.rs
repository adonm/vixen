//! CSS Writing Modes 3 § 3 + CSS Logical Properties 1 — the
//! `writing-mode` / `direction` → block + inline axis + the logical →
//! physical side mapping the layout + paint paths resolve against (Phase 4
//! prep). Pure given the two authored keywords; the cascade resolves them
//! first, then everything downstream (`margin-inline-start`, `inset-block-end`,
//! `inline-size`, `block-size`, the flex/grid main-axis selection, the
//! `text-align: start`/`end` resolution) folds into the physical box model
//! via this one mapping.
//!
//! What lives here:
//! - [`WritingMode`] — CSS Writing Modes 3 § 3.1 `writing-mode` (the five
//!   values: `horizontal-tb` / `vertical-rl` / `vertical-lr` / `sideways-rl`
//!   / `sideways-lr`).
//! - [`Direction`] — CSS Writing Modes 3 § 2.1 `direction` (`ltr` / `rtl`).
//! - [`Flow`] — the `(writing_mode, direction)` pair + the derived axis /
//!   side projections: [`Flow::block_axis`] / [`Flow::inline_axis`] (which
//!   physical axis each logical axis runs along), [`Flow::block_start`] /
//!   [`Flow::block_end`] / [`Flow::inline_start`] / [`Flow::inline_end`] →
//!   [`PhysicalSide`] (`top` / `right` / `bottom` / `left`).
//! - [`LogicalRect`] / [`LogicalSize`] / [`LogicalInsets`] — the
//!   logical-box shapes + [`LogicalRect::to_physical`] / [`LogicalSize::to_physical`]
//!   the box model + layout feed off.
//!
//! What does *not* live here:
//! - The `unicode-bidi` algorithm (the Unicode Bidirectional Algorithm § P1
//!   → P3 + the embedding/override reordering) — the text-shaping layer
//!   (Phase 4 layout) owns it; this module only carries the *paragraph*
//!   direction the bidi resolver seeds.
//! - The `text-orientation` glyph rotation (`mixed` / `upright` / `sideways`)
//!   — the paint path's glyph atlas consults it; the axis mapping here is
//!   unaffected (sideways-* writing modes reuse the vertical-* axis mapping
//!   per § 3.1, with the glyph rotation being the paint path's concern).
//! - Bi-directional override (`dir="auto"` on a container, the `bdi`/`bdo`
//!   element rules) — the host hook resolves the per-element `direction`
//!   before constructing the [`Flow`].
//!
//! ## The mapping table
//!
//! The § 7 logical → physical side mapping, condensed:
//!
//! | writing-mode      | dir | block-start | block-end | inline-start | inline-end |
//! |-------------------|-----|-------------|-----------|--------------|------------|
//! | horizontal-tb     | ltr | top         | bottom    | left         | right      |
//! | horizontal-tb     | rtl | top         | bottom    | right        | left       |
//! | vertical-rl       | ltr | right       | left      | top          | bottom     |
//! | vertical-rl       | rtl | right       | left      | bottom       | top        |
//! | vertical-lr       | ltr | left        | right     | top          | bottom     |
//! | vertical-lr       | rtl | left        | right     | bottom       | top        |
//! | sideways-rl       | *   | right       | left      | as vertical-rl (§ 3.1) |
//! | sideways-lr       | *   | left        | right     | as vertical-lr (§ 3.1) |
//!
//! `sideways-*` reuse the `vertical-*` axis + side mapping per § 3.1; only
//! the glyph rotation differs (the paint path). The `sideways-lr` default
//! inline direction is bottom-to-top, captured by the caller passing
//! [`Direction::Rtl`] when the host hook resolves `dir` for that mode.
//!
//! Reference: <https://www.w3.org/TR/css-writing-modes-3/>,
//! logical properties <https://www.w3.org/TR/css-logical-1/>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// WritingMode + Direction
// ---------------------------------------------------------------------------

/// CSS Writing Modes 3 § 3.1 `writing-mode` — the five values v1.0 supports.
/// `sideways-*` are CSS Writing Modes 4 (the § 3.1 axis mapping is shared
/// with the `vertical-*` counterparts; only the glyph rotation differs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WritingMode {
    /// `horizontal-tb` (the default) — block flow top-to-bottom, inline flow
    /// horizontal.
    #[default]
    HorizontalTb,
    /// `vertical-rl` — block flow right-to-left, inline flow vertical.
    VerticalRl,
    /// `vertical-lr` — block flow left-to-right, inline flow vertical.
    VerticalLr,
    /// `sideways-rl` (CSS WM 4) — as `vertical-rl` for the axis mapping; the
    /// glyphs are rotated 90° clockwise (the paint path).
    SidewaysRl,
    /// `sideways-lr` (CSS WM 4) — as `vertical-lr` for the axis mapping; the
    /// glyphs are rotated 90° counter-clockwise (the paint path).
    SidewaysLr,
}

impl WritingMode {
    /// Parse the `writing-mode` keyword (ASCII-case-insensitive per § 3.1).
    /// Unknown values fail closed to `None`; the cascade treats an unknown
    /// value as an invalid declaration (the caller's job to drop).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "horizontal-tb" => Some(Self::HorizontalTb),
            "vertical-rl" => Some(Self::VerticalRl),
            "vertical-lr" => Some(Self::VerticalLr),
            "sideways-rl" => Some(Self::SidewaysRl),
            "sideways-lr" => Some(Self::SidewaysLr),
            _ => None,
        }
    }

    /// The CSS serialised form (canonical lowercase).
    pub fn to_keyword(self) -> &'static str {
        match self {
            Self::HorizontalTb => "horizontal-tb",
            Self::VerticalRl => "vertical-rl",
            Self::VerticalLr => "vertical-lr",
            Self::SidewaysRl => "sideways-rl",
            Self::SidewaysLr => "sideways-lr",
        }
    }
}

/// CSS Writing Modes 3 § 2.1 `direction` — the inline-base direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Direction {
    /// `ltr` (the default) — inline content flows left-to-right (horizontal
    /// modes) or top-to-bottom (vertical modes).
    #[default]
    Ltr,
    /// `rtl` — inline content flows right-to-left (horizontal modes) or
    /// bottom-to-top (vertical modes).
    Rtl,
}

impl Direction {
    /// Parse the `direction` keyword (ASCII-case-insensitive per § 2.1).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ltr" => Some(Self::Ltr),
            "rtl" => Some(Self::Rtl),
            _ => None,
        }
    }

    /// The CSS serialised form (canonical lowercase).
    pub fn to_keyword(self) -> &'static str {
        match self {
            Self::Ltr => "ltr",
            Self::Rtl => "rtl",
        }
    }
}

// ---------------------------------------------------------------------------
// Flow — the (writing_mode, direction) pair + the derived projections
// ---------------------------------------------------------------------------

/// A physical axis: horizontal (x) or vertical (y).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    /// The x-axis (left ↔ right).
    Horizontal,
    /// The y-axis (top ↔ bottom).
    Vertical,
}

/// A physical box side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhysicalSide {
    Top,
    Right,
    Bottom,
    Left,
}

/// The resolved `(writing_mode, direction)` flow + the derived axis / side
/// projections the layout + paint paths consult.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flow {
    /// The writing mode.
    pub writing_mode: WritingMode,
    /// The inline-base direction.
    pub direction: Direction,
}

impl Flow {
    /// Construct a flow from the two authored keywords.
    pub const fn new(writing_mode: WritingMode, direction: Direction) -> Self {
        Self {
            writing_mode,
            direction,
        }
    }

    /// The block axis — the axis along which the block flow runs. `horizontal-tb`
    /// → vertical (blocks stack top-to-bottom); the vertical modes → horizontal
    /// (blocks stack right-to-left / left-to-right).
    pub fn block_axis(self) -> Axis {
        match self.writing_mode {
            WritingMode::HorizontalTb => Axis::Vertical,
            _ => Axis::Horizontal,
        }
    }

    /// The inline axis — the axis along which inline content flows.
    /// `horizontal-tb` → horizontal; the vertical modes → vertical.
    pub fn inline_axis(self) -> Axis {
        match self.writing_mode {
            WritingMode::HorizontalTb => Axis::Horizontal,
            _ => Axis::Vertical,
        }
    }

    /// `true` iff the writing mode is vertical (`vertical-*` / `sideways-*`).
    /// Convenience for the box model's width/height swap.
    pub fn is_vertical(self) -> bool {
        !matches!(self.writing_mode, WritingMode::HorizontalTb)
    }

    /// The physical side the `block-start` logical edge maps to (§ 7).
    pub fn block_start(self) -> PhysicalSide {
        match self.writing_mode {
            WritingMode::HorizontalTb => PhysicalSide::Top,
            WritingMode::VerticalRl | WritingMode::SidewaysRl => PhysicalSide::Right,
            WritingMode::VerticalLr | WritingMode::SidewaysLr => PhysicalSide::Left,
        }
    }

    /// The physical side the `block-end` logical edge maps to (§ 7) — the
    /// opposite of [`Self::block_start`].
    pub fn block_end(self) -> PhysicalSide {
        opposite(self.block_start())
    }

    /// The physical side the `inline-start` logical edge maps to (§ 7).
    pub fn inline_start(self) -> PhysicalSide {
        match self.writing_mode {
            WritingMode::HorizontalTb => match self.direction {
                Direction::Ltr => PhysicalSide::Left,
                Direction::Rtl => PhysicalSide::Right,
            },
            // Vertical modes: inline axis is vertical.
            _ => match self.direction {
                Direction::Ltr => PhysicalSide::Top,
                Direction::Rtl => PhysicalSide::Bottom,
            },
        }
    }

    /// The physical side the `inline-end` logical edge maps to (§ 7) — the
    /// opposite of [`Self::inline_start`].
    pub fn inline_end(self) -> PhysicalSide {
        opposite(self.inline_start())
    }
}

/// The opposite of a physical side (top ↔ bottom, left ↔ right).
fn opposite(side: PhysicalSide) -> PhysicalSide {
    match side {
        PhysicalSide::Top => PhysicalSide::Bottom,
        PhysicalSide::Bottom => PhysicalSide::Top,
        PhysicalSide::Left => PhysicalSide::Right,
        PhysicalSide::Right => PhysicalSide::Left,
    }
}

// ---------------------------------------------------------------------------
// Logical box shapes → physical
// ---------------------------------------------------------------------------

/// A logical (inline, block) size — the `inline-size` / `block-size` pair
/// CSS Logical Properties 1 introduces. [`LogicalSize::to_physical`] resolves
/// to a (width, height) physical size given the flow.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalSize {
    /// The inline-axis extent (`inline-size`).
    pub inline: f32,
    /// The block-axis extent (`block-size`).
    pub block: f32,
}

impl LogicalSize {
    /// Construct a logical size.
    pub const fn new(inline: f32, block: f32) -> Self {
        Self { inline, block }
    }

    /// Resolve to `(width, height)` given the flow. A vertical writing mode
    /// swaps the axes: `inline-size` → `height`, `block-size` → `width`.
    pub fn to_physical(self, flow: Flow) -> PhysicalSize {
        if flow.is_vertical() {
            PhysicalSize {
                width: self.block,
                height: self.inline,
            }
        } else {
            PhysicalSize {
                width: self.inline,
                height: self.block,
            }
        }
    }
}

/// A physical (width, height) size — the output of [`LogicalSize::to_physical`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicalSize {
    /// The x-axis extent.
    pub width: f32,
    /// The y-axis extent.
    pub height: f32,
}

/// A logical four-sided inset — the `inset-inline-start` / `inset-inline-end`
/// / `inset-block-start` / `inset-block-end` family (CSS Logical Properties 1
/// § 5.5). Resolves to the physical `(top, right, bottom, left)` insets the
/// box model consumes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalInsets {
    pub inline_start: f32,
    pub inline_end: f32,
    pub block_start: f32,
    pub block_end: f32,
}

/// A physical four-sided inset — `(top, right, bottom, left)`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicalInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl LogicalInsets {
    /// Resolve to the physical `(top, right, bottom, left)` insets given the
    /// flow (the § 7 side mapping applied to each logical edge).
    pub fn to_physical(self, flow: Flow) -> PhysicalInsets {
        let mut out = PhysicalInsets::default();
        place(flow.inline_start(), self.inline_start, &mut out);
        place(flow.inline_end(), self.inline_end, &mut out);
        place(flow.block_start(), self.block_start, &mut out);
        place(flow.block_end(), self.block_end, &mut out);
        out
    }
}

/// Write `value` into the physical side `side` of `out`.
fn place(side: PhysicalSide, value: f32, out: &mut PhysicalInsets) {
    match side {
        PhysicalSide::Top => out.top = value,
        PhysicalSide::Right => out.right = value,
        PhysicalSide::Bottom => out.bottom = value,
        PhysicalSide::Left => out.left = value,
    }
}

/// A logical rectangle — the `(inline-start, block-start, inline-size,
/// block-size)` tuple the layout produces for a box; resolves to a physical
/// `(x, y, width, height)` rect for paint.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalRect {
    /// The offset from the container's inline-start edge.
    pub inline_start: f32,
    /// The offset from the container's block-start edge.
    pub block_start: f32,
    /// The inline-axis extent.
    pub inline_size: f32,
    /// The block-axis extent.
    pub block_size: f32,
}

/// A physical `(x, y, width, height)` rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicalRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl LogicalRect {
    /// Resolve to a physical `(x, y, width, height)` rect given the flow +
    /// the containing block's physical size (needed to map an `inline-start`
    /// offset to an x/y coordinate when the inline direction is reversed).
    ///
    /// For `rtl` horizontal flow, `inline-start` is measured from the right
    /// edge: `x = container_width - inline_start - inline_size`. For the
    /// vertical modes, `block-start` is measured from the right/left edge
    /// analogously, and `inline-start` from the top/bottom edge.
    pub fn to_physical(self, flow: Flow, container: PhysicalSize) -> PhysicalRect {
        let physical = LogicalSize::new(self.inline_size, self.block_size).to_physical(flow);
        // Resolve the inline-start offset to the physical coordinate along the
        // inline axis.
        let (inline_pos, block_pos) = match flow.writing_mode {
            WritingMode::HorizontalTb => {
                let x = match flow.direction {
                    Direction::Ltr => self.inline_start,
                    Direction::Rtl => container.width - self.inline_start - self.inline_size,
                };
                (x, self.block_start)
            }
            WritingMode::VerticalRl | WritingMode::SidewaysRl => {
                // block-start is from the right edge; inline-start is vertical.
                let y = match flow.direction {
                    Direction::Ltr => self.inline_start,
                    Direction::Rtl => container.height - self.inline_start - self.inline_size,
                };
                let x = container.width - self.block_start - self.block_size;
                (x, y)
            }
            WritingMode::VerticalLr | WritingMode::SidewaysLr => {
                let y = match flow.direction {
                    Direction::Ltr => self.inline_start,
                    Direction::Rtl => container.height - self.inline_start - self.inline_size,
                };
                let x = self.block_start;
                (x, y)
            }
        };
        PhysicalRect {
            x: inline_pos,
            y: block_pos,
            width: physical.width,
            height: physical.height,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- WritingMode / Direction parse --------------------------------

    #[test]
    fn writing_mode_parse_case_insensitive() {
        assert_eq!(
            WritingMode::parse("horizontal-tb"),
            Some(WritingMode::HorizontalTb)
        );
        assert_eq!(
            WritingMode::parse("  Vertical-RL  "),
            Some(WritingMode::VerticalRl)
        );
        assert_eq!(
            WritingMode::parse("vertical-lr"),
            Some(WritingMode::VerticalLr)
        );
        assert_eq!(
            WritingMode::parse("sideways-rl"),
            Some(WritingMode::SidewaysRl)
        );
        assert_eq!(
            WritingMode::parse("sideways-lr"),
            Some(WritingMode::SidewaysLr)
        );
        assert_eq!(WritingMode::parse("bogus"), None);
    }

    #[test]
    fn writing_mode_round_trip() {
        for wm in [
            WritingMode::HorizontalTb,
            WritingMode::VerticalRl,
            WritingMode::VerticalLr,
            WritingMode::SidewaysRl,
            WritingMode::SidewaysLr,
        ] {
            assert_eq!(WritingMode::parse(wm.to_keyword()), Some(wm));
        }
    }

    #[test]
    fn direction_parse_case_insensitive() {
        assert_eq!(Direction::parse("ltr"), Some(Direction::Ltr));
        assert_eq!(Direction::parse("RTL"), Some(Direction::Rtl));
        assert_eq!(Direction::parse("auto"), None);
    }

    // --- axis projections --------------------------------------------

    #[test]
    fn horizontal_tb_axes() {
        let f = Flow::new(WritingMode::HorizontalTb, Direction::Ltr);
        assert_eq!(f.block_axis(), Axis::Vertical);
        assert_eq!(f.inline_axis(), Axis::Horizontal);
        assert!(!f.is_vertical());
    }

    #[test]
    fn vertical_modes_axes() {
        for wm in [
            WritingMode::VerticalRl,
            WritingMode::VerticalLr,
            WritingMode::SidewaysRl,
            WritingMode::SidewaysLr,
        ] {
            let f = Flow::new(wm, Direction::Ltr);
            assert_eq!(f.block_axis(), Axis::Horizontal, "{:?} block", wm);
            assert_eq!(f.inline_axis(), Axis::Vertical, "{:?} inline", wm);
            assert!(f.is_vertical(), "{:?} is_vertical", wm);
        }
    }

    // --- side mapping (the § 7 table) --------------------------------

    #[test]
    fn horizontal_tb_sides_ltr() {
        let f = Flow::new(WritingMode::HorizontalTb, Direction::Ltr);
        assert_eq!(f.block_start(), PhysicalSide::Top);
        assert_eq!(f.block_end(), PhysicalSide::Bottom);
        assert_eq!(f.inline_start(), PhysicalSide::Left);
        assert_eq!(f.inline_end(), PhysicalSide::Right);
    }

    #[test]
    fn horizontal_tb_sides_rtl() {
        let f = Flow::new(WritingMode::HorizontalTb, Direction::Rtl);
        assert_eq!(f.block_start(), PhysicalSide::Top);
        assert_eq!(f.block_end(), PhysicalSide::Bottom);
        assert_eq!(f.inline_start(), PhysicalSide::Right);
        assert_eq!(f.inline_end(), PhysicalSide::Left);
    }

    #[test]
    fn vertical_rl_sides_ltr() {
        let f = Flow::new(WritingMode::VerticalRl, Direction::Ltr);
        assert_eq!(f.block_start(), PhysicalSide::Right);
        assert_eq!(f.block_end(), PhysicalSide::Left);
        assert_eq!(f.inline_start(), PhysicalSide::Top);
        assert_eq!(f.inline_end(), PhysicalSide::Bottom);
    }

    #[test]
    fn vertical_rl_sides_rtl() {
        let f = Flow::new(WritingMode::VerticalRl, Direction::Rtl);
        assert_eq!(f.block_start(), PhysicalSide::Right);
        assert_eq!(f.block_end(), PhysicalSide::Left);
        assert_eq!(f.inline_start(), PhysicalSide::Bottom);
        assert_eq!(f.inline_end(), PhysicalSide::Top);
    }

    #[test]
    fn vertical_lr_sides_ltr() {
        let f = Flow::new(WritingMode::VerticalLr, Direction::Ltr);
        assert_eq!(f.block_start(), PhysicalSide::Left);
        assert_eq!(f.block_end(), PhysicalSide::Right);
        assert_eq!(f.inline_start(), PhysicalSide::Top);
        assert_eq!(f.inline_end(), PhysicalSide::Bottom);
    }

    #[test]
    fn vertical_lr_sides_rtl() {
        let f = Flow::new(WritingMode::VerticalLr, Direction::Rtl);
        assert_eq!(f.block_start(), PhysicalSide::Left);
        assert_eq!(f.block_end(), PhysicalSide::Right);
        assert_eq!(f.inline_start(), PhysicalSide::Bottom);
        assert_eq!(f.inline_end(), PhysicalSide::Top);
    }

    #[test]
    fn sideways_reuses_vertical_axis_mapping() {
        // sideways-rl shares vertical-rl's side mapping.
        let sw = Flow::new(WritingMode::SidewaysRl, Direction::Ltr);
        let vr = Flow::new(WritingMode::VerticalRl, Direction::Ltr);
        assert_eq!(sw.block_start(), vr.block_start());
        assert_eq!(sw.inline_start(), vr.inline_start());
        // sideways-lr shares vertical-lr's side mapping.
        let sw = Flow::new(WritingMode::SidewaysLr, Direction::Ltr);
        let vl = Flow::new(WritingMode::VerticalLr, Direction::Ltr);
        assert_eq!(sw.block_start(), vl.block_start());
        assert_eq!(sw.inline_start(), vl.inline_start());
    }

    // --- LogicalSize → physical --------------------------------------

    #[test]
    fn logical_size_horizontal_no_swap() {
        let f = Flow::new(WritingMode::HorizontalTb, Direction::Ltr);
        let p = LogicalSize::new(100.0, 200.0).to_physical(f);
        assert_eq!(
            p,
            PhysicalSize {
                width: 100.0,
                height: 200.0
            }
        );
    }

    #[test]
    fn logical_size_vertical_swaps() {
        let f = Flow::new(WritingMode::VerticalRl, Direction::Ltr);
        // inline=100 → height, block=200 → width.
        let p = LogicalSize::new(100.0, 200.0).to_physical(f);
        assert_eq!(
            p,
            PhysicalSize {
                width: 200.0,
                height: 100.0
            }
        );
    }

    // --- LogicalInsets → physical ------------------------------------

    #[test]
    fn logical_insets_horizontal_ltr() {
        let f = Flow::new(WritingMode::HorizontalTb, Direction::Ltr);
        let li = LogicalInsets {
            inline_start: 10.0,
            inline_end: 20.0,
            block_start: 30.0,
            block_end: 40.0,
        };
        let p = li.to_physical(f);
        // inline_start → left, inline_end → right, block_start → top, block_end → bottom.
        assert_eq!(
            p,
            PhysicalInsets {
                top: 30.0,
                right: 20.0,
                bottom: 40.0,
                left: 10.0
            }
        );
    }

    #[test]
    fn logical_insets_horizontal_rtl_swaps_inline() {
        let f = Flow::new(WritingMode::HorizontalTb, Direction::Rtl);
        let li = LogicalInsets {
            inline_start: 10.0,
            inline_end: 20.0,
            block_start: 30.0,
            block_end: 40.0,
        };
        let p = li.to_physical(f);
        // rtl: inline_start → right, inline_end → left.
        assert_eq!(
            p,
            PhysicalInsets {
                top: 30.0,
                right: 10.0,
                bottom: 40.0,
                left: 20.0
            }
        );
    }

    #[test]
    fn logical_insets_vertical_rl() {
        let f = Flow::new(WritingMode::VerticalRl, Direction::Ltr);
        let li = LogicalInsets {
            inline_start: 10.0,
            inline_end: 20.0,
            block_start: 30.0,
            block_end: 40.0,
        };
        let p = li.to_physical(f);
        // block_start → right, block_end → left; inline_start → top, inline_end → bottom.
        assert_eq!(
            p,
            PhysicalInsets {
                top: 10.0,
                right: 30.0,
                bottom: 20.0,
                left: 40.0
            }
        );
    }

    // --- LogicalRect → physical --------------------------------------

    #[test]
    fn logical_rect_horizontal_ltr() {
        let f = Flow::new(WritingMode::HorizontalTb, Direction::Ltr);
        let r = LogicalRect {
            inline_start: 10.0,
            block_start: 20.0,
            inline_size: 100.0,
            block_size: 50.0,
        };
        let p = r.to_physical(
            f,
            PhysicalSize {
                width: 1000.0,
                height: 1000.0,
            },
        );
        assert_eq!(
            p,
            PhysicalRect {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 50.0
            }
        );
    }

    #[test]
    fn logical_rect_horizontal_rtl_flips_x() {
        let f = Flow::new(WritingMode::HorizontalTb, Direction::Rtl);
        let r = LogicalRect {
            inline_start: 10.0,
            block_start: 20.0,
            inline_size: 100.0,
            block_size: 50.0,
        };
        let p = r.to_physical(
            f,
            PhysicalSize {
                width: 1000.0,
                height: 1000.0,
            },
        );
        // x = container_width - inline_start - inline_size = 1000 - 10 - 100 = 890.
        assert_eq!(
            p,
            PhysicalRect {
                x: 890.0,
                y: 20.0,
                width: 100.0,
                height: 50.0
            }
        );
    }

    #[test]
    fn logical_rect_vertical_rl_ltr() {
        let f = Flow::new(WritingMode::VerticalRl, Direction::Ltr);
        let r = LogicalRect {
            inline_start: 10.0,
            block_start: 30.0,
            inline_size: 100.0,
            block_size: 50.0,
        };
        let p = r.to_physical(
            f,
            PhysicalSize {
                width: 1000.0,
                height: 1000.0,
            },
        );
        // inline_start → y (ltr: from top), block_start → x (from right edge).
        // x = 1000 - 30 - 50 = 920; y = 10; width = block_size = 50; height = inline_size = 100.
        assert_eq!(
            p,
            PhysicalRect {
                x: 920.0,
                y: 10.0,
                width: 50.0,
                height: 100.0
            }
        );
    }

    #[test]
    fn logical_rect_vertical_lr_rtl_flips_y() {
        let f = Flow::new(WritingMode::VerticalLr, Direction::Rtl);
        let r = LogicalRect {
            inline_start: 10.0,
            block_start: 30.0,
            inline_size: 100.0,
            block_size: 50.0,
        };
        let p = r.to_physical(
            f,
            PhysicalSize {
                width: 1000.0,
                height: 1000.0,
            },
        );
        // block_start → x (from left) = 30; inline_start → y (rtl: from bottom).
        // y = 1000 - 10 - 100 = 890; width = 50; height = 100.
        assert_eq!(
            p,
            PhysicalRect {
                x: 30.0,
                y: 890.0,
                width: 50.0,
                height: 100.0
            }
        );
    }
}
