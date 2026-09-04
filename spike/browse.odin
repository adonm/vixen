package spike

// Browsing session: profile-backed net layers, persistent font bank,
// one live page (frame + links + title), back/forward URL stacks.
// Single-page CLI modes keep using render_page directly.

import "core:fmt"
import "core:strings"

Page :: struct {
	url:   string, // owned absolute URL
	title: string, // owned
	lines: [dynamic]Line, // owned laid-out lines (glyphs + text)
	links: [dynamic]Link, // owned urls
	text:  [dynamic]string, // owned laid-out line texts (text driver, tests)
	width: int,   // layout width, px
	height: int,  // total content height, px
}

Browse_Session :: struct {
	bank:  Font_Bank,
	store: ^Store, // heap-held: Cache/Jar keep this pointer across copies
	fc:    Fetch_Ctx,
	jar:   Jar,
	cache: Cache,
	ss:    Session_Storage,
	tab:   int,
	back:  [dynamic]string, // owned, oldest-first
	fwd:   [dynamic]string, // owned
	page:  Page,
	has:   bool,
	width: int,
}

browse_open :: proc(profile: string, width: int) -> (Browse_Session, bool) {
	sess: Browse_Session
	sp := new(Store)
	st, ok := store_open(profile)
	if !ok {
		free(sp)
		return sess, false
	}
	sp^ = st
	sess.store = sp
	fc, fok := fetch_ctx_new()
	if !fok {
		store_close(sess.store)
		free(sess.store)
		sess.store = nil
		return sess, false
	}
	sess.fc = fc
	sess.jar = jar_open(sess.store)
	sess.cache = cache_open(sess.store)
	sess.ss = session_storage_open()
	sess.tab = 1
	sess.width = width
	bank, bok := font_bank_load(20)
	if !bok {
		fetch_ctx_free(&sess.fc)
		jar_close(&sess.jar)
		cache_close(&sess.cache)
		session_storage_close(&sess.ss)
		store_close(sess.store)
		free(sess.store)
		sess.store = nil
		return sess, false
	}
	sess.bank = bank
	return sess, true
}

browse_close :: proc(sess: ^Browse_Session) {
	browse_drop_page(sess)
	for s in sess.back {
		delete(s)
	}
	delete(sess.back)
	for s in sess.fwd {
		delete(s)
	}
	delete(sess.fwd)
	font_bank_free(&sess.bank)
	fetch_ctx_free(&sess.fc)
	jar_close(&sess.jar)
	cache_close(&sess.cache)
	session_storage_close(&sess.ss)
	store_close(sess.store)
	free(sess.store)
	sess.store = nil
}

browse_drop_page :: proc(sess: ^Browse_Session) {
	if !sess.has {
		return
	}
	delete(sess.page.url)
	delete(sess.page.title)
	for &ln in sess.page.lines {
		delete(ln.glyphs)
		delete(ln.text)
	}
	// NOTE: Odin delete() leaves headers dangling; nil everything the next
	// page reuses, or appends write into freed backing (use-after-free).
	delete(sess.page.lines)
	sess.page.lines = nil
	for &l in sess.page.links {
		delete(l.url)
	}
	delete(sess.page.links)
	sess.page.links = nil
	for t in sess.page.text {
		delete(t)
	}
	delete(sess.page.text)
	sess.page.text = nil
	sess.has = false
}

// Fetch + render a URL into the live page. push_hist records back-stack.
browse_navigate :: proc(sess: ^Browse_Session, url: string, push_hist: bool) -> bool {
	pc := phase_start(url)
	now := tnow()
	r, info, ok := cached_fetch(&sess.cache, &sess.fc, &sess.jar, "GET", url, nil, nil, now)
	if !ok {
		return browse_error_page(sess, url, "fetch failed")
	}
	defer delete_response(&r)
	phase_end(&pc, "fetch")
	ct, _ := headers_get_first(&r, "content-type")
	is_html := strings.contains(ct, "html") || len(ct) == 0
	if r.status >= 400 || !is_html {
		msg := fmt.aprintf("%d %s", r.status, ct)
	defer delete(msg)
		return browse_error_page(sess, url, msg)
	}
	_ = info
	rc, rok := layout_bytes(r.body, sess.width, &sess.bank, url)
	if !rok {
		return browse_error_page(sess, url, "parse failed")
	}
	defer render_ctx_free(&rc)
	phase_end(&pc, "parse+layout")
	if push_hist && sess.has {
		append(&sess.back, strings.clone(sess.page.url))
		for s in sess.fwd {
			delete(s)
		}
		clear(&sess.fwd)
	}
	browse_drop_page(sess)
	sess.page.url = strings.clone(url)
	sess.page.title = strings.clone(rc.title)
	sess.page.width = sess.width
	// Deep-copy laid-out lines (glyphs are plain structs; texts cloned).
	// The framebuffer is rasterized per viewport on demand — never whole.
	for &ln in rc.lines {
		g := make([dynamic]Placed, len(ln.glyphs))
		copy(g[:], ln.glyphs[:])
		append(&sess.page.lines, Line{g, strings.clone(ln.text), ln.baseline, ln.height})
	}
	sess.page.height = 120
	if len(rc.lines) > 0 {
		last := rc.lines[len(rc.lines) - 1]
		sess.page.height = int(last.baseline + rc.body_px * 0.6 + rc.margin)
	}
	for &l in rc.links {
		append(&sess.page.links, Link{strings.clone(l.url), l.x0, l.y0, l.x1, l.y1})
	}
	for &ln in rc.lines {
		append(&sess.page.text, strings.clone(ln.text))
	}
	sess.has = true
	fmt.eprintfln("browse %-40s %dx%d lines=%d links=%d title=%q", url, sess.width, sess.page.height, len(rc.lines), len(sess.page.links), sess.page.title)
	return true
}

browse_error_page :: proc(sess: ^Browse_Session, url, msg: string) -> bool {
	// Navigable error state: keeps back/forward coherent.
	if sess.has {
		append(&sess.back, strings.clone(sess.page.url))
	}
	browse_drop_page(sess)
	sess.page.url = strings.clone(url)
	sess.page.title = strings.clone(msg)
	append(&sess.page.text, strings.clone(msg))
	sess.page.width = sess.width
	sess.page.height = 120
	sess.has = true
	return true
}

browse_back :: proc(sess: ^Browse_Session) -> bool {
	if len(sess.back) == 0 {
		return false
	}
	u := pop(&sess.back)
	defer delete(u)
	if sess.has {
		append(&sess.fwd, strings.clone(sess.page.url))
	}
	return browse_navigate(sess, u, false)
}

browse_forward :: proc(sess: ^Browse_Session) -> bool {
	if len(sess.fwd) == 0 {
		return false
	}
	u := pop(&sess.fwd)
	defer delete(u)
	if sess.has {
		append(&sess.back, strings.clone(sess.page.url))
	}
	return browse_navigate(sess, u, false)
}

browse_reload :: proc(sess: ^Browse_Session) -> bool {
	if !sess.has {
		return false
	}
	u := strings.clone(sess.page.url)
	defer delete(u)
	// Plain re-navigate: fresh entries serve from cache, stale revalidate.
	return browse_navigate(sess, u, false)
}
