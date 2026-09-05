package vixen

// Interactive TUI: keyboard-driven browsing over a live session.
// Kitty graphics is required; headless text is an explicit separate mode.
// Headless operation (piped stdout): use `browse --dump` instead; the loop
// requires a terminal and refuses to start without one.

import "core:fmt"
import "core:strings"
import "core:time"

import edit "core:text/edit"

Tui :: struct {
	sess:       ^Browse_Session,
	scroll_y:   int, // px viewport offset
	cols, rows: int,
	cell_w, cell_h: int,
	slice:      []u8, // reused viewport RGBA buffer
	slice_w, slice_h: int,
	inbuf:      [dynamic]u8, // all terminal input flows through here
	input_at:   time.Time,
	metrics_at: time.Time,
	metrics_ready: bool,
	pasting:    bool,
	page_dirty, chrome_dirty: bool,
	hint:       [dynamic]u8, // typed hint digits
	url_active: bool,
	url_state:  edit.State,
	url_build:  strings.Builder,
	find_active: bool,
	find_state:  edit.State,
	find_build:  strings.Builder,
	find_matches: [dynamic]Find_Match,
	find_current: int, // index into matches (-1 = none)
	focus:      int, // focused field edit index (-1 = none)
	field_edits: [dynamic]Field_Edit,
	status:     string, // owned transient message
	// Visible links this frame (indices into sess.page.links).
	vis:        [dynamic]int,
}

// Per-field edit state, rebuilt on every navigation (values mirror into
// sess.page.fields per keystroke so submit reads current text).
Field_Edit :: struct {
	field: int, // index into sess.page.fields
	state: edit.State,
	build: strings.Builder,
	xoff:  int, // horizontal scroll offset, px (keeps caret visible)
}

tui_status :: proc(t: ^Tui, msg: string) {
	if t.status == msg { return }
	delete(t.status)
	t.status = strings.clone(msg)
	t.chrome_dirty = true
}

tui_view_height :: proc(t: ^Tui) -> int { return max(t.rows - 2, 0) * t.cell_h }

tui_clamp_scroll :: proc(t: ^Tui) {
	fr_h := t.sess.page.height
	view_h := tui_view_height(t)
	max_y := max(fr_h - view_h, 0)
	t.scroll_y = clamp(t.scroll_y, 0, max_y)
}

// Collect visible link indices for the current viewport.
tui_visible_links :: proc(t: ^Tui) {
	clear(&t.vis)
	y0 := f32(t.scroll_y)
	y1 := f32(t.scroll_y + tui_view_height(t))
	for l, i in t.sess.page.links {
		if l.y1 >= y0 && l.y0 <= y1 {
			append(&t.vis, i)
		}
	}
}

// Draw numbered hint boxes for visible links into the slice buffer.
tui_draw_hints :: proc(t: ^Tui, sw, sh: int) {
	rc: Render_Ctx
	rc.bank = &t.sess.bank
	for n, hi in t.vis {
		l := &t.sess.page.links[n]
		bx := int(l.x0)
		by := int(l.y0) - t.scroll_y - 18
		if by < 0 {
			by = 0
		}
		// Stack buffer: no allocation, nothing to free (tprintf results
		// are temp-allocator backed and must never be delete()d).
		lbuf: [8]byte
		label := fmt.bprintf(lbuf[:], "%d", hi + 1)
		bw := len(label) * 9 + 6
		bh := 17
		for yy in by ..< min(by + bh, sh) {
			for xx in bx ..< min(bx + bw, sw) {
				if xx < 0 || yy < 0 {
					continue
				}
				o := (yy * sw + xx) * 4
				t.slice[o + 0], t.slice[o + 1], t.slice[o + 2] = 40, 40, 60
			}
		}
		// Digit glyphs via the shared blitter.
		line: Cur_Line
		shape_word(&rc, label, 13, &line)
		for p in line.glyphs {
			blit_glyph_px(t.slice, sw, sh, &t.sess.bank.fonts[p.font],
				p.gid, f32(bx + 3) + p.x, f32(by + 14), [4]u8{255, 220, 130, 255})
		}
		for w in line.words {
			delete(w)
		}
		delete(line.glyphs)
		delete(line.words)
	}
}

// Digits accumulate, Enter follows a visible link.
tui_follow_hint :: proc(t: ^Tui) {
	if len(t.hint) == 0 {
		return
	}
	n := 0
	for d in t.hint {
		n = n * 10 + int(d - '0')
	}
	clear(&t.hint)
	if n < 1 || n > len(t.vis) {
		tui_status(t, "no such link")
		return
	}
	url := t.sess.page.links[t.vis[n-1]].url
	tui_save_anchor(t)
	if !browse_navigate(t.sess, url, true) {
		tui_status(t, "navigation failed")
		return
	}
	if !t.sess.page_rebuilt {
		tui_scroll_to_fragment(t)
		t.page_dirty, t.chrome_dirty = true, true
		return
	}
	if strings.index_byte(t.sess.page.url, '#') >= 0 {
		tui_scroll_to_fragment(t)
	} else {
		t.scroll_y = 0
	}
	tui_sync_fields(t)
}

tui_go_back :: proc(t: ^Tui) -> bool {
	tui_save_anchor(t)
	if !browse_back(t.sess) {
		return false
	}
	tui_restore_anchor(t)
	if t.sess.page_rebuilt {
		tui_sync_fields(t)
	} else {
		t.page_dirty, t.chrome_dirty = true, true
	}
	return true
}

tui_go_forward :: proc(t: ^Tui) -> bool {
	tui_save_anchor(t)
	if !browse_forward(t.sess) {
		return false
	}
	tui_restore_anchor(t)
	if t.sess.page_rebuilt {
		tui_sync_fields(t)
	} else {
		t.page_dirty, t.chrome_dirty = true, true
	}
	return true
}

tui_reload :: proc(t: ^Tui) {
	tui_save_anchor(t)
	browse_reload(t.sess)
	tui_restore_anchor(t)
	tui_sync_fields(t)
}

tui_draw :: proc(t: ^Tui) -> bool {
	if !t.page_dirty && !t.chrome_dirty { return true }
	sess := t.sess
	tui_clamp_scroll(t)
	if t.page_dirty && tui_drawable(t) {
		sw, sh := t.cols*t.cell_w, tui_view_height(t)
		// Keep one viewport buffer, not a second full-sized copy.
		delete(t.slice)
		t.slice = nil
		vfr := raster_slice(&sess.bank, sess.page.lines, sess.page.placements[:], sess.page.images[:], sw, t.scroll_y, sh)
		t.slice = vfr.px
		t.slice_w, t.slice_h = sw, sh
		tui_visible_links(t)
		tui_draw_find(t, sw, sh)
		tui_draw_fields(t, sw, sh)
		tui_draw_hints(t, sw, sh)
		png, ok := frame_encode_slice(t.slice, sw, sh)
		if !ok { return false }
		defer delete(png)
		term_move(1, 1)
		if !kitty_transmit_png(png, sw, sh, TUI_IMAGE_ID, t.cols, t.rows-2) { return false }
	} else if t.page_dirty {
		if !kitty_delete_viewport() { return false }
		delete(t.slice)
		t.slice = nil
		clear(&t.vis)
	}
	if t.chrome_dirty { tui_status_line(t) }
	t.page_dirty, t.chrome_dirty = false, false
	return true
}

tui_status_line :: proc(t: ^Tui) {
	if t.cols <= 0 || t.rows <= 0 { return }
	pct := 100 * t.scroll_y / max(t.sess.page.height - tui_view_height(t), 1)
	message := t.status
	if len(t.hint) > 0 { message = fmt.tprintf("link: %s", string(t.hint[:])) }
	line := fmt.tprintf("%d%% %s %s", pct, message, t.sess.page.url)
	if !tui_drawable(t) { line = "Viewport too small or too large; resize or q to quit" }
	tui_chrome_row(t, max(t.rows-1, 1), line, true)
	if t.rows < 2 { return }
	line = "[q]uit [u]rl [/]find [b]ack [f]wd [r]eload [tab]field [n]ext"
	if t.url_active { line = fmt.tprintf("URL: %s", strings.to_string(t.url_build)) }
	else if t.find_active {
		q := strings.to_string(t.find_build)
		if len(t.find_matches) > 0 {
			line = fmt.tprintf("/%s %d/%d", q, t.find_current+1, len(t.find_matches))
		} else if len(q) > 0 {
			line = fmt.tprintf("/%s no matches", q)
		} else {
			line = "/"
		}
	}
	tui_chrome_row(t, t.rows, line, false)
}


tui_url_key :: proc(t: ^Tui, k: Term_Key) -> bool {
	// Returns true when the bar closes (commit or cancel).
	// NOTE: no edit.begin/end here — begin resets the selection to
	// {len, 0} (select-all), which would wipe the bar on every keystroke.
	// The bar is initialized once via setup_once when opened.
	switch v in k {
	case rune:
		if v >= 32 && v != 127 {
			edit.input_rune(&t.url_state, v)
		}
	case Key_Special:
		switch v {
		case .Enter:
			url := strings.clone(strings.to_string(t.url_build))
			defer delete(url)
			t.url_active = false
			if len(strings.trim_space(url)) > 0 {
				tui_navigate_bar(t, url)
			}
			return true
		case .Esc:
			t.url_active = false
			return true
		case .Backspace:
			edit.delete_to(&t.url_state, .Left)
		case .Left:
			edit.move_to(&t.url_state, .Left)
		case .Right:
			edit.move_to(&t.url_state, .Right)
		case .Up, .Down, .Tab, .ShiftTab, .CtrlC, .CtrlD, .Unknown:
		}
	}
	return false
}

tui_navigate_bar :: proc(t: ^Tui, text: string) {
	url := strings.trim_space(text)
	if !strings.contains(url, "://") {
		url = strings.concatenate([]string{"https://", url}, context.temp_allocator)
	}
	tui_save_anchor(t)
	clear(&t.hint)
	if !browse_navigate(t.sess, url, true) {
		tui_status(t, "navigation failed")
		return
	}
	if !t.sess.page_rebuilt {
		tui_scroll_to_fragment(t)
		t.page_dirty, t.chrome_dirty = true, true
		return
	}
	if strings.index_byte(t.sess.page.url, '#') >= 0 {
		tui_scroll_to_fragment(t)
	} else {
		t.scroll_y = 0
	}
	tui_sync_fields(t)
}

// Rebuild per-field edit states after navigation; drops focus and find.
// Call after every successful browse_* that replaces the page.
tui_sync_fields :: proc(t: ^Tui) {
	t.page_dirty, t.chrome_dirty = true, true
	t.find_active = false
	strings.builder_reset(&t.find_build)
	t.find_state.selection = {0, 0}
	clear(&t.find_matches)
	t.find_current = -1
	for &fe in t.field_edits {
		edit.destroy(&fe.state)
		strings.builder_destroy(&fe.build)
	}
	clear(&t.field_edits)
	t.focus = -1
	for &f, i in t.sess.page.fields {
		if f.kind == .hidden {
			continue
		}
		fe: Field_Edit
		fe.field = i
		edit.init(&fe.state, context.allocator, context.allocator)
		strings.builder_init(&fe.build)
		strings.write_string(&fe.build, f.value)
		edit.setup_once(&fe.state, &fe.build)
		// Collapse: setup_once selects all ({len,0}); caret goes to end.
		n := len(fe.build.buf)
		fe.state.selection = {n, n}
		append(&t.field_edits, fe)
	}
	// Rebind: setup_once pointed each state at the loop-local Builder;
	// append copies the struct, and growth may move elements. Every
	// state must point at its own array element's builder.
	for &e in t.field_edits {
		e.state.builder = &e.build
	}
}

// Mirror the focused field's builder back into the page field value.
tui_field_sync :: proc(t: ^Tui) {
	fe := &t.field_edits[t.focus]
	f := &t.sess.page.fields[fe.field]
	if f.value == strings.to_string(fe.build) { return }
	delete(f.value)
	f.value = strings.clone(strings.to_string(fe.build))
	t.page_dirty = true
}

tui_focus_move :: proc(t: ^Tui, dir: int) {
	n := len(t.field_edits)
	if n == 0 {
		tui_status(t, "no fields")
		return
	}
	if t.focus < 0 {
		t.focus = 0 if dir > 0 else n - 1
	} else {
		t.focus = (t.focus + dir + n) % n
	}
	f := &t.sess.page.fields[t.field_edits[t.focus].field]
	if len(f.name) > 0 {
		tui_status(t, fmt.tprintf("field %s", f.name))
	} else {
		tui_status(t, fmt.tprintf("%s field", "text" if f.kind == .text else "button"))
	}
	tui_ensure_field_visible(t)
}

// Focused field's line + caret byte offset for cursor display.
tui_focused_line :: proc(t: ^Tui) -> (line, caret: int, ok: bool) {
	if t.focus < 0 || t.focus >= len(t.field_edits) {
		return 0, 0, false
	}
	fe := &t.field_edits[t.focus]
	f := &t.sess.page.fields[fe.field]
	if f.line < 0 {
		return 0, 0, false
	}
	caret = clamp(fe.state.selection[0], 0, len(f.value))
	return f.line, caret, true
}

// Keys while a field is focused. Returns false to quit.
tui_field_key :: proc(t: ^Tui, k: Term_Key) -> bool {
	fe := &t.field_edits[t.focus]
	f := &t.sess.page.fields[fe.field]
	_ = f
	switch v in k {
	case rune:
		if v >= 32 && v != 127 {
			if t.sess.page.fields[fe.field].kind == .text {
				edit.input_rune(&fe.state, v)
				tui_field_sync(t)
			}
		}
	case Key_Special:
		switch v {
		case .Enter:
			idx := fe.field
			t.focus = -1
			tui_save_anchor(t)
			if !browse_submit(t.sess, idx) {
				if len(t.sess.page.fields[idx].action) == 0 {
					tui_status(t, "not in a form")
				} else {
					tui_status(t, "submission failed")
				}
			} else {
				t.scroll_y = 0
				tui_sync_fields(t)
			}
		case .Esc:
			t.focus = -1
		case .Backspace:
			if t.sess.page.fields[fe.field].kind == .text {
				edit.delete_to(&fe.state, .Left)
				tui_field_sync(t)
			}
		case .Left:
			edit.move_to(&fe.state, .Left)
		case .Right:
			edit.move_to(&fe.state, .Right)
		case .Tab:
			tui_focus_move(t, 1)
		case .ShiftTab:
			tui_focus_move(t, -1)
		case .Up, .Down:
			t.focus = -1
			t.scroll_y += -40 if v == .Up else 40
			tui_clamp_scroll(t)
		case .CtrlC, .CtrlD:
			return false
		case .Unknown:
		}
	}
	return true
}

tui_handle_key :: proc(t: ^Tui, k: Term_Key) -> bool {
	// Track visible changes, not merely input arrival. Navigation marks a
	// new page in sync_fields; status changes mark chrome in tui_status.
	old_y, old_focus, old_hint := t.scroll_y, t.focus, len(t.hint)
	caret: [2]int
	if t.focus >= 0 && t.focus < len(t.field_edits) { caret = t.field_edits[t.focus].state.selection }
	defer {
		if old_y != t.scroll_y || old_focus != t.focus {
			t.page_dirty, t.chrome_dirty = true, true
		}
		if old_hint != len(t.hint) { t.chrome_dirty = true }
		if t.focus >= 0 && t.focus < len(t.field_edits) &&
		   caret != t.field_edits[t.focus].state.selection { t.page_dirty = true }
	}
	if special, ok := k.(Key_Special); ok && (special == .CtrlC || special == .CtrlD) {
		return false
	}
	if t.url_active {
		tui_url_key(t, k)
		if special, ok := k.(Key_Special); !ok || special != .Unknown { t.chrome_dirty = true }
		return true
	}
	if t.find_active {
		tui_find_key(t, k)
		return true
	}
	if t.focus >= 0 {
		return tui_field_key(t, k)
	}
	switch v in k {
	case rune:
		switch v {
		case 'q':
			return false
		case 'j':
			t.scroll_y += 40
		case 'k':
			t.scroll_y -= 40
		case ' ':
			t.scroll_y += tui_view_height(t)
		case 'b':
			if !tui_go_back(t) {
				tui_status(t, "no back history")
			}
		case 'f':
			if !tui_go_forward(t) {
				tui_status(t, "no forward history")
			}
		case 'r':
			tui_reload(t)
		case 'u':
			t.url_active = true
			t.chrome_dirty = true
			strings.builder_reset(&t.url_build)
			edit.setup_once(&t.url_state, &t.url_build)
		case '/':
			tui_find_start(t)
		case 'n':
			tui_find_next(t, 1)
		case 'N':
			tui_find_next(t, -1)
		case 'g':
			t.scroll_y = 0
		case 'G':
			t.scroll_y = 1 << 30
		case '0', '1', '2', '3', '4', '5', '6', '7', '8', '9':
			if len(t.hint) < 9 { append(&t.hint, u8(v)) }
		}
	case Key_Special:
		switch v {
		case .Up:
			t.scroll_y -= 40
		case .Down:
			t.scroll_y += 40
		case .Enter:
			tui_follow_hint(t)
		case .Esc:
			clear(&t.hint)
		case .Tab:
			tui_focus_move(t, 1)
		case .ShiftTab:
			tui_focus_move(t, -1)
		case .CtrlC, .CtrlD:
			return false
		case .Backspace:
			if !tui_go_back(t) {
				tui_status(t, "no back history")
			}
		case .Left, .Right:
			clear(&t.hint)
		case .Unknown:
		}
	}
	tui_clamp_scroll(t)
	return true
}
