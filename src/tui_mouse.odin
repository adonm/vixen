package vixen

// Mouse: SGR clicks/wheel mapped onto retained page geometry. Coordinates
// arrive 1-based in cells (1006) or pixels (1016, only after a DECRQM
// confirm); mapping resolves units via t.mouse_pixels, then everything
// downstream works in document pixels through geometry.odin.

import "core:unicode/utf8"

// Raw report -> document pixels. The page image fills terminal rows
// 1..rows-2 at full viewport width; chrome rows and undrawable viewports
// miss (callers treat a miss as a no-op, never clamp — clamping would
// mis-fire neighbor actions).
tui_mouse_to_doc :: proc(t: ^Tui, x, y: int) -> (dx, dy: int, ok: bool) {
	if !tui_drawable(t) { return 0, 0, false }
	sw, sh := t.cols * t.cell_w, tui_view_height(t)
	if t.mouse_pixels {
		if x < 1 || y < 1 || x - 1 >= sw || y - 1 >= sh { return 0, 0, false }
		return x - 1, (y - 1) + t.scroll_y, true
	}
	if x < 1 || x > t.cols || y < 1 || y > t.rows - 2 { return 0, 0, false }
	px := (x - 1) * t.cell_w + t.cell_w / 2
	py := (y - 1) * t.cell_h + t.cell_h / 2
	return px, py + t.scroll_y, true
}

// Nearest byte offset in display whose shaped prefix width best matches the
// click. Offsets stay on rune boundaries; out-of-range clicks clamp to the
// ends. O(log n) shapes per click; clicks are rare.
tui_caret_at_width :: proc(bank: ^Font_Bank, display: string, px, target: f32) -> int {
	if len(display) == 0 || target <= 0 { return 0 }
	full := tui_text_width(bank, display, px)
	if target >= full { return len(display) }
	bounds: [dynamic]int // rune-boundary byte offsets
	defer delete(bounds)
	append(&bounds, 0)
	i := 0
	for i < len(display) {
		_, n := utf8.decode_rune(display[i:])
		i += max(n, 1)
		append(&bounds, i)
	}
	lo, hi := 0, len(bounds) - 1
	for lo < hi {
		mid := (lo + hi + 1) / 2
		if tui_text_width(bank, display[:bounds[mid]], px) <= target {
			lo = mid
		} else {
			hi = mid - 1
		}
	}
	best, best_d := bounds[lo], abs(tui_text_width(bank, display[:bounds[lo]], px) - target)
	if lo + 1 < len(bounds) {
		w := tui_text_width(bank, display[:bounds[lo+1]], px)
		if abs(w - target) < best_d {
			best = bounds[lo + 1]
		}
	}
	return best
}

// Absolute byte offset of (value row, in-row byte col) in a textarea value.
tui_textarea_abs :: proc(rows: []string, row, col: int) -> int {
	off := 0
	for r, i in rows {
		if i == row {
			return off + clamp(col, 0, len(r))
		}
		off += len(r) + 1 // +1: the split newline
	}
	return off
}

tui_handle_mouse :: proc(t: ^Tui, m: Term_Mouse) {
	if m.leave || m.release { return }
	if m.button & 64 != 0 {
		// Wheel. Modified wheels (shift=hscroll elsewhere; we have no
		// h-scroll) and horizontal wheels are ignored, never mis-scrolled.
		if m.button & (4 | 8 | 16) != 0 { return }
		old := t.scroll_y
		switch m.button & 3 {
		case 0:
			t.scroll_y -= 3 * t.cell_h
		case 1:
			t.scroll_y += 3 * t.cell_h
		case:
			return
		}
		tui_clamp_scroll(t)
		if t.scroll_y != old {
			t.page_dirty, t.chrome_dirty = true, true
		}
		return
	}
	if m.button & 32 != 0 { return } // motion: no drag support yet
	if m.button & 3 != 0 { return } // middle/right: no context menu yet
	if m.button & (4 | 8 | 16) != 0 { return } // modified clicks ignored
	// Left press. Chrome editors dismiss (keeping their text/matches);
	// the click itself is otherwise ignored while one is open.
	if t.url_active {
		t.url_active = false
		t.chrome_dirty = true
		return
	}
	if t.find_active {
		tui_find_close_keep(t)
		return
	}
	dx, dy, ok := tui_mouse_to_doc(t, m.x, m.y)
	if !ok { return }
	// Fields paint over links, so they win ties (mirrors paint order).
	if fi, hit := page_field_at(&t.sess.page, dx, dy); hit {
		tui_click_field(t, fi, dx, dy)
		return
	}
	if li, hit := page_link_at(&t.sess.page, dx, dy); hit {
		tui_follow_url(t, t.sess.page.links[li].url)
		return
	}
	if t.focus >= 0 { // empty space blurs, browser-style
		t.focus = -1
		t.page_dirty, t.chrome_dirty = true, true
	}
}

// Focus a clicked field; text controls place the caret, buttons submit.
tui_click_field :: proc(t: ^Tui, fi, dx, dy: int) {
	ei := -1
	for e, i in t.field_edits {
		if e.field == fi {
			ei = i
			break
		}
	}
	if ei < 0 { return } // desync guard: sync covers all non-hidden
	f := &t.sess.page.fields[fi]
	if f.kind == .submit {
		t.focus = ei
		tui_press_button(t)
		return
	}
	old_focus := t.focus
	t.focus = ei
	if old_focus != ei {
		tui_announce_focus(t)
		t.page_dirty, t.chrome_dirty = true, true
	}
	fe := &t.field_edits[ei]
	old_caret := fe.state.selection
	sw := t.cols * t.cell_w
	_, _, tx0, tx1, bok := tui_field_box_x(f, sw)
	if !bok { return }
	if f.kind == .textarea {
		rows := tui_textarea_rows(f.value)
		// Click viewport row -> box row -> value row (pinned scroll).
		nlines := max(f.nlines, 1)
		if f.line + nlines - 1 >= len(t.sess.page.lines) {
			nlines = len(t.sess.page.lines) - f.line
		}
		voff := clamp(fe.voff, 0, max(len(rows) - 1, 0))
		// Click viewport row -> box row (last row whose top is above the
		// click; float->int truncation can leave 1px gaps between rows).
		bi := 0
		for b in 0 ..< nlines {
			li := f.line + b
			if li < 0 || li >= len(t.sess.page.lines) { continue }
			ln := &t.sess.page.lines[li]
			if int(ln.baseline - ln.height * 0.8) <= dy {
				bi = b
			}
		}
		vi := voff + bi
		off: int
		if vi >= len(rows) {
			off = len(f.value) // blank box area below short values
		} else {
			disp := tui_field_display(rows[vi])
			defer delete(disp)
			// Caret-row scroll matches the painter (others paint at 0).
			xoff := 0
			caret_byte := tui_rune_clamp(f.value, fe.state.selection[0])
			cur_cr, _ := tui_textarea_caret_row(f.value, caret_byte)
			if vi == cur_cr {
				xoff = max(fe.xoff, 0)
			}
			col := tui_caret_at_width(&t.sess.bank, disp, f.px, f32(dx - tx0) + f32(xoff))
			off = tui_textarea_abs(rows, vi, col)
		}
		fe.state.selection = {off, off}
	} else {
		display := tui_field_display(f.value)
		defer delete(display)
		off := tui_caret_at_width(&t.sess.bank, display, f.px, f32(dx - tx0) + f32(max(fe.xoff, 0)))
		fe.state.selection = {off, off}
	}
	if fe.state.selection != old_caret {
		t.page_dirty = true
	}
}
