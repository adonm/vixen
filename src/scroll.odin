package vixen

// Reading-position anchors: character offsets survive reflow, history, and
// reload better than raw pixel offsets. History entries carry the offset of
// the page being left; callers restore by mapping the offset back to pixels.

import "core:strings"

History_Entry :: struct {
	url:    string, // owned absolute URL
	anchor: int,    // char offset of the top visible line (0 = top)
}

delete_history_entry :: proc(e: ^History_Entry) {
	delete(e.url)
}

history_push :: proc(stack: ^[dynamic]History_Entry, url: string, anchor: int) {
	append(stack, History_Entry{strings.clone(url), anchor})
}

history_clear :: proc(stack: ^[dynamic]History_Entry) {
	for &e in stack {
		delete_history_entry(&e)
	}
	clear(stack)
}

// Character offset of the first visible line. Newlines count as one char.
page_char_offset :: proc(page: ^Page, scroll_y: int) -> int {
	if len(page.lines) == 0 {
		return 0
	}
	off := 0
	for &ln, i in page.lines {
		top := int(ln.baseline - ln.height * 0.8)
		bot := int(ln.baseline + ln.height * 0.2)
		line_len := len(ln.text) + 1
		if scroll_y < bot {
			frac := f32(0)
			if bot > top {
				frac = f32(clamp(scroll_y - top, 0, bot - top)) / f32(bot - top)
			}
			_ = i
			return off + int(frac * f32(max(len(ln.text), 1)))
		}
		off += line_len
	}
	return off
}

// Map a saved offset back to a pixel scroll for the current layout.
page_scroll_for_offset :: proc(page: ^Page, offset, view_h: int) -> int {
	if len(page.lines) == 0 {
		return 0
	}
	off := max(offset, 0)
	acc := 0
	for &ln in page.lines {
		line_len := len(ln.text) + 1
		if off < acc + line_len {
			in_line := clamp(off - acc, 0, len(ln.text))
			frac := f32(0)
			if len(ln.text) > 0 {
				frac = f32(in_line) / f32(len(ln.text))
			}
			top := int(ln.baseline - ln.height * 0.8)
			y := top + int(frac * ln.height)
			max_y := max(page.height - view_h, 0)
			return clamp(y, 0, max_y)
		}
		acc += line_len
	}
	return max(page.height - view_h, 0)
}

// Frontend helpers: save the current top into the session before any
// history-pushing navigation; restore after back/forward/reload/relayout.
tui_save_anchor :: proc(t: ^Tui) {
	if t.sess.has {
		t.sess.cur_anchor = page_char_offset(&t.sess.page, t.scroll_y)
	} else {
		t.sess.cur_anchor = 0
	}
}

tui_restore_anchor :: proc(t: ^Tui) {
	t.scroll_y = page_scroll_for_offset(&t.sess.page, t.sess.cur_anchor, tui_view_height(t))
	tui_clamp_scroll(t)
}
