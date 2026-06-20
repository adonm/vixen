//! CSS Multi-column Layout 1 § 3 — the `column-width` / `column-count`
//! / `column-gap` resolution the layout layer's column-box distribution
//! reduces to (Phase 4 prep). Pure given cascade-resolved px values; the
//! `column-gap: normal` → `1em` resolution + the column-box height balancing
//! (§ 8 the `column-fill: balance` step) stay in `layout_2020` where they
//! compose against real text metrics.
//!
//! What lives here:
//! - [`ColumnWidth`] — `column-width` (`auto` or a px length).
//! - [`ColumnCount`] — `column-count` (`auto` or a ≥ 1 integer).
//! - [`ColumnSpec`] — the `(column-width, column-count, gap)` triple +
//!   [`ColumnSpec::resolve`] running the § 3.4 pseudo-algorithm to produce
//!   the used [`ResolvedColumns`] (count + column-width + gap).
//! - [`ResolvedColumns::column_x`] — the x-offset of column `i` (the
//!   `i * (column_width + gap)` stride the box model feeds off).
//! - [`ResolvedColumns::total_width`] — the column-row content width.
//!
//! What does *not* live here:
//! - The `column-gap: normal` → `1em` length resolution — the cascade
//!   resolves `1em` to px before constructing the [`ColumnSpec`]; the caller
//!   passes the resolved px gap (or `0.0` if the caller treats `normal` as
//!   no-gap for a given context).
//! - The column-box height balancing (§ 8 `column-fill: balance`) — needs
//!   real text shaping; stays in `layout_2020`.
//! - The `column-rule` paint (the `column-rule-width` / `style` / `colour`
//!   between-column border) — the paint path; the gap this module carries
//!   is the rule's slot.
//! - Spanning elements (`column-span: all`) — the layout layer's
//!   span-element out-of-flow carve; this module is the column-row geometry.
//!
//! ## The § 3.4 pseudo-algorithm
//!
//! Given `available-width` (the containing block's content width) + the
//! authored `column-width` (`W`, or auto) + `column-count` (`N`, or auto) +
//! `column-gap` (`g`):
//!
//! ```text
//! (01)  if W = auto and N = auto:
//! (02)      count := 1;              width := available
//! (03)  else if W = auto and N ≠ auto:
//! (04)      count := N;              width := max(0, (available + g)/count - g)
//! (05)  else if W ≠ auto and N = auto:
//! (06)      count := max(1, ⌊(available + g)/(W + g)⌋)
//! (07)      width := (available + g)/count - g
//! (08)  else: -- both non-auto
//! (09)      count := min(N, max(1, ⌊(available + g)/(W + g)⌋))
//! (10)      width := (available + g)/count - g
//! (11)      if count = 1 and W > available:
//! (12)          width := available
//! ```
//!
//! A final `width := max(0, width)` clamp guards a too-large `count` from
//! producing a negative column width; `count` is always `≥ 1`.
//!
//! Reference: <https://www.w3.org/TR/css-multicol-1/>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// column-width + column-count
// ---------------------------------------------------------------------------

/// CSS Multi-column 1 § 3.1 `column-width` — `auto` or a px length. `auto`
/// means "let the available width + `column-count` derive the width".
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ColumnWidth {
    /// `auto` (the default).
    #[default]
    Auto,
    /// A definite px length (the cascade resolves `em`/`%` first; a
    /// negative length is invalid and the caller drops it to `auto`).
    Length(f32),
}

/// CSS Multi-column 1 § 3.2 `column-count` — `auto` or a ≥ 1 integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColumnCount {
    /// `auto` (the default).
    #[default]
    Auto,
    /// A definite count (≥ 1; `0` is invalid and the caller drops it to
    /// `auto`).
    Count(u32),
}

impl ColumnCount {
    /// Parse `column-count`: `auto` or a non-negative integer. `0` is an
    /// invalid value per § 3.2 and fails closed to `None`.
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("auto") {
            return Some(Self::Auto);
        }
        let n: u32 = t.parse().ok()?;
        if n == 0 { None } else { Some(Self::Count(n)) }
    }
}

impl ColumnWidth {
    /// Parse `column-width`: `auto` or a non-negative length (the caller
    /// passes the already-resolved px value; this helper accepts the bare
    /// numeric form for the unit-test surface).
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("auto") {
            return Some(Self::Auto);
        }
        let v: f32 = t.parse().ok()?;
        if v.is_finite() && v >= 0.0 {
            Some(Self::Length(v))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// ColumnSpec + resolve
// ---------------------------------------------------------------------------

/// The `(column-width, column-count, gap)` triple + the § 3.4 resolver. The
/// `gap` is the already-resolved px `column-gap` (the caller resolves
/// `normal` → `1em` → px; pass `0.0` for a no-gap column-row).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColumnSpec {
    /// `column-width` (auto or px).
    pub column_width: ColumnWidth,
    /// `column-count` (auto or ≥ 1).
    pub column_count: ColumnCount,
    /// `column-gap` in px (≥ 0).
    pub gap: f32,
}

/// The used column-row geometry: the resolved count + per-column width +
/// the gap, with the per-column x-offset helper.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedColumns {
    /// The used column count (always ≥ 1).
    pub count: u32,
    /// The used column width in px (always ≥ 0).
    pub column_width: f32,
    /// The used column gap in px.
    pub gap: f32,
    /// The available width the row was resolved against (kept for
    /// `total_width` + the overflow check).
    pub available_width: f32,
}

impl ColumnSpec {
    /// Construct a spec from the three cascade-resolved values.
    pub const fn new(column_width: ColumnWidth, column_count: ColumnCount, gap: f32) -> Self {
        Self {
            column_width,
            column_count,
            gap,
        }
    }

    /// Run the § 3.4 pseudo-algorithm against `available_width` (the
    /// containing-block content width, px). Returns the used count +
    /// column-width + gap.
    pub fn resolve(self, available_width: f32) -> ResolvedColumns {
        let g = if self.gap.is_finite() && self.gap >= 0.0 {
            self.gap
        } else {
            0.0
        };
        let avail = if available_width.is_finite() && available_width > 0.0 {
            available_width
        } else {
            0.0
        };
        let (count, mut width) = match (self.column_width, self.column_count) {
            // (01)–(02): both auto ⇒ single column at the available width.
            (ColumnWidth::Auto, ColumnCount::Auto) => (1u32, avail),
            // (03)–(04): count set, width auto.
            (ColumnWidth::Auto, ColumnCount::Count(n)) => (n, (avail + g) / n as f32 - g),
            // (05)–(07): width set, count auto.
            (ColumnWidth::Length(w), ColumnCount::Auto) => {
                let n = floor_div(avail + g, w + g).max(1);
                (n, (avail + g) / n as f32 - g)
            }
            // (08)–(12): both set.
            (ColumnWidth::Length(w), ColumnCount::Count(n)) => {
                let fit = floor_div(avail + g, w + g).max(1);
                let count = n.min(fit).max(1);
                let mut width = (avail + g) / count as f32 - g;
                // (11)–(12): single column whose authored width exceeds the
                // available width ⇒ the column becomes the available width
                // (content overflows).
                if count == 1 && w > avail {
                    width = avail;
                }
                (count, width)
            }
        };
        // Final clamp: a too-large count never produces a negative column.
        if width < 0.0 {
            width = 0.0;
        }
        if !width.is_finite() {
            width = 0.0;
        }
        ResolvedColumns {
            count,
            column_width: width,
            gap: g,
            available_width: avail,
        }
    }
}

/// `⌊a / b⌋` with a `0` divisor guard (returns `0` when `b ≤ 0`).
fn floor_div(a: f32, b: f32) -> u32 {
    if b <= 0.0 || !b.is_finite() || !a.is_finite() {
        return 0;
    }
    if a < 0.0 {
        return 0;
    }
    (a / b).floor() as u32
}

impl ResolvedColumns {
    /// The x-offset of column `i` (0-indexed): `i * (column_width + gap)`.
    /// Out-of-range `i` is clamped to the last column's offset (the caller
    /// passes `i < count`).
    pub fn column_x(&self, i: u32) -> f32 {
        let i = i.min(self.count.saturating_sub(1));
        i as f32 * (self.column_width + self.gap)
    }

    /// The total column-row content width: `count * column_width +
    /// (count - 1) * gap`.
    pub fn total_width(&self) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        self.count as f32 * self.column_width + (self.count - 1) as f32 * self.gap
    }

    /// `true` iff the column-row overflows the available width (the single
    /// `column-width > available` case § 3.4 lines (11)–(12) produce).
    pub fn overflows(&self) -> bool {
        self.total_width() > self.available_width + 0.001
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
    fn parse_column_count_auto_and_integers() {
        assert_eq!(ColumnCount::parse("auto"), Some(ColumnCount::Auto));
        assert_eq!(ColumnCount::parse("3"), Some(ColumnCount::Count(3)));
        assert_eq!(ColumnCount::parse("  2  "), Some(ColumnCount::Count(2)));
        assert_eq!(ColumnCount::parse("0"), None, "0 is invalid per § 3.2");
        assert_eq!(ColumnCount::parse("1.5"), None);
        assert_eq!(ColumnCount::parse("auto-ish"), None);
    }

    #[test]
    fn parse_column_width_auto_and_lengths() {
        assert_eq!(ColumnWidth::parse("auto"), Some(ColumnWidth::Auto));
        assert_eq!(ColumnWidth::parse("100"), Some(ColumnWidth::Length(100.0)));
        assert_eq!(ColumnWidth::parse("-5"), None, "negative is invalid");
    }

    // --- § 3.4 branches ----------------------------------------------

    #[test]
    fn both_auto_yields_single_column() {
        // (01)–(02): count=1, width=available.
        let spec = ColumnSpec::new(ColumnWidth::Auto, ColumnCount::Auto, 0.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.count, 1);
        assert_eq!(r.column_width, 800.0);
        assert_eq!(r.total_width(), 800.0);
        assert!(!r.overflows());
    }

    #[test]
    fn count_set_width_auto_distributes_evenly() {
        // (03)–(04): count=4, no gap → width = 800/4 = 200.
        let spec = ColumnSpec::new(ColumnWidth::Auto, ColumnCount::Count(4), 0.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.count, 4);
        assert_eq!(r.column_width, 200.0);
        assert_eq!(r.total_width(), 800.0);
    }

    #[test]
    fn count_set_width_auto_with_gap() {
        // (03)–(04): count=3, gap=20 → width = (800+20)/3 - 20 = 273.33 - 20 = 253.33.
        let spec = ColumnSpec::new(ColumnWidth::Auto, ColumnCount::Count(3), 20.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.count, 3);
        let expected = (800.0 + 20.0) / 3.0 - 20.0;
        assert!((r.column_width - expected).abs() < 0.01);
        assert_eq!(r.total_width(), 800.0);
    }

    #[test]
    fn width_set_count_auto_derives_count() {
        // (05)–(07): width=200, gap=0 → count = floor(800/200) = 4, width recomputed = 200.
        let spec = ColumnSpec::new(ColumnWidth::Length(200.0), ColumnCount::Auto, 0.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.count, 4);
        assert_eq!(r.column_width, 200.0);
    }

    #[test]
    fn width_set_count_auto_with_gap() {
        // (05)–(07): width=200, gap=20 → count = floor((800+20)/(200+20)) = floor(820/220) = 3.
        // width = (800+20)/3 - 20 = 273.33 - 20 = 253.33.
        let spec = ColumnSpec::new(ColumnWidth::Length(200.0), ColumnCount::Auto, 20.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.count, 3);
        let expected = (800.0 + 20.0) / 3.0 - 20.0;
        assert!((r.column_width - expected).abs() < 0.01);
    }

    #[test]
    fn both_set_takes_minimum() {
        // (08)–(10): width=200, count=5, gap=0 → fit = floor(800/200) = 4.
        // count = min(5, 4) = 4; width = 800/4 = 200.
        let spec = ColumnSpec::new(ColumnWidth::Length(200.0), ColumnCount::Count(5), 0.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.count, 4);
        assert_eq!(r.column_width, 200.0);
    }

    #[test]
    fn both_set_count_smaller_than_fit() {
        // (08)–(10): width=100, count=3, gap=0 → fit = floor(800/100) = 8.
        // count = min(3, 8) = 3; width = 800/3 = 266.67.
        let spec = ColumnSpec::new(ColumnWidth::Length(100.0), ColumnCount::Count(3), 0.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.count, 3);
        let expected = 800.0 / 3.0;
        assert!((r.column_width - expected).abs() < 0.01);
    }

    #[test]
    fn both_set_single_column_overflow_clamps_width() {
        // (11)–(12): width=1000, count=1, available=800, gap=0.
        // fit = floor(800/1000) = 0 → max(1,0) = 1. count = min(1,1) = 1.
        // width = 800/1 - 0 = 800, then count==1 && w(1000) > avail(800) ⇒ width = 800.
        let spec = ColumnSpec::new(ColumnWidth::Length(1000.0), ColumnCount::Count(1), 0.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.count, 1);
        assert_eq!(r.column_width, 800.0);
        assert!(!r.overflows(), "clamped to available, no overflow reported");
    }

    // --- guards ------------------------------------------------------

    #[test]
    fn too_many_columns_clamps_width_to_zero() {
        // count=100, available=800, gap=0 → width = 800/100 - 0 = 8 (positive).
        // Push to count=10000 → width = 0.08; not negative yet.
        // count=1_000_000 → width = 0.0008; still positive. Use a count that
        // forces negative via a large gap: count=10, gap=200, avail=800.
        // width = (800+200)/10 - 200 = 100 - 200 = -100 → clamped to 0.
        let spec = ColumnSpec::new(ColumnWidth::Auto, ColumnCount::Count(10), 200.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.count, 10);
        assert_eq!(r.column_width, 0.0, "negative width clamped to 0");
    }

    #[test]
    fn zero_available_width_yields_zero_columns_width() {
        let spec = ColumnSpec::new(ColumnWidth::Length(200.0), ColumnCount::Auto, 0.0);
        let r = spec.resolve(0.0);
        assert_eq!(r.count, 1, "at least one column even with no room");
        assert_eq!(r.column_width, 0.0);
    }

    #[test]
    fn negative_gap_treated_as_zero() {
        let spec = ColumnSpec::new(ColumnWidth::Auto, ColumnCount::Count(4), -50.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.gap, 0.0);
        assert_eq!(r.column_width, 200.0);
    }

    // --- column_x + total_width --------------------------------------

    #[test]
    fn column_x_offsets_with_gap() {
        // count=3, width=200, gap=20: offsets 0, 220, 440.
        let spec = ColumnSpec::new(ColumnWidth::Length(200.0), ColumnCount::Count(3), 20.0);
        let r = spec.resolve(640.0); // (640+20)/(200+20)=3; width=(640+20)/3-20=200
        assert_eq!(r.count, 3);
        assert_eq!(r.column_x(0), 0.0);
        assert_eq!(r.column_x(1), 220.0);
        assert_eq!(r.column_x(2), 440.0);
        // Out-of-range i clamps to the last column.
        assert_eq!(r.column_x(99), 440.0);
    }

    #[test]
    fn total_width_sums_columns_and_gaps() {
        // 3 columns × 200 + 2 × 20 = 640.
        let spec = ColumnSpec::new(ColumnWidth::Length(200.0), ColumnCount::Count(3), 20.0);
        let r = spec.resolve(640.0);
        assert_eq!(r.total_width(), 640.0);
    }

    #[test]
    fn overflows_when_gaps_alone_exceed_available() {
        // count=10, gap=200, avail=800 → width = (800+200)/10 - 200 = -100
        // → clamped to 0. The gaps alone (9 × 200 = 1800) exceed the
        // available 800, so the column-row overflows.
        let spec = ColumnSpec::new(ColumnWidth::Auto, ColumnCount::Count(10), 200.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.column_width, 0.0);
        assert_eq!(r.total_width(), 1800.0);
        assert!(r.overflows());
    }

    #[test]
    fn single_column_authored_wider_than_available_does_not_overflow() {
        // § 3.4 (11)-(12): the column is clamped to the available width, so
        // the row fits exactly (content overflows the column, not the row).
        let spec = ColumnSpec::new(ColumnWidth::Length(1000.0), ColumnCount::Count(1), 0.0);
        let r = spec.resolve(800.0);
        assert_eq!(r.column_width, 800.0);
        assert_eq!(r.total_width(), 800.0);
        assert!(!r.overflows());
    }
}
