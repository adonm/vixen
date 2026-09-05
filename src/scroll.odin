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
	if off <= 0 {
		return 0 // top of document includes the margin above line one
	}
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

// Same document (fragment-only change)? Compares normalized scheme/host/
// port/path/query, ignoring userinfo case? No: exact normalized compare
// except fragment. Unparseable URLs fall back to raw prefix compare.
urls_same_document :: proc(a, b: string) -> bool {
	pa, oka := url_parse(a)
	pb, okb := url_parse(b)
	if !oka || !okb {
		delete_parsed_url(&pa)
		delete_parsed_url(&pb)
		// Fallback: strip fragments and compare raw strings.
		sa := a
		if i := strings.index_byte(a, '#'); i >= 0 {
			sa = a[:i]
		}
		sb := b
		if i := strings.index_byte(b, '#'); i >= 0 {
			sb = b[:i]
		}
		return sa == sb
	}
	defer delete_parsed_url(&pa)
	defer delete_parsed_url(&pb)
	return pa.scheme == pb.scheme && pa.host == pb.host && pa.port == pb.port &&
		pa.path == pb.path && pa.query == pb.query
}

// Percent-decode a fragment (UTF-8 bytes preserved). Owns the result.
// Invalid % sequences stay literal; '+' stays '+' (fragments, not queries).
fragment_decode :: proc(frag: string) -> string {
	if strings.index_byte(frag, '%') < 0 {
		return strings.clone(frag)
	}
	b: strings.Builder
	strings.builder_init(&b, context.temp_allocator)
	defer delete(b.buf)
	i := 0
	for i < len(frag) {
		if frag[i] == '%' && i+2 < len(frag) {
			hi := frag[i+1]
			lo := frag[i+2]
			hv := -1
			lv := -1
			if hi >= '0' && hi <= '9' { hv = int(hi-'0') }
			else if hi >= 'a' && hi <= 'f' { hv = int(hi-'a')+10 }
			else if hi >= 'A' && hi <= 'F' { hv = int(hi-'A')+10 }
			if lo >= '0' && lo <= '9' { lv = int(lo-'0') }
			else if lo >= 'a' && lo <= 'f' { lv = int(lo-'a')+10 }
			else if lo >= 'A' && lo <= 'F' { lv = int(lo-'A')+10 }
			if hv >= 0 && lv >= 0 {
				strings.write_byte(&b, u8(hv*16+lv))
				i += 3
				continue
			}
		}
		strings.write_byte(&b, frag[i])
		i += 1
	}
	return strings.clone(strings.to_string(b))
}

// Document y of a fragment id (first match), or -1 when absent.
// Empty fragment (or "top") means the top (y=0, found=true).
fragment_target_y :: proc(page: ^Page, frag: string) -> (int, bool) {
	if len(frag) == 0 || frag == "top" {
		return 0, true
	}
	dec := fragment_decode(frag)
	defer delete(dec)
	for &t in page.targets {
		if t.id == dec {
			return t.y, true
		}
	}
	return -1, false
}

// Scroll to the current page URL's fragment (after a fresh navigate).
// Unknown fragments show a status and leave position unchanged.
tui_scroll_to_fragment :: proc(t: ^Tui) {
	hash := strings.index_byte(t.sess.page.url, '#')
	frag := ""
	if hash >= 0 {
		frag = t.sess.page.url[hash+1:]
	}
	y, found := fragment_target_y(&t.sess.page, frag)
	if !found {
		tui_status(t, "no such anchor")
		return
	}
	t.scroll_y = max(y - 8, 0)
	tui_clamp_scroll(t)
}
