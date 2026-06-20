//! CSS box model resolution — Phase 4 layout prep (pure logic called out by
//! `docs/PLAN.md` "Testing strategy" as a Rust-unit-test surface). Turns the
//! computed edge lengths (`margin`/`border`/`padding`), the computed
//! `width`/`height`, and the `box-sizing` value into the four physical boxes
//! (content / padding / border / margin) that layout positions and that the
//! display-list builder paints.
//!
//! What lives here:
//! - [`BoxSizing`] — `content-box` / `border-box` (CSS Box Sizing 3 § 4).
//! - [`LengthOrAuto`] — `width`/`height`/`margin` are `auto`-able; `border`
//!   and `padding` never are (they default to `0`/medium, not `auto`).
//! - [`Edges`] — a four-sided `top/right/bottom/left` length bag.
//! - [`BoxModelInput`] — the computed values a single block generates.
//! - [`BoxModel`] / [`resolve_box_model`] — the resolved boxes + the
//!   CSS2 § 10.3.3 `auto`-margin / `auto`-width constraint solve.
//!
//! What does *not* live here:
//! - Real layout. Inline, flex, grid, replaced elements, floats, and vertical
//!   writing modes are all upstream `layout_2020`'s job (docs/PLAN.md Phase 4).
//!   This module covers only the *block-level non-replaced, horizontal-tb*
//!   constraint equation that the v1.0 headless `--computed-style` projection
//!   re-derives, and that layout's blockFormatting-context pre-pass feeds in.
//! - `auto` margin distribution in flex/grid containers (a layout concern).
//! - Negative-margin collapsing (a layout concern; the box model itself keeps
//!   them — the caller feeds already-collapsed edges if it wants).
//!
//! The horizontal constraint is CSS2 § 10.3.3:
//! `margin-l + border-l + padding-l + width + padding-r + border-r + margin-r
//!  == containing-block width`, with `auto` values resolved as:
//! 1. `auto` margins → `0` first (CSS2 § 10.3.3 rule 4, except the one/two
//!    remaining autos, handled below).
//! 2. If `width` is `auto`, the leftover space becomes `width` (clamped ≥ 0).
//! 3. If `width` is fixed and exactly one of L/R margin is `auto`, it absorbs
//!    the leftover (can be negative).
//! 4. If both L/R margins are `auto` (and `width` is fixed), they're equal —
//!    the block is centered — but clamped to `≥ 0`; if there's no space they
//!    both become `0` (CSS2 § 10.3.3 final paragraph).
//!
//! Vertical `auto` margins are always `0` for in-flow blocks (CSS2 § 10.6.3),
//! and `auto` height resolves against the content — out of scope here, so an
//! `auto` height is reported as `0` (layout fills the real value).
//!
//! Reference: <https://www.w3.org/TR/CSS2/visudet.html> (§ 10.3.3, 10.6.3),
//! CSS Box Sizing 3 § 4 (<https://www.w3.org/TR/css-box-3/>).

#![forbid(unsafe_code)]

use crate::display_list::Rect;

// ---------------------------------------------------------------------------
// Enums + edge bags
// ---------------------------------------------------------------------------

/// CSS `box-sizing` (CSS Box Sizing 3 § 4). `content-box` is the initial
/// value; `border-box` makes the declared `width`/`height` cover the border
/// and padding (the historical default for the user-agent stylesheet on form
/// controls, and the common `*,*::before,*::after { box-sizing: border-box }`
/// reset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoxSizing {
    #[default]
    ContentBox,
    BorderBox,
}

/// A length that may be `auto`. Only `width`, `height`, and `margin` accept
/// `auto`; `border`/`padding` are always definite (defaults: `border-width:
/// medium` ~3px or `0`, `padding: 0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthOrAuto {
    /// A definite length in CSS px (already resolved — percentages already
    /// turned into px by the cascade before reaching here).
    Px(f32),
    /// `auto` — resolved by [`resolve_box_model`] per CSS2 § 10.3 / 10.6.
    Auto,
}

impl LengthOrAuto {
    /// `true` for the `auto` variant.
    pub fn is_auto(self) -> bool {
        matches!(self, LengthOrAuto::Auto)
    }

    /// The definite px value, or `None` for `auto`.
    pub fn px(self) -> Option<f32> {
        match self {
            LengthOrAuto::Px(v) => Some(v),
            LengthOrAuto::Auto => None,
        }
    }
}

/// A four-sided length bag. Used for margin / border / padding. Physical
/// sides (the v1.0 scope is horizontal-tb per docs/ACCEPTANCE.md; logical
/// → physical mapping is the cascade's job).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub const ZERO: Edges = Edges {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    /// Sum of the inline (horizontal) sides — `left + right`.
    pub fn inline_sum(self) -> f32 {
        self.left + self.right
    }

    /// Sum of the block (vertical) sides — `top + bottom`.
    pub fn block_sum(self) -> f32 {
        self.top + self.bottom
    }
}

/// A four-sided `auto`-able length bag (margins only — border/padding are
/// always definite, see [`Edges`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoEdges {
    pub top: LengthOrAuto,
    pub right: LengthOrAuto,
    pub bottom: LengthOrAuto,
    pub left: LengthOrAuto,
}

impl AutoEdges {
    /// All four sides definite px.
    pub const fn px_all(v: f32) -> Self {
        Self {
            top: LengthOrAuto::Px(v),
            right: LengthOrAuto::Px(v),
            bottom: LengthOrAuto::Px(v),
            left: LengthOrAuto::Px(v),
        }
    }

    /// All four sides `auto`.
    pub const AUTO: AutoEdges = AutoEdges {
        top: LengthOrAuto::Auto,
        right: LengthOrAuto::Auto,
        bottom: LengthOrAuto::Auto,
        left: LengthOrAuto::Auto,
    };
}

// ---------------------------------------------------------------------------
// Input + output
// ---------------------------------------------------------------------------

/// Computed box-model inputs for one block-level, non-replaced, horizontal-tb
/// box. Everything is already cascade-resolved (percentages → px, lengths →
/// px via [`crate::length`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxModelInput {
    pub box_sizing: BoxSizing,
    /// Containing-block inline size (px). The CSS2 § 10.3.3 constraint
    /// equation equals this.
    pub containing_inline: f32,
    /// Margin edges (the only `auto`-able edge box).
    pub margin: AutoEdges,
    /// Border widths (px). Never `auto`.
    pub border: Edges,
    /// Padding (px). Never `auto`.
    pub padding: Edges,
    /// The declared `width` (interpreted per `box_sizing`).
    pub width: LengthOrAuto,
    /// The declared `height`. `auto` is reported as `0` (real height is a
    /// layout-time concern — see module docs).
    pub height: LengthOrAuto,
}

/// A resolved box model. Every field is a definite px value; the four
/// [`Rect`]s ([`BoxModel::margin_box`] / [`BoxModel::border_box`] /
/// [`BoxModel::padding_box`] / [`BoxModel::content_box`]) nest around the
/// border-box: `border_box` sits at origin `(0.0, 0.0)`, `padding_box` and
/// `content_box` are inset, `margin_box` is outset — `margin_box ⊃ border_box
/// ⊃ padding_box ⊃ content_box`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxModel {
    /// Margins after `auto` resolution (CSS2 § 10.3.3 for inline, § 10.6.3
    /// for block — vertical `auto` margins are always `0`).
    pub margin: Edges,
    pub border: Edges,
    pub padding: Edges,
    /// Content-box size (px). `width: auto` is the constraint-equation
    /// leftover; `height: auto` is `0` here (layout fills the real value).
    pub content_w: f32,
    pub content_h: f32,
}

impl BoxModel {
    /// Inline (horizontal) outer size: `margin + border + padding + content`.
    pub fn margin_inline_size(self) -> f32 {
        self.margin.inline_sum()
            + self.border.inline_sum()
            + self.padding.inline_sum()
            + self.content_w
    }

    /// Block (vertical) outer size.
    pub fn margin_block_size(self) -> f32 {
        self.margin.block_sum()
            + self.border.block_sum()
            + self.padding.block_sum()
            + self.content_h
    }

    /// Border-box rect, origin `(0,0)`, size = border + padding + content.
    pub fn border_box(self) -> Rect {
        let w = self.border.inline_sum() + self.padding.inline_sum() + self.content_w;
        let h = self.border.block_sum() + self.padding.block_sum() + self.content_h;
        Rect::new(0.0, 0.0, w, h)
    }

    /// Padding-box rect, inset from the border-box by the border edges.
    pub fn padding_box(self) -> Rect {
        let bx = self.border_box();
        Rect::new(
            bx.x + self.border.left,
            bx.y + self.border.top,
            bx.w - self.border.inline_sum(),
            bx.h - self.border.block_sum(),
        )
    }

    /// Content-box rect, inset from the padding-box by the padding edges.
    pub fn content_box(self) -> Rect {
        let p = self.padding_box();
        Rect::new(
            p.x + self.padding.left,
            p.y + self.padding.top,
            p.w - self.padding.inline_sum(),
            p.h - self.padding.block_sum(),
        )
    }

    /// Margin-box rect, outset from the border-box by the margin edges.
    pub fn margin_box(self) -> Rect {
        let bx = self.border_box();
        Rect::new(
            bx.x - self.margin.left,
            bx.y - self.margin.top,
            bx.w + self.margin.inline_sum(),
            bx.h + self.margin.block_sum(),
        )
    }
}

// ---------------------------------------------------------------------------
// Resolution (CSS2 § 10.3.3 inline constraint, § 10.6.3 block)
// ---------------------------------------------------------------------------

/// Resolve a [`BoxModelInput`] into a definite [`BoxModel`].
///
/// See the module docs for the exact `auto`-resolution rules. Borders and
/// padding are taken as-is; only margins and width are solved against the
/// containing-block inline size.
pub fn resolve_box_model(input: &BoxModelInput) -> BoxModel {
    // ---- Step 0: normalise the declared width against `box-sizing`. ----
    // `border-box` means the declared `width` *includes* border + padding, so
    // the content width is `declared - border - padding` (clamped ≥ 0). CSS
    // Box Sizing 3 § 4.
    let content_w_from_decl = |decl: f32| -> f32 {
        (decl - input.border.inline_sum() - input.padding.inline_sum()).max(0.0)
    };

    // ---- Step 1: provisional margins = auto → 0 (CSS2 § 10.3.3 rule 4). ----
    let mut ml = input.margin.left.px().unwrap_or(0.0);
    let mut mr = input.margin.right.px().unwrap_or(0.0);

    // ---- Step 2: resolve width. ----
    let content_w = match input.width {
        LengthOrAuto::Auto => {
            // Width absorbs the leftover; CSS2 § 10.3.3 rule 5. For
            // `border-box`, the leftover is the *outer* size and content is
            // derived after subtracting border+padding.
            let outer = input.containing_inline
                - ml
                - mr
                - input.border.inline_sum()
                - input.padding.inline_sum();
            outer.max(0.0)
        }
        LengthOrAuto::Px(decl) => {
            // A definite width. For border-box it covers border+padding.
            if input.box_sizing == BoxSizing::BorderBox {
                content_w_from_decl(decl)
            } else {
                decl.max(0.0)
            }
        }
    };

    // ---- Step 3: distribute leftover to auto margins (CSS2 § 10.3.3). ----
    // Only runs when width is definite (auto-width already consumed the
    // leftover above and there's nothing left to distribute).
    if matches!(input.width, LengthOrAuto::Px(_)) {
        let used = ml + mr + input.border.inline_sum() + input.padding.inline_sum() + content_w;
        let leftover = input.containing_inline - used;
        match (input.margin.left.is_auto(), input.margin.right.is_auto()) {
            (false, false) => {}
            (true, false) => ml += leftover,
            (false, true) => mr += leftover,
            (true, true) => {
                // Centered: equal split. Negative leftover → both stay 0
                // (CSS2 § 10.3.3 final paragraph — the constraints are
                // over-constrained, autos go to 0).
                let half = (leftover / 2.0).max(0.0);
                ml = half;
                mr = half;
            }
        }
    }

    // ---- Step 4: vertical (block) axis. ----
    // CSS2 § 10.6.3: `auto` margins top/bottom are `0` for in-flow blocks.
    let mt = input.margin.top.px().unwrap_or(0.0);
    let mb = input.margin.bottom.px().unwrap_or(0.0);
    let content_h = input.height.px().unwrap_or(0.0).max(0.0);

    BoxModel {
        margin: Edges {
            top: mt,
            right: mr,
            bottom: mb,
            left: ml,
        },
        border: input.border,
        padding: input.padding,
        content_w,
        content_h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: a minimal input with definite margins and no padding/border.
    fn input(cb: f32, width: LengthOrAuto) -> BoxModelInput {
        BoxModelInput {
            box_sizing: BoxSizing::ContentBox,
            containing_inline: cb,
            margin: AutoEdges {
                top: LengthOrAuto::Px(0.0),
                right: LengthOrAuto::Px(0.0),
                bottom: LengthOrAuto::Px(0.0),
                left: LengthOrAuto::Px(0.0),
            },
            border: Edges::ZERO,
            padding: Edges::ZERO,
            width,
            height: LengthOrAuto::Auto,
        }
    }

    // --- Definite everything -------------------------------------------

    #[test]
    fn definite_content_box_uses_declared_width() {
        // cb=800, width=200, no chrome → content=200, outer=200.
        let m = resolve_box_model(&input(800.0, LengthOrAuto::Px(200.0)));
        assert!((m.content_w - 200.0).abs() < 1e-4);
        assert!((m.margin_inline_size() - 200.0).abs() < 1e-4);
        assert_eq!(m.margin.left, 0.0);
        assert_eq!(m.margin.right, 0.0);
    }

    #[test]
    fn definite_border_box_subtracts_chrome() {
        // border-box width=300, border l/r=10 each, padding l/r=20 each →
        // content = 300 - 20 - 40 = 240.
        let i = BoxModelInput {
            box_sizing: BoxSizing::BorderBox,
            containing_inline: 1000.0,
            margin: AutoEdges::px_all(0.0),
            border: Edges {
                top: 0.0,
                right: 10.0,
                bottom: 0.0,
                left: 10.0,
            },
            padding: Edges {
                top: 0.0,
                right: 20.0,
                bottom: 0.0,
                left: 20.0,
            },
            width: LengthOrAuto::Px(300.0),
            height: LengthOrAuto::Auto,
        };
        let m = resolve_box_model(&i);
        assert!((m.content_w - 240.0).abs() < 1e-4, "got {}", m.content_w);
        // Border-box size is the declared 300.
        assert!((m.border_box().w - 300.0).abs() < 1e-4);
    }

    #[test]
    fn border_box_clamps_negative_content_to_zero() {
        // border-box width=10, but border+padding=40 → content would be -30;
        // clamped to 0.
        let i = BoxModelInput {
            box_sizing: BoxSizing::BorderBox,
            containing_inline: 1000.0,
            margin: AutoEdges::px_all(0.0),
            border: Edges {
                top: 0.0,
                right: 20.0,
                bottom: 0.0,
                left: 20.0,
            },
            padding: Edges::ZERO,
            width: LengthOrAuto::Px(10.0),
            height: LengthOrAuto::Auto,
        };
        let m = resolve_box_model(&i);
        assert_eq!(m.content_w, 0.0);
    }

    // --- Auto width ----------------------------------------------------

    #[test]
    fn auto_width_consumes_leftover() {
        // cb=800, margins=0, no chrome, width=auto → content=800.
        let m = resolve_box_model(&input(800.0, LengthOrAuto::Auto));
        assert!((m.content_w - 800.0).abs() < 1e-4);
    }

    #[test]
    fn auto_width_after_chrome_and_margins() {
        // cb=1000, margin l/r = 50 each (100 total), padding l/r=20 (40),
        // border l/r=10 (20). Leftover = 1000 - 100 - 40 - 20 = 840.
        let i = BoxModelInput {
            box_sizing: BoxSizing::ContentBox,
            containing_inline: 1000.0,
            margin: AutoEdges::px_all(50.0),
            border: Edges {
                top: 0.0,
                right: 10.0,
                bottom: 0.0,
                left: 10.0,
            },
            padding: Edges {
                top: 0.0,
                right: 20.0,
                bottom: 0.0,
                left: 20.0,
            },
            width: LengthOrAuto::Auto,
            height: LengthOrAuto::Auto,
        };
        let m = resolve_box_model(&i);
        assert!((m.content_w - 840.0).abs() < 1e-4, "got {}", m.content_w);
    }

    // --- Auto margins --------------------------------------------------

    #[test]
    fn one_auto_margin_absorbs_leftover() {
        // cb=800, width=200, margin-left=100 definite, margin-right=auto →
        // right = 800 - 200 - 100 = 500.
        let i = BoxModelInput {
            box_sizing: BoxSizing::ContentBox,
            containing_inline: 800.0,
            margin: AutoEdges {
                top: LengthOrAuto::Px(0.0),
                right: LengthOrAuto::Auto,
                bottom: LengthOrAuto::Px(0.0),
                left: LengthOrAuto::Px(100.0),
            },
            border: Edges::ZERO,
            padding: Edges::ZERO,
            width: LengthOrAuto::Px(200.0),
            height: LengthOrAuto::Auto,
        };
        let m = resolve_box_model(&i);
        assert!(
            (m.margin.right - 500.0).abs() < 1e-4,
            "got {}",
            m.margin.right
        );
        assert_eq!(m.margin.left, 100.0);
    }

    #[test]
    fn two_auto_margins_center_the_block() {
        // cb=800, width=200, both margins auto → each = (800-200)/2 = 300.
        let i = BoxModelInput {
            box_sizing: BoxSizing::ContentBox,
            containing_inline: 800.0,
            margin: AutoEdges {
                top: LengthOrAuto::Px(0.0),
                right: LengthOrAuto::Auto,
                bottom: LengthOrAuto::Px(0.0),
                left: LengthOrAuto::Auto,
            },
            border: Edges::ZERO,
            padding: Edges::ZERO,
            width: LengthOrAuto::Px(200.0),
            height: LengthOrAuto::Auto,
        };
        let m = resolve_box_model(&i);
        assert!((m.margin.left - 300.0).abs() < 1e-4);
        assert!((m.margin.right - 300.0).abs() < 1e-4);
    }

    #[test]
    fn two_auto_margins_clamp_when_over_constrained() {
        // cb=100, width=200 (overflows) → negative leftover; both auto margins
        // clamp to 0 rather than going negative.
        let i = BoxModelInput {
            box_sizing: BoxSizing::ContentBox,
            containing_inline: 100.0,
            margin: AutoEdges {
                top: LengthOrAuto::Px(0.0),
                right: LengthOrAuto::Auto,
                bottom: LengthOrAuto::Px(0.0),
                left: LengthOrAuto::Auto,
            },
            border: Edges::ZERO,
            padding: Edges::ZERO,
            width: LengthOrAuto::Px(200.0),
            height: LengthOrAuto::Auto,
        };
        let m = resolve_box_model(&i);
        assert_eq!(m.margin.left, 0.0);
        assert_eq!(m.margin.right, 0.0);
    }

    #[test]
    fn auto_margin_not_distributed_when_width_auto() {
        // width=auto already consumes the leftover; auto margins stay 0
        // (CSS2 § 10.3.3: there's nothing left to distribute).
        let i = BoxModelInput {
            box_sizing: BoxSizing::ContentBox,
            containing_inline: 800.0,
            margin: AutoEdges::AUTO,
            border: Edges::ZERO,
            padding: Edges::ZERO,
            width: LengthOrAuto::Auto,
            height: LengthOrAuto::Auto,
        };
        let m = resolve_box_model(&i);
        assert_eq!(m.margin.left, 0.0);
        assert_eq!(m.margin.right, 0.0);
        assert!((m.content_w - 800.0).abs() < 1e-4);
    }

    // --- Vertical axis -------------------------------------------------

    #[test]
    fn vertical_auto_margins_are_zero() {
        let i = BoxModelInput {
            box_sizing: BoxSizing::ContentBox,
            containing_inline: 800.0,
            margin: AutoEdges::AUTO,
            border: Edges::ZERO,
            padding: Edges::ZERO,
            width: LengthOrAuto::Px(100.0),
            height: LengthOrAuto::Px(50.0),
        };
        let m = resolve_box_model(&i);
        assert_eq!(m.margin.top, 0.0);
        assert_eq!(m.margin.bottom, 0.0);
        assert!((m.content_h - 50.0).abs() < 1e-4);
    }

    #[test]
    fn auto_height_reports_zero() {
        // Real height is a layout-time concern (content-based); the box model
        // reports 0 so callers can detect "needs layout" deterministically.
        let i = BoxModelInput {
            box_sizing: BoxSizing::ContentBox,
            containing_inline: 800.0,
            margin: AutoEdges::px_all(0.0),
            border: Edges::ZERO,
            padding: Edges::ZERO,
            width: LengthOrAuto::Px(100.0),
            height: LengthOrAuto::Auto,
        };
        let m = resolve_box_model(&i);
        assert_eq!(m.content_h, 0.0);
    }

    // --- Boxes ----------------------------------------------------------

    #[test]
    fn boxes_nest_from_common_origin() {
        // margin 10 all, border 5, padding 15, content 100x100.
        let i = BoxModelInput {
            box_sizing: BoxSizing::ContentBox,
            containing_inline: 1000.0,
            margin: AutoEdges::px_all(10.0),
            border: Edges {
                top: 5.0,
                right: 5.0,
                bottom: 5.0,
                left: 5.0,
            },
            padding: Edges {
                top: 15.0,
                right: 15.0,
                bottom: 15.0,
                left: 15.0,
            },
            width: LengthOrAuto::Px(100.0),
            height: LengthOrAuto::Px(100.0),
        };
        let m = resolve_box_model(&i);
        let content = m.content_box();
        let padding = m.padding_box();
        let border = m.border_box();
        let margin = m.margin_box();

        // The four boxes nest around the border-box at origin (0,0):
        //   border_box (0,0) 140x140
        //   padding_box inset by border 5 → (5,5) 130x130
        //   content_box inset by padding 15 → (20,20) 100x100
        //   margin_box outset by margin 10 → (-10,-10) 160x160
        assert_eq!(border, Rect::new(0.0, 0.0, 140.0, 140.0));
        assert_eq!(padding, Rect::new(5.0, 5.0, 130.0, 130.0));
        assert_eq!(content, Rect::new(20.0, 20.0, 100.0, 100.0));
        assert_eq!(margin, Rect::new(-10.0, -10.0, 160.0, 160.0));
    }

    #[test]
    fn edges_helpers() {
        let e = Edges {
            top: 1.0,
            right: 2.0,
            bottom: 3.0,
            left: 4.0,
        };
        assert_eq!(e.inline_sum(), 6.0);
        assert_eq!(e.block_sum(), 4.0);
        assert_eq!(Edges::ZERO.inline_sum(), 0.0);
    }

    #[test]
    fn length_or_auto_introspection() {
        assert!(LengthOrAuto::Auto.is_auto());
        assert!(!LengthOrAuto::Px(3.0).is_auto());
        assert_eq!(LengthOrAuto::Px(3.0).px(), Some(3.0));
        assert_eq!(LengthOrAuto::Auto.px(), None);
        assert_eq!(AutoEdges::px_all(2.0).left, LengthOrAuto::Px(2.0));
        assert!(AutoEdges::AUTO.top.is_auto());
    }
}
