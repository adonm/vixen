package spike

// Interactive TUI: keyboard-driven browsing over a live session.
// Graphical driver (Kitty PNG slices) when detected, text driver otherwise.
// Headless operation (piped stdout): use `browse --dump` instead; the loop
// requires a terminal and refuses to start without one.

import "core:fmt"
import "core:os"
import "core:strings"
import "core:terminal"
import "core:unicode/utf8"

import "core:sys/linux"

import edit "core:text/edit"

Tui :: struct {
	sess:       ^Browse_Session,
	scroll_y:   int, // px offset (graphical) 
	scroll_ln:  int, // line offset (text)
	kitty:      bool,
	cols, rows: int,
	cell_w, cell_h: int,
	slice:      []u8, // reused viewport RGBA buffer
	slice_w, slice_h: int,
	inbuf:      [dynamic]u8, // all terminal input flows through here
	hint:       [dynamic]u8, // typed hint digits
	url_active: bool,
	show_list:  bool, // text-mode link list overlay
	url_state:  edit.State,
	url_build:  strings.Builder,
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
}

tui_status :: proc(t: ^Tui, msg: string) {
	delete(t.status)
	t.status = strings.clone(msg)
}

tui_frame_geom :: proc(t: ^Tui) {
	t.cols, t.rows, _ = term_size()
	if t.cols < 20 {
		t.cols = 20
	}
	if t.rows < 10 {
		t.rows = 10
	}
}

tui_clamp_scroll :: proc(t: ^Tui) {
	fr_h := t.sess.page.height
	view_h := (t.rows - 2) * t.cell_h
	max_y := max(fr_h - view_h, 0)
	t.scroll_y = clamp(t.scroll_y, 0, max_y)
	n := len(t.sess.page.text)
	max_ln := max(n - (t.rows - 2), 0)
	t.scroll_ln = clamp(t.scroll_ln, 0, max_ln)
}

// Collect visible link indices for the current viewport.
tui_visible_links :: proc(t: ^Tui) {
	clear(&t.vis)
	if t.kitty {
		y0 := f32(t.scroll_y)
		y1 := f32(t.scroll_y + (t.rows - 2) * t.cell_h)
		for l, i in t.sess.page.links {
			if l.y1 >= y0 && l.y0 <= y1 {
				append(&t.vis, i)
			}
		}
	} else {
		for i in 0 ..< len(t.sess.page.links) {
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

// Caret bar for the focused text field, drawn into the viewport slice.
// Prefix is re-shaped, so the caret can sit slightly off inside ligatures.
tui_draw_cursor :: proc(t: ^Tui, sw, sh: int) {
	if t.focus < 0 || t.focus >= len(t.field_edits) {
		return
	}
	fe := &t.field_edits[t.focus]
	f := &t.sess.page.fields[fe.field]
	if f.kind != .text || f.line < 0 || f.line >= len(t.sess.page.lines) {
		return
	}
	ln := &t.sess.page.lines[f.line]
	caret := clamp(fe.state.selection[0], 0, len(f.value))
	rc := Render_Ctx{bank = &t.sess.bank}
	probe: Cur_Line
	w := shape_word(&rc, f.value[:caret], f.px, &probe)
	for wd in probe.words {
		delete(wd)
	}
	delete(probe.glyphs)
	delete(probe.words)
	cx := int(f.x0 + w + 0.5)
	top := int(ln.baseline - f.px + 0.5) - t.scroll_y
	bot := int(ln.baseline + 2 + 0.5) - t.scroll_y
	for yy in max(top, 0) ..< min(bot, sh) {
		for xx in max(cx, 0) ..< min(cx + 2, sw) {
			o := (yy * sw + xx) * 4
			t.slice[o + 0], t.slice[o + 1], t.slice[o + 2] = 255, 255, 255
		}
	}
}

// Number entry shared by both drivers: digits accumulate, Enter follows.
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
	t.scroll_y = 0
	t.scroll_ln = 0
	if !browse_navigate(t.sess, url, true) {
		tui_status(t, "navigation failed")
		return
	}
	tui_sync_fields(t)
}

tui_draw_graphical :: proc(t: ^Tui) {
	sess := t.sess
	view_h := (t.rows - 2) * t.cell_h
	if view_h < t.cell_h {
		view_h = t.cell_h
	}
	tui_clamp_scroll(t)
	sw := min(sess.page.width, t.cols * t.cell_w)
	sh := min(view_h, max(sess.page.height - t.scroll_y, 0))
	if sh <= 0 || sw <= 0 {
		return
	}
	// Rasterize only the viewport slice from retained lines.
	vfr := raster_slice(&sess.bank, sess.page.lines, sess.page.placements[:], sess.page.images[:], sess.page.width, t.scroll_y, sh)
	defer delete(vfr.px)
	if len(t.slice) != sw*sh*4 || t.slice_w != sw || t.slice_h != sh {
		delete(t.slice)
		t.slice = make([]u8, sw*sh*4)
		t.slice_w, t.slice_h = sw, sh
	}
	for row in 0 ..< sh {
		copy(t.slice[row*sw*4:], vfr.px[row*vfr.w*4:][:sw*4])
	}
	tui_visible_links(t)
	tui_draw_hints(t, sw, sh)
	tui_draw_cursor(t, sw, sh)
	png, ok := frame_encode_slice(t.slice, sw, sh)
	if !ok {
		return
	}
	defer delete(png)
	term_move(1, 1)
	kitty_transmit_png(png, sw, sh)
	// Status bar below the image; clear stale rows under it.
	imgrows := (sh + t.cell_h - 1) / t.cell_h
	term_move(1, imgrows + 2)
	tui_status_line(t)
	fmt.print("\x1b[J")
}

tui_status_line :: proc(t: ^Tui) {
	sess := t.sess
	pct := 0
	if sess.page.height > 0 {
		pct = 100 * t.scroll_y / max(sess.page.height, 1)
	}
	msg := t.status
	fmt.printf("\x1b[7m %-60.60s %3d%% links=%d \x1b[0m",
		sess.page.url, pct, len(sess.page.links))
	// NOTE: hint digits print inline (no tprintf): tprintf is
	// temp-allocator backed and must NEVER be delete()d — freeing temp
	// memory corrupts the heap (this segfaulted the TUI).
	if len(t.hint) > 0 {
		fmt.printf(" link: %s", string(t.hint[:]))
	} else if len(msg) > 0 {
		fmt.printf(" %s", msg)
	}
	if t.url_active {
		fmt.printf("\nURL: %s\x1b[K", strings.to_string(t.url_build))
	} else {
		fmt.printf("\n[q]uit [u]rl [b]ack [f]wd [r]eload [l]inks [tab]field\x1b[K")
	}
}

tui_draw_text :: proc(t: ^Tui) {
	sess := t.sess
	tui_clamp_scroll(t)
	fline, fcaret, has_focus := tui_focused_line(t)
	n := len(sess.page.text)
	rows := t.rows - 2
	for i in 0 ..< rows {
		li := t.scroll_ln + i
		if li < n {
			line := sess.page.text[li]
			if has_focus && fline == li {
				// Caret overlay on a temp copy; never mutates page text.
				c := clamp(fcaret, 0, len(line))
				line = strings.concatenate([]string{line[:c], "|", line[c:]}, context.temp_allocator)
			}
			if len(line) > t.cols {
				line = line[:t.cols]
			}
			fmt.println(line)
		} else {
			fmt.println("~")
		}
	}
	tui_status_line(t)
	fmt.println()
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
	t.scroll_y = 0
	t.scroll_ln = 0
	clear(&t.hint)
	if !browse_navigate(t.sess, url, true) {
		tui_status(t, "navigation failed")
		return
	}
	tui_sync_fields(t)
}

// Rebuild per-field edit states after navigation; drops focus. Call after
// every successful browse_* that replaces the page.
tui_sync_fields :: proc(t: ^Tui) {
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
	delete(f.value)
	f.value = strings.clone(strings.to_string(fe.build))
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
			if !browse_submit(t.sess, idx) {
				if len(t.sess.page.fields[idx].action) == 0 {
					tui_status(t, "not in a form")
				} else {
					tui_status(t, "submission failed")
				}
			} else {
				t.scroll_y, t.scroll_ln = 0, 0
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
			if t.kitty {
				t.scroll_y += -40 if v == .Up else 40
			} else {
				t.scroll_ln += -1 if v == .Up else 1
			}
			tui_clamp_scroll(t)
		case .CtrlC, .CtrlD:
			return false
		case .Unknown:
		}
	}
	return true
}

tui_handle_key :: proc(t: ^Tui, k: Term_Key) -> bool {
	// Returns false to quit.
	if t.url_active {
		tui_url_key(t, k)
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
			if t.kitty {
				t.scroll_y += 40
			} else {
				t.scroll_ln += 1
			}
		case 'k':
			if t.kitty {
				t.scroll_y -= 40
			} else {
				t.scroll_ln -= 1
			}
		case ' ':
			if t.kitty {
				t.scroll_y += (t.rows - 2) * t.cell_h
			} else {
				t.scroll_ln += t.rows - 2
			}
		case 'b':
			if !browse_back(t.sess) {
				tui_status(t, "no back history")
			} else {
				t.scroll_y, t.scroll_ln = 0, 0
				tui_sync_fields(t)
			}
		case 'f':
			if !browse_forward(t.sess) {
				tui_status(t, "no forward history")
			} else {
				t.scroll_y, t.scroll_ln = 0, 0
				tui_sync_fields(t)
			}
		case 'r':
			browse_reload(t.sess)
			tui_sync_fields(t)
		case 'u':
			t.url_active = true
			strings.builder_reset(&t.url_build)
			edit.setup_once(&t.url_state, &t.url_build)
		case 'g':
			t.scroll_y, t.scroll_ln = 0, 0
		case 'G':
			t.scroll_y = 1 << 30
			t.scroll_ln = 1 << 30
		case 'l':
			if !t.kitty {
				t.show_list = !t.show_list
			}
		case '0', '1', '2', '3', '4', '5', '6', '7', '8', '9':
			append(&t.hint, u8(v))
		}
	case Key_Special:
		switch v {
		case .Up:
			if t.kitty {
				t.scroll_y -= 40
			} else {
				t.scroll_ln -= 1
			}
		case .Down:
			if t.kitty {
				t.scroll_y += 40
			} else {
				t.scroll_ln += 1
			}
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
			if !browse_back(t.sess) {
				tui_status(t, "no back history")
			} else {
				t.scroll_y, t.scroll_ln = 0, 0
				tui_sync_fields(t)
			}
		case .Left, .Right, .Unknown:
			clear(&t.hint)
		}
	}
	tui_clamp_scroll(t)
	return true
}

// Fill the input buffer (blocking if timeout_ms < 0).
tui_fill :: proc(t: ^Tui, timeout_ms: int) -> bool {
	if timeout_ms < 0 {
		b: [64]u8
		m, err := os.read(os.stdin, b[:])
		if err != nil || m <= 0 {
			return false
		}
		append(&t.inbuf, ..b[:m])
		return true
	}
	pfd := linux.Poll_Fd{fd = 0, events = {.IN}}
	pfds := [1]linux.Poll_Fd{pfd}
	n, _ := linux.poll(pfds[:], i32(timeout_ms))
	if n <= 0 {
		return false
	}
	b: [64]u8
	m, err := os.read(os.stdin, b[:])
	if err != nil || m <= 0 {
		return false
	}
	append(&t.inbuf, ..b[:m])
	return true
}

// Scan the buffer for a "CSI kind ; a ; b <final>" reply; consume through
// the final byte, leaving any trailing typeahead in place.
tui_csi_reply :: proc(t: ^Tui, kind: byte, timeout_ms: int) -> (a, b: int, ok: bool) {
	deadline := timeout_ms
	for {
		s := string(t.inbuf[:])
		if i := strings.index_byte(s, '\x1b'); i >= 0 {
			if j := strings.index_byte(s[i:], '['); j >= 0 {
				rest := s[i+1+j:]
				parts := strings.split(rest, ";", context.temp_allocator)
				if len(parts) >= 3 && len(parts[0]) == 1 && parts[0][0] == kind {
					k := 0
					for k < len(rest) && !(rest[k] >= 'A' || rest[k] == '~') {
						if (rest[k] < '0' || rest[k] > '9') && rest[k] != ';' && rest[k] != ' ' && rest[k] != '?' && rest[k] != ':' {
							break
						}
						k += 1
					}
					consume := (i + 1 + j) + k + 1
					if consume <= len(t.inbuf) {
						a = parse_int_or(strings.trim_space(parts[1]), -1)
						b = parse_int_or(strings.trim_space(parts[2]), -1)
						inbuf_consume(t, consume)
						return a, b, a >= 0 && b >= 0
					}
					return 0, 0, false
				}
			}
		}
		if deadline <= 0 {
			return 0, 0, false
		}
		step := min(deadline, 50)
		tui_fill(t, step)
		deadline -= step
	}
}

tui_cell_size :: proc(t: ^Tui) {
	os.write_string(os.stdout, "\x1b[14t\x1b[18t")
	pw, ph, ok1 := tui_csi_reply(t, '4', 300)
	cr, cc, ok2 := tui_csi_reply(t, '8', 300)
	if !ok1 || !ok2 || cr == 0 || cc == 0 {
		t.cell_w, t.cell_h = 8, 16
		return
	}
	t.cell_w, t.cell_h = pw / cc, ph / cr
	if t.cell_w <= 0 {
		t.cell_w = 8
	}
	if t.cell_h <= 0 {
		t.cell_h = 16
	}
}

// Drop the first n input bytes, preserving the rest.
inbuf_consume :: proc(t: ^Tui, n: int) {
	if n <= 0 {
		return
	}
	if n >= len(t.inbuf) {
		clear(&t.inbuf)
		return
	}
	copy(t.inbuf[:], t.inbuf[n:])
	resize(&t.inbuf, len(t.inbuf) - n)
}
tui_read_key :: proc(t: ^Tui) -> Term_Key {
	for len(t.inbuf) == 0 {
		if !tui_fill(t, -1) {
			return Key_Special.Unknown
		}
	}
	if t.inbuf[0] != 0x1b {
		b0 := t.inbuf[0]
		ordered_remove(&t.inbuf, 0)
		switch b0 {
		case '\r', '\n':
			return Key_Special.Enter
		case 127:
			return Key_Special.Backspace
		case '\t':
			return Key_Special.Tab
		case 3:
			return Key_Special.CtrlC
		case 4:
			return Key_Special.CtrlD
		}
		// ASCII fast path (control keys handled above).
		if b0 < 0x80 {
			return rune(b0)
		}
		// Multi-byte lead: gather the full sequence, waiting briefly.
		for len(t.inbuf) < 4 {
			sz := utf8.rune_size(rune(t.inbuf[0]))
			if len(t.inbuf) >= sz {
				break
			}
			if !tui_fill(t, 25) {
				break
			}
		}
		r, _ := utf8.decode_rune(string(t.inbuf[:]))
		inbuf_consume(t, utf8.rune_size(r))
		return r
	}
	// ESC: need follow-up bytes; wait briefly.
	ordered_remove(&t.inbuf, 0)
	tui_fill(t, 25)
	if len(t.inbuf) == 0 {
		return Key_Special.Esc
	}
	if t.inbuf[0] == '[' {
		// CSI: consume '[' + final byte (params ignored for arrows).
		for len(t.inbuf) < 2 {
			if !tui_fill(t, 25) {
				break
			}
		}
		if len(t.inbuf) < 2 {
			return Key_Special.Unknown
		}
		fin := t.inbuf[1]
		inbuf_consume(t, 2)
		switch fin {
		case 'A':
			return Key_Special.Up
		case 'B':
			return Key_Special.Down
		case 'C':
			return Key_Special.Right
		case 'D':
			return Key_Special.Left
		}
		return Key_Special.Unknown
	}
	// Alt+key and other escapes: report Unknown, keep the byte.
	return Key_Special.Unknown
}

// Main interactive loop. Caller owns sess; blocks until quit.
tui_loop :: proc(sess: ^Browse_Session, start_url: string) {
	if !terminal.is_terminal(os.stdin) || !terminal.is_terminal(os.stdout) {
		fmt.eprintln("tui: refusing interactive loop on non-terminal (use browse --dump)")
		return
	}
	saved, ok := term_raw_enable()
	if !ok {
		fmt.eprintln("tui: raw mode failed")
		return
	}
	defer term_setattr(&saved)
	term_enter_alt()
	defer term_exit_alt()
	t: Tui
	t.sess = sess
	t.kitty = kitty_env_supported()
	tui_cell_size(&t)
	if t.cell_w <= 0 {
		t.cell_w = 8
	}
	if t.cell_h <= 0 {
		t.cell_h = 16
	}
	t.status = strings.clone("")
	defer delete(t.status)
	defer delete(t.slice)
	defer delete(t.hint)
	defer delete(t.vis)
	t.focus = -1 // zero value 0 is a valid field index
	defer {
		for &fe in t.field_edits {
			edit.destroy(&fe.state)
			strings.builder_destroy(&fe.build)
		}
		delete(t.field_edits)
	}
	edit.init(&t.url_state, context.allocator, context.allocator)
	defer edit.destroy(&t.url_state)
	strings.builder_init(&t.url_build)
	defer strings.builder_destroy(&t.url_build)
	if !browse_navigate(sess, start_url, true) {
		tui_status(&t, "initial navigation failed")
	}
	tui_sync_fields(&t)
	for {
		tui_frame_geom(&t)
		if t.show_list && !t.kitty {
			tui_draw_link_list(&t)
		} else if t.kitty {
			tui_draw_graphical(&t)
		} else {
			tui_draw_text(&t)
		}
		if !tui_handle_key(&t, tui_read_key(&t)) {
			break
		}
	}
}

// Text-mode link list overlay.
tui_draw_link_list :: proc(t: ^Tui) {
	tui_visible_links(t)
	fmt.println("links (number+enter, l to close):")
	for n in t.vis {
		l := &t.sess.page.links[n]
		fmt.printfln("  %d. %s", n + 1, l.url)
	}
}
