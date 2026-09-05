package vixen

// Find-in-page: live search over document prose (field boxes excluded),
// line-level highlights, n/N cycling. Query persists across relayout
// (matches recomputed); new pages clear it. Caret is not shown in the
// chrome bar (same limitation as the URL bar).

import "core:fmt"
import "core:strings"

import edit "core:text/edit"

Find_Match :: struct {
	line: int, // index into page.lines
	lo:   int, // byte offset in line.text (kept for future substring boxes)
	hi:   int,
}

// ASCII case-insensitive substring search (no allocation, byte offsets
// stable). Non-ASCII bytes compare exactly.
find_occurrences :: proc(hay, needle: string, line: int, out: ^[dynamic]Find_Match) {
	if len(needle) == 0 || len(hay) < len(needle) {
		return
	}
	i := 0
	for i + len(needle) <= len(hay) {
		match := true
		for j in 0 ..< len(needle) {
			a, b := hay[i+j], needle[j]
			if a == b {
				continue
			}
			// ASCII-fold only; bytes >= 128 must match exactly.
			if a < 128 && b < 128 {
				la := a + 32 if a >= 'A' && a <= 'Z' else a
				lb := b + 32 if b >= 'A' && b <= 'Z' else b
				if la != lb {
					match = false
					break
				}
			} else {
				match = false
				break
			}
		}
		if match {
			append(out, Find_Match{line, i, i+len(needle)})
			i += max(len(needle), 1)
		} else {
			i += 1
		}
	}
}

tui_find_is_field_line :: proc(t: ^Tui, line: int) -> bool {
	for &f in t.sess.page.fields {
		if f.line == line {
			return true
		}
	}
	return false
}

// Recompute matches for the current query; jump to the first (or keep the
// given index when relayouting). Marks page+chrome dirty when changed.
tui_find_update :: proc(t: ^Tui, keep_current := -1) {
	clear(&t.find_matches)
	t.find_current = -1
	query := strings.to_string(t.find_build)
	if len(query) == 0 {
		t.page_dirty, t.chrome_dirty = true, true
		return
	}
	for &ln, i in t.sess.page.lines {
		if tui_find_is_field_line(t, i) {
			continue
		}
		find_occurrences(ln.text, query, i, &t.find_matches)
	}
	if len(t.find_matches) > 0 {
		t.find_current = 0
		if keep_current >= 0 && keep_current < len(t.find_matches) {
			t.find_current = keep_current
		}
		tui_find_jump(t)
	}
	t.page_dirty, t.chrome_dirty = true, true
}

tui_find_jump :: proc(t: ^Tui) {
	if t.find_current < 0 || t.find_current >= len(t.find_matches) {
		return
	}
	tui_ensure_line_visible(t, t.find_matches[t.find_current].line)
}

tui_ensure_line_visible :: proc(t: ^Tui, line_idx: int) {
	if line_idx < 0 || line_idx >= len(t.sess.page.lines) {
		return
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

tui_find_next :: proc(t: ^Tui, dir: int) {
	if len(t.find_matches) == 0 {
		tui_status(t, "no find matches")
		return
	}
	n := len(t.find_matches)
	t.find_current = (t.find_current + dir + n) % n
	tui_find_jump(t)
	q := strings.to_string(t.find_build)
	tui_status(t, fmt.tprintf("%d/%d %s", t.find_current+1, n, q))
	t.page_dirty, t.chrome_dirty = true, true
}

tui_find_start :: proc(t: ^Tui) {
	t.find_active = true
	t.chrome_dirty = true
	strings.builder_reset(&t.find_build)
	edit.setup_once(&t.find_state, &t.find_build)
	t.find_state.selection = {0, 0}
	clear(&t.find_matches)
	t.find_current = -1
}

tui_find_close_keep :: proc(t: ^Tui) {
	t.find_active = false
	t.chrome_dirty = true
	t.page_dirty = true // highlight set unchanged, but chrome flips to help
}

tui_find_clear :: proc(t: ^Tui) {
	t.find_active = false
	strings.builder_reset(&t.find_build)
	t.find_state.selection = {0, 0}
	clear(&t.find_matches)
	t.find_current = -1
	t.page_dirty, t.chrome_dirty = true, true
}

// Keys while the find bar has focus.
tui_find_key :: proc(t: ^Tui, k: Term_Key) {
	switch v in k {
	case rune:
		if v >= 32 && v != 127 {
			edit.input_rune(&t.find_state, v)
			tui_find_update(t)
		}
	case Key_Special:
		switch v {
		case .Enter:
			tui_find_close_keep(t)
		case .Esc:
			tui_find_clear(t)
		case .Backspace:
			edit.delete_to(&t.find_state, .Left)
			tui_find_update(t)
		case .Left:
			edit.move_to(&t.find_state, .Left)
			t.chrome_dirty = true
		case .Right:
			edit.move_to(&t.find_state, .Right)
			t.chrome_dirty = true
		case .Up:
			tui_find_next(t, -1)
		case .Down, .Tab:
			tui_find_next(t, 1)
		case .ShiftTab:
			tui_find_next(t, -1)
		case .CtrlC, .CtrlD, .Unknown:
		}
	}
}

// Line-level highlights for visible matches (current brighter). Paints
// before the field overlay so stale field-line text never shows through;
// find skips field lines entirely. Re-blits existing glyphs over the fill.
tui_draw_find :: proc(t: ^Tui, sw, sh: int) {
	if len(t.find_matches) == 0 || sw <= 0 || sh <= 0 {
		return
	}
	// Group current line for precedence (single pass, matches in line order).
	cur_line := -1
	if t.find_current >= 0 && t.find_current < len(t.find_matches) {
		cur_line = t.find_matches[t.find_current].line
	}
	done: [dynamic]int
	defer delete(done)
	for &m in t.find_matches {
		skip := false
		for d in done {
			if d == m.line {
				skip = true
				break
			}
		}
		if skip {
			continue
		}
		append(&done, m.line)
		if m.line < 0 || m.line >= len(t.sess.page.lines) {
			continue
		}
		ln := &t.sess.page.lines[m.line]
		top := int(ln.baseline - ln.height * 0.8) - t.scroll_y
		bot := int(ln.baseline + ln.height * 0.2) - t.scroll_y
		if bot <= 0 || top >= sh {
			continue
		}
		ctop, cbot := max(top, 0), min(bot, sh)
		bg := [3]u8{40, 80, 40}
		if m.line == cur_line {
			bg = [3]u8{160, 100, 30}
		}
		for yy in ctop ..< cbot {
			for xx in 0 ..< sw {
				o := (yy * sw + xx) * 4
				t.slice[o + 0], t.slice[o + 1], t.slice[o + 2] = bg[0], bg[1], bg[2]
			}
		}
		for p in ln.glyphs {
			blit_glyph_px(t.slice, sw, sh, &t.sess.bank.fonts[p.font],
				p.gid, p.x + p.off_x, ln.baseline - f32(t.scroll_y) + p.off_y,
				[4]u8{232, 232, 238, 255})
		}
	}
}
