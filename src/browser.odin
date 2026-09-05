package vixen

// Browsing session: profile-backed net layers, persistent font bank,
// one live page (source + layout + resources), back/forward URL stacks.
// Single-page CLI modes keep using render_page directly.

import "core:fmt"
import "core:strings"

Page :: struct {
	url:   string, // owned absolute URL
	title: string, // owned
	src:   []u8,   // owned HTML source (enables width-change relayout)
	lines: [dynamic]Line, // owned laid-out lines (glyphs + text)
	links: [dynamic]Link, // owned urls
	fields: [dynamic]Field, // form controls in tree order (owned)
	images: [dynamic]Image, // decoded page images (owned pixels)
	placements: [dynamic]Image_Placement, // image blocks (plain structs)
	targets: [dynamic]Anchor_Target, // fragment ids in tree order (owned)
	text:  [dynamic]string, // owned laid-out line texts (headless dump, tests)
	width: int,   // layout width, px
	height: int,  // total content height, px
	is_error: bool, // failed fetch/parse/non-HTML (navigable, but dump fails)
}

Browse_Session :: struct {
	bank:  Font_Bank,
	store: ^Store, // heap-held: Cache/Jar keep this pointer across copies
	fc:    Fetch_Ctx,
	jar:   Jar,
	cache: Cache,
	ss:    Session_Storage,
	tab:   int,
	back:  [dynamic]History_Entry, // owned, oldest-first
	fwd:   [dynamic]History_Entry, // owned
	page:  Page,
	has:   bool,
	width: int,
	// Char offset of the current top line (set by the frontend before any
	// navigation that pushes history; updated to the restored anchor after
	// back/forward). Direct session callers leave it 0 (top).
	cur_anchor: int,
	// False when the last navigate was a same-document fragment (no fetch,
	// layout, or field rebuild; frontend skips tui_sync_fields).
	page_rebuilt: bool,
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
	history_clear(&sess.back)
	delete(sess.back)
	history_clear(&sess.fwd)
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
	page_free(&sess.page)
	sess.has = false
}

// Lifetime follows the owned Page, not a separate visibility flag. Clear
// the entire value so a second drop or a new page can't reuse freed headers.
page_free :: proc(page: ^Page) {
	delete(page.url)
	delete(page.title)
	delete(page.src)
	for &ln in page.lines {
		delete(ln.glyphs)
		delete(ln.text)
	}
	delete(page.lines)
	for &l in page.links {
		delete(l.url)
	}
	delete(page.links)
	for &f in page.fields {
		delete_field(&f)
	}
	delete(page.fields)
	for &im in page.images {
		delete_image(&im)
	}
	delete(page.images)
	delete(page.placements)
	for &t in page.targets {
		delete_anchor_target(&t)
	}
	delete(page.targets)
	for t in page.text {
		delete(t)
	}
	delete(page.text)
	page^ = {}
}

// Fetch + render a URL into the live page. push_hist records back-stack.
browse_navigate :: proc(sess: ^Browse_Session, url: string, push_hist: bool) -> bool {
	return browse_navigate_request(sess, "GET", url, nil, push_hist)
}

// General navigation with method + body (form POST). GET callers use
// browse_navigate; POST responses are never cached (no cache key).
browse_navigate_request :: proc(sess: ^Browse_Session, method, url: string, body: []u8, push_hist: bool) -> bool {
	// Same-document fragment (GET, different #): no fetch/layout/field
	// rebuild. History still pushes; the frontend scrolls to the target.
	if method == "GET" && sess.has && urls_same_document(sess.page.url, url) {
		cur_frag := ""
		if i := strings.index_byte(sess.page.url, '#'); i >= 0 {
			cur_frag = sess.page.url[i+1:]
		}
		new_frag := ""
		if i := strings.index_byte(url, '#'); i >= 0 {
			new_frag = url[i+1:]
		}
		if cur_frag != new_frag {
			if push_hist {
				history_push(&sess.back, sess.page.url, sess.cur_anchor)
				history_clear(&sess.fwd)
			}
			new_url := strings.clone(url)
			delete(sess.page.url)
			sess.page.url = new_url
			sess.page_rebuilt = false
			fmt.eprintfln("browse %-40s fragment #%s (same-document)", sess.page.url, new_frag)
			return true
		}
	}
	pc := phase_start(url)
	now := tnow()
	extra: []string
	if method == "POST" {
		extra = []string{"Content-Type: application/x-www-form-urlencoded"}
	}
	r, info, ok := cached_fetch(&sess.cache, &sess.fc, &sess.jar, method, url, extra, body, now)
	if !ok {
		return browse_error_page(sess, url, "fetch failed")
	}
	defer delete_response(&r)
	phase_end(&pc, "fetch")
	// Final URL after redirects (with requested fragment preserved) is the
	// page identity and the base for relative links/resources. Requested URL
	// is only for history push of the page being left (handled below).
	final_url := r.final_url if len(r.final_url) > 0 else url
	ct, _ := headers_get_first(&r, "content-type")
	is_html := strings.contains(ct, "html") || len(ct) == 0
	if r.status >= 400 || !is_html {
		msg := fmt.aprintf("%d %s", r.status, ct)
		defer delete(msg)
		return browse_error_page(sess, final_url, msg)
	}
	_ = info
	doc, dok := parse_document(r.body)
	if !dok {
		return browse_error_page(sess, final_url, "parse failed")
	}
	defer lxb_html_document_destroy(doc)
	// Eager image pre-pass: layout reserves display sizes. Existing image
	// caps do not yet bound all network buffering/natural-size decoding.
	refs := collect_image_urls(doc, final_url, MAX_PAGE_IMAGES)
	imgs := page_load_images(sess, refs[:])
	delete_image_refs(refs)
	rc := render_ctx_new(&sess.bank, 20, f32(sess.width), final_url)
	// Move decoded pixels into the layout context BEFORE layout runs.
	for &im in imgs {
		append(&rc.images, im)
	}
	delete(imgs)
	layout_html(&rc, doc)
	finalize_links(&rc)
	rok := true
	if !rok {
		return browse_error_page(sess, final_url, "layout failed")
	}
	defer render_ctx_free(&rc)
	phase_end(&pc, "parse+layout")
	if push_hist && sess.has {
		history_push(&sess.back, sess.page.url, sess.cur_anchor)
		history_clear(&sess.fwd)
	}
	// Build before dropping: url may alias a link in the current page.
	page := page_from_layout(&rc, final_url, rc.title, sess.width)
	page.src = make([]u8, len(r.body))
	copy(page.src, r.body)
	browse_drop_page(sess)
	sess.page = page
	sess.has = true
	sess.page_rebuilt = true
	if push_hist {
		sess.cur_anchor = 0 // fresh page starts at the top
	}
	fmt.eprintfln("browse %-40s %dx%d lines=%d links=%d fields=%d images=%d title=%q", sess.page.url, sess.width, sess.page.height, len(rc.lines), len(sess.page.links), len(rc.fields), len(sess.page.images), sess.page.title)
	return true
}

// Construct an independently owned page without modifying the live session.
// Copies layout values and moves owned images; callers supply source bytes.
page_from_layout :: proc(rc: ^Render_Ctx, url, title: string, width: int) -> Page {
	page: Page
	page.url = strings.clone(url)
	page.title = strings.clone(title)
	page.width = width
	// Deep-copy laid-out lines (glyphs are plain structs; texts cloned).
	// The framebuffer is rasterized per viewport on demand — never whole.
	for &ln in rc.lines {
		g := make([dynamic]Placed, len(ln.glyphs))
		copy(g[:], ln.glyphs[:])
		append(&page.lines, Line{g, strings.clone(ln.text), ln.baseline, ln.height})
	}
	page.height = 120
	if len(rc.lines) > 0 {
		last := rc.lines[len(rc.lines) - 1]
		page.height = int(last.baseline + rc.body_px * 0.6 + rc.margin)
	}
	for &l in rc.links {
		append(&page.links, Link{strings.clone(l.url), l.x0, l.y0, l.x1, l.y1})
	}
	for &f in rc.fields {
		append(&page.fields, Field{
			f.kind,
			strings.clone(f.name),
			strings.clone(f.value),
			strings.clone(f.label),
			strings.clone(f.action),
			strings.clone(f.method),
			f.form,
			f.line, f.x0, f.px,
		})
	}
	for &ln in rc.lines {
		append(&page.text, strings.clone(ln.text))
	}
	for &t in rc.targets {
		append(&page.targets, Anchor_Target{strings.clone(t.id), t.y})
	}
	page.images = rc.images
	rc.images = nil
	append(&page.placements, ..rc.placements[:])
	return page
}

// Re-layout the live page at a new measure width (terminal resize)
// without refetching: source is retained, decoded images are reused at
// their existing display sizes, typed field values survive by index.
// No history push; the caller re-clamps scroll and resyncs fields.
browse_relayout :: proc(sess: ^Browse_Session, width: int) -> bool {
	if !sess.has || width <= 0 || width == sess.width || len(sess.page.src) == 0 {
		return false
	}
	doc, dok := parse_document(sess.page.src)
	if !dok {
		return false
	}
	defer lxb_html_document_destroy(doc)
	rc := render_ctx_new(&sess.bank, 20, f32(width), sess.page.url)
	// Layout only reads these images. Do not transfer/free live resources
	// until the replacement is complete, including on an early return.
	rc.images = sess.page.images
	defer {
		rc.images = nil
		render_ctx_free(&rc)
	}
	layout_html(&rc, doc)
	finalize_links(&rc)
	rc.images = nil // end borrow before constructing the owned replacement
	page := page_from_layout(&rc, sess.page.url, sess.page.title, width)
	for &f, i in page.fields {
		if i < len(sess.page.fields) {
			delete(f.value)
			f.value = strings.clone(sess.page.fields[i].value)
		}
	}
	// Commit together. Transfer source and images, then free only obsolete
	// layout/state. The replacement remains live and can be resized again.
	old := sess.page
	page.src, old.src = old.src, nil
	page.images, old.images = old.images, nil
	sess.page = page
	sess.width = width
	sess.has = true
	page_free(&old)
	return true
}

browse_error_page :: proc(sess: ^Browse_Session, url, msg: string) -> bool {
	// Navigable error state: keeps back/forward coherent.
	if sess.has {
		history_push(&sess.back, sess.page.url, sess.cur_anchor)
	}
	// Error URLs can also alias current-page links.
	page: Page
	page.url = strings.clone(url)
	page.title = strings.clone(msg)
	append(&page.text, strings.clone(msg))
	page.width = sess.width
	page.height = 120
	page.is_error = true
	browse_drop_page(sess)
	sess.page = page
	sess.has = true
	sess.cur_anchor = 0
	sess.page_rebuilt = true
	return true
}

browse_back :: proc(sess: ^Browse_Session) -> bool {
	if len(sess.back) == 0 {
		return false
	}
	target := pop(&sess.back)
	defer delete_history_entry(&target)
	if sess.has {
		history_push(&sess.fwd, sess.page.url, sess.cur_anchor)
	}
	u := strings.clone(target.url)
	defer delete(u)
	if !browse_navigate(sess, u, false) {
		return false
	}
	sess.cur_anchor = target.anchor
	return true
}

browse_forward :: proc(sess: ^Browse_Session) -> bool {
	if len(sess.fwd) == 0 {
		return false
	}
	target := pop(&sess.fwd)
	defer delete_history_entry(&target)
	if sess.has {
		history_push(&sess.back, sess.page.url, sess.cur_anchor)
	}
	u := strings.clone(target.url)
	defer delete(u)
	if !browse_navigate(sess, u, false) {
		return false
	}
	sess.cur_anchor = target.anchor
	return true
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

// Submit the form owning field submitter (a submit button, or a text field
// whose Enter submits its own form with no button). Reads current field
// values; the TUI mirrors edits into them per keystroke. Returns false
// when there is nothing to submit (no form owner, bad action URL).
browse_submit :: proc(sess: ^Browse_Session, submitter: int) -> bool {
	if !sess.has || submitter < 0 || submitter >= len(sess.page.fields) {
		return false
	}
	f := &sess.page.fields[submitter]
	if len(f.action) == 0 {
		return false
	}
	btn := submitter if f.kind == .submit else -1
	req, ok := form_submit(f.action, f.method, sess.page.fields[:], f.form, btn)
	if !ok {
		return false
	}
	defer delete_form_request(&req)
	if req.method == "POST" {
		return browse_navigate_request(sess, "POST", req.url, transmute([]u8)req.body, true)
	}
	return browse_navigate(sess, req.url, true)
}
