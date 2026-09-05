package vixen

// Visible form controls: overlay repaint of current values, selection,
// caret, focus ring, and scroll-into-view. Layout glyphs are stale as soon
// as the user types (and after relayout, which preserves edited values but
// re-lays-out from source), so every page frame repaints all visible field
// boxes from sess.page.fields. Single-line display: newlines/tabs in
// textarea values render as spaces; the stored value keeps them.

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
	ln := &t.sess.page.lines[f.line]
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
