package spike

// tuitest: layout/link/table/title behavior, headless (no display).
//   vixen tuitest

import "core:fmt"
import "core:os"
import "core:strings"

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
	fmt.printfln("tuitest: %d failures", fails)
	if fails > 0 {
		return false
	}
	if !tuitest_status_hints() {
		return false
	}
	// Server-backed browse section: navigate, back, forward, dump content.
	return tuitest_browse()
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
	prof := "/tmp/opencode/tuitest-profile"
	os.remove_all(prof)
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
		check(&fails, "browse/follow", browse_navigate(&sess, u, true), "")
		check(&fails, "browse/back", browse_back(&sess) && sess.page.title == "Test Article", sess.page.title)
		check(&fails, "browse/forward", browse_forward(&sess), "")
	}
	fmt.printfln("tuitest-browse: %d failures", fails)
	return fails == 0
}
