package vixen

// HTTP fetch over curl easy (one reusable handle, manual redirects).
// Cookie engine OFF (jar owns policy); TLS verification always on (curl
// defaults for peer+host; the mincurl binding exposes no way to disable).

import "base:runtime"
import "core:c"
import "core:fmt"
import "core:strings"


Fetch_Ctx :: struct {
	handle: ^Curl,
	ua:     string,
}

fetch_ctx_new :: proc(ua := "Vixen/0.1") -> (Fetch_Ctx, bool) {
	fc: Fetch_Ctx
	fc.handle = easy_init()
	if fc.handle == nil {
		return fc, false
	}
	fc.ua = strings.clone(ua)
	easy_setopt(fc.handle, .NOSIGNAL, 1)
	easy_setopt(fc.handle, .FOLLOWLOCATION, 0)
	easy_setopt(fc.handle, .CONNECTTIMEOUT_MS, 10000)
	easy_setopt(fc.handle, .TIMEOUT_MS, 60000)
	easy_setopt(fc.handle, .ACCEPT_ENCODING, "")
	easy_setopt(fc.handle, .USERAGENT, strings.clone_to_cstring(fc.ua, context.temp_allocator))
	return fc, true
}

fetch_ctx_free :: proc(fc: ^Fetch_Ctx) {
	easy_cleanup(fc.handle)
	fc.handle = nil
	delete(fc.ua)
}

Header :: struct {
	name:  string, // lowercased
	value: string,
}

Response :: struct {
	status:     int,
	headers:    [dynamic]Header,
	body:       []u8,
	final_url:  string,
	hops:       int,
}

delete_response :: proc(r: ^Response) {
	for &h in r.headers {
		delete(h.name)
		delete(h.value)
	}
	delete(r.headers)
	delete(r.body)
	delete(r.final_url)
}

headers_get_all :: proc(r: ^Response, name: string) -> [dynamic]string {
	out: [dynamic]string
	for &h in r.headers {
		if h.name == name {
			append(&out, h.value)
		}
	}
	return out
}

headers_get_first :: proc(r: ^Response, name: string) -> (string, bool) {
	for &h in r.headers {
		if h.name == name {
			return h.value, true
		}
	}
	return "", false
}

Fetch_State :: struct {
	body:    [dynamic]u8,
	rawhead: [dynamic]u8,
}

// Transfer caps (decompressed bytes, enforced mid-transfer by aborting).
// HTML pages larger than DOC_HTML_MAX are rejected after fetch (see browser).
FETCH_BODY_MAX   :: 32 * 1024 * 1024
FETCH_HEADER_MAX :: 256 * 1024
DOC_HTML_MAX     :: 8 * 1024 * 1024

fetch_write_cb :: proc "c" (ptr: rawptr, size, nmemb: c.size_t, userdata: rawptr) -> c.size_t {
	context = runtime.default_context()
	st := (^Fetch_State)(userdata)
	n := int(size * nmemb)
	if len(st.body) + n > FETCH_BODY_MAX {
		return 0 // abort transfer (curl reports WRITE_ERROR)
	}
	append(&st.body, ..([^]u8)(ptr)[:n])
	return c.size_t(n)
}

fetch_head_cb :: proc "c" (ptr: rawptr, size, nmemb: c.size_t, userdata: rawptr) -> c.size_t {
	context = runtime.default_context()
	st := (^Fetch_State)(userdata)
	n := int(size * nmemb)
	if len(st.rawhead) + n > FETCH_HEADER_MAX {
		return 0
	}
	append(&st.rawhead, ..([^]u8)(ptr)[:n])
	return c.size_t(n)
}

// One HTTP exchange, no redirect following. Extra headers as "Name: value".
fetch_once :: proc(fc: ^Fetch_Ctx, method, url: string, extra: []string, body: []u8) -> (Response, bool) {
	r: Response
	st: Fetch_State
	defer delete(st.body)
	defer delete(st.rawhead)
	h := fc.handle
	easy_reset(h)
	easy_setopt(h, .NOSIGNAL, 1)
	easy_setopt(h, .FOLLOWLOCATION, 0)
	easy_setopt(h, .CONNECTTIMEOUT_MS, 10000)
	easy_setopt(h, .TIMEOUT_MS, 60000)
	easy_setopt(h, .ACCEPT_ENCODING, "")
	easy_setopt(h, .USERAGENT, strings.clone_to_cstring(fc.ua, context.temp_allocator))
	easy_setopt(h, .URL, strings.clone_to_cstring(url, context.temp_allocator))
	is_get := method == "GET"
	is_head := method == "HEAD"
	easy_setopt(h, .HTTPGET, 1 if is_get else 0)
	easy_setopt(h, .NOBODY, 1 if is_head else 0)
	if !is_get && !is_head {
		easy_setopt(h, .CUSTOMREQUEST, strings.clone_to_cstring(method, context.temp_allocator))
		if len(body) > 0 {
			easy_setopt(h, .POSTFIELDS, raw_data(body))
			easy_setopt(h, .POSTFIELDSIZE, len(body))
		} else {
			easy_setopt(h, .POSTFIELDSIZE, 0)
		}
	}
	easy_setopt(h, .WRITEFUNCTION, fetch_write_cb)
	easy_setopt(h, .WRITEDATA, &st)
	easy_setopt(h, .HEADERFUNCTION, fetch_head_cb)
	easy_setopt(h, .HEADERDATA, &st)
	var_hdrs: ^Curl_Slist
	defer if var_hdrs != nil {
		slist_free_all(var_hdrs)
	}
	for e in extra {
		var_hdrs = slist_append(var_hdrs, strings.clone_to_cstring(e, context.temp_allocator))
	}
	if var_hdrs != nil {
		easy_setopt(h, .HTTPHEADER, var_hdrs)
	} else {
		easy_setopt(h, .HTTPHEADER, nil)
	}
	if easy_perform(h) != .E_OK {
		if len(st.body) >= FETCH_BODY_MAX || len(st.rawhead) >= FETCH_HEADER_MAX {
			fmt.eprintfln("fetch: %s %s too large (>%d/%d bytes)", method, url, FETCH_BODY_MAX, FETCH_HEADER_MAX)
		} else {
			fmt.eprintfln("fetch: %s %s failed", method, url)
		}
		return r, false
	}
	code: c.long
	easy_getinfo(h, .RESPONSE_CODE, &code)
	r.status = int(code)
	r.final_url = strings.clone(url)
	// Parse raw headers: "Name: value" lines, lowercased names.
	for line in strings.split_lines(string(st.rawhead[:])) {
		t := strings.trim_space(line)
		if len(t) == 0 || strings.has_prefix(t, "HTTP/") {
			continue
		}
		if i := strings.index_byte(t, ':'); i >= 0 {
			append(&r.headers, Header{
				strings.to_lower(strings.trim_space(t[:i])),
				strings.clone(strings.trim_space(t[i+1:])),
			})
		}
	}
	r.body = make([]u8, len(st.body))
	copy(r.body, st.body[:])
	return r, true
}

// Full fetch with manual redirect following (max 5 hops, loop-guarded).
// Per-hop: jar cookies stored from every response, attached per request URL.
fetch_url :: proc(fc: ^Fetch_Ctx, j: ^Jar, st: ^Store, method, url: string, extra: []string, body: []u8, now: i64) -> (Response, bool) {
	cur := strings.clone(url)
	defer delete(cur)
	cur_method := method
	hists: [dynamic]string
	defer {
		for s in hists {
			delete(s)
		}
		delete(hists)
	}
	for hop in 0 ..< 6 {
		loop := false
		for s in hists {
			if s == cur {
				loop = true
			}
		}
		if loop {
			fmt.eprintfln("fetch: redirect loop at %s", cur)
			return {}, false
		}
		append(&hists, strings.clone(cur))
		u, ok := url_parse(cur)
		if !ok {
			return {}, false
		}
		// Fresh Cookie header for THIS hop's URL.
		hop_extra := make([dynamic]string, context.temp_allocator)
		for e in extra {
			append(&hop_extra, e)
		}
		if ck, has := jar_header(j, st, u.scheme, u.host, u.path, now); has {
			append(&hop_extra, strings.concatenate([]string{"Cookie: ", ck}, context.temp_allocator))
			delete(ck)
		}
		// Fragments are never sent (curl strips them too, but be explicit).
		network_url := cur
		if i := strings.index_byte(cur, '#'); i >= 0 {
			network_url = cur[:i]
		}
		r, rok := fetch_once(fc, cur_method, network_url, hop_extra[:], body)
		// Store cookies from this response before anything else.
		sc_list := headers_get_all(&r, "set-cookie")
		defer delete(sc_list)
		for sc in sc_list {
			c, cok := cookie_parse(sc, u.scheme, u.host, u.path, now)
			if cok {
				jar_store(j, st, &c, now)
				delete_cookie(&c)
			}
		}
		delete_parsed_url(&u)
		if !rok {
			delete_response(&r)
			return {}, false
		}
		r.hops = hop
		is_redir := r.status == 301 || r.status == 302 || r.status == 303 || r.status == 307 || r.status == 308
		if is_redir {
			loc, has := headers_get_first(&r, "location")
			if !has || len(loc) == 0 {
				return r, true
			}
			next, nok := url_resolve(cur, loc)
			if !nok {
				delete_response(&r)
				return {}, false
			}
			// Preserve the previous fragment when Location has none (fetch).
			if strings.index_byte(loc, '#') < 0 {
				if i := strings.index_byte(cur, '#'); i >= 0 {
					with_frag := strings.concatenate([]string{next, cur[i:]})
					delete(next)
					next = with_frag
				}
			}
			// Method rewrite per fetch semantics.
			if r.status == 303 && cur_method != "HEAD" {
				cur_method = "GET"
			} else if (r.status == 301 || r.status == 302) && cur_method == "POST" {
				cur_method = "GET"
			}
			delete_response(&r)
			delete(cur)
			cur = next
			continue
		}
		delete(r.final_url)
		r.final_url = strings.clone(cur)
		return r, true
	}
	fmt.eprintfln("fetch: too many redirects at %s", cur)
	return {}, false
}
