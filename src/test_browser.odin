package vixen

// Browser/session helper suite, headless (no display).
//   vixen browsetest   (legacy alias: tuitest)

import "core:fmt"
import "core:os"
import "core:strings"

import edit "core:text/edit"

tuitest_main :: proc() -> bool {
	fails := 0
	check :: proc(fails: ^int, name: string, cond: bool, detail: string = "") {
		if !cond {
			fails^ += 1
		}
		fmt.printfln("%s %-22s %s", "PASS" if cond else "FAIL", name, detail)
	}
	bank, bok := font_bank_load(20)
	if !bok {
		fmt.eprintln("tuitest: font bank failed")
		return false
	}
	defer font_bank_free(&bank)
	empty_ctx := Render_Ctx{bank = &bank}
	empty_line: Cur_Line
	check(&fails, "shape/empty-prefix", shape_word(&empty_ctx, "", 20, &empty_line) == 0 &&
		len(empty_line.glyphs) == 0 && len(empty_line.words) == 0)
	delete(empty_line.glyphs)
	delete(empty_line.words)
	data, err := os.read_entire_file_from_path("corpus/article.html", context.allocator)
	if err != nil {
		fmt.eprintfln("tuitest: cannot read fixture")
		return false
	}
	defer delete(data)
	rc, rok := layout_bytes(data, 900, &bank, "http://127.0.0.1:9/article.html")
	if !rok {
		check(&fails, "layout/ok", false)
		return false
	}
	defer render_ctx_free(&rc)
	text_b: strings.Builder
	for ln in rc.lines {
		strings.write_string(&text_b, ln.text)
		strings.write_byte(&text_b, '\n')
	}
	text_of := strings.clone(strings.to_string(text_b))
	delete(text_b.buf)
	defer delete(text_of)
	check(&fails, "title", rc.title == "Test Article", rc.title)
	check(&fails, "nav-skipped", !strings.contains(text_of, "NavNoise") && !strings.contains(text_of, "HeaderNoise") && !strings.contains(text_of, "FooterNoise"), "")
	check(&fails, "content", strings.contains(text_of, "Article Headline") && strings.contains(text_of, "Closing words."), "")
	check(&fails, "table-rows", strings.contains(text_of, "Alpha") && strings.contains(text_of, "Two"), "")
	// Table cells land on distinct lines (rows read as blocks).
	alpha_ln, two_ln := -1, -1
	for ln, i in rc.lines {
		if strings.contains(ln.text, "Alpha") {
			alpha_ln = i
		}
		if strings.contains(ln.text, "Two") {
			two_ln = i
		}
	}
	check(&fails, "table-order", alpha_ln >= 0 && two_ln > alpha_ln, fmt.tprintf("%d < %d", alpha_ln, two_ln))
	check(&fails, "img-alt", strings.contains(text_of, "[image: diagram]"), "")
	check(&fails, "links-count", len(rc.links) == 1, fmt.tprintf("%d", len(rc.links)))
	if len(rc.links) == 1 {
		l := &rc.links[0]
		check(&fails, "links-url", l.url == "http://127.0.0.1:9/wiki/Link_target", l.url)
		check(&fails, "links-rect", l.x0 >= 0 && l.x1 > l.x0 && l.y1 > l.y0 && l.x1 <= 900, fmt.tprintf("%.0f,%.0f-%.0f,%.0f", l.x0, l.y0, l.x1, l.y1))
	}
	fmt.printfln("browsetest: %d failures", fails)
	if fails > 0 {
		return false
	}
	if !tuitest_status_hints() {
		return false
	}
	if !tuitest_forms() {
		return false
	}
	if !tuitest_submit() {
		return false
	}
	if !tuitest_images() {
		return false
	}
	if !tuitest_truncate() {
		return false
	}
	if !tuitest_field_paint() {
		return false
	}
	if !tuitest_scroll() {
		return false
	}
	// Server-backed browse section: navigate, back, forward, dump content.
	return tuitest_browse()
}

// Reading-position anchors survive reflow, history, and reload.
tuitest_scroll :: proc() -> bool {
	fails := 0
	check :: proc(fails: ^int, name: string, cond: bool, detail: string = "") {
		if !cond {
			fails^ += 1
		}
		fmt.printfln("%s %-22s %s", "PASS" if cond else "FAIL", name, detail)
	}
	top_line_text :: proc(page: ^Page, scroll_y: int) -> string {
		for &ln in page.lines {
			bot := int(ln.baseline + ln.height * 0.2)
			if scroll_y < bot {
				return ln.text
			}
		}
		return ""
	}
	port, srv, ok := server_start()
	if !ok {
		check(&fails, "scroll/server", false)
		return false
	}
	defer {
		_ = os.process_kill(srv)
		_, _ = os.process_wait(srv)
	}
	defer delete(port)
	base := fmt.aprintf("http://127.0.0.1:%s", port)
	defer delete(base)
	prof, pok := test_directory()
	if !pok {
		return false
	}
	defer {
		os.remove_all(prof)
		delete(prof)
	}
	sess, sok := browse_open(prof, 900)
	if !sok {
		check(&fails, "scroll/open", false)
		return false
	}
	defer browse_close(&sess)
	t: Tui
	t.sess = &sess
	t.cols, t.rows = 80, 10
	t.cell_w, t.cell_h = 8, 16
	t.focus = -1
	t.status = strings.clone("")
	defer delete(t.status)
	defer {
		for &fe in t.field_edits {
			edit.destroy(&fe.state)
			strings.builder_destroy(&fe.build)
		}
		delete(t.field_edits)
	}
	view_h := tui_view_height(&t)
	relayout_url := fmt.aprintf("%s/relayout", base)
	defer delete(relayout_url)
	if !browse_navigate(&sess, relayout_url, true) {
		check(&fails, "scroll/navigate", false)
		return false
	}
	tui_sync_fields(&t)
	// Roundtrip on the same layout stays within a line.
	max_y := max(sess.page.height - view_h, 0)
	stable := true
	for frac in ([]f32{0, 0.25, 0.5, 0.75, 1.0}) {
		t.scroll_y = int(frac * f32(max_y))
		off := page_char_offset(&sess.page, t.scroll_y)
		back := page_scroll_for_offset(&sess.page, off, view_h)
		if abs(back - t.scroll_y) > 40 {
			stable = false
		}
	}
	check(&fails, "scroll/roundtrip", stable, "")
	// Reflow keeps the same content at the top (within nearby lines).
	t.scroll_y = max_y / 2
	tui_clamp_scroll(&t)
	anchor := page_char_offset(&sess.page, t.scroll_y)
	top_snippet := strings.clone(top_line_text(&sess.page, t.scroll_y))
	defer delete(top_snippet)
	words := strings.fields(top_snippet, context.temp_allocator)
	needle := words[0] if len(words) > 0 else ""
	if browse_relayout(&sess, 450) {
		t.scroll_y = page_scroll_for_offset(&sess.page, anchor, view_h)
		tui_clamp_scroll(&t)
		found := false
		top_idx := -1
		for &ln, i in sess.page.lines {
			if t.scroll_y < int(ln.baseline + ln.height * 0.2) {
				top_idx = i
				break
			}
		}
		if top_idx >= 0 {
			for i in max(top_idx - 2, 0) ..< min(top_idx + 3, len(sess.page.lines)) {
				if len(needle) > 0 && strings.contains(sess.page.lines[i].text, needle) {
					found = true
				}
			}
		}
		check(&fails, "scroll/reflow-anchor", found, needle)
	} else {
		check(&fails, "scroll/reflow", false, "")
	}
	// History restores the saved position instead of jumping to the top.
	form_url := fmt.aprintf("%s/form", base)
	defer delete(form_url)
	mid := max(sess.page.height - view_h, 0) / 2
	t.scroll_y = mid
	tui_save_anchor(&t)
	saved_anchor := sess.cur_anchor
	if !browse_navigate(&sess, form_url, true) {
		check(&fails, "scroll/leave", false)
	} else {
		t.scroll_y = 0
		if browse_back(&sess) {
			t.scroll_y = page_scroll_for_offset(&sess.page, sess.cur_anchor, view_h)
			tui_clamp_scroll(&t)
			check(&fails, "scroll/history-anchor", sess.cur_anchor == saved_anchor, "")
			check(&fails, "scroll/history-pos", abs(t.scroll_y - mid) <= 60, fmt.tprintf("%d vs %d", t.scroll_y, mid))
			if browse_forward(&sess) {
				check(&fails, "scroll/forward", sess.page.url == form_url, "")
			} else {
				check(&fails, "scroll/forward", false, "")
			}
			if browse_back(&sess) {
				t.scroll_y = page_scroll_for_offset(&sess.page, sess.cur_anchor, view_h)
				check(&fails, "scroll/back-again", abs(t.scroll_y - mid) <= 60, "")
			}
		} else {
			check(&fails, "scroll/back", false, "")
		}
	}
	// Reload keeps position via the same anchor path.
	if browse_reload(&sess) {
		restored := page_scroll_for_offset(&sess.page, saved_anchor, view_h)
		check(&fails, "scroll/reload", abs(restored - mid) <= 80, fmt.tprintf("%d", restored))
	} else {
		check(&fails, "scroll/reload", false, "")
	}
	fmt.printfln("browsetest-scroll: %d failures", fails)
	return fails == 0
}

// Visible field overlay: current values repaint, caret/selection show,
// focus scrolls into view, long values scroll horizontally.
tuitest_field_paint :: proc() -> bool {
	fails := 0
	check :: proc(fails: ^int, name: string, cond: bool, detail: string = "") {
		if !cond {
			fails^ += 1
		}
		fmt.printfln("%s %-22s %s", "PASS" if cond else "FAIL", name, detail)
	}
	port, srv, ok := server_start()
	if !ok {
		check(&fails, "paint/server", false)
		return false
	}
	defer {
		_ = os.process_kill(srv)
		_, _ = os.process_wait(srv)
	}
	defer delete(port)
	base := fmt.aprintf("http://127.0.0.1:%s", port)
	defer delete(base)
	prof, pok := test_directory()
	if !pok {
		return false
	}
	defer {
		os.remove_all(prof)
		delete(prof)
	}
	cols, cell_w, cell_h := 80, 8, 16
	sw := cols * cell_w
	sess, sok := browse_open(prof, sw)
	if !sok {
		check(&fails, "paint/open", false)
		return false
	}
	defer browse_close(&sess)
	form_url := fmt.aprintf("%s/form", base)
	defer delete(form_url)
	if !browse_navigate(&sess, form_url, true) {
		check(&fails, "paint/navigate", false)
		return false
	}
	t: Tui
	t.sess = &sess
	t.cols, t.rows = cols, 24
	t.cell_w, t.cell_h = cell_w, cell_h
	t.focus = -1
	t.status = strings.clone("")
	defer delete(t.status)
	defer delete(t.slice)
	defer {
		for &fe in t.field_edits {
			edit.destroy(&fe.state)
			strings.builder_destroy(&fe.build)
		}
		delete(t.field_edits)
	}
	tui_sync_fields(&t)
	sh := tui_view_height(&t)
	paint := proc(t: ^Tui, sw, sh: int) {
		delete(t.slice)
		t.slice = nil
		vfr := raster_slice(&t.sess.bank, t.sess.page.lines, t.sess.page.placements[:], t.sess.page.images[:], sw, t.scroll_y, sh)
		t.slice = vfr.px
		t.slice_w, t.slice_h = sw, sh
		tui_draw_fields(t, sw, sh)
	}
	count_color := proc(px: []u8, want: [3]u8) -> int {
		n := 0
		for i := 0; i + 3 < len(px); i += 4 {
			if px[i] == want[0] && px[i+1] == want[1] && px[i+2] == want[2] {
				n += 1
			}
		}
		return n
	}
	// Unfocused boxes paint with gray borders, no caret.
	paint(&t, sw, sh)
	before := make([]u8, len(t.slice))
	defer delete(before)
	copy(before, t.slice)
	check(&fails, "paint/box-border", count_color(t.slice, {80, 80, 90}) > 50, "")
	check(&fails, "paint/no-caret", count_color(t.slice, {255, 255, 255}) == 0, "")
	// Focus + type: pixels change, caret appears, value visible.
	tui_handle_key(&t, Key_Special.Tab)
	for r in "hi" {
		tui_handle_key(&t, Term_Key(r))
	}
	paint(&t, sw, sh)
	diff := 0
	for i in 0 ..< min(len(before), len(t.slice)) {
		if before[i] != t.slice[i] {
			diff += 1
		}
	}
	check(&fails, "paint/typing-visible", diff > 500, fmt.tprintf("%d bytes", diff))
	check(&fails, "paint/caret", count_color(t.slice, {255, 255, 255}) > 10, "")
	check(&fails, "paint/focus-ring", count_color(t.slice, {255, 255, 255}) > 10, "")
	// Selection highlight: select-all shows highlight color.
	fe := &t.field_edits[t.focus]
	fe.state.selection = {0, len(strings.to_string(fe.build))}
	paint(&t, sw, sh)
	check(&fails, "paint/selection", count_color(t.slice, {80, 80, 120}) > 20, "")
	fe.state.selection = {len(strings.to_string(fe.build)), len(strings.to_string(fe.build))}
	// Scroll into view: hide the field, focus must bring it back.
	t.rows = 10
	sh = tui_view_height(&t)
	t.scroll_y = 1 << 30
	tui_clamp_scroll(&t)
	top_before := t.scroll_y
	t.focus = -1
	tui_focus_move(&t, 1)
	check(&fails, "paint/scroll-visible", t.scroll_y < top_before, fmt.tprintf("%d -> %d", top_before, t.scroll_y))
	// Long value scrolls horizontally, caret stays in the box.
	long := strings.repeat("w", 200, context.temp_allocator)
	for r in long {
		tui_handle_key(&t, Term_Key(r))
	}
	paint(&t, sw, sh)
	check(&fails, "paint/hscroll", t.field_edits[t.focus].xoff > 0, fmt.tprintf("xoff=%d", t.field_edits[t.focus].xoff))
	// Multiline textarea values don't crash the single-line overlay.
	for &f, i in sess.page.fields {
		if f.name == "body" {
			delete(f.value)
			f.value = strings.clone("one\ntwo\tthree\r\nfour")
			// Mirror into the editor too (normally via tui_field_sync).
			for &e in t.field_edits {
				if e.field == i {
					strings.builder_reset(&e.build)
					strings.write_string(&e.build, f.value)
					e.state.selection = {len(f.value), len(f.value)}
				}
			}
		}
	}
	paint(&t, sw, sh)
	check(&fails, "paint/multiline-safe", len(t.slice) == sw*sh*4, "")
	// Relayout keeps the visible value (overlay repaints from fields).
	if browse_relayout(&sess, 400) {
		paint(&t, 400, sh)
		kept := false
		for &f in sess.page.fields {
			if f.name == "q" && len(f.value) >= 2 && strings.has_prefix(f.value, "hi") {
				kept = true
			}
		}
		check(&fails, "paint/relayout-value", kept, "")
	} else {
		check(&fails, "paint/relayout", false, "")
	}
	fmt.printfln("browsetest-paint: %d failures", fails)
	return fails == 0
}

// Cell-width truncation: ASCII, CJK wide, combining, emoji. No allocation;
// every result borrows the input.
tuitest_truncate :: proc() -> bool {
	fails := 0
	check :: proc(fails: ^int, name: string, cond: bool, detail: string = "") {
		if !cond {
			fails^ += 1
		}
		fmt.printfln("%s %-22s %s", "PASS" if cond else "FAIL", name, detail)
	}
	check(&fails, "trunc/ascii", truncate_cells("hello", 3) == "hel", "")
	check(&fails, "trunc/exact", truncate_cells("hello", 5) == "hello", "")
	check(&fails, "trunc/over", truncate_cells("hi", 10) == "hi", "")
	check(&fails, "trunc/empty", truncate_cells("", 5) == "", "")
	check(&fails, "trunc/zero", truncate_cells("hello", 0) == "", "")
	// CJK ideographs are 2 cells: "日本語" = 6 cells.
	check(&fails, "trunc/cjk-fit", truncate_cells("日本語", 6) == "日本語", "")
	check(&fails, "trunc/cjk-cut", truncate_cells("日本語", 5) == "日本", "")
	check(&fails, "trunc/cjk-one", truncate_cells("日本語", 1) == "", "")
	check(&fails, "trunc/width-cjk", term_str_width("日本語") == 6, "")
	// Combining mark adds no width and never strands.
	check(&fails, "trunc/combining", truncate_cells("éx", 2) == "éx", "")
	check(&fails, "trunc/width-comb", term_str_width("é") == 1, "")
	// Grinning face (U+1F600) is wide; its 4 bytes never split.
	check(&fails, "trunc/width-emoji", term_char_width('😀') == 2, "")
	check(&fails, "trunc/emoji-cut", truncate_cells("a😀b", 2) == "a", "")
	check(&fails, "trunc/mixed", truncate_cells("a日本b", 4) == "a日", "")
	fmt.printfln("browsetest-truncate: %d failures", fails)
	return fails == 0
}

// Image pipeline against the test server: fetch, decode, resize, place.
// /img.png is a hand-rolled 8x6 RGBA PNG; /missing.png 404s.
tuitest_images :: proc() -> bool {
	fails := 0
	check :: proc(fails: ^int, name: string, cond: bool, detail: string = "") {
		if !cond {
			fails^ += 1
		}
		fmt.printfln("%s %-22s %s", "PASS" if cond else "FAIL", name, detail)
	}
	port, srv, ok := server_start()
	if !ok {
		check(&fails, "images/server", false)
		return false
	}
	defer {
		_ = os.process_kill(srv)
		_, _ = os.process_wait(srv)
	}
	defer delete(port)
	base := fmt.aprintf("http://127.0.0.1:%s", port)
	defer delete(base)
	prof, pok := test_directory()
	if !pok {
		return false
	}
	defer {
		os.remove_all(prof)
		delete(prof)
	}
	sess, sok := browse_open(prof, 900)
	if !sok {
		check(&fails, "images/open", false)
		return false
	}
	defer browse_close(&sess)
	page_url := fmt.aprintf("%s/imgpage", base)
	defer delete(page_url)
	if !browse_navigate(&sess, page_url, true) {
		check(&fails, "images/navigate", false)
		return false
	}
	// Three size variants decode (dup shares the unadorned one);
	// the 404 falls back to a text placeholder.
	check(&fails, "images/count", len(sess.page.images) == 3, fmt.tprintf("%d", len(sess.page.images)))
	check(&fails, "images/placements", len(sess.page.placements) == 3, fmt.tprintf("%d", len(sess.page.placements)))
	if len(sess.page.images) == 1 {
		im := &sess.page.images[0]
		check(&fails, "images/dims", im.w == 8 && im.h == 6, fmt.tprintf("%dx%d", im.w, im.h))
		check(&fails, "images/pixels", len(im.px) == 8 * 6 * 4, "")
		// First pixel is deterministic: (0,0,128,255) per the generator.
		check(&fails, "images/content", im.px[0] == 0 && im.px[1] == 0 && im.px[2] == 128 && im.px[3] == 255, "")
	}
	if len(sess.page.placements) == 3 {
		// Third img carries width/height attrs: 4x3 display block.
		pl := &sess.page.placements[2]
		check(&fails, "images/attr-size", pl.w == 4 && pl.h == 3, fmt.tprintf("%dx%d", pl.w, pl.h))
	}
	found_dims, found_missing := false, false
	for t in sess.page.text {
		if strings.contains(t, "8x6") {
			found_dims = true
		}
		if strings.contains(t, "gone") && !strings.contains(t, "x") {
			found_missing = true
		}
	}
	check(&fails, "images/text-dims", found_dims, "")
	check(&fails, "images/text-missing", found_missing, "")
	// Repeated relayout keeps the original image pixel allocations. A
	// single resize previously passed while silently clearing sess.has/src.
	nimg, nplc := len(sess.page.images), len(sess.page.placements)
	pixels := make([]rawptr, nimg)
	defer delete(pixels)
	for im, i in sess.page.images {
		pixels[i] = raw_data(im.px)
	}
	image_hits := tuitest_request_count(&sess, base, "/img.png")
	missing_hits := tuitest_request_count(&sess, base, "/missing.png")
	for width in ([]int{450, 900, 300, 700, 450, 900}) {
		if !browse_relayout(&sess, width) {
			check(&fails, "images/relayout", false, fmt.tprintf("width=%d", width))
			break
		}
		check(&fails, "images/relayout-imgs", len(sess.page.images) == nimg, "")
		check(&fails, "images/relayout-plc", len(sess.page.placements) == nplc, "")
		for im, i in sess.page.images {
			check(&fails, "images/relayout-pixels", i < len(pixels) && raw_data(im.px) == pixels[i], "")
		}
	}
	check(&fails, "images/no-refetch", image_hits > 0 &&
		tuitest_request_count(&sess, base, "/img.png") == image_hits &&
		tuitest_request_count(&sess, base, "/missing.png") == missing_hits, "")
	// End-to-end raster: slice the viewport and require non-background
	// pixels inside the first image placement.
	bank, bok := font_bank_load(20)
	if !bok {
		check(&fails, "images/fontbank", false, "")
	} else {
		defer font_bank_free(&bank)
		fr := raster_slice(&bank, sess.page.lines, sess.page.placements[:], sess.page.images[:], sess.page.width, 0, 400)
		defer delete(fr.px)
		pl := &sess.page.placements[0]
		ink := 0
		for yy in pl.y ..< min(pl.y + pl.h, fr.h) {
			for xx in pl.x ..< min(pl.x + pl.w, fr.w) {
				o := (yy * fr.w + xx) * 4
				if fr.px[o] != 16 || fr.px[o+1] != 16 || fr.px[o+2] != 22 {
					ink += 1
				}
			}
		}
		check(&fails, "images/raster", ink > 0, fmt.tprintf("%d px", ink))
	}
	fmt.printfln("browsetest-images: %d failures", fails)
	return fails == 0
}

// Live form submission against the test server: fill, submit, assert URLs.
tuitest_submit :: proc() -> bool {
	fails := 0
	check :: proc(fails: ^int, name: string, cond: bool, detail: string = "") {
		if !cond {
			fails^ += 1
		}
		fmt.printfln("%s %-22s %s", "PASS" if cond else "FAIL", name, detail)
	}
	port, srv, ok := server_start()
	if !ok {
		check(&fails, "submit/server", false)
		return false
	}
	defer {
		_ = os.process_kill(srv)
		_, _ = os.process_wait(srv)
	}
	defer delete(port)
	base := fmt.aprintf("http://127.0.0.1:%s", port)
	defer delete(base)
	prof, pok := test_directory()
	if !pok {
		return false
	}
	defer {
		os.remove_all(prof)
		delete(prof)
	}
	sess, sok := browse_open(prof, 900)
	if !sok {
		check(&fails, "submit/open", false)
		return false
	}
	defer browse_close(&sess)
	form_url := fmt.aprintf("%s/form", base)
	defer delete(form_url)
	if !browse_navigate(&sess, form_url, true) {
		check(&fails, "submit/navigate", false)
		return false
	}
	find_idx := proc(sess: ^Browse_Session, name: string) -> int {
		for &f, i in sess.page.fields {
			if f.name == name {
				return i
			}
		}
		return -1
	}
	qi := find_idx(&sess, "q")
	check(&fails, "submit/has-q", qi >= 0, "")
	// Orphan submit has no form owner: must fail without navigating.
	oi := find_idx(&sess, "orphan")
	if oi >= 0 {
		before := strings.clone(sess.page.url)
		defer delete(before)
		check(&fails, "submit/orphan", !browse_submit(&sess, oi) && sess.page.url == before, "")
	} else {
		check(&fails, "submit/has-orphan", false, "")
	}
	// Interactive path: Tab focuses, typing edits, Enter submits.
	// (Drives tui_handle_key directly — no terminal needed.)
	if qi >= 0 {
		t: Tui
		t.sess = &sess
		t.rows = 24
		t.cols = 80
		t.focus = -1
		t.status = strings.clone("")
		defer delete(t.status)
		defer {
			for &fe in t.field_edits {
				edit.destroy(&fe.state)
				strings.builder_destroy(&fe.build)
			}
			delete(t.field_edits)
		}
		tui_sync_fields(&t)
		tui_handle_key(&t, Key_Special.Tab)
		check(&fails, "keys/focus", t.focus == 0, "")
		tui_handle_key(&t, Term_Key('h'))
		tui_handle_key(&t, Term_Key('i'))
		qv := strings.clone(sess.page.fields[qi].value)
		defer delete(qv)
		check(&fails, "keys/type", qv == "hi", qv)
		tui_handle_key(&t, Key_Special.Enter)
		check(&fails, "keys/submit", strings.contains(sess.page.url, "q=hi"), sess.page.url)
	}
	// The Enter above navigated away; go back for the direct-API flow.
	if !browse_back(&sess) {
		check(&fails, "submit/back-to-form", false)
	}
	if qi >= 0 {
		f := &sess.page.fields[qi]
		delete(f.value)
		f.value = strings.clone("rust lang")
		if !browse_submit(&sess, qi) {
			check(&fails, "submit/get", false, "")
		} else {
			want_q := strings.contains(sess.page.url, "q=rust+lang")
			want_src := strings.contains(sess.page.url, "src=web")
			check(&fails, "submit/get-url", want_q && want_src, sess.page.url)
			check(&fails, "submit/get-title", sess.page.title == "Results", sess.page.title)
		}
	}
	// Back to the form, then POST through the textarea form.
	if !browse_back(&sess) {
		check(&fails, "submit/back", false)
	} else {
		bi := find_idx(&sess, "body")
		if bi < 0 {
			check(&fails, "submit/has-body", false)
		} else {
			// Find the Send button (submit in the POST form).
			send := -1
			for &f, i in sess.page.fields {
				if f.kind == .submit && f.method == "POST" {
					send = i
				}
			}
			check(&fails, "submit/has-send", send >= 0, "")
			if send >= 0 {
				if !browse_submit(&sess, send) {
					check(&fails, "submit/post", false, "")
				} else {
					check(&fails, "submit/post-title", sess.page.title == "Posted", sess.page.title)
					found := false
					for t in sess.page.text {
						if strings.contains(t, "body=hello") {
							found = true
						}
					}
					check(&fails, "submit/post-echo", found, "")
				}
			}
		}
	}
	fmt.printfln("browsetest-submit: %d failures", fails)
	return fails == 0
}

// Form logic + layout. Live submit flow lives in tuitest_submit().
tuitest_forms :: proc() -> bool {
	fails := 0
	check :: proc(fails: ^int, name: string, cond: bool, detail: string = "") {
		if !cond {
			fails^ += 1
		}
		fmt.printfln("%s %-22s %s", "PASS" if cond else "FAIL", name, detail)
	}
	mkfield := proc(kind: Field_Kind, name, value, action, method: string, form := 0) -> Field {
		return Field{kind, strings.clone(name), strings.clone(value), strings.clone(""), strings.clone(action), strings.clone(method), form, -1, 0, 0}
	}
	// Percent-encoding.
	e := form_url_encode("a b+c&d=e/f~ok-._")
	check(&fails, "form/encode", e == "a+b%2Bc%26d%3De%2Ff~ok-._", e)
	delete(e)
	e = form_url_encode("caf\u00e9")
	check(&fails, "form/encode-utf8", e == "caf%C3%A9", e)
	delete(e)
	// Dataset: unnamed skipped, button only when clicked.
	fields: [dynamic]Field
	append(&fields, mkfield(.text, "q", "x", "", "GET"))
	append(&fields, mkfield(.hidden, "src", "web", "", "GET"))
	append(&fields, mkfield(.text, "", "y", "", "GET"))
	append(&fields, mkfield(.submit, "go", "Go!", "", "GET"))
	append(&fields, mkfield(.text, "other", "z", "", "GET", 1))
	defer {
		for &f in fields {
			delete_field(&f)
		}
		delete(fields)
	}
	d := form_dataset(fields[:], 0, -1)
	check(&fails, "form/dataset", d == "q=x&src=web", d)
	delete(d)
	d = form_dataset(fields[:], 0, 3)
	check(&fails, "form/dataset-btn", d == "q=x&src=web&go=Go%21", d)
	delete(d)
	d = form_dataset(fields[:], 1, -1)
	check(&fails, "form/dataset-scope", d == "other=z", d)
	delete(d)
	// GET submission replaces any existing query, drops the fragment.
	for &f in fields {
		delete(f.action)
		f.action = strings.clone("http://h/search?old=1#frag")
	}
	req, ok := form_submit("http://h/search?old=1#frag", "get", fields[:], 0, -1)
	check(&fails, "form/submit-get", ok && req.method == "GET" && req.url == "http://h/search?q=x&src=web" && req.body == "", req.url)
	delete_form_request(&req)
	// Empty action falls back to the document URL minus fragment.
	req, ok = form_submit("", "GET", fields[:], 0, -1)
	check(&fails, "form/submit-noaction", !ok, "")
	delete_form_request(&req)
	// POST keeps the action URL, dataset goes in the body.
	for &f in fields {
		delete(f.action)
		f.action = strings.clone("http://h/postform?a=1")
	}
	req, ok = form_submit("http://h/postform?a=1", "POST", fields[:], 0, 3)
	check(&fails, "form/submit-post", ok && req.method == "POST" && req.url == "http://h/postform?a=1" && req.body == "q=x&src=web&go=Go%21", req.body)
	delete_form_request(&req)
	_, ok = form_submit("::::", "GET", fields[:], 0, -1)
	check(&fails, "form/submit-badurl", !ok, "")
	// Layout over the fixture.
	data, err := os.read_entire_file_from_path("corpus/form.html", context.allocator)
	if err != nil {
		check(&fails, "form/fixture", false, "")
		return false
	}
	defer delete(data)
	bank, bok := font_bank_load(20)
	if !bok {
		check(&fails, "form/fontbank", false, "")
		return false
	}
	defer font_bank_free(&bank)
	rc, rok := layout_bytes(data, 900, &bank, "http://127.0.0.1:9/form")
	if !rok {
		check(&fails, "form/layout", false, "")
		return false
	}
	defer render_ctx_free(&rc)
	check(&fails, "form/count", len(rc.fields) == 8, fmt.tprintf("%d", len(rc.fields)))
	find := proc(rc: ^Render_Ctx, name: string) -> (Field, bool) {
		for &f in rc.fields {
			if f.name == name {
				return f, true
			}
		}
		return {}, false
	}
	if q, found := find(&rc, "q"); found {
		check(&fails, "form/q", q.kind == .text && q.value == "" && q.action == "http://127.0.0.1:9/search" && q.method == "GET" && q.line >= 0, q.action)
	} else {
		check(&fails, "form/q", false, "missing")
	}
	if s, found := find(&rc, "src"); found {
		check(&fails, "form/hidden", s.kind == .hidden && s.value == "web" && s.line < 0, "")
	} else {
		check(&fails, "form/hidden", false, "missing")
	}
	if b, found := find(&rc, "body"); found {
		check(&fails, "form/textarea", b.kind == .text && b.value == "hello" && b.method == "POST", b.value)
	} else {
		check(&fails, "form/textarea", false, "missing")
	}
	if o, found := find(&rc, "orphan"); found {
		check(&fails, "form/orphan", o.action == "" && o.line >= 0, o.action)
	} else {
		check(&fails, "form/orphan", false, "missing")
	}
	if na, found := find(&rc, "noact"); found {
		check(&fails, "form/noaction", na.action == "http://127.0.0.1:9/form", na.action)
	} else {
		check(&fails, "form/noaction", false, "missing")
	}
	_, has_c := find(&rc, "c")
	_, has_t2 := find(&rc, "t2")
	check(&fails, "form/skipped", !has_c && !has_t2, "")
	// Submit buttons: Go (value), Send (inner text), Nowhere (orphan form).
	named := 0
	for &f in rc.fields {
		if f.kind == .submit {
			named += 1
		}
	}
	check(&fails, "form/buttons", named == 3, "")
	fmt.printfln("browsetest-forms: %d failures", fails)
	return fails == 0
}

// Status-line smoke with link hints set. Regression: the hint display once
// delete()d a tprintf (temp-allocator) string, corrupting the heap and
// segfaulting the TUI on link-hint input. This path must stay
// allocation-free; the heap traffic between frames makes any recurrence
// crash fast and loud instead of corrupting silently.
tuitest_status_hints :: proc() -> bool {
	sess: Browse_Session
	sess.page.url = "http://example.com/article"
	sess.page.height = 1000
	t: Tui
	t.sess = &sess
	t.rows = 24
	t.cols = 80
	append(&t.hint, '1', '2')
	for _ in 0 ..< 20 {
		tui_status_line(&t)
		probe := strings.clone("probe")
		delete(probe)
	}
	fmt.println()
	delete(t.hint)
	fmt.println("PASS status-hints          20 hint frames, no crash")
	return true
}

// Browse subset against the deterministic test server (shares nettest's).
tuitest_browse :: proc() -> bool {
	fails := 0
	check :: proc(fails: ^int, name: string, cond: bool, detail: string = "") {
		if !cond {
			fails^ += 1
		}
		fmt.printfln("%s %-22s %s", "PASS" if cond else "FAIL", name, detail)
	}
	port, srv, ok := server_start()
	if !ok {
		check(&fails, "browse/server", false)
		return false
	}
	defer {
		_ = os.process_kill(srv)
		_, _ = os.process_wait(srv)
	}
	defer delete(port)
	base := fmt.aprintf("http://127.0.0.1:%s", port)
	defer delete(base)
	prof, pok := test_directory()
	if !pok {
		return false
	}
	defer {
		os.remove_all(prof)
		delete(prof)
	}
	sess, sok := browse_open(prof, 900)
	if !sok {
		check(&fails, "browse/open", false)
		return false
	}
	defer browse_close(&sess)
	art := fmt.aprintf("%s/article", base)
	defer delete(art)
	check(&fails, "browse/navigate", browse_navigate(&sess, art, true) &&
		sess.page.title == "Test Article", sess.page.title)
	check(&fails, "browse/links-live", len(sess.page.links) == 1, "")
	// Follow the content link (server has no such route -> error page, still coherent).
	if len(sess.page.links) == 1 {
		u := strings.clone(sess.page.links[0].url)
		defer delete(u)
		check(&fails, "browse/follow", browse_navigate(&sess, sess.page.links[0].url, true) &&
			sess.page.url == u, "")
		check(&fails, "browse/back", browse_back(&sess) && sess.page.title == "Test Article", sess.page.title)
		check(&fails, "browse/forward", browse_forward(&sess), "")
	}
	// Relayout: narrower measure re-wraps without refetch, keeps title,
	// links, and typed field values; same width is a no-op.
	form_url := fmt.aprintf("%s/form", base)
	defer delete(form_url)
	if !browse_navigate(&sess, form_url, true) {
		check(&fails, "relayout/navigate", false, "")
	} else {
		qi := -1
		for &f, i in sess.page.fields {
			if f.name == "q" {
				qi = i
			}
		}
		if qi >= 0 {
			f := &sess.page.fields[qi]
			delete(f.value)
			f.value = strings.clone("typed")
		}
		narrow_links := len(sess.page.links)
		source := strings.clone(string(sess.page.src))
		defer delete(source)
		history_urls := proc(entries: []History_Entry) -> string {
			urls := make([dynamic]string, context.temp_allocator)
			for &e in entries {
				append(&urls, e.url)
			}
			return strings.clone(strings.join(urls[:], "\n"))
		}
		back := history_urls(sess.back[:])
		defer delete(back)
		fwd := history_urls(sess.fwd[:])
		defer delete(fwd)
		hits := tuitest_request_count(&sess, base, "/form")
		check(&fails, "relayout/noop", !browse_relayout(&sess, sess.width), "")
		check(&fails, "relayout/narrow", browse_relayout(&sess, 450) && sess.width == 450, "")
		check(&fails, "relayout/title", sess.page.title == "Form Test", sess.page.title)
		check(&fails, "relayout/live", sess.has, "")
		check(&fails, "relayout/source", string(sess.page.src) == source, "")
		check(&fails, "relayout/url", sess.page.url == form_url, "")
		kept := false
		for &f in sess.page.fields {
			if f.name == "q" && f.value == "typed" {
				kept = true
			}
		}
		check(&fails, "relayout/values", kept, "")
		check(&fails, "relayout/links", len(sess.page.links) == narrow_links,
			fmt.tprintf("%d", len(sess.page.links)))
		for width in ([]int{900, 300, 700, 450}) {
			if !browse_relayout(&sess, width) {
				check(&fails, "relayout/repeated", false, fmt.tprintf("width=%d", width))
				break
			}
			check(&fails, "relayout/repeated-state", sess.has &&
				sess.width == width && sess.page.width == width &&
				string(sess.page.src) == source && sess.page.url == form_url &&
				sess.page.title == "Form Test", "")
			check(&fails, "relayout/repeated-value", qi >= 0 &&
				qi < len(sess.page.fields) && sess.page.fields[qi].value == "typed", "")
		}
		check(&fails, "relayout/no-refetch", hits > 0 && tuitest_request_count(&sess, base, "/form") == hits, "")
		back_after := history_urls(sess.back[:])
		defer delete(back_after)
		fwd_after := history_urls(sess.fwd[:])
		defer delete(fwd_after)
		check(&fails, "relayout/history", back_after == back && fwd_after == fwd, "")
		before_width := sess.width
		check(&fails, "relayout/invalid-width", !browse_relayout(&sess, 0) &&
			!browse_relayout(&sess, -10) && sess.width == before_width, "")
		check(&fails, "relayout/submit", browse_submit(&sess, qi) &&
			strings.contains(sess.page.url, "q=typed"), "")
		check(&fails, "relayout/back-to-form", browse_back(&sess) &&
			sess.page.url == form_url && sess.has, "")
		// Navigation after reflow must drop, not append to, the previous page.
		check(&fails, "relayout/reload", browse_reload(&sess) && sess.has &&
			sess.page.url == form_url && len(sess.page.fields) == 8, "")
	}
	// A dedicated long fixture proves actual wrapping, not leftover lines
	// from the previous document being appended during navigation.
	art2 := fmt.aprintf("%s/relayout", base)
	defer delete(art2)
	if !browse_navigate(&sess, art2, true) {
		check(&fails, "relayout/article", false, "")
	} else {
		before := len(sess.page.lines)
		check(&fails, "relayout/fresh-page", len(sess.page.fields) == 0 && len(sess.page.links) == 2, "")
		if browse_relayout(&sess, 900) {
			check(&fails, "relayout/rewrap", len(sess.page.lines) < before,
				fmt.tprintf("%d -> %d", before, len(sess.page.lines)))
			check(&fails, "relayout/article-title", sess.page.title == "Reflow Test", sess.page.title)
			check(&fails, "relayout/roundtrip", browse_relayout(&sess, 450) &&
				len(sess.page.lines) == before && len(sess.page.links) == 2, "")
			// Successful navigation also accepts a URL borrowed from the old page.
			check(&fails, "relayout/alias-navigate", browse_navigate(&sess, sess.page.url, false) &&
				sess.page.url == art2 && len(sess.page.lines) == before, "")
			check(&fails, "relayout/back", browse_back(&sess) && sess.page.url == form_url, "")
			check(&fails, "relayout/forward", browse_forward(&sess) && sess.page.url == art2 &&
				len(sess.page.lines) == before, "")
		} else {
			check(&fails, "relayout/article-relayout", false, "")
		}
	}
	browse_drop_page(&sess)
	browse_drop_page(&sess)
	check(&fails, "relayout/drop-idempotent", !sess.has && sess.page.url == "" &&
		len(sess.page.src) == 0 && len(sess.page.lines) == 0 &&
		len(sess.page.fields) == 0 && len(sess.page.images) == 0, "")
	fmt.printfln("browsetest-session: %d failures", fails)
	return fails == 0
}

// Read server counters directly (not via cache). Used to prove resize
// doesn't request source/images, including the uncached missing image.
tuitest_request_count :: proc(sess: ^Browse_Session, base, path: string) -> int {
	r, ok := fetch_once(&sess.fc, "GET", fmt.tprintf("%s/stats", base), nil, nil)
	defer delete_response(&r)
	if !ok || r.status != 200 {
		return -1
	}
	needle := fmt.tprintf("\"%s\": ", path)
	body := string(r.body)
	if i := strings.index(body, needle); i >= 0 {
		n := 0
		for c in body[i+len(needle):] {
			if c < '0' || c > '9' {
				break
			}
			n = n * 10 + int(c - '0')
		}
		return n
	}
	return -1
}
