//! Display-list invariant enforcement — Phase 5 prep (docs/SPEC.md
//! "Display-list invariants"). The eight rules there are Vixen's paint
//! contract, applied to the command stream *before* WebRender sees it. They
//! are pure logic over a flat command list, so they live here as Rust unit
//! tests (docs/PLAN.md "Testing strategy"), not WPT fixtures.
//!
//! # Painting model
//!
//! The layout tree emits [`DrawItem`]s in document order, each carrying its
//! already-intersected clip (`clip`, the product of ancestor `overflow:
//! hidden` per rule 2), its resolved effective opacity (rule 3 — the layout
//! tree multiplies ancestor group opacities; see [`effective_opacity`]), its
//! z-index, visibility, and the three boxes the background paints against.
//!
//! [`DisplayListBuilder::build`] consumes that flat list and emits a sorted,
//! pruned [`PaintCommand`] stream with every SPEC rule applied:
//!
//! 1. **z-index stacking** — stable sort into negative → zero → positive
//!    tiers, viewport background forced first ([`ZTier`]).
//! 2. **Clip stacking** — `overflow: hidden` clips content, not borders; the
//!    background fill rect is the `border_box` clipped to `clip`, while
//!    `background_clip` narrows it further to padding/content ([`clip_rects`]).
//! 3. **Opacity groups** — stack-based multiplication in [`effective_opacity`];
//!    an item with effective opacity `0` skips paint entirely (early-exit).
//! 4. **Visibility** — `hidden` / `collapse` skip paint but keep layout space
//!    (the layout tree still reserves the box; this builder just drops the
//!    draw, per rule 4).
//! 5. **Background clip** — `border-box` paints unclipped; `padding-box` /
//!    `content-box` narrow the fill ([`background_fill_rect`]).
//! 6. **Background attachment** — `fixed` uses viewport-relative positioning;
//!    the builder records the attachment on the command for the painter.
//! 7. **Background origin** — positions the image rect; captured on the
//!    command ([`origin_rect`]).
//! 8. **Empty clip skip** — any command whose pre-intersected clip is empty
//!    is dropped ([`Rect::is_empty`]).
//!
//! Reference: docs/SPEC.md "Display-list invariants". CSS 2.1 § 11.1.1 for the
//! "clips content not borders" rule. CSS Backgrounds 3 § 3.10 for clip/origin.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// An axis-aligned rectangle in physical device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Rule 8 (empty-clip skip): a zero-or-negative extent is empty.
    pub fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    /// Intersection; returns `None` when the result is empty (rule 8).
    pub fn intersect(self, other: Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        let r = Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0));
        if r.is_empty() { None } else { Some(r) }
    }
}

/// 8-bit RGBA. `#[derive(Copy)]` keeps the command stream cheap to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
}

// ---------------------------------------------------------------------------
// Item model (layout-tree output)
// ---------------------------------------------------------------------------

/// CSS `background-clip` / `background-origin` values (CSS Backgrounds 3 §
/// 3.10). `text` is post-v1.0 (docs/SPEC.md rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundBox {
    BorderBox,
    PaddingBox,
    ContentBox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Visible,
    /// `visibility: hidden` — skip paint, keep layout box (SPEC rule 4).
    Hidden,
    /// `visibility: collapse` — same paint behaviour as `hidden` for the
    /// display-list builder (layout shrinkage is a layout-time concern).
    Collapse,
}

impl Visibility {
    pub fn paints(self) -> bool {
        matches!(self, Visibility::Visible)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundAttachment {
    Scroll,
    Local,
    /// `fixed` → the painter repositions against the viewport (SPEC rule 6).
    Fixed,
}

/// One drawable emitted by layout. Fields mirror the SPEC invariants directly
/// so the builder's contract is auditable.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawItem {
    /// Document-order index; preserved as the stable-sort tiebreaker (rule 1).
    pub order: u32,
    pub z_index: i32,
    pub visibility: Visibility,
    /// Resolved effective opacity (ancestor groups multiplied). `0.0` skips
    /// paint (rule 3 early-exit). Computed via [`effective_opacity`].
    pub opacity: f32,
    /// The element's own `overflow: hidden` clip, already intersected with all
    /// ancestor clips (rule 2). `None` = unclipped.
    pub clip: Option<Rect>,
    /// The viewport-background item is always painted first (rule 1).
    pub is_viewport_background: bool,
    // Boxes for background clip/origin (CSS Backgrounds 3 § 3.10).
    pub border_box: Rect,
    pub padding_box: Rect,
    pub content_box: Rect,
    pub background_clip: BackgroundBox,
    pub background_origin: BackgroundBox,
    pub background_attachment: BackgroundAttachment,
    pub background: Option<Color>,
}

impl DrawItem {
    /// True when this item paints nothing (visibility or opacity cull it).
    pub fn paints_anything(&self) -> bool {
        self.visibility.paints() && self.opacity > 0.0
    }
}

// ---------------------------------------------------------------------------
// Output commands
// ---------------------------------------------------------------------------

/// A paint command after invariant enforcement. One paint path consumes this
/// (docs/ACCEPTANCE.md "One display list, one paint path").
#[derive(Debug, Clone, PartialEq)]
pub enum PaintCommand {
    /// Background fill. The rect is already the rule-2/rule-5 intersected fill
    /// rect; `attachment` carries rule-6 positioning; `origin` carries rule-7.
    Background {
        fill: Rect,
        color: Color,
        attachment: BackgroundAttachment,
        origin: BackgroundBox,
    },
}

// ---------------------------------------------------------------------------
// Pure helpers (each maps to a SPEC rule, each independently tested)
// ---------------------------------------------------------------------------

/// Rule 1: the z-index tier an item sorts into. Viewport background is a
/// special leading tier painted before any negative-z content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ZTier {
    ViewportBackground,
    NegativeZ,
    ZeroZ,
    PositiveZ,
}

/// Classify an item into its paint tier (rule 1).
pub fn z_tier(item: &DrawItem) -> ZTier {
    if item.is_viewport_background {
        return ZTier::ViewportBackground;
    }
    if item.z_index < 0 {
        ZTier::NegativeZ
    } else if item.z_index == 0 {
        ZTier::ZeroZ
    } else {
        ZTier::PositiveZ
    }
}

/// Rule 3: effective opacity is the product of the ancestor-group stack. A
/// single `0.0` anywhere in the chain zeroes the result (early-exit paint).
pub fn effective_opacity(stack: &[f32]) -> f32 {
    let mut acc = 1.0f32;
    for &o in stack {
        acc *= o;
        if acc == 0.0 {
            return 0.0;
        }
    }
    acc
}

/// Rule 5: which box the background fills into.
pub fn background_fill_rect(item: &DrawItem) -> Rect {
    match item.background_clip {
        BackgroundBox::BorderBox => item.border_box,
        BackgroundBox::PaddingBox => item.padding_box,
        BackgroundBox::ContentBox => item.content_box,
    }
}

/// Rule 7: which box the background image is positioned against.
pub fn origin_rect(item: &DrawItem) -> Rect {
    match item.background_origin {
        BackgroundBox::BorderBox => item.border_box,
        BackgroundBox::PaddingBox => item.padding_box,
        BackgroundBox::ContentBox => item.content_box,
    }
}

/// Rules 2 + 5 + 8: the rect actually painted for the background, after the
/// element clip (content, not borders) and the background-clip box intersect,
/// dropping empties (rule 8).
pub fn background_paint_rect(item: &DrawItem) -> Option<Rect> {
    let mut fill = background_fill_rect(item);
    // Rule 2: overflow:hidden clips content. The element's own clip applies to
    // the painted region. (CSS 2.1 § 11.1.1 — borders are *not* clipped by
    // overflow, but the background fill is content-like for this purpose.)
    if let Some(clip) = item.clip {
        fill = fill.intersect(clip)?;
    }
    if fill.is_empty() {
        return None;
    }
    Some(fill)
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Collects [`DrawItem`]s and emits the invariant-enforced [`PaintCommand`]
/// stream. One paint path (docs/ACCEPTANCE.md).
#[derive(Debug, Default)]
pub struct DisplayListBuilder {
    items: Vec<DrawItem>,
}

impl DisplayListBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: DrawItem) {
        self.items.push(item);
    }

    /// Apply every SPEC invariant and return the paint command stream.
    pub fn build(self) -> Vec<PaintCommand> {
        // Rule 4 (visibility) + rule 3 (opacity early-exit): drop culled items
        // first so the sort and empty-clip pass see only paintable work.
        let mut live: Vec<DrawItem> = self
            .items
            .into_iter()
            .filter(|i| i.paints_anything())
            .collect();

        // Rule 1: stable sort by tier, document order as tiebreaker. The
        // viewport background sorts before everything (its tier is lowest).
        live.sort_by(|a, b| {
            z_tier(a)
                .cmp(&z_tier(b))
                .then_with(|| a.order.cmp(&b.order))
        });

        let mut out = Vec::with_capacity(live.len());
        for item in live {
            let Some(color) = item.background else {
                continue;
            };
            // Rule 8 + rules 2/5: drop empties after clip intersection.
            let Some(fill) = background_paint_rect(&item) else {
                continue;
            };
            out.push(PaintCommand::Background {
                fill,
                color,
                attachment: item.background_attachment,
                origin: item.background_origin,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(order: u32, z: i32, color: Color) -> DrawItem {
        DrawItem {
            order,
            z_index: z,
            visibility: Visibility::Visible,
            opacity: 1.0,
            clip: None,
            is_viewport_background: false,
            border_box: Rect::new(0.0, 0.0, 10.0, 10.0),
            padding_box: Rect::new(1.0, 1.0, 8.0, 8.0),
            content_box: Rect::new(2.0, 2.0, 6.0, 6.0),
            background_clip: BackgroundBox::BorderBox,
            background_origin: BackgroundBox::PaddingBox,
            background_attachment: BackgroundAttachment::Scroll,
            background: Some(color),
        }
    }

    // --- Geometry ------------------------------------------------------

    #[test]
    fn rect_intersect_and_empty() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(
            a.intersect(Rect::new(5.0, 5.0, 10.0, 10.0)),
            Some(Rect::new(5.0, 5.0, 5.0, 5.0))
        );
        // Disjoint → None (rule 8).
        assert_eq!(a.intersect(Rect::new(20.0, 0.0, 5.0, 5.0)), None);
        // Zero-extent is empty.
        assert!(Rect::new(0.0, 0.0, 0.0, 5.0).is_empty());
        assert!(Rect::new(0.0, 0.0, -1.0, 5.0).is_empty());
    }

    // --- Rule 1: z-index stacking -------------------------------------

    #[test]
    fn z_tier_classification() {
        assert_eq!(
            z_tier(&{
                let mut i = item(0, 0, Color::WHITE);
                i.is_viewport_background = true;
                i
            }),
            ZTier::ViewportBackground
        );
        assert_eq!(z_tier(&item(0, -1, Color::WHITE)), ZTier::NegativeZ);
        assert_eq!(z_tier(&item(0, 0, Color::WHITE)), ZTier::ZeroZ);
        assert_eq!(z_tier(&item(0, 2, Color::WHITE)), ZTier::PositiveZ);
    }

    #[test]
    fn build_sorts_by_z_tier_then_document_order() {
        // Insert deliberately out of order.
        let mut b = DisplayListBuilder::new();
        b.push(item(0, 1, Color::rgb(10, 0, 0))); // positive
        b.push(item(1, -1, Color::rgb(20, 0, 0))); // negative
        b.push(item(2, 0, Color::rgb(30, 0, 0))); // zero
        b.push(item(3, -1, Color::rgb(40, 0, 0))); // negative, later in doc
        b.push(item(4, 0, Color::rgb(50, 0, 0))); // zero, later in doc
        let out = b.build();
        let colors: Vec<u8> = out
            .iter()
            .map(|c| match c {
                PaintCommand::Background { color, .. } => color.r,
            })
            .collect();
        // Expected: negative(20,40) then zero(30,50) then positive(10).
        assert_eq!(colors, vec![20, 40, 30, 50, 10]);
    }

    #[test]
    fn viewport_background_always_first() {
        let mut b = DisplayListBuilder::new();
        b.push(item(0, -5, Color::rgb(1, 0, 0))); // negative z, would normally lead
        let mut bg = item(1, 0, Color::rgb(2, 0, 0));
        bg.is_viewport_background = true;
        b.push(bg);
        let out = b.build();
        let first = out.first().unwrap();
        assert!(matches!(first, PaintCommand::Background { color, .. } if color.r == 2));
    }

    // --- Rule 3: opacity multiplication + early exit ------------------

    #[test]
    fn effective_opacity_multiplies_and_zeroes() {
        assert!((effective_opacity(&[0.5, 0.5]) - 0.25).abs() < 1e-6);
        assert!((effective_opacity(&[0.5, 0.5, 0.8]) - 0.2).abs() < 1e-6);
        // A single zero anywhere zeroes the chain.
        assert_eq!(effective_opacity(&[0.5, 0.0, 0.9]), 0.0);
        assert_eq!(effective_opacity(&[]), 1.0);
    }

    #[test]
    fn opacity_zero_skips_paint() {
        let mut i = item(0, 0, Color::WHITE);
        i.opacity = 0.0;
        let mut b = DisplayListBuilder::new();
        b.push(i);
        assert!(b.build().is_empty(), "opacity:0 must skip paint");
    }

    // --- Rule 4: visibility -------------------------------------------

    #[test]
    fn visibility_hidden_and_collapse_skip_paint() {
        for v in [Visibility::Hidden, Visibility::Collapse] {
            let mut i = item(0, 0, Color::WHITE);
            i.visibility = v;
            let mut b = DisplayListBuilder::new();
            b.push(i);
            assert!(b.build().is_empty(), "{v:?} must skip paint");
        }
        assert!(Visibility::Visible.paints());
    }

    // --- Rules 5 + 7: background clip / origin boxes ------------------

    #[test]
    fn background_clip_selects_fill_box() {
        let mut i = item(0, 0, Color::WHITE);
        i.background_clip = BackgroundBox::ContentBox;
        // Content box is (2,2,6,6).
        assert_eq!(background_fill_rect(&i), Rect::new(2.0, 2.0, 6.0, 6.0));
        i.background_clip = BackgroundBox::PaddingBox;
        assert_eq!(background_fill_rect(&i), Rect::new(1.0, 1.0, 8.0, 8.0));
        i.background_clip = BackgroundBox::BorderBox;
        assert_eq!(background_fill_rect(&i), Rect::new(0.0, 0.0, 10.0, 10.0));
    }

    #[test]
    fn origin_rect_tracks_background_origin() {
        let mut i = item(0, 0, Color::WHITE);
        i.background_origin = BackgroundBox::ContentBox;
        assert_eq!(origin_rect(&i), Rect::new(2.0, 2.0, 6.0, 6.0));
    }

    // --- Rules 2 + 8: clip + empty-clip skip --------------------------

    #[test]
    fn background_paint_rect_intersects_clip_and_drops_empty() {
        let mut i = item(0, 0, Color::WHITE);
        i.border_box = Rect::new(0.0, 0.0, 10.0, 10.0);
        i.background_clip = BackgroundBox::BorderBox;
        // Partial overlap → intersected.
        i.clip = Some(Rect::new(8.0, 0.0, 10.0, 10.0));
        assert_eq!(
            background_paint_rect(&i),
            Some(Rect::new(8.0, 0.0, 2.0, 10.0))
        );
        // Disjoint clip → None (rule 8 empty-clip skip).
        i.clip = Some(Rect::new(50.0, 50.0, 5.0, 5.0));
        assert_eq!(background_paint_rect(&i), None);
    }

    #[test]
    fn build_drops_empty_clip_items() {
        let mut i = item(0, 0, Color::WHITE);
        // Disjoint clip → intersected region is empty (rule 8).
        i.clip = Some(Rect::new(1000.0, 1000.0, 5.0, 5.0));
        let mut b = DisplayListBuilder::new();
        b.push(i);
        assert!(b.build().is_empty());
    }

    // --- Rule 6: attachment carried through ---------------------------

    #[test]
    fn background_attachment_carried_onto_command() {
        let mut i = item(0, 0, Color::WHITE);
        i.background_attachment = BackgroundAttachment::Fixed;
        let out = DisplayListBuilder::from_items([i]).build();
        assert!(matches!(
            out[0],
            PaintCommand::Background {
                attachment: BackgroundAttachment::Fixed,
                ..
            }
        ));
    }

    // --- No-background items vanish -----------------------------------

    #[test]
    fn item_without_background_emits_nothing() {
        let mut i = item(0, 0, Color::WHITE);
        i.background = None;
        let mut b = DisplayListBuilder::new();
        b.push(i);
        assert!(b.build().is_empty());
    }

    // Helper for the attachment test: construct from an iterator.
    impl DisplayListBuilder {
        fn from_items<I: IntoIterator<Item = DrawItem>>(items: I) -> Self {
            let mut b = DisplayListBuilder::new();
            for i in items {
                b.push(i);
            }
            b
        }
    }
}
