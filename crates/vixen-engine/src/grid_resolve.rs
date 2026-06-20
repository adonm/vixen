//! CSS Grid track sizing — Phase 4 layout prep (pure logic called out by
//! `docs/PLAN.md` "Testing strategy"). Implements CSS Grid 1 § 12.5
//! "Distribute Extra Space" + § 11.7 "Maximize Tracks" so the layout engine
//! has one source of truth for the flex-factor distribution grid columns /
//! rows reduce to. Complements [`crate::flex_resolve`] (Flexbox main-axis
//! distribution) — both are pure given cascade-resolved definite base sizes.
//!
//! What lives here:
//! - [`GridTrack`] — one column or row track: the resolved base size
//!   (the § 11.2 "min track size" — the caller resolved content
//!   contributions to a definite number), the growth limit (the § 11.3
//!   "max track size"; `f32::INFINITY` for unbounded), and the flex factor
//!   (`Nfr`; `0.0` for non-flex tracks).
//! - [`GridResolution`] — the resolved used sizes + per-track frozen reason.
//! - [`resolve_tracks`] — the § 12.5 algorithm: distribute the container's
//!   leftover space to flex tracks proportionally to their flex factor,
//!   freeze tracks that hit their growth limit, redistribute the excess to
//!   the remaining flex tracks, then grow non-flex tracks up to their growth
//!   limit if there is leftover (§ 11.7).
//!
//! What does *not* live here:
//! - Content-based sizing (`min-content` / `max-content` / `auto` base
//!   sizes). Resolving those requires real text-shaping + item-spanning
//!   contribution distribution (§ 11.2–§ 11.4); the caller resolves the
//!   content contribution to a definite `base` before calling.
//! - Items spanning multiple tracks (§ 11.3 "Increase sizes to fit spanning
//!   items"). The caller folds each spanning item's contribution into the
//!   `base` of the tracks it spans before calling.
//! - Grid item placement + auto-placement (§ 8). The wrapper in layout
//!   partitions items into the track matrix first.
//! - The grid's `gap` (caller subtracts it from `container_size` before
//!   calling).
//! - Baseline alignment + the orthogonal-flow cases (§ 11.1).
//!
//! ## Algorithm (CSS Grid 1 § 12.5)
//!
//! Given a definite `container_size`:
//! 1. Leftover = `container_size − Σ base`.
//! 2. If leftover > 0 and there are flex tracks, distribute leftover
//!    proportionally to `flex`. Freeze any track whose share would exceed its
//!    growth limit; redistribute the excess to the other flex tracks.
//!    Iterate until every flex track is frozen.
//! 3. § 11.7: if leftover remains (no flex tracks, or all hit growth limits),
//!    grow non-flex tracks up to their growth limits, equally.
//!
//! Reference: <https://www.w3.org/TR/css-grid-1/#algo-spanning-items>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// One grid track (a column or a row) — the inputs to the track sizer. The
/// `base` is the § 11.2 min track size the caller already resolved (a
/// definite length, or the content contribution); `growth_limit` is the § 11.3
/// max track size; `flex` is the `Nfr` flex factor.
///
/// Defaults match CSS Grid 1 § 7.2: `flex: 0` (a non-flex track) and
/// `growth_limit: ∞` (unbounded above). The common `1fr` track is
/// [`GridTrack::fr`] with base `0.0`; a `minmax(100px, 1fr)` track is base
/// `100`, growth limit `∞`, flex `1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridTrack {
    /// The min track size (§ 11.2). Always definite; the caller resolved any
    /// `auto`/`min-content`/`max-content` to a number first.
    pub base: f32,
    /// The max track size (§ 11.3). `f32::INFINITY` for an unbounded track
    /// (`auto`/`max-content`/`1fr` with no explicit max).
    pub growth_limit: f32,
    /// The flex factor (`Nfr`). `0.0` for non-flex tracks.
    pub flex: f32,
}

impl GridTrack {
    /// A definite-length track (`100px`): base = growth_limit = `len`, flex 0.
    pub const fn length(len: f32) -> Self {
        Self {
            base: len,
            growth_limit: len,
            flex: 0.0,
        }
    }

    /// An `Nfr` track with base `0.0`, growth limit `∞`, flex `n`.
    pub const fn fr(n: f32) -> Self {
        Self {
            base: 0.0,
            growth_limit: f32::INFINITY,
            flex: n,
        }
    }

    /// A `minmax(min, max)` track with an optional flex factor.
    pub const fn minmax(min: f32, max: f32, flex: f32) -> Self {
        Self {
            base: min,
            growth_limit: max,
            flex,
        }
    }

    /// `true` if this track has a non-zero flex factor.
    pub fn is_flex(self) -> bool {
        self.flex > 0.0
    }
}

/// Why a track was frozen at its size. Used for inspection (the future
/// `--explain-grid` CDP surface mirrors [`crate::flex_resolve::FrozenReason`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridFrozenReason {
    /// The track has no flex factor — it stays at its base (or grows only via
    /// the § 11.7 maximize step).
    NonFlex,
    /// The track is flex but there was no free space to distribute (leftover
    /// ≤ 0); it stays at its base.
    NoFreeSpace,
    /// The flex share would have exceeded the growth limit; the track is
    /// clamped to its growth limit.
    GrowthLimitClamped,
    /// The track received its proportional share with no clamp.
    Distributed,
}

/// The result of [`resolve_tracks`]: the used sizes (same order as the input
/// tracks) plus per-track freeze reasons.
#[derive(Debug, Clone, PartialEq)]
pub struct GridResolution {
    /// One resolved track size per input track, in input order.
    pub sizes: Vec<f32>,
    /// Why each track was frozen (parallel to `sizes`).
    pub frozen_at: Vec<GridFrozenReason>,
}

// ---------------------------------------------------------------------------
// § 12.5 — Distribute Extra Space
// ---------------------------------------------------------------------------

/// Resolve the used track sizes for one axis (columns or rows) per CSS Grid 1
/// § 12.5. `container_size` is the grid container's inner size in this axis
/// (already gap-subtracted); `tracks` is the per-track input row.
///
/// Returns the per-track used sizes. Tracks sum to at most `container_size`
/// (exactly when leftover ≥ 0 and there's room to grow); when the bases
/// already exceed the container, tracks stay at their bases (grid does not
/// shrink tracks below their min — the § 12.5 algorithm only *grows*).
///
/// Returns the tracks at their bases if `tracks` is empty or
/// `container_size` is non-positive (defensive — degenerate containers are
/// treated as zero-size).
pub fn resolve_tracks(container_size: f32, tracks: &[GridTrack]) -> GridResolution {
    let n = tracks.len();
    if n == 0 {
        return GridResolution {
            sizes: Vec::new(),
            frozen_at: Vec::new(),
        };
    }

    // Initialize every track at its base.
    let mut sizes: Vec<f32> = tracks.iter().map(|t| t.base).collect();
    let mut frozen: Vec<bool> = vec![false; n];
    let mut frozen_at: Vec<GridFrozenReason> = tracks
        .iter()
        .map(|t| {
            if t.is_flex() {
                GridFrozenReason::Distributed
            } else {
                GridFrozenReason::NonFlex
            }
        })
        .collect();

    // Non-flex tracks start frozen at their base (they only grow via the
    // § 11.7 maximize step, after the flex distribution).
    for i in 0..n {
        if !tracks[i].is_flex() {
            frozen[i] = true;
            frozen_at[i] = GridFrozenReason::NonFlex;
        }
    }

    if container_size <= 0.0 {
        // Degenerate container; every track stays at its base.
        return GridResolution { sizes, frozen_at };
    }

    // ---- § 12.5: distribute leftover to flex tracks. ------------------
    let leftover = compute_leftover(container_size, tracks, &sizes);
    if leftover > 0.0 && tracks.iter().any(|t| t.is_flex()) {
        distribute_to_flex_tracks(
            container_size,
            tracks,
            &mut sizes,
            &mut frozen,
            &mut frozen_at,
        );
    } else if leftover <= 0.0 {
        // No free space; flex tracks stay at base (the § 12.5 algorithm
        // grows only when there is leftover).
        for i in 0..n {
            if tracks[i].is_flex() && !frozen[i] {
                frozen[i] = true;
                frozen_at[i] = GridFrozenReason::NoFreeSpace;
            }
        }
    }

    // ---- § 11.7: maximize non-flex tracks if leftover remains. --------
    let leftover_after_flex = compute_leftover(container_size, tracks, &sizes);
    if leftover_after_flex > 0.0 {
        maximize_tracks(leftover_after_flex, tracks, &mut sizes, &mut frozen_at);
    }

    GridResolution { sizes, frozen_at }
}

/// The free space left after subtracting the current track sizes from the
/// container (§ 12.4 "Count the number of ... free space").
fn compute_leftover(container_size: f32, tracks: &[GridTrack], sizes: &[f32]) -> f32 {
    let _ = tracks;
    let used: f32 = sizes.iter().sum();
    container_size - used
}

/// § 12.5 step (B): distribute the container's leftover to the unfrozen flex
/// tracks proportionally to their flex factor. Freeze any track whose share
/// would exceed its growth limit; the next round recomputes the leftover
/// (`container_size − Σ size`) and redistributes to the still-unfrozen flex
/// tracks. Iterate until every flex track is frozen (bounded by `n + 1`
/// rounds — at least one freeze per round in the worst case).
fn distribute_to_flex_tracks(
    container_size: f32,
    tracks: &[GridTrack],
    sizes: &mut [f32],
    frozen: &mut [bool],
    frozen_at: &mut [GridFrozenReason],
) {
    let n = tracks.len();
    let max_rounds = n + 1;
    let mut rounds = 0;
    loop {
        rounds += 1;
        if rounds > max_rounds {
            break; // Defensive: shouldn't happen.
        }

        let unfrozen: Vec<usize> = (0..n)
            .filter(|&i| tracks[i].is_flex() && !frozen[i])
            .collect();
        if unfrozen.is_empty() {
            break;
        }

        let sum_flex: f32 = unfrozen.iter().map(|&i| tracks[i].flex).sum();
        if sum_flex <= 0.0 {
            break;
        }

        // Recompute the leftover from the current sizes so clamps in a prior
        // round reduce the distributable pool correctly.
        let used: f32 = sizes.iter().sum();
        let remaining = container_size - used;
        if remaining <= 0.0 {
            break;
        }

        // First pass: detect any track that would exceed its growth limit and
        // clamp it. If any clamped, loop to redistribute the freed pool.
        let mut clamped_any = false;
        for &i in &unfrozen {
            let share = remaining * (tracks[i].flex / sum_flex);
            let proposed = sizes[i] + share;
            if proposed >= tracks[i].growth_limit && tracks[i].growth_limit.is_finite() {
                sizes[i] = tracks[i].growth_limit;
                frozen[i] = true;
                frozen_at[i] = GridFrozenReason::GrowthLimitClamped;
                clamped_any = true;
            }
        }
        if clamped_any {
            continue;
        }

        // No clamps this round: every remaining flex track takes its full
        // proportional share and is frozen.
        for &i in &unfrozen {
            let share = remaining * (tracks[i].flex / sum_flex);
            sizes[i] += share;
            frozen[i] = true;
            frozen_at[i] = GridFrozenReason::Distributed;
        }
        break;
    }
}

/// § 11.7 "Maximize Tracks": if leftover remains after the flex distribution
/// (no flex tracks, or every flex track hit its growth limit), grow the
/// non-flex tracks up to their growth limits, distributing the leftover
/// equally so each track reaches its growth limit at the same rate.
fn maximize_tracks(
    leftover: f32,
    tracks: &[GridTrack],
    sizes: &mut [f32],
    frozen_at: &mut [GridFrozenReason],
) {
    let n = tracks.len();
    // Iteratively grow tracks: each round, divide leftover by the number of
    // tracks that still have headroom; grow each by that amount, clamping to
    // the growth limit. Freeze tracks that reach their growth limit.
    let mut remaining = leftover;
    let max_rounds = n + 1;
    let mut rounds = 0;
    loop {
        rounds += 1;
        if rounds > max_rounds || remaining <= 0.0 {
            break;
        }
        // Tracks with remaining headroom (size < growth_limit).
        let growable: Vec<usize> = (0..n)
            .filter(|&i| sizes[i] < tracks[i].growth_limit)
            .collect();
        if growable.is_empty() {
            break;
        }
        let share = remaining / (growable.len() as f32);
        let mut clamped_any = false;
        let mut consumed_this_round = 0.0_f32;
        for &i in &growable {
            let headroom = tracks[i].growth_limit - sizes[i];
            if share >= headroom {
                sizes[i] = tracks[i].growth_limit;
                consumed_this_round += headroom;
                clamped_any = true;
                // Mark as growth-limit-clamped only if it wasn't already
                // Distributed by the flex step; preserve the flex reason.
                if frozen_at[i] == GridFrozenReason::NonFlex {
                    frozen_at[i] = GridFrozenReason::GrowthLimitClamped;
                }
            } else {
                sizes[i] += share;
                consumed_this_round += share;
            }
        }
        remaining -= consumed_this_round;
        if !clamped_any {
            // Every growable track took its full share; no more rounds needed.
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Degenerate ----------------------------------------------------

    #[test]
    fn empty_tracks_returns_empty() {
        let r = resolve_tracks(100.0, &[]);
        assert!(r.sizes.is_empty());
        assert!(r.frozen_at.is_empty());
    }

    #[test]
    fn zero_container_keeps_bases() {
        let tracks = [GridTrack::fr(1.0), GridTrack::fr(2.0)];
        let r = resolve_tracks(0.0, &tracks);
        // No free space; flex tracks stay at base 0.
        assert_eq!(r.sizes, vec![0.0, 0.0]);
    }

    #[test]
    fn negative_container_keeps_bases() {
        let tracks = [GridTrack::length(50.0), GridTrack::length(60.0)];
        let r = resolve_tracks(-10.0, &tracks);
        // Bases preserved (grid does not shrink below min).
        assert_eq!(r.sizes, vec![50.0, 60.0]);
    }

    // --- No flex tracks ------------------------------------------------

    #[test]
    fn fixed_length_tracks_no_flex_no_leftover() {
        // Three fixed tracks summing exactly to the container.
        let tracks = [
            GridTrack::length(100.0),
            GridTrack::length(200.0),
            GridTrack::length(100.0),
        ];
        let r = resolve_tracks(400.0, &tracks);
        assert_eq!(r.sizes, vec![100.0, 200.0, 100.0]);
        assert!(r.frozen_at.iter().all(|&x| x == GridFrozenReason::NonFlex));
    }

    #[test]
    fn fixed_tracks_with_leftover_maximized_to_growth_limit() {
        // Fixed tracks (length ⇒ growth_limit == base) cannot grow; leftover
        // is discarded (no headroom).
        let tracks = [GridTrack::length(100.0), GridTrack::length(100.0)];
        let r = resolve_tracks(400.0, &tracks);
        assert_eq!(r.sizes, vec![100.0, 100.0]); // leftover 200 discarded
    }

    #[test]
    fn non_flex_tracks_with_headroom_grow_via_maximize() {
        // minmax(100, 200) non-flex tracks: base 100, growth_limit 200, flex 0.
        let tracks = [
            GridTrack::minmax(100.0, 200.0, 0.0),
            GridTrack::minmax(100.0, 200.0, 0.0),
        ];
        let r = resolve_tracks(400.0, &tracks);
        // Leftover 200 split equally → each grows by 100 to its growth limit.
        assert_eq!(r.sizes, vec![200.0, 200.0]);
    }

    #[test]
    fn non_flex_tracks_partial_headroom() {
        // minmax(100, 150) non-flex tracks: only 50 headroom each.
        let tracks = [
            GridTrack::minmax(100.0, 150.0, 0.0),
            GridTrack::minmax(100.0, 150.0, 0.0),
        ];
        let r = resolve_tracks(280.0, &tracks);
        // Leftover 80; each grows by 40 → 140, 140 (under growth limit 150).
        assert_eq!(r.sizes, vec![140.0, 140.0]);
    }

    // --- Flex distribution ---------------------------------------------

    #[test]
    fn equal_fr_tracks_split_leftover_evenly() {
        let tracks = [GridTrack::fr(1.0), GridTrack::fr(1.0), GridTrack::fr(1.0)];
        let r = resolve_tracks(300.0, &tracks);
        assert_eq!(r.sizes, vec![100.0, 100.0, 100.0]);
        assert!(
            r.frozen_at
                .iter()
                .all(|&x| x == GridFrozenReason::Distributed)
        );
    }

    #[test]
    fn weighted_fr_tracks_split_proportionally() {
        // 1fr + 2fr splits leftover 1:2.
        let tracks = [GridTrack::fr(1.0), GridTrack::fr(2.0)];
        let r = resolve_tracks(300.0, &tracks);
        assert_eq!(r.sizes, vec![100.0, 200.0]);
    }

    #[test]
    fn fr_tracks_with_nonzero_base_grow_from_base() {
        // minmax(50, ∞, 1fr): base 50, then flex distribution adds leftover.
        let tracks = [
            GridTrack::minmax(50.0, f32::INFINITY, 1.0),
            GridTrack::minmax(50.0, f32::INFINITY, 1.0),
        ];
        let r = resolve_tracks(300.0, &tracks);
        // Bases 50+50=100; leftover 200 split 1:1 → +100 each → 150, 150.
        assert_eq!(r.sizes, vec![150.0, 150.0]);
    }

    #[test]
    fn mixed_fixed_and_fr_tracks() {
        // 100px + 1fr + 2fr in a 400 container.
        let tracks = [
            GridTrack::length(100.0),
            GridTrack::fr(1.0),
            GridTrack::fr(2.0),
        ];
        let r = resolve_tracks(400.0, &tracks);
        // Leftover 300 split 1:2 → 100, 200.
        assert_eq!(r.sizes, vec![100.0, 100.0, 200.0]);
    }

    // --- Growth-limit clamping -----------------------------------------

    #[test]
    fn fr_track_clamped_to_growth_limit_redistributes() {
        // minmax(0, 100, 1fr) + minmax(0, ∞, 1fr): first clamps at 100, the
        // rest goes to the second.
        let tracks = [
            GridTrack::minmax(0.0, 100.0, 1.0),
            GridTrack::minmax(0.0, f32::INFINITY, 1.0),
        ];
        let r = resolve_tracks(300.0, &tracks);
        assert_eq!(r.sizes, vec![100.0, 200.0]);
        // First clamped; second distributed.
        assert_eq!(r.frozen_at[0], GridFrozenReason::GrowthLimitClamped);
        assert_eq!(r.frozen_at[1], GridFrozenReason::Distributed);
    }

    #[test]
    fn all_fr_tracks_clamped_leaves_leftover() {
        // Two minmax(0, 100, 1fr) in a 400 container: both clamp at 100,
        // leftover 200 is discarded (no headroom anywhere).
        let tracks = [
            GridTrack::minmax(0.0, 100.0, 1.0),
            GridTrack::minmax(0.0, 100.0, 1.0),
        ];
        let r = resolve_tracks(400.0, &tracks);
        assert_eq!(r.sizes, vec![100.0, 100.0]);
    }

    #[test]
    fn fr_clamp_then_maximize_non_flex() {
        // minmax(0, 100, 1fr) [flex, clamps at 100] + minmax(50, 200, 0)
        // [non-flex, base 50, headroom to 200]. In a 400 container:
        //   bases: 0 + 50 = 50; leftover 350.
        //   flex distribution: first track wants 350 (1fr / 1fr) but clamps
        //   at 100; 250 leftover.
        //   maximize: non-flex second track grows 50 → 200 (headroom 150),
        //   consuming 150; 100 leftover discarded.
        let tracks = [
            GridTrack::minmax(0.0, 100.0, 1.0),
            GridTrack::minmax(50.0, 200.0, 0.0),
        ];
        let r = resolve_tracks(400.0, &tracks);
        assert_eq!(r.sizes[0], 100.0); // clamped
        assert_eq!(r.sizes[1], 200.0); // maximized
    }

    // --- Invariants ----------------------------------------------------

    #[test]
    fn resolved_sizes_never_exceed_growth_limit() {
        let tracks = [
            GridTrack::minmax(10.0, 80.0, 1.0),
            GridTrack::minmax(10.0, 80.0, 2.0),
            GridTrack::minmax(10.0, 80.0, 1.0),
        ];
        let r = resolve_tracks(1000.0, &tracks);
        for (i, &size) in r.sizes.iter().enumerate() {
            assert!(
                size <= tracks[i].growth_limit + 1e-3,
                "track {i} size {size} exceeds growth limit {}",
                tracks[i].growth_limit
            );
        }
    }

    #[test]
    fn resolved_sizes_never_below_base() {
        let tracks = [
            GridTrack::minmax(50.0, 200.0, 1.0),
            GridTrack::length(100.0),
            GridTrack::fr(3.0),
        ];
        let r = resolve_tracks(50.0, &tracks); // tiny container
        for (i, &size) in r.sizes.iter().enumerate() {
            assert!(
                size >= tracks[i].base - 1e-3,
                "track {i} size {size} below base {}",
                tracks[i].base
            );
        }
    }

    #[test]
    fn no_free_space_marks_flex_tracks_no_free_space() {
        let tracks = [GridTrack::length(200.0), GridTrack::fr(1.0)];
        let r = resolve_tracks(200.0, &tracks);
        // Container exactly filled by the fixed track; flex track gets 0.
        assert_eq!(r.sizes, vec![200.0, 0.0]);
        assert_eq!(r.frozen_at[1], GridFrozenReason::NoFreeSpace);
    }

    // --- Constructors --------------------------------------------------

    #[test]
    fn length_constructor_is_non_flex() {
        let t = GridTrack::length(100.0);
        assert_eq!(t.base, 100.0);
        assert_eq!(t.growth_limit, 100.0);
        assert_eq!(t.flex, 0.0);
        assert!(!t.is_flex());
    }

    #[test]
    fn fr_constructor_is_flex_unbounded() {
        let t = GridTrack::fr(2.0);
        assert_eq!(t.base, 0.0);
        assert!(t.growth_limit.is_infinite());
        assert_eq!(t.flex, 2.0);
        assert!(t.is_flex());
    }

    #[test]
    fn minmax_constructor() {
        let t = GridTrack::minmax(50.0, 200.0, 1.0);
        assert_eq!(t.base, 50.0);
        assert_eq!(t.growth_limit, 200.0);
        assert_eq!(t.flex, 1.0);
        assert!(t.is_flex());
    }
}
