//! CSS Scroll Snap 1 § 5 — the `scroll-snap-align` snap-position
//! computation + the `scroll-snap-type` axis/strictness model the scroll
//! container's snap targeting reduces to (Phase 4 prep). Pure given
//! cascade-resolved px geometry; the actual scroll animation + the
//! proximity-vs-mandatory policy decision stay in the input/scroll layer
//! where the user's gesture + the scrollable-overflow size live.
//!
//! What lives here:
//! - [`ScrollSnapType`] — § 5.1 `scroll-snap-type` (`none`, or
//!   `(axis, strictness)`; the axis is `x` / `y` / `block` / `inline` /
//!   `both`, the strictness `proximity` / `mandatory`).
//! - [`SnapAlign`] — § 5.2 `scroll-snap-align` (`none` / `start` / `end` /
//!   `center`, per axis).
//! - [`SnapStop`] — § 5.3 `scroll-snap-stop` (`normal` / `always`).
//! - [`SnapArea`] — a snap target's margin-box rect in scrollable-content
//!   coordinates (offset + size).
//! - [`compute_axis`] — the § 5 "snap position" for one axis: given the
//!   scrollport size, the area's offset + size, and the align, the scroll
//!   offset that brings the area to the alignment, clamped to the
//!   scrollable range `[0, max_scroll]`.
//! - [`compute_snap`] — the `(x, y)` pair for a snap target.
//! - [`should_snap`] — the strictness policy: `mandatory` always snaps;
//!   `proximity` snaps iff the target is within `proximity_threshold` of
//!   the current scroll (default half the scrollport, the § 5.1 "sufficient
//!   proximity" reading).
//!
//! What does *not* live here:
//! - The scrollable-overflow computation (the union of the scroll
//!   container's content + descendants' margin boxes) — the layout layer's
//!   job; the caller passes `scrollable_overflow` per axis.
//! - The scroll animation / easing to the snap position — the input layer
//!   drives the animated scroll; this module only computes the target.
//! - `scroll-padding` + `scroll-margin` (the snapport / snaparea insets) —
//!   the caller resolves them into the `scrollport_size` / `SnapArea`
//!   geometry before calling (the § 5 "snapport" = scrollport minus
//!   `scroll-padding`; the § 5 "snap area" = snap target's margin box plus
//!   `scroll-margin`).
//! - Resnapping on content/layout change (§ 5.4) — the host hook's job.
//!
//! ## The snap-position formula
//!
//! For one axis, with `S` = scrollport size, `O` = area offset within the
//! scrollable content, `A` = area size:
//!
//! ```text
//! start  : scroll = O                       (area.start meets scrollport.start)
//! end    : scroll = O + A - S               (area.end meets scrollport.end)
//! center : scroll = O + A/2 - S/2           (centres coincide)
//! ```
//!
//! Then clamp `scroll` to `[0, max(0, overflow - S)]` so a snap target
//! never scrolls past the scrollable range.
//!
//! Reference: <https://www.w3.org/TR/css-scroll-snap-1/>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// scroll-snap-type
// ---------------------------------------------------------------------------

/// CSS Scroll Snap 1 § 5.1 `scroll-snap-type` axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SnapAxis {
    /// `x` — snap along the horizontal axis only.
    X,
    /// `y` — snap along the vertical axis only.
    #[default]
    Y,
    /// `block` — snap along the block axis (maps via the writing-mode flow).
    Block,
    /// `inline` — snap along the inline axis.
    Inline,
    /// `both` — snap along both axes independently.
    Both,
}

/// CSS Scroll Snap 1 § 5.1 `scroll-snap-type` strictness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SnapStrictness {
    /// `proximity` (the default) — the viewport *may* come to rest on a
    /// snap position if sufficiently close.
    #[default]
    Proximity,
    /// `mandatory` — the viewport *must* come to rest on a snap position.
    Mandatory,
}

/// CSS Scroll Snap 1 § 5.1 `scroll-snap-type` — `none`, or a typed
/// `(axis, strictness)` pair. `none` disables snapping entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScrollSnapType {
    /// `none` (the default) — no snapping.
    #[default]
    None,
    /// A typed snap container.
    Typed {
        axis: SnapAxis,
        strictness: SnapStrictness,
    },
}

impl ScrollSnapType {
    /// Parse `scroll-snap-type` (§ 5.1). Accepts `none`, a single axis, a
    /// single strictness, or the `axis strictness` pair (either order; the
    /// spec grammar is `none | [ x | y | block | inline | both ] [ mandatory | proximity ]?`).
    /// Unknown tokens fail closed to `None`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut axis: Option<SnapAxis> = None;
        let mut strictness: Option<SnapStrictness> = None;
        for tok in s.split_ascii_whitespace() {
            match tok.to_ascii_lowercase().as_str() {
                "none" => return Some(Self::None),
                "x" => axis = Some(SnapAxis::X),
                "y" => axis = Some(SnapAxis::Y),
                "block" => axis = Some(SnapAxis::Block),
                "inline" => axis = Some(SnapAxis::Inline),
                "both" => axis = Some(SnapAxis::Both),
                "mandatory" => strictness = Some(SnapStrictness::Mandatory),
                "proximity" => strictness = Some(SnapStrictness::Proximity),
                _ => return None,
            }
        }
        match (axis, strictness) {
            (None, None) => None,
            (a, s) => Some(Self::Typed {
                axis: a.unwrap_or(SnapAxis::Y),
                strictness: s.unwrap_or(SnapStrictness::Proximity),
            }),
        }
    }

    /// `true` iff snapping is enabled (`none` ⇒ `false`).
    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::None)
    }
}

// ---------------------------------------------------------------------------
// scroll-snap-align + scroll-snap-stop
// ---------------------------------------------------------------------------

/// CSS Scroll Snap 1 § 5.2 `scroll-snap-align` — the per-axis alignment of
/// a snap target within its scroll container's snapport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SnapAlign {
    /// `none` (the default) — this axis does not snap.
    #[default]
    None,
    /// `start` — the area's start edge aligns with the snapport's start.
    Start,
    /// `end` — the area's end edge aligns with the snapport's end.
    End,
    /// `center` — the area's centre aligns with the snapport's centre.
    Center,
}

impl SnapAlign {
    /// Parse a single `scroll-snap-align` value (`none` / `start` / `end` /
    /// `center`), ASCII-case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "start" => Some(Self::Start),
            "end" => Some(Self::End),
            "center" | "centre" => Some(Self::Center),
            _ => None,
        }
    }

    /// Parse the 1–2 value `scroll-snap-align` form (§ 5.2: one value
    /// applies to both axes; two values are `(block, inline)`). Returns
    /// `(block_align, inline_align)`.
    pub fn parse_pair(s: &str) -> Option<(Self, Self)> {
        let mut it = s.split_ascii_whitespace();
        let first = Self::parse(it.next()?)?;
        match it.next() {
            None => Some((first, first)),
            Some(second) => {
                let second = Self::parse(second)?;
                if it.next().is_some() {
                    None
                } else {
                    Some((first, second))
                }
            }
        }
    }
}

/// CSS Scroll Snap 1 § 5.3 `scroll-snap-stop` — whether the scroll
/// container may skip past this snap target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SnapStop {
    /// `normal` (the default) — the scroll may pass over this snap target.
    #[default]
    Normal,
    /// `always` — the scroll must come to rest on this snap target before
    /// continuing (the "you can't skip me" flag).
    Always,
}

impl SnapStop {
    /// Parse `scroll-snap-stop` (`normal` / `always`), ASCII-case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(Self::Normal),
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// SnapArea + the § 5 snap-position computation
// ---------------------------------------------------------------------------

/// A snap target's rect in scrollable-content coordinates (the § 5 "snap
/// area" = the element's margin box + `scroll-margin`, already resolved to
/// px by the caller).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SnapArea {
    /// The area's x offset within the scrollable content.
    pub x: f32,
    /// The area's y offset within the scrollable content.
    pub y: f32,
    /// The area's width.
    pub width: f32,
    /// The area's height.
    pub height: f32,
}

/// The § 5 snap position for one axis: given the scrollport size `S`, the
/// area's content offset `O` + size `A`, the total scrollable overflow, and
/// the alignment, returns the scroll offset that brings the area to the
/// alignment, clamped to `[0, max(0, overflow - S)]`. Returns `None` when
/// the alignment is [`SnapAlign::None`] (this axis doesn't snap).
pub fn compute_axis(
    scrollport_size: f32,
    scrollable_overflow: f32,
    area_offset: f32,
    area_size: f32,
    align: SnapAlign,
) -> Option<f32> {
    let raw = match align {
        SnapAlign::None => return None,
        SnapAlign::Start => area_offset,
        SnapAlign::End => area_offset + area_size - scrollport_size,
        SnapAlign::Center => area_offset + area_size / 2.0 - scrollport_size / 2.0,
    };
    let max_scroll = (scrollable_overflow - scrollport_size).max(0.0);
    Some(clamp_f32(raw, 0.0, max_scroll))
}

/// The `(x, y)` snap offsets for a snap target given the scrollport's
/// `(width, height)` + the total scrollable overflow `(overflow_x,
/// overflow_y)`. Each axis is `None` when the corresponding align is
/// [`SnapAlign::None`].
pub fn compute_snap(
    scrollport: (f32, f32),
    overflow: (f32, f32),
    area: SnapArea,
    block_align: SnapAlign,
    inline_align: SnapAlign,
    writing_mode_is_vertical: bool,
) -> (Option<f32>, Option<f32>) {
    // In a horizontal flow, block = y, inline = x. In a vertical flow, the
    // block/inline axes swap (the caller passes the resolved flow flag).
    let (x_align, y_align) = if writing_mode_is_vertical {
        (block_align, inline_align)
    } else {
        (inline_align, block_align)
    };
    let x = compute_axis(scrollport.0, overflow.0, area.x, area.width, x_align);
    let y = compute_axis(scrollport.1, overflow.1, area.y, area.height, y_align);
    (x, y)
}

/// The strictness policy: `mandatory` always snaps; `proximity` snaps iff
/// the `|target - current|` distance is within `threshold`. The § 5.1
/// "sufficient proximity" reading defaults the threshold to half the
/// scrollport size; the caller may pass a tighter policy.
pub fn should_snap(strictness: SnapStrictness, current: f32, target: f32, threshold: f32) -> bool {
    match strictness {
        SnapStrictness::Mandatory => true,
        SnapStrictness::Proximity => (target - current).abs() <= threshold,
    }
}

/// Clamp `v` to `[lo, hi]` (assumes `lo ≤ hi`).
fn clamp_f32(v: f32, lo: f32, hi: f32) -> f32 {
    if v.is_nan() || v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse -------------------------------------------------------

    #[test]
    fn parse_scroll_snap_type_axis_strictness_either_order() {
        assert_eq!(ScrollSnapType::parse("none"), Some(ScrollSnapType::None));
        assert_eq!(
            ScrollSnapType::parse("y mandatory"),
            Some(ScrollSnapType::Typed {
                axis: SnapAxis::Y,
                strictness: SnapStrictness::Mandatory
            })
        );
        assert_eq!(
            ScrollSnapType::parse("mandatory both"),
            Some(ScrollSnapType::Typed {
                axis: SnapAxis::Both,
                strictness: SnapStrictness::Mandatory
            })
        );
        assert_eq!(
            ScrollSnapType::parse("x"),
            Some(ScrollSnapType::Typed {
                axis: SnapAxis::X,
                strictness: SnapStrictness::Proximity
            })
        );
        assert_eq!(ScrollSnapType::parse("bogus"), None);
    }

    #[test]
    fn scroll_snap_type_is_enabled() {
        assert!(!ScrollSnapType::None.is_enabled());
        assert!(ScrollSnapType::parse("y proximity").unwrap().is_enabled());
    }

    #[test]
    fn parse_snap_align_single_and_pair() {
        assert_eq!(SnapAlign::parse("center"), Some(SnapAlign::Center));
        assert_eq!(SnapAlign::parse("Centre"), Some(SnapAlign::Center));
        assert_eq!(SnapAlign::parse("start"), Some(SnapAlign::Start));
        assert_eq!(
            SnapAlign::parse_pair("end"),
            Some((SnapAlign::End, SnapAlign::End))
        );
        assert_eq!(
            SnapAlign::parse_pair("start end"),
            Some((SnapAlign::Start, SnapAlign::End))
        );
        assert_eq!(
            SnapAlign::parse_pair("start end center"),
            None,
            "3 values invalid"
        );
    }

    #[test]
    fn parse_snap_stop() {
        assert_eq!(SnapStop::parse("always"), Some(SnapStop::Always));
        assert_eq!(SnapStop::parse("NORMAL"), Some(SnapStop::Normal));
        assert_eq!(SnapStop::parse("sometimes"), None);
    }

    // --- compute_axis ------------------------------------------------

    #[test]
    fn compute_axis_start_aligns_area_start_with_viewport_start() {
        // scrollport 600, area at offset 1000, size 200, overflow 2000.
        // start ⇒ scroll = 1000.
        let s = compute_axis(600.0, 2000.0, 1000.0, 200.0, SnapAlign::Start);
        assert_eq!(s, Some(1000.0));
    }

    #[test]
    fn compute_axis_end_aligns_area_end_with_viewport_end() {
        // end ⇒ scroll = 1000 + 200 - 600 = 600.
        let s = compute_axis(600.0, 2000.0, 1000.0, 200.0, SnapAlign::End);
        assert_eq!(s, Some(600.0));
    }

    #[test]
    fn compute_axis_center_aligns_centres() {
        // center ⇒ scroll = 1000 + 100 - 300 = 800.
        let s = compute_axis(600.0, 2000.0, 1000.0, 200.0, SnapAlign::Center);
        assert_eq!(s, Some(800.0));
    }

    #[test]
    fn compute_axis_none_is_none() {
        assert_eq!(
            compute_axis(600.0, 2000.0, 1000.0, 200.0, SnapAlign::None),
            None
        );
    }

    #[test]
    fn compute_axis_clamps_to_zero() {
        // start at offset -100 (before content) ⇒ clamped to 0.
        let s = compute_axis(600.0, 2000.0, -100.0, 200.0, SnapAlign::Start);
        assert_eq!(s, Some(0.0));
    }

    #[test]
    fn compute_axis_clamps_to_max_scroll() {
        // overflow 2000, scrollport 600 ⇒ max_scroll = 1400.
        // center at area offset 5000, size 200 ⇒ raw = 5000 + 100 - 300 = 4800
        // ⇒ clamped to 1400.
        let s = compute_axis(600.0, 2000.0, 5000.0, 200.0, SnapAlign::Center);
        assert_eq!(s, Some(1400.0));
    }

    #[test]
    fn compute_axis_max_scroll_is_zero_when_area_smaller_than_viewport() {
        // overflow 500, scrollport 600 ⇒ max_scroll = max(0, -100) = 0.
        // Any alignment clamps to 0.
        let s = compute_axis(600.0, 500.0, 100.0, 200.0, SnapAlign::Start);
        assert_eq!(s, Some(0.0));
    }

    // --- compute_snap ------------------------------------------------

    #[test]
    fn compute_snap_horizontal_flow() {
        // block = y, inline = x.
        let area = SnapArea {
            x: 100.0,
            y: 200.0,
            width: 50.0,
            height: 80.0,
        };
        let (x, y) = compute_snap(
            (600.0, 400.0),
            (2000.0, 2000.0),
            area,
            SnapAlign::Start, // block → y
            SnapAlign::End,   // inline → x
            false,
        );
        // x: end ⇒ 100 + 50 - 600 = -450 → clamped to 0.
        // y: start ⇒ 200.
        assert_eq!(x, Some(0.0));
        assert_eq!(y, Some(200.0));
    }

    #[test]
    fn compute_snap_vertical_flow_swaps_block_inline() {
        let area = SnapArea {
            x: 100.0,
            y: 200.0,
            width: 50.0,
            height: 80.0,
        };
        let (x, y) = compute_snap(
            (600.0, 400.0),
            (2000.0, 2000.0),
            area,
            SnapAlign::Start, // block → x (vertical flow)
            SnapAlign::End,   // inline → y (vertical flow)
            true,
        );
        // x: start ⇒ 100.
        // y: end ⇒ 200 + 80 - 400 = -120 → clamped to 0.
        assert_eq!(x, Some(100.0));
        assert_eq!(y, Some(0.0));
    }

    #[test]
    fn compute_snap_none_axis_returns_none() {
        let area = SnapArea {
            x: 100.0,
            y: 200.0,
            width: 50.0,
            height: 80.0,
        };
        let (x, y) = compute_snap(
            (600.0, 400.0),
            (2000.0, 2000.0),
            area,
            SnapAlign::None,
            SnapAlign::Start,
            false,
        );
        assert_eq!(x, Some(100.0)); // inline=start → x
        assert_eq!(y, None); // block=none → y
    }

    // --- should_snap -------------------------------------------------

    #[test]
    fn mandatory_always_snaps() {
        assert!(should_snap(SnapStrictness::Mandatory, 100.0, 500.0, 10.0));
    }

    #[test]
    fn proximity_snaps_within_threshold() {
        assert!(should_snap(SnapStrictness::Proximity, 100.0, 130.0, 50.0));
        assert!(!should_snap(SnapStrictness::Proximity, 100.0, 200.0, 50.0));
    }

    #[test]
    fn proximity_threshold_is_absolute_distance() {
        // Either direction counts.
        assert!(should_snap(SnapStrictness::Proximity, 100.0, 70.0, 50.0));
        assert!(!should_snap(SnapStrictness::Proximity, 100.0, 40.0, 50.0));
    }
}
