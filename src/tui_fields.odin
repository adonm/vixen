package vixen

// Visible form controls: overlay repaint of current values, selection,
// caret, focus ring, and scroll-into-view. Layout glyphs are stale as soon
// as the user types (and after relayout, which preserves edited values but
// re-lays-out from source), so every page frame repaints all visible field
// boxes from sess.page.fields. Textareas show multiple rows with vertical
// scroll; tabs render as spaces (stored intact).

import "core:strings"
import "core:unicode/utf8"

import edit "core:text/edit"

// Clamp a byte offset to a rune boundary inside s.
tui_rune_clamp :: proc(s: string, off: int) -> int {
	o := clamp(off, 0, len(s))
	for o > 0 && o < len(s) && (s[o] & 0xc0) == 0x80 {
		o -= 1
	}
	return o
}

// Display copy of a text value: newlines/tabs become spaces (1:1 bytes, so
// caret/selection byte offsets stay valid). Caller owns the result.
tui_field_display :: proc(value: string) -> string {
	if len(value) == 0 {
		return strings.clone("")
	}
	b := strings.clone(value)
	bb := transmute([]u8)b
	for i in 0 ..< len(bb) {
		if bb[i] == '\n' || bb[i] == '\r' || bb[i] == '\t' {
			bb[i] = ' '
		}
	}
	return b
}

// Width of text shaped at px. Returns 0 for empty.
tui_text_width :: proc(bank: ^Font_Bank, text: string, px: f32) -> f32 {
	if len(text) == 0 {
		return 0
	}
	rc := Render_Ctx{bank = bank}
	line: Cur_Line
	w := shape_word(&rc, text, px, &line)
	for wd in line.words {
		delete(wd)
	}
	delete(line.glyphs)
	delete(line.words)
	return w
}

// Ensure the focused field's box is fully inside the viewport.
// For textareas, ensures the caret row (not just the box top) is visible.
tui_ensure_field_visible :: proc(t: ^Tui) {
	if t.focus < 0 || t.focus >= len(t.field_edits) {
		return
	}
	fe := &t.field_edits[t.focus]
	if fe.field < 0 || fe.field >= len(t.sess.page.fields) {
		return
	}
	f := &t.sess.page.fields[fe.field]
	if f.line < 0 || f.line >= len(t.sess.page.lines) {
		return
	}
	line_idx := f.line
	if f.kind == .textarea && f.nlines > 1 {
		// Caret row → visible box row → layout line.
		caret := tui_rune_clamp(f.value, fe.state.selection[0])
		cr, _ := tui_textarea_caret_row(f.value, caret)
		br := clamp(cr - fe.voff, 0, f.nlines-1)
		line_idx = clamp(f.line + br, 0, len(t.sess.page.lines)-1)
	}
	ln := &t.sess.page.lines[line_idx]
	top := int(ln.baseline - ln.height * 0.8)
	bot := int(ln.baseline + ln.height * 0.2)
	view_h := tui_view_height(t)
	if view_h <= 0 {
		return
	}
	if top < t.scroll_y {
		t.scroll_y = max(top - 8, 0)
	} else if bot > t.scroll_y + view_h {
		t.scroll_y = bot - view_h + 8
	}
	tui_clamp_scroll(t)
}

// Split a textarea value into rows (borrowed substrings, no copy).
// Empty value gives one empty row; trailing newline gives trailing empty row.
tui_textarea_rows :: proc(value: string) -> []string {
	if len(value) == 0 {
		r := make([]string, 1, context.temp_allocator)
		r[0] = ""
		return r
	}
	// Count rows first (newlines + 1).
	n := 1
	for c in value {
		if c == '\n' {
			n += 1
		}
	}
	rows := make([]string, n, context.temp_allocator)
	ri, start := 0, 0
	for i in 0 ..< len(value) {
		if value[i] == '\n' {
			rows[ri] = value[start:i]
			ri += 1
			start = i+1
		}
	}
	rows[ri] = value[start:]
	return rows
}

// Map a byte caret to (row, col_byte) in split rows. Col is a byte offset
// within the row (rune-clamped by the caller when shaping).
tui_textarea_caret_row :: proc(value: string, caret: int) -> (row, col: int) {
	rows := tui_textarea_rows(value)
	pos := 0
	for r, i in rows {
		// Row byte range is [pos, pos+len(r)]; newline (if any) at pos+len(r).
		if caret <= pos + len(r) {
			return i, caret - pos
		}
		pos += len(r) + 1 // skip newline
	}
	// Caret at/past end (e.g., trailing newline): last row, end.
	if len(rows) > 0 {
		return len(rows)-1, len(rows[len(rows)-1])
	}
	return 0, 0
}

// Repaint all visible field boxes from current values. Call after
// raster_slice, before hints (hints stay on top).
tui_draw_fields :: proc(t: ^Tui, sw, sh: int) {
	if len(t.field_edits) == 0 || sw <= 0 || sh <= 0 {
		return
	}
	sess := t.sess
	rc := Render_Ctx{bank = &sess.bank}
	for fe_idx in 0 ..< len(t.field_edits) {
		fe := &t.field_edits[fe_idx]
		if fe.field < 0 || fe.field >= len(sess.page.fields) {
			continue
		}
		f := &sess.page.fields[fe.field]
		if f.kind == .hidden || f.line < 0 || f.line >= len(sess.page.lines) {
			continue
		}
		if f.kind == .textarea {
			tui_draw_textarea(t, fe, fe_idx == t.focus, sw, sh, &rc)
			continue
		}
		ln := &sess.page.lines[f.line]
		top_doc := int(ln.baseline - ln.height * 0.8)
		bot_doc := int(ln.baseline + ln.height * 0.2)
		top := top_doc - t.scroll_y
		bot := bot_doc - t.scroll_y
		if bot <= 0 || top >= sh {
			continue
		}
		ctop, cbot := max(top, 0), min(bot, sh)
		bx0 := clamp(int(f.x0) - 4, 0, sw - 1)
		bx1 := sw - 4
		if sw < 32 {
			bx0, bx1 = 0, sw
		}
		if bx1 <= bx0 + 12 {
			continue
		}
		focused := fe_idx == t.focus
		bg := [4]u8{28, 28, 36, 255}
		border := [4]u8{80, 80, 90, 255}
		fg := [4]u8{232, 232, 238, 255}
		if f.kind == .submit {
			bg = [4]u8{50, 50, 65, 255}
			fg = [4]u8{255, 220, 130, 255}
			if focused {
				bg = [4]u8{70, 70, 90, 255}
			}
		} else if focused {
			bg = [4]u8{40, 40, 55, 255}
		}
		if focused {
			border = [4]u8{255, 255, 255, 255}
		}
		// Fill + border (clipped to viewport).
		for yy in ctop ..< cbot {
			for xx in bx0 ..< bx1 {
				o := (yy * sw + xx) * 4
				t.slice[o + 0], t.slice[o + 1], t.slice[o + 2] = bg[0], bg[1], bg[2]
			}
		}
		for xx in bx0 ..< bx1 {
			if ctop < cbot {
				o0 := (ctop * sw + xx) * 4
				o1 := ((cbot - 1) * sw + xx) * 4
				t.slice[o0 + 0], t.slice[o0 + 1], t.slice[o0 + 2] = border[0], border[1], border[2]
				t.slice[o1 + 0], t.slice[o1 + 1], t.slice[o1 + 2] = border[0], border[1], border[2]
			}
		}
		for yy in ctop ..< cbot {
			o0 := (yy * sw + bx0) * 4
			o1 := (yy * sw + bx1 - 1) * 4
			t.slice[o0 + 0], t.slice[o0 + 1], t.slice[o0 + 2] = border[0], border[1], border[2]
			t.slice[o1 + 0], t.slice[o1 + 1], t.slice[o1 + 2] = border[0], border[1], border[2]
		}
		tx0, tx1 := bx0 + 8, bx1 - 8
		if tx1 <= tx0 {
			continue
		}
		// Text source: current value (text) or label (submit).
		display: string
		display_owned := false
		caret_byte, sel_lo, sel_hi := 0, 0, 0
		has_sel := false
		if f.kind == .submit {
			display = f.label
		} else {
			display = tui_field_display(f.value)
			display_owned = true
			if focused {
				caret_byte = tui_rune_clamp(display, fe.state.selection[0])
				lo, hi := edit.sorted_selection(&fe.state)
				sel_lo = tui_rune_clamp(display, lo)
				sel_hi = tui_rune_clamp(display, hi)
				has_sel = sel_lo != sel_hi
			}
		}
		// Shape full text once for blitting.
		line: Cur_Line
		full_w := shape_word(&rc, display, f.px, &line)
		// Horizontal scroll: keep the caret visible for focused text.
		xoff := 0
		if focused && f.kind == .text {
			interior := tx1 - tx0
			caret_w := tui_text_width(&sess.bank, display[:caret_byte], f.px)
			xoff = fe.xoff
			if interior > 20 {
				if caret_w < f32(xoff) {
					xoff = max(int(caret_w) - 8, 0)
				} else if caret_w - f32(xoff) > f32(interior - 12) {
					xoff = int(caret_w) - (interior - 12)
				}
				if full_w <= f32(interior) {
					xoff = 0
				}
				xoff = max(xoff, 0)
			} else {
				xoff = 0
			}
			fe.xoff = xoff
		}
		baseline := ln.baseline - f32(t.scroll_y)
		// Submit labels are centered; text is left-aligned with scroll.
		text_origin := f32(tx0)
		if f.kind == .submit && full_w < f32(tx1 - tx0) {
			text_origin = f32(tx0) + (f32(tx1 - tx0) - full_w) / 2
		}
		// Selection highlight behind the text.
		if focused && has_sel {
			lo_w := tui_text_width(&sess.bank, display[:sel_lo], f.px)
			hi_w := tui_text_width(&sess.bank, display[:sel_hi], f.px)
			sx0 := int(text_origin + lo_w - f32(xoff))
			sx1 := int(text_origin + hi_w - f32(xoff))
			sx0, sx1 = max(sx0, tx0), min(sx1, tx1)
			if sx1 > sx0 {
				for yy in max(ctop + 2, 0) ..< min(cbot - 2, sh) {
					for xx in sx0 ..< sx1 {
						o := (yy * sw + xx) * 4
						t.slice[o + 0], t.slice[o + 1], t.slice[o + 2] = 80, 80, 120
					}
				}
			}
		}
		for p in line.glyphs {
			pen_x := text_origin + p.x - f32(xoff)
			// Clip to the text interior (not just the viewport).
			if pen_x + p.adv < f32(tx0) || pen_x > f32(tx1) {
				continue
			}
			blit_glyph_px(t.slice, sw, sh, &sess.bank.fonts[p.font],
				p.gid, pen_x + p.off_x, baseline + p.off_y, fg)
		}
		for wd in line.words {
			delete(wd)
		}
		delete(line.glyphs)
		delete(line.words)
		// Caret bar for focused text.
		if focused && f.kind == .text {
			caret_w := tui_text_width(&sess.bank, display[:caret_byte], f.px)
			cx := int(text_origin + caret_w - f32(xoff) + 0.5)
			cx = clamp(cx, tx0, tx1 - 2)
			for yy in max(ctop + 2, 0) ..< min(cbot - 2, sh) {
				for xx in max(cx, 0) ..< min(cx + 2, sw) {
					if xx < tx0 || xx >= tx1 {
						continue
					}
					o := (yy * sw + xx) * 4
					t.slice[o + 0], t.slice[o + 1], t.slice[o + 2] = 255, 255, 255
				}
			}
		}
		if display_owned {
			delete(display)
		}
		_ = utf8.rune_size // keep utf8 import referenced for future grapheme caret work
	}
}

// Multi-row textarea overlay: fixed box height (layout rows), vertical
// scroll to keep the caret visible, per-row horizontal scroll for the caret
// row only (other rows show from the start). Selection highlights span rows.
tui_draw_textarea :: proc(t: ^Tui, fe: ^Field_Edit, focused: bool, sw, sh: int, rc: ^Render_Ctx) {
	sess := t.sess
	f := &sess.page.fields[fe.field]
	nlines := max(f.nlines, 1)
	if f.line + nlines - 1 >= len(sess.page.lines) {
		nlines = len(sess.page.lines) - f.line
	}
	if nlines <= 0 {
		return
	}
	top_ln := &sess.page.lines[f.line]
	bot_ln := &sess.page.lines[f.line + nlines - 1]
	top := int(top_ln.baseline - top_ln.height * 0.8) - t.scroll_y
	bot := int(bot_ln.baseline + bot_ln.height * 0.2) - t.scroll_y
	if bot <= 0 || top >= sh {
		return
	}
	ctop, cbot := max(top, 0), min(bot, sh)
	bx0 := clamp(int(f.x0) - 4, 0, sw - 1)
	bx1 := sw - 4
	if sw < 32 {
		bx0, bx1 = 0, sw
	}
	if bx1 <= bx0 + 12 {
		return
	}
	bg := [4]u8{28, 28, 36, 255}
	border := [4]u8{80, 80, 90, 255}
	if focused {
		bg = [4]u8{40, 40, 55, 255}
		border = [4]u8{255, 255, 255, 255}
	}
	for yy in ctop ..< cbot {
		for xx in bx0 ..< bx1 {
			o := (yy * sw + xx) * 4
			t.slice[o + 0], t.slice[o + 1], t.slice[o + 2] = bg[0], bg[1], bg[2]
		}
	}
	for xx in bx0 ..< bx1 {
		if ctop < cbot {
			o0 := (ctop * sw + xx) * 4
			o1 := ((cbot - 1) * sw + xx) * 4
			t.slice[o0 + 0], t.slice[o0 + 1], t.slice[o0 + 2] = border[0], border[1], border[2]
			t.slice[o1 + 0], t.slice[o1 + 1], t.slice[o1 + 2] = border[0], border[1], border[2]
		}
	}
	for yy in ctop ..< cbot {
		o0 := (yy * sw + bx0) * 4
		o1 := (yy * sw + bx1 - 1) * 4
		t.slice[o0 + 0], t.slice[o0 + 1], t.slice[o0 + 2] = border[0], border[1], border[2]
		t.slice[o1 + 0], t.slice[o1 + 1], t.slice[o1 + 2] = border[0], border[1], border[2]
	}
	tx0, tx1 := bx0 + 8, bx1 - 8
	if tx1 <= tx0 {
		return
	}
	rows := tui_textarea_rows(f.value)
	// Caret row/col (byte offsets, rune-clamped when shaping).
	caret_byte := 0
	sel_lo, sel_hi := 0, 0
	has_sel := false
	caret_row, caret_col := 0, 0
	if focused {
		caret_byte = tui_rune_clamp(f.value, fe.state.selection[0])
		lo, hi := edit.sorted_selection(&fe.state)
		sel_lo = tui_rune_clamp(f.value, lo)
		sel_hi = tui_rune_clamp(f.value, hi)
		has_sel = sel_lo != sel_hi
		caret_row, caret_col = tui_textarea_caret_row(f.value, caret_byte)
		// Vertical scroll: keep the caret row in the box viewport.
		if caret_row < fe.voff {
			fe.voff = caret_row
		} else if caret_row >= fe.voff + nlines {
			fe.voff = caret_row - nlines + 1
		}
		fe.voff = max(fe.voff, 0)
	}
	voff := clamp(fe.voff, 0, max(len(rows) - 1, 0))
	// Byte offset of each value row start (for selection mapping).
	row_starts := make([]int, len(rows), context.temp_allocator)
	pos := 0
	for r, i in rows {
		row_starts[i] = pos
		pos += len(r) + 1
	}
	fg := [4]u8{232, 232, 238, 255}
	for bi in 0 ..< nlines {
		vi := voff + bi
		li := f.line + bi
		if li < 0 || li >= len(sess.page.lines) {
			continue
		}
		ln := &sess.page.lines[li]
		rtop := int(ln.baseline - ln.height * 0.8) - t.scroll_y
		rbot := int(ln.baseline + ln.height * 0.2) - t.scroll_y
		if rbot <= 0 || rtop >= sh {
			continue
		}
		if vi >= len(rows) {
			continue // box taller than value: blank row
		}
		disp := tui_field_display(rows[vi])
		defer delete(disp)
		// Horizontal scroll: caret row keeps the caret visible, others at 0.
		xoff := 0
		if focused && vi == caret_row {
			interior := tx1 - tx0
			cc := clamp(caret_col, 0, len(disp))
			// Clamp to rune boundary within the row.
			for cc > 0 && cc < len(disp) && (disp[cc] & 0xc0) == 0x80 {
				cc -= 1
			}
			caret_w := tui_text_width(&sess.bank, disp[:cc], f.px)
			if interior > 20 && caret_w - f32(fe.xoff) > f32(interior - 12) {
				fe.xoff = int(caret_w) - (interior - 12)
			} else if caret_w < f32(fe.xoff) {
				fe.xoff = max(int(caret_w) - 8, 0)
			}
			if tui_text_width(&sess.bank, disp, f.px) <= f32(interior) {
				fe.xoff = 0
			}
			xoff = max(fe.xoff, 0)
		}
		baseline := ln.baseline - f32(t.scroll_y)
		// Selection highlight for this row (if overlapping).
		if focused && has_sel {
			rs := row_starts[vi]
			re := rs + len(rows[vi])
			lo := max(sel_lo, rs) - rs
			hi := min(sel_hi, re) - rs
			if hi > lo {
				lo_w := tui_text_width(&sess.bank, disp[:clamp(lo, 0, len(disp))], f.px)
				hi_w := tui_text_width(&sess.bank, disp[:clamp(hi, 0, len(disp))], f.px)
				sx0 := int(f32(tx0) + lo_w - f32(xoff))
				sx1 := int(f32(tx0) + hi_w - f32(xoff))
				sx0, sx1 = max(sx0, tx0), min(sx1, tx1)
				if sx1 > sx0 {
					for yy in max(rtop + 2, 0) ..< min(rbot - 2, sh) {
						for xx in sx0 ..< sx1 {
							o := (yy * sw + xx) * 4
							t.slice[o + 0], t.slice[o + 1], t.slice[o + 2] = 80, 80, 120
						}
					}
				}
			}
		}
		line: Cur_Line
		shape_word(rc, disp, f.px, &line)
		for p in line.glyphs {
			pen_x := f32(tx0) + p.x - f32(xoff)
			if pen_x + p.adv < f32(tx0) || pen_x > f32(tx1) {
				continue
			}
			blit_glyph_px(t.slice, sw, sh, &sess.bank.fonts[p.font],
				p.gid, pen_x + p.off_x, baseline + p.off_y, fg)
		}
		for wd in line.words {
			delete(wd)
		}
		delete(line.glyphs)
		delete(line.words)
		// Caret bar on the caret row.
		if focused && vi == caret_row {
			cc := clamp(caret_col, 0, len(disp))
			for cc > 0 && cc < len(disp) && (disp[cc] & 0xc0) == 0x80 {
				cc -= 1
			}
			caret_w := tui_text_width(&sess.bank, disp[:cc], f.px)
			cx := int(f32(tx0) + caret_w - f32(xoff) + 0.5)
			cx = clamp(cx, tx0, tx1 - 2)
			for yy in max(rtop + 2, 0) ..< min(rbot - 2, sh) {
				for xx in max(cx, 0) ..< min(cx + 2, sw) {
					if xx < tx0 || xx >= tx1 {
						continue
					}
					o := (yy * sw + xx) * 4
					t.slice[o + 0], t.slice[o + 1], t.slice[o + 2] = 255, 255, 255
				}
			}
		}
	}
}
