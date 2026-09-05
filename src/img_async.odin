package vixen

// Async image loading (interactive browsing): cache hits decode synchronously
// for first paint; misses fetch in the background via curl multi (single
// thread, pumped each TUI tick, max 4 concurrent in document order). File
// rendering keeps the synchronous path (deterministic one-shot output).
// Navigations abandon pending transfers synchronously, so no stale completion
// can land on a newer document (no generations needed single-threaded).

import "core:c"
import "core:fmt"
import "core:strings"
import "core:time"

IMG_CONCURRENT :: 4
IMG_TIMEOUT_MS :: 15000
IMG_REDIRECT_MAX :: 3

Img_Transfer :: struct {
	url:      string, // owned absolute URL (current hop; updated on redirect)
	attr_w:   int,
	attr_h:   int,
	hops:     int,
	easy:     ^Curl, // nil until started (queued)
	state:    Fetch_State, // owned buffers (stable address: transfer is heap-held)
	url_cstr: cstring, // owned (curl holds it across ticks; temp would dangle)
	hdrs:     ^Curl_Slist, // owned extra headers (Cookie)
}

img_transfer_free :: proc(tr: ^Img_Transfer) {
	if tr.easy != nil {
		easy_cleanup(tr.easy)
		tr.easy = nil
	}
	if tr.url_cstr != nil {
		delete(tr.url_cstr)
		tr.url_cstr = nil
	}
	if tr.hdrs != nil {
		slist_free_all(tr.hdrs)
		tr.hdrs = nil
	}
	delete(tr.state.body)
	delete(tr.state.rawhead)
	delete(tr.url)
	free(tr)
}

img_async_open :: proc(sess: ^Browse_Session) {
	sess.img_multi = multi_init()
	reserve(&sess.img_queue, MAX_PAGE_IMAGES)
}

img_async_close :: proc(sess: ^Browse_Session) {
	img_async_abandon(sess)
	delete(sess.img_queue)
	if sess.img_multi != nil {
		multi_cleanup(sess.img_multi)
		sess.img_multi = nil
	}
}

// Drop all pending/active transfers (new document coming). Synchronous:
// after this returns, no image completion can fire (handles removed).
img_async_abandon :: proc(sess: ^Browse_Session) {
	if sess.img_multi != nil {
		for tr in sess.img_queue {
			if tr.easy != nil {
				multi_remove_handle(sess.img_multi, tr.easy)
			}
		}
	}
	for tr in sess.img_queue {
		img_transfer_free(tr)
	}
	clear(&sess.img_queue)
}

// Queue misses for background fetch; fresh cache hits decode immediately
// (returned for first paint). Owns the returned images.
img_async_begin :: proc(sess: ^Browse_Session, refs: []Image_Ref) -> [dynamic]Image {
	ready: [dynamic]Image
	now := tnow()
	total := 0
	for r in refs {
		if len(ready) + len(sess.img_queue) >= MAX_PAGE_IMAGES || total >= MAX_IMAGE_TOTAL {
			continue
		}
		// Fresh cache hit? Decode synchronously (warm path, no network).
		if u, uok := url_parse(r.url); uok {
			key := url_cache_key(&u)
			delete_parsed_url(&u)
			defer delete(key)
			if hit, found := cache_lookup(&sess.cache, key, nil, now); found {
				defer cache_release(&sess.cache, &hit)
				if hit.fresh && len(hit.entry.body) > 0 {
					if im, dok := decode_image(r.url, hit.entry.body, sess.width, r.attr_w, r.attr_h); dok {
						total += len(im.px)
						append(&ready, im)
						continue
					}
				}
			}
		}
		// Miss/stale/undecodable-cached: fetch in background (no validators;
		// stale images re-download fully, overwriting the entry on completion).
		tr := new(Img_Transfer)
		tr.url = strings.clone(r.url)
		tr.attr_w, tr.attr_h = r.attr_w, r.attr_h
		append(&sess.img_queue, tr)
	}
	img_async_pump_starts(sess)
	return ready
}

// Start queued transfers while slots free (document order = visible first).
img_async_pump_starts :: proc(sess: ^Browse_Session) {
	if sess.img_multi == nil {
		return
	}
	active := 0
	for tr in sess.img_queue {
		if tr.easy != nil {
			active += 1
		}
	}
	for tr in sess.img_queue {
		if active >= IMG_CONCURRENT {
			return
		}
		if tr.easy == nil {
			if img_async_start(sess, tr) {
				active += 1
			} else {
				// Mark failed without losing queue position; pump completions
				// never sees it (easy nil). Drop it now to avoid spin.
				tr.hops = IMG_REDIRECT_MAX + 1
			}
		}
	}
	// Drop failed-to-start transfers (hops sentinel, easy nil).
	for i := len(sess.img_queue) - 1; i >= 0; i -= 1 {
		tr := sess.img_queue[i]
		if tr.easy == nil && tr.hops > IMG_REDIRECT_MAX {
			ordered_remove(&sess.img_queue, i)
			img_transfer_free(tr)
		}
	}
}

img_async_start :: proc(sess: ^Browse_Session, tr: ^Img_Transfer) -> bool {
	h := easy_init()
	if h == nil {
		return false
	}
	tr.easy = h
	easy_setopt(h, .NOSIGNAL, 1)
	easy_setopt(h, .FOLLOWLOCATION, 0)
	easy_setopt(h, .CONNECTTIMEOUT_MS, 10000)
	easy_setopt(h, .TIMEOUT_MS, IMG_TIMEOUT_MS)
	easy_setopt(h, .ACCEPT_ENCODING, "")
	easy_setopt(h, .USERAGENT, strings.clone_to_cstring(sess.fc.ua, context.temp_allocator))
	tr.url_cstr = strings.clone_to_cstring(tr.url, context.allocator)
	easy_setopt(h, .URL, tr.url_cstr)
	easy_setopt(h, .HTTPGET, 1)
	easy_setopt(h, .WRITEFUNCTION, fetch_write_cb)
	easy_setopt(h, .WRITEDATA, &tr.state)
	easy_setopt(h, .HEADERFUNCTION, fetch_head_cb)
	easy_setopt(h, .HEADERDATA, &tr.state)
	// Per-transfer cookies for the image host (third-party hosts get only
	// their own cookies, like browsers; jar enforces PSL/policy).
	if u, ok := url_parse(tr.url); ok {
		defer delete_parsed_url(&u)
		if ck, has := jar_header(&sess.jar, sess.store, u.scheme, u.host, u.path, tnow()); has {
			hdr := strings.concatenate([]string{"Cookie: ", ck}, context.temp_allocator)
			tr.hdrs = slist_append(tr.hdrs, strings.clone_to_cstring(hdr, context.temp_allocator))
			delete(ck)
		}
	}
	easy_setopt(h, .HTTPHEADER, tr.hdrs if tr.hdrs != nil else nil)
	if multi_add_handle(sess.img_multi, h) != 0 {
		return false
	}
	return true
}

// Non-blocking pump: run transfers, harvest completions into page.images.
// Returns true when at least one image installed (caller should refresh).
img_async_poll :: proc(sess: ^Browse_Session) -> bool {
	if sess.img_multi == nil || len(sess.img_queue) == 0 {
		return false
	}
	running: c.int
	multi_perform(sess.img_multi, &running)
	installed := false
	msgs_left: c.int
	for {
		msg := multi_info_read(sess.img_multi, &msgs_left)
		if msg == nil {
			break
		}
		if msg.msg != CURLMSG_DONE {
			continue
		}
		// Find the transfer by easy handle (linear; max 12, completions rare).
		idx := -1
		for tr, i in sess.img_queue {
			if tr.easy == msg.easy {
				idx = i
				break
			}
		}
		if idx < 0 {
			continue
		}
		tr := sess.img_queue[idx]
		multi_remove_handle(sess.img_multi, tr.easy)
		// 0 = dropped, 1 = installed, 2 = requeued (redirect; keep queued).
		outcome := 0
		if msg.result == 0 {
			outcome = img_async_finish(sess, tr)
		}
		if outcome == 1 {
			installed = true
		}
		if outcome != 2 {
			ordered_remove(&sess.img_queue, idx)
			img_transfer_free(tr)
		}
		// outcome==2 (redirect): transfer reconfigured in place (easy==nil),
		// stays queued at the same index; pump_starts restarts it below.
	}
	img_async_pump_starts(sess)
	return installed
}

// Handle one completed transfer: 0 dropped, 1 installed, 2 requeued.
// Redirects (<=3) reconfigure the same transfer in place (new URL, cleared
// buffers, easy torn down) and return 2; the caller keeps it queued and
// pump_starts restarts it (priority preserved by queue position).
img_async_finish :: proc(sess: ^Browse_Session, tr: ^Img_Transfer) -> int {
	code: c.long
	easy_getinfo(tr.easy, .RESPONSE_CODE, &code)
	status := int(code)
	// Parse headers (same shape as fetch_once).
	hdrs: [dynamic]Header
	defer {
		for &h in hdrs {
			delete(h.name)
			delete(h.value)
		}
		delete(hdrs)
	}
	for line in strings.split_lines(string(tr.state.rawhead[:])) {
		t := strings.trim_space(line)
		if len(t) == 0 || strings.has_prefix(t, "HTTP/") {
			continue
		}
		if i := strings.index_byte(t, ':'); i >= 0 {
			append(&hdrs, Header{
				strings.to_lower(strings.trim_space(t[:i])),
				strings.clone(strings.trim_space(t[i+1:])),
			})
		}
	}
	// Store cookies before anything else (per-hop, like fetch_url).
	if u, ok := url_parse(tr.url); ok {
		defer delete_parsed_url(&u)
		now := tnow()
		for &h in hdrs {
			if h.name == "set-cookie" {
				if c, cok := cookie_parse(h.value, u.scheme, u.host, u.path, now); cok {
					jar_store(&sess.jar, sess.store, &c, now)
					delete_cookie(&c)
				}
			}
		}
	}
	is_redir := status == 301 || status == 302 || status == 303 || status == 307 || status == 308
	if is_redir && tr.hops < IMG_REDIRECT_MAX {
		loc := ""
		for &h in hdrs {
			if h.name == "location" && len(h.value) > 0 {
				loc = h.value
				break
			}
		}
		if len(loc) > 0 {
			if next, nok := url_resolve(tr.url, loc); nok {
				defer delete(next)
				// Preserve prior fragment when Location has none.
				final := next
				owned := false
				if strings.index_byte(loc, '#') < 0 {
					if i := strings.index_byte(tr.url, '#'); i >= 0 {
						final = strings.concatenate([]string{next, tr.url[i:]})
						owned = true
					}
				}
				defer if owned { delete(final) }
				// Validate + reconfigure in place (stays GET for images).
				if u, ok := url_parse(final); ok {
					delete_parsed_url(&u)
					easy_cleanup(tr.easy)
					tr.easy = nil
					delete(tr.url_cstr)
					tr.url_cstr = nil
					if tr.hdrs != nil {
						slist_free_all(tr.hdrs)
						tr.hdrs = nil
					}
					clear(&tr.state.body)
					clear(&tr.state.rawhead)
					delete(tr.url)
					tr.url = strings.clone(final)
					tr.hops += 1
					return 2
				}
			}
		}
	}
	// Success: 200 + image/*, decode + budget + cache (direct only).
	if status != 200 {
		return 0
	}
	ct := ""
	for &h in hdrs {
		if h.name == "content-type" {
			ct = h.value
			break
		}
	}
	if !strings.has_prefix(ct, "image/") {
		return 0
	}
	im, dok := decode_image(tr.url, tr.state.body[:], sess.width, tr.attr_w, tr.attr_h)
	if !dok {
		return 0
	}
	// Total budget (completion order; over-budget completions dropped).
	total := len(im.px)
	for &e in sess.page.images {
		total += len(e.px)
	}
	if total > MAX_IMAGE_TOTAL {
		delete_image(&im)
		return 0
	}
	// Cache direct responses (redirected bodies never cached under the key).
	if tr.hops == 0 {
		if u, ok := url_parse(tr.url); ok {
			key := url_cache_key(&u)
			delete_parsed_url(&u)
			defer delete(key)
			resp: Response
			resp.status = status
			for &h in hdrs {
				append(&resp.headers, Header{strings.clone(h.name), strings.clone(h.value)})
			}
			resp.body = make([]u8, len(tr.state.body))
			copy(resp.body, tr.state.body[:])
			resp.final_url = strings.clone(tr.url)
			maybe_store(&sess.cache, key, &resp, nil, nil, tnow())
			delete_response(&resp)
		}
	}
	append(&sess.page.images, im)
	return 1
}

// Blocking drain for tests/scripts (pumps until idle or timeout).
browse_poll_blocking :: proc(sess: ^Browse_Session, timeout_ms: int) -> bool {
	elapsed := 0
	for len(sess.img_queue) > 0 && elapsed < timeout_ms {
		img_async_poll(sess)
		if len(sess.img_queue) == 0 {
			return true
		}
		// Sleep briefly (test-only; the TUI pumps on its own tick).
		time.sleep(10 * time.Millisecond)
		elapsed += 10
	}
	img_async_poll(sess)
	return len(sess.img_queue) == 0
}
