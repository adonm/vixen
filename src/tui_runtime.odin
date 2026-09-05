package vixen

// Event pump and validated terminal geometry. No text-driver branches.
import "core:fmt"
import "core:os"
import "core:strings"
import "core:terminal"
import "core:time"
import edit "core:text/edit"

tui_cell_metrics :: proc(t: ^Tui, cw, ch: int) {
	if cw <= 0 || ch <= 0 || cw > 256 || ch > 512 { return }
	t.metrics_ready = true
	if cw != t.cell_w || ch != t.cell_h {
		t.cell_w, t.cell_h = cw, ch
		t.page_dirty, t.chrome_dirty = true, true
	}
}

tui_frame_geom :: proc(t: ^Tui) {
	w, ok := term_winsize()
	if !ok { return }
	if t.cols != int(w.col) || t.rows != int(w.row) {
		t.cols, t.rows = int(w.col), int(w.row)
		t.metrics_ready = false
		t.metrics_at = time.now()
		t.page_dirty, t.chrome_dirty = true, true
		// Non-blocking query: replies join the same stream as keystrokes.
		os.write_string(os.stdout, "\x1b[16t\x1b[14t")
		if t.cols > 0 && t.rows > 0 { fmt.print("\x1b[2J") }
	}
	if w.col > 0 && w.row > 0 && w.xpixel > 0 && w.ypixel > 0 {
		tui_cell_metrics(t, int(w.xpixel)/int(w.col), int(w.ypixel)/int(w.row))
	}
}

tui_metrics_reply :: proc(t: ^Tui, m: Term_Metrics) {
	if m.kind == 6 { // CSI 6 ; height ; width t (cell pixels)
		tui_cell_metrics(t, m.b, m.a)
	} else if m.kind == 4 && t.cols > 0 && t.rows > 0 {
		tui_cell_metrics(t, m.b/t.cols, m.a/t.rows)
	}
	// Row/column reports never override authoritative TIOCGWINSZ.
}

tui_drawable :: proc(t: ^Tui) -> bool {
	// Bound allocations independently of the terminal's advertised size.
	w, h := t.cols * t.cell_w, tui_view_height(t)
	return w > 0 && h > 0 && w <= 4096 && h <= 4096
}

tui_input :: proc(t: ^Tui, timeout_ms: int) -> bool {
	buf: [4096]u8
	n := term_read_timeout(buf[:], timeout_ms)
	if n < 0 { return false }
	if n > 0 {
		append(&t.inbuf, ..buf[:n])
		t.input_at = time.now()
	}
	pos := 0
	for pos < len(t.inbuf) {
		event, used := term_decode(t.inbuf[pos:], time.since(t.input_at) >= 100*time.Millisecond)
		if used == 0 { break }
		pos += used
		switch e in event {
		case Term_Metrics:
			if !t.pasting { tui_metrics_reply(t, e) }
		case Term_Paste:
			t.pasting = e.start
		case Term_Key:
			key := e
			if t.pasting {
				// Pasted content never runs navigation/submit/quit shortcuts.
				// Current single-line editors normalize pasted line breaks.
				if !t.url_active && t.focus < 0 { continue }
				if special, ok := e.(Key_Special); ok {
					if special != .Enter && special != .Tab { continue }
					key = rune(' ')
				}
			}
			if !tui_handle_key(t, key) { return false }
		}
	}
	if pos > 0 {
		copy(t.inbuf[:], t.inbuf[pos:])
		resize(&t.inbuf, len(t.inbuf) - pos)
	}
	return true
}

tui_loop :: proc(sess: ^Browse_Session, start_url: string) -> bool {
	if !terminal.is_terminal(os.stdin) || !terminal.is_terminal(os.stdout) {
		fmt.eprintln("tui: terminal required (use browse --dump)")
		return false
	}
	// Odin diagnostics are redirected while fullscreen is active. Leave
	// native fd 2 alone so runtime/sanitizer failures remain observable.
	log_path := fmt.aprintf("%s/tui.log", sess.store.dir)
	defer delete(log_path)
	log, err := os.open(log_path, {.Write, .Create, .Append}, {.Read_User, .Write_User})
	if err != nil {
		fmt.eprintfln("tui: cannot open diagnostics log: %s", log_path)
		return false
	}
	defer os.close(log)
	stderr := os.stderr
	os.stderr = log
	defer { os.stderr = stderr }
	saved, ok := term_raw_enable()
	if !ok { return false }
	// Clear last (registered first): handlers stay active through cleanup.
	term_install_signal_handlers(&saved)
	defer term_clear_signal_handlers()
	defer term_setattr(&saved)
	term_enter_alt()
	defer term_exit_alt()
	defer kitty_delete_viewport()
	t := Tui{sess = sess, focus = -1, cell_w = 8, cell_h = 16, page_dirty = true, chrome_dirty = true}
	defer delete(t.inbuf)
	defer delete(t.slice)
	defer delete(t.hint)
	defer delete(t.vis)
	defer delete(t.status)
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
	tui_frame_geom(&t)
	if tui_drawable(&t) { sess.width = t.cols * t.cell_w }
	if !browse_navigate(sess, start_url, true) { tui_status(&t, "initial navigation failed") }
	tui_sync_fields(&t)
	wait_ms := 0 // drain replies/typeahead before the first frame
	for {
		tui_frame_geom(&t)
		if !tui_input(&t, wait_ms) { return true }
		// Poll wakes for input or every 100ms, including idle resize. No
		// page work occurs on timeout unless dimensions actually changed.
		tui_frame_geom(&t)
		if !t.metrics_ready && time.since(t.metrics_at) < 150*time.Millisecond {
			wait_ms = 10
			continue
		}
		if tui_drawable(&t) {
			width := t.cols * t.cell_w
			if width != sess.width {
				anchor := page_char_offset(&sess.page, t.scroll_y) if sess.has else 0
				if browse_relayout(sess, width) {
					// Source/control order is unchanged: keep builders, selection,
					// and focus rather than rebuilding field editors on resize.
					t.scroll_y = page_scroll_for_offset(&sess.page, anchor, tui_view_height(&t))
					tui_clamp_scroll(&t)
					t.page_dirty, t.chrome_dirty = true, true
					tui_ensure_field_visible(&t)
				}
			}
		}
		if !tui_draw(&t) { return false }
		// Persistent session/edit state owns its strings; transient format
		// and layout scratch must not accumulate across an idle session.
		free_all(context.temp_allocator)
		wait_ms = 100
	}
}
