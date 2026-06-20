//! CSS stacking-context formation + paint-layer classification — Phase 5
//! paint prep (pure logic called out by `docs/PLAN.md` "Testing strategy" as a
//! Rust-unit-test surface). Implements the two pieces of the CSS stacking
//! model the display-list builder reduces to:
//!
//! 1. **What creates a stacking context** ([`forms_stacking_context`]) — CSS
//!    2.1 § 9.9.1 plus the CSS Positioned Layout 3 § 6, CSS Compositing 1
//!    § 3 (`isolation`), CSS Transforms 1 § 12, and Filter Effects § 5
//!    additions. The v1.0 rule set covers the surfaces the cascade resolves
//!    today (root, `position` + `z-index`, `opacity < 1`, `transform`,
//!    `filter`, `isolation`, `will-change`, `mix-blend-mode`, flex/grid items
//!    with `z-index`, `contain: paint|layout|strict|content`).
//! 2. **The seven-layer paint order** within a stacking context
//!    ([`StackingLayer`] + [`classify_descendant`]) — CSS 2.1 § App. E.2.1
//!    "the stacking context formation rule, layer-by-layer paint order". This
//!    is finer-grained than [`crate::display_list::z_tier`] (which collapses
//!    to negative/zero/positive); the display-list builder classifies every
//!    DrawItem into its [`StackingLayer`] and then sorts by layer (with
//!    document order as the stable tiebreaker).
//!
//! What lives here:
//! - [`StackingContextInputs`] — the cascade-resolved computed values the
//!   formation rules consult.
//! - [`forms_stacking_context`] — the formation predicate.
//! - [`StackingLayer`] — the seven paint layers in CSS 2.1 App. E order.
//! - [`DescendantInputs`] — the cascade-resolved values the classifier consults.
//! - [`classify_descendant`] — slot a descendant into its layer.
//!
//! What does *not* live here:
//! - The actual z-index numeric comparison for siblings within the negative
//!   or positive z-index children (the display-list builder's stable sort
//!   handles that, with [`StackingLayer`] as the primary key).
//! - 3D transforms' "sibling sorting" extension (CSS Transforms 2 § 5);
//!   v1.0 has the 2D surface only.
//! - `clip-path` / `mask` / `contain: paint` "pseudo-stacking-context"
//!   subtleties (the formation predicate captures them as a single boolean;
//!   the layer classification stays 7-layer per CSS 2.1).
//!
//! ## CSS 2.1 § App. E.2.1 layer order (bottom-to-top)
//!
//! Within a stacking context, descendants are painted in this exact order:
//! 1. The SC root's own backgrounds and borders.
//! 2. Stacking contexts with negative `z-index` (most negative first).
//! 3. In-flow, non-inline, non-positioned block-level descendants.
//! 4. Non-positioned floats.
//! 5. In-flow, non-positioned inline-level descendants.
//! 6. Positioned descendants with `z-index: auto` or `z-index: 0`.
//! 7. Stacking contexts with positive `z-index` (least positive first).
//!
//! [`crate::display_list::z_tier`] collapses this to the three z-buckets the
//! invariant builder needs; this module is the fine-grained layering the
//! paint pass uses for the in-flow layers (3–5) — those are the layers that
//! historically get the order wrong.
//!
//! Reference:
//! - CSS 2.1 § 9.9.1 + App. E.2: <https://www.w3.org/TR/CSS21/zindex.html>.
//! - CSS Positioned Layout 3 § 6: <https://www.w3.org/TR/css-position-3/#stacking>.
//! - CSS Compositing 1 § 3: <https://www.w3.org/TR/compositing-1/>.
//! - CSS Transforms 1 § 12: <https://www.w3.org/TR/css-transforms-1/>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Stacking-context formation
// ---------------------------------------------------------------------------

/// The `position` keyword on an element (CSS 2.1 § 9.3 + CSS Positioned
/// Layout 3 § 3.7). The formation predicate consults this to apply the
/// "position: fixed/sticky always forms a stacking context" rule and the
/// "position: absolute/relative needs z-index != auto" rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// `position: static` (the default) — in-flow, not positioned.
    #[default]
    Static,
    /// `position: relative` — positioned, in-flow slot.
    Relative,
    /// `position: absolute` — positioned, out of flow.
    Absolute,
    /// `position: fixed` — positioned, always forms a stacking context.
    Fixed,
    /// `position: sticky` — positioned, always forms a stacking context.
    Sticky,
}

impl Position {
    /// `true` for `absolute` / `relative` / `fixed` / `sticky`. Per CSS
    /// Positioned Layout 3, this is the predicate that distinguishes
    /// "positioned" from "in-flow" descendants for the § App. E.2.1 layer 3
    /// (in-flow block) vs layer 6 (positioned auto-z) split.
    pub fn is_positioned(self) -> bool {
        !matches!(self, Position::Static)
    }
}

/// The `z-index` value (CSS 2.1 § 9.9.1). `Auto` means "use the parent's
/// stacking context" for paint order; an integer means "this element forms a
/// stacking context" if it's also positioned (or is a flex/grid item —
/// [`forms_stacking_context`] handles that).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZIndex {
    /// `z-index: auto` (the default). The element participates in its
    /// parent's stacking context at the in-flow or positioned-auto layer.
    #[default]
    Auto,
    /// An explicit integer `z-index`.
    Integer(i32),
}

impl ZIndex {
    /// `true` for `Auto`.
    pub fn is_auto(self) -> bool {
        matches!(self, ZIndex::Auto)
    }

    /// The integer value, or `0` for `Auto` (CSS 2.1 § 9.9.6 treats `auto`
    /// as `0` for paint-order comparisons).
    pub fn as_int_or_zero(self) -> i32 {
        match self {
            ZIndex::Auto => 0,
            ZIndex::Integer(n) => n,
        }
    }

    /// `true` iff the value is a strictly-negative integer (`Auto` returns
    /// false; `Integer(0)` returns false).
    pub fn is_negative(self) -> bool {
        matches!(self, ZIndex::Integer(n) if n < 0)
    }

    /// `true` iff the value is a strictly-positive integer.
    pub fn is_positive(self) -> bool {
        matches!(self, ZIndex::Integer(n) if n > 0)
    }
}

/// The cascade-resolved computed values the formation predicate consults.
/// Each flag corresponds to one of the CSS 2.1 § 9.9.1 / Positioned Layout 3
/// § 6 / Compositing 1 § 3 / Transforms 1 § 12 rules. Defaults match the
/// initial values (notably `opacity == 1.0`); construct via
/// [`StackingContextInputs::default`] and set the fields the cascade populated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StackingContextInputs {
    /// `true` for the root element (always forms a stacking context).
    pub is_root: bool,
    /// `position` keyword.
    pub position: Position,
    /// `z-index` value.
    pub z_index: ZIndex,
    /// `opacity` — a value `< 1` forms a stacking context (Compositing 1 § 3).
    pub opacity: f32,
    /// `true` if `transform` is anything but `none` (Transforms 1 § 12).
    pub has_transform: bool,
    /// `true` if `filter` is anything but `none` (Filter Effects § 5).
    pub has_filter: bool,
    /// `true` if `clip-path` / `mask` / `mask-*` is set (the "visual effect"
    /// family that CSS Compositing 1 § 3 says forms a stacking context).
    pub has_clip_or_mask: bool,
    /// `true` if `isolation: isolate` (Compositing 1 § 3.3).
    pub isolation_isolate: bool,
    /// `true` if `mix-blend-mode != normal` (Compositing 1 § 3.1.4).
    pub mix_blend_mode_not_normal: bool,
    /// `true` if the element is a flex item AND `z-index != auto` (CSS Flex
    /// Box 1 § 4.4: flex items are stacking-context roots iff they have a
    /// `z-index` value).
    pub is_flex_item_with_z_index: bool,
    /// `true` if the element is a grid item AND `z-index != auto` (CSS Grid
    /// 1 § 8.6 mirrors the flex rule).
    pub is_grid_item_with_z_index: bool,
    /// `true` if `will-change` names any of the stacking-context-forming
    /// properties (`transform`, `opacity`, `filter`, …). CSS Will Change 1
    /// § 4: "if any of the properties being changed would form a stacking
    /// context, then the element forms one".
    pub will_change_stacking: bool,
    /// `true` if `contain: paint`, `contain: strict`, or `contain: content`
    /// is set (CSS Contain 1 § 3: `contain: paint` always forms a SC;
    /// `strict` and `content` imply `paint`).
    pub containment_paint: bool,
}

impl Default for StackingContextInputs {
    /// Initial values for every CSS property referenced above. Note that
    /// `opacity` defaults to `1.0` (NOT `f32::default()` which is `0.0` —
    /// that would spuriously form a stacking context for every default-
    /// constructed input).
    fn default() -> Self {
        Self {
            is_root: false,
            position: Position::Static,
            z_index: ZIndex::Auto,
            opacity: 1.0,
            has_transform: false,
            has_filter: false,
            has_clip_or_mask: false,
            isolation_isolate: false,
            mix_blend_mode_not_normal: false,
            is_flex_item_with_z_index: false,
            is_grid_item_with_z_index: false,
            will_change_stacking: false,
            containment_paint: false,
        }
    }
}

/// The CSS 2.1 § 9.9.1 + CSS Positioned Layout 3 § 6 stacking-context
/// formation predicate. Returns `true` when an element with these computed
/// values establishes a new stacking context (its descendants are painted
/// relative to it, not its parent SC).
pub fn forms_stacking_context(s: &StackingContextInputs) -> bool {
    // CSS 2.1 § 9.9.1: the root element always forms a stacking context.
    if s.is_root {
        return true;
    }
    // CSS Positioned Layout 3 § 6: position: fixed/sticky always forms a SC.
    if matches!(s.position, Position::Fixed | Position::Sticky) {
        return true;
    }
    // CSS 2.1 § 9.9.1: position: absolute/relative with z-index != auto.
    if matches!(s.position, Position::Absolute | Position::Relative) && !s.z_index.is_auto() {
        return true;
    }
    // CSS Compositing 1 § 3.2: opacity < 1.
    if s.opacity < 1.0 {
        return true;
    }
    // CSS Transforms 1 § 12: transform != none.
    if s.has_transform {
        return true;
    }
    // CSS Filter Effects § 5: filter != none.
    if s.has_filter {
        return true;
    }
    // CSS Compositing 1 § 3: clip-path / mask / mask-*.
    if s.has_clip_or_mask {
        return true;
    }
    // CSS Compositing 1 § 3.3: isolation: isolate.
    if s.isolation_isolate {
        return true;
    }
    // CSS Compositing 1 § 3.1.4: mix-blend-mode != normal.
    if s.mix_blend_mode_not_normal {
        return true;
    }
    // CSS Flexbox 1 § 4.4 + CSS Grid 1 § 8.6.
    if s.is_flex_item_with_z_index || s.is_grid_item_with_z_index {
        return true;
    }
    // CSS Will Change 1 § 4.
    if s.will_change_stacking {
        return true;
    }
    // CSS Contain 1 § 3.
    if s.containment_paint {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Seven-layer paint order (CSS 2.1 § App. E.2.1)
// ---------------------------------------------------------------------------

/// The seven paint layers within a stacking context, ordered bottom-to-top
/// (layer 1 painted first, layer 7 last). Use as the primary sort key for
/// the display list; document order is the stable tiebreaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum StackingLayer {
    /// Layer 1: the SC root's own backgrounds and borders. The display-list
    /// builder emits this implicitly when it opens the SC.
    ContextBackgroundAndBorders = 0,
    /// Layer 2: stacking contexts with negative `z-index` (most negative
    /// painted first; ties broken by document order).
    NegativeZChildren = 1,
    /// Layer 3: in-flow, non-inline, non-positioned block-level descendants.
    InFlowBlockLevel = 2,
    /// Layer 4: non-positioned floats.
    NonPositionedFloats = 3,
    /// Layer 5: in-flow, non-positioned inline-level descendants.
    InFlowInlineLevel = 4,
    /// Layer 6: positioned descendants with `z-index: auto` or `z-index: 0`.
    PositionedZeroZ = 5,
    /// Layer 7: stacking contexts with positive `z-index` (least positive
    /// painted first; ties broken by document order).
    PositiveZChildren = 6,
}

impl StackingLayer {
    /// The CSS 2.1 § App. E.2.1 layer number (1-indexed, matching the spec
    /// prose). Useful for diagnostics and the `--dump-display-list` projection.
    pub fn layer_number(self) -> u8 {
        // The discriminant is 0-indexed; the spec is 1-indexed.
        self as u8 + 1
    }
}

/// The cascade-resolved values the descendant classifier consults. This is
/// the subset of [`StackingContextInputs`] needed for layer 3–6 slotting.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DescendantInputs {
    /// The descendant's `position`.
    pub position: Position,
    /// The descendant's `z-index`.
    pub z_index: ZIndex,
    /// `true` if the descendant is floated (`float: left/right`).
    pub is_floated: bool,
    /// `true` if the descendant is inline-level (`display: inline`,
    /// `inline-block`, etc.).
    pub is_inline_level: bool,
    /// `true` if the descendant *itself* forms a stacking context (per
    /// [`forms_stacking_context`]). Layer 2/7 membership requires this —
    /// `z-index: 5` on an in-flow element that doesn't form a SC stays in
    /// layer 3/5.
    pub forms_stacking_context: bool,
}

/// Slot a descendant into its [`StackingLayer`] within its parent stacking
/// context. Per CSS 2.1 § App. E.2.1:
///
/// - `forms_stacking_context` descendants with negative `z-index` ⇒ layer 2.
/// - `forms_stacking_context` descendants with positive `z-index` ⇒ layer 7.
/// - Positioned (`absolute`/`relative`/`fixed`/`sticky`) descendants with
///   `z-index: auto` or `0` ⇒ layer 6.
/// - Non-positioned floats ⇒ layer 4.
/// - In-flow non-positioned inline-level ⇒ layer 5.
/// - In-flow non-positioned block-level ⇒ layer 3.
///
/// A descendant that forms a stacking context with `z-index: 0` is treated
/// as layer 6 by CSS 2.1 § 9.9.1 letter (it's painted with the auto/zero
/// positioned descendants), so it doesn't go into layers 2 or 7.
pub fn classify_descendant(d: &DescendantInputs) -> StackingLayer {
    // Stacking-context-forming children with non-zero z-index go in layer 2
    // (negative) or layer 7 (positive). The check is "forms SC AND z != 0".
    if d.forms_stacking_context && d.z_index.is_negative() {
        return StackingLayer::NegativeZChildren;
    }
    if d.forms_stacking_context && d.z_index.is_positive() {
        return StackingLayer::PositiveZChildren;
    }
    // Layer 6: positioned descendants with z auto/0. (CSS 2.1 § App. E.2
    // says "positioned descendants with z-index: auto or 0"; a stacking-
    // context-forming child with z-index 0 also paints in this layer.)
    if d.position.is_positioned() && !d.z_index.is_negative() && !d.z_index.is_positive() {
        return StackingLayer::PositionedZeroZ;
    }
    // Layer 4: non-positioned floats (CSS 2.1 § App. E.2 layer 4).
    if d.is_floated && !d.position.is_positioned() {
        return StackingLayer::NonPositionedFloats;
    }
    // Layer 5: in-flow non-positioned inline-level.
    if !d.position.is_positioned() && d.is_inline_level {
        return StackingLayer::InFlowInlineLevel;
    }
    // Layer 3: in-flow non-positioned block-level (the fall-through).
    StackingLayer::InFlowBlockLevel
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ZIndex helpers ------------------------------------------------

    #[test]
    fn z_index_auto_methods() {
        let z = ZIndex::Auto;
        assert!(z.is_auto());
        assert!(!z.is_negative());
        assert!(!z.is_positive());
        assert_eq!(z.as_int_or_zero(), 0);
    }

    #[test]
    fn z_index_integer_methods() {
        assert!(ZIndex::Integer(-1).is_negative());
        assert!(!ZIndex::Integer(0).is_negative());
        assert!(!ZIndex::Integer(0).is_positive());
        assert!(ZIndex::Integer(5).is_positive());
        assert_eq!(ZIndex::Integer(7).as_int_or_zero(), 7);
    }

    #[test]
    fn position_positioned_predicate() {
        assert!(!Position::Static.is_positioned());
        assert!(Position::Relative.is_positioned());
        assert!(Position::Absolute.is_positioned());
        assert!(Position::Fixed.is_positioned());
        assert!(Position::Sticky.is_positioned());
    }

    // --- forms_stacking_context ---------------------------------------

    #[test]
    fn root_always_forms() {
        let s = StackingContextInputs {
            is_root: true,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn static_in_flow_does_not_form() {
        let s = StackingContextInputs::default();
        assert!(!forms_stacking_context(&s));
    }

    #[test]
    fn absolute_with_z_index_forms() {
        let s = StackingContextInputs {
            position: Position::Absolute,
            z_index: ZIndex::Integer(0),
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn absolute_with_auto_z_does_not_form() {
        let s = StackingContextInputs {
            position: Position::Absolute,
            ..Default::default()
        };
        assert!(!forms_stacking_context(&s));
    }

    #[test]
    fn fixed_always_forms() {
        // Fixed forms a SC regardless of z-index (Positioned Layout 3 § 6).
        let s = StackingContextInputs {
            position: Position::Fixed,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
        let s = StackingContextInputs {
            position: Position::Fixed,
            z_index: ZIndex::Integer(5),
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn sticky_always_forms() {
        let s = StackingContextInputs {
            position: Position::Sticky,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn opacity_below_one_forms() {
        let s = StackingContextInputs {
            opacity: 0.99,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
        let s = StackingContextInputs {
            opacity: 1.0,
            ..Default::default()
        };
        assert!(!forms_stacking_context(&s));
    }

    #[test]
    fn transform_forms() {
        let s = StackingContextInputs {
            has_transform: true,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn filter_forms() {
        let s = StackingContextInputs {
            has_filter: true,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn clip_path_or_mask_forms() {
        let s = StackingContextInputs {
            has_clip_or_mask: true,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn isolation_isolate_forms() {
        let s = StackingContextInputs {
            isolation_isolate: true,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn mix_blend_mode_forms() {
        let s = StackingContextInputs {
            mix_blend_mode_not_normal: true,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn flex_item_with_z_index_forms() {
        let s = StackingContextInputs {
            is_flex_item_with_z_index: true,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
        // Without the z-index, a flex item is in-flow.
        let s = StackingContextInputs::default();
        assert!(!forms_stacking_context(&s));
    }

    #[test]
    fn grid_item_with_z_index_forms() {
        let s = StackingContextInputs {
            is_grid_item_with_z_index: true,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn will_change_stacking_forms() {
        let s = StackingContextInputs {
            will_change_stacking: true,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn containment_paint_forms() {
        let s = StackingContextInputs {
            containment_paint: true,
            ..Default::default()
        };
        assert!(forms_stacking_context(&s));
    }

    #[test]
    fn combination_of_non_stacking_flags_does_not_form() {
        // Two irrelevant properties don't combine to form a SC.
        let s = StackingContextInputs {
            position: Position::Relative, // positioned but z auto
            opacity: 1.0,                 // not < 1
            ..Default::default()
        };
        assert!(!forms_stacking_context(&s));
    }

    // --- classify_descendant ------------------------------------------

    #[test]
    fn negative_z_stacking_context_goes_to_layer_2() {
        let d = DescendantInputs {
            forms_stacking_context: true,
            z_index: ZIndex::Integer(-1),
            ..Default::default()
        };
        assert_eq!(classify_descendant(&d), StackingLayer::NegativeZChildren);
        assert_eq!(StackingLayer::NegativeZChildren.layer_number(), 2);
    }

    #[test]
    fn positive_z_stacking_context_goes_to_layer_7() {
        let d = DescendantInputs {
            forms_stacking_context: true,
            z_index: ZIndex::Integer(5),
            ..Default::default()
        };
        assert_eq!(classify_descendant(&d), StackingLayer::PositiveZChildren);
        assert_eq!(StackingLayer::PositiveZChildren.layer_number(), 7);
    }

    #[test]
    fn z_index_on_non_stacking_element_does_not_go_to_layer_2_or_7() {
        // An in-flow element with z-index: -1 but no SC-forming property
        // stays in the in-flow layers (CSS 2.1: z-index applies only to
        // positioned / flex / grid items).
        let d = DescendantInputs {
            forms_stacking_context: false,
            z_index: ZIndex::Integer(-1),
            ..Default::default()
        };
        assert_eq!(classify_descendant(&d), StackingLayer::InFlowBlockLevel);
    }

    #[test]
    fn positioned_auto_z_goes_to_layer_6() {
        let d = DescendantInputs {
            position: Position::Absolute,
            z_index: ZIndex::Auto,
            ..Default::default()
        };
        assert_eq!(classify_descendant(&d), StackingLayer::PositionedZeroZ);
        assert_eq!(StackingLayer::PositionedZeroZ.layer_number(), 6);
    }

    #[test]
    fn positioned_zero_z_stacking_context_also_layer_6() {
        // A stacking-context-forming child with z-index: 0 paints in layer 6
        // (CSS 2.1 § App. E.2).
        let d = DescendantInputs {
            forms_stacking_context: true,
            position: Position::Relative,
            z_index: ZIndex::Integer(0),
            ..Default::default()
        };
        assert_eq!(classify_descendant(&d), StackingLayer::PositionedZeroZ);
    }

    #[test]
    fn non_positioned_float_goes_to_layer_4() {
        let d = DescendantInputs {
            is_floated: true,
            ..Default::default()
        };
        assert_eq!(classify_descendant(&d), StackingLayer::NonPositionedFloats);
        assert_eq!(StackingLayer::NonPositionedFloats.layer_number(), 4);
    }

    #[test]
    fn positioned_float_goes_to_layer_6_not_layer_4() {
        // `position: absolute` overrides float for stacking purposes.
        let d = DescendantInputs {
            position: Position::Absolute,
            is_floated: true,
            z_index: ZIndex::Auto,
            ..Default::default()
        };
        assert_eq!(classify_descendant(&d), StackingLayer::PositionedZeroZ);
    }

    #[test]
    fn in_flow_inline_goes_to_layer_5() {
        let d = DescendantInputs {
            is_inline_level: true,
            ..Default::default()
        };
        assert_eq!(classify_descendant(&d), StackingLayer::InFlowInlineLevel);
        assert_eq!(StackingLayer::InFlowInlineLevel.layer_number(), 5);
    }

    #[test]
    fn in_flow_block_goes_to_layer_3() {
        let d = DescendantInputs::default();
        assert_eq!(classify_descendant(&d), StackingLayer::InFlowBlockLevel);
        assert_eq!(StackingLayer::InFlowBlockLevel.layer_number(), 3);
    }

    // --- Layer ordering invariant ------------------------------------

    #[test]
    fn layers_are_in_paint_order() {
        // The discriminants must be in CSS 2.1 § App. E.2.1 bottom-to-top
        // order so a sort ascending paints in the right order.
        assert!(StackingLayer::ContextBackgroundAndBorders < StackingLayer::NegativeZChildren);
        assert!(StackingLayer::NegativeZChildren < StackingLayer::InFlowBlockLevel);
        assert!(StackingLayer::InFlowBlockLevel < StackingLayer::NonPositionedFloats);
        assert!(StackingLayer::NonPositionedFloats < StackingLayer::InFlowInlineLevel);
        assert!(StackingLayer::InFlowInlineLevel < StackingLayer::PositionedZeroZ);
        assert!(StackingLayer::PositionedZeroZ < StackingLayer::PositiveZChildren);
    }
}
