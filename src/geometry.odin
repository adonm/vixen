package vixen

// Shared hit-testing over retained layout geometry (doc pixels, not viewport
// cells). Block-level (links/fields/lines) for mouse clicks and future copy;
// char-precise text selection is M4 (needs glyph-to-char maps, lost in flush).
// Multi-line link boxes are approximations (first bbox match wins); per-line
// fragments are future work.

// Line containing doc y (binary search; baselines increase top-to-bottom).
// Margins above the first line or below the last miss.
page_line_at_y :: proc(page: ^Page, y: int) -> (int, bool) {
	lo, hi := 0, len(page.lines)
	for lo < hi {
		mid := (lo + hi) / 2
		ln := &page.lines[mid]
		top := int(ln.baseline - ln.height * 0.8)
		bot := int(ln.baseline + ln.height * 0.2)
		if y < top {
			hi = mid
		} else if y >= bot {
			lo = mid + 1
		} else {
			return mid, true
		}
	}
	return -1, false
}

// First link whose bbox contains (x, y). Single-line links are exact;
// multi-line link boxes span whole rows (may overlap neighbors; first wins).
page_link_at :: proc(page: ^Page, x, y: int) -> (int, bool) {
	for &l, i in page.links {
		if f32(x) >= l.x0 && f32(x) < l.x1 && f32(y) >= l.y0 && f32(y) < l.y1 {
			return i, true
		}
	}
	return -1, false
}

// Field box x-geometry shared by both painters, hit-testing, and click
// mapping. All call sites must use this so a click lands exactly where the
// pixels are.
tui_field_box_x :: proc(f: ^Field, sw: int) -> (bx0, bx1, tx0, tx1: int, ok: bool) {
	if sw <= 0 { return 0, 0, 0, 0, false }
	bx0 = clamp(int(f.x0) - 4, 0, sw - 1)
	bx1 = sw - 4
	if sw < 32 {
		bx0, bx1 = 0, sw
	}
	if bx1 <= bx0 + 12 { return 0, 0, 0, 0, false }
	tx0, tx1 = bx0 + 8, bx1 - 8
	if tx1 <= tx0 { return 0, 0, 0, 0, false }
	return bx0, bx1, tx0, tx1, true
}

// Field box containing (x, y). Boxes span f.line..f.line+nlines-1 rows;
// x-range is the exact painter box above (unpainted boxes are unclickable).
page_field_at :: proc(page: ^Page, x, y: int) -> (int, bool) {
	for &f, i in page.fields {
		if f.kind == .hidden || f.line < 0 || f.line >= len(page.lines) {
			continue
		}
		bx0, bx1, _, _, ok := tui_field_box_x(&page.fields[i], page.width)
		if !ok { continue }
		nlines := max(f.nlines, 1)
		last := min(f.line + nlines - 1, len(page.lines) - 1)
		top := int(page.lines[f.line].baseline - page.lines[f.line].height * 0.8)
		bot := int(page.lines[last].baseline + page.lines[last].height * 0.2)
		if x >= bx0 && x < bx1 && y >= top && y < bot {
			return i, true
		}
	}
	return -1, false
}
