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

// Field box containing (x, y). Boxes span f.line..f.line+nlines-1 rows and
// f.x0-4..page.width-4 horizontally (TUI overlay geometry; no h-scroll).
page_field_at :: proc(page: ^Page, x, y: int) -> (int, bool) {
	for &f, i in page.fields {
		if f.kind == .hidden || f.line < 0 || f.line >= len(page.lines) {
			continue
		}
		nlines := max(f.nlines, 1)
		last := min(f.line + nlines - 1, len(page.lines) - 1)
		top := int(page.lines[f.line].baseline - page.lines[f.line].height * 0.8)
		bot := int(page.lines[last].baseline + page.lines[last].height * 0.2)
		x0 := max(int(f.x0) - 4, 0)
		x1 := page.width - 4
		if x >= x0 && x < x1 && y >= top && y < bot {
			return i, true
		}
	}
	return -1, false
}
