package spike

// HTTP cache (RFC 9111 subset): memory LRU tier + disk tier (SQLite index,
// content-hashed body files). GET only. Private cache.
// Freshness: max-age, else Expires-Date; no heuristic freshness.
// Revalidation: ETag (If-None-Match) and Last-Modified (If-Modified-Since).
// Vary is honored. no-store bypasses; no-cache forces revalidation.

import "core:fmt"
import "core:hash"
import "core:os"
import "core:strings"
import "core:time"

MEM_CACHE_MAX   :: 32 * 1024 * 1024
DISK_CACHE_MAX  :: 256 * 1024 * 1024
SMALL_BODY_MAX  :: 256 * 1024 // bodies above this always live on disk

Cache_Entry :: struct {
	url:        string,
	status:     int,
	headers:    [dynamic]Header,
	etag:       string,
	lastmod:    string,
	fetched_at: i64,
	max_age:    i64, // -1 = must revalidate / stale immediately
	vary:       string,
	req_vary:   string, // "name=value\n..." of the stored request's varying fields
	body:       []u8,  // memory tier only
	body_path:  string, // disk tier only (relative to profile)
	size:       int,
	accessed:   i64,
}

delete_cache_entry :: proc(e: ^Cache_Entry) {
	delete(e.url)
	for &h in e.headers {
		delete(h.name)
		delete(h.value)
	}
	delete(e.headers)
	delete(e.etag)
	delete(e.lastmod)
	delete(e.vary)
	delete(e.req_vary)
	delete(e.body)
	delete(e.body_path)
}

Cache :: struct {
	st:      ^Store,
	mem:     map[string]^Cache_Entry, // keyed by cache key
	mem_use: int,
	order:   [dynamic]string, // LRU: front = oldest
}

cache_open :: proc(st: ^Store) -> Cache {
	c: Cache
	c.st = st
	c.mem = make(map[string]^Cache_Entry)
	// Startup sweep: drop rows whose body files vanished; enforce disk cap.
	stmt := store_prepare(st, "SELECT url, body_path, size FROM cache")
	if stmt != nil {
		missing: [dynamic]string
		defer delete(missing)
		for sqlite3_step(stmt) == SQLITE_ROW {
			p := col_text(stmt, 1)
			defer delete(p)
			full := fmt.aprintf("%s/%s", st.dir, p)
			defer delete(full)
			if !os.exists(full) {
				u := col_text(stmt, 0)
				defer delete(u)
				append(&missing, strings.clone(u))
			}
		}
		sqlite3_finalize(stmt)
		for u in missing {
			d := store_prepare(st, "DELETE FROM cache WHERE url=?")
			if d != nil {
				bind_text_copy(d, 1, u)
				sqlite3_step(d)
				sqlite3_finalize(d)
			}
			delete(u)
		}
	}
	cache_enforce_disk_cap(st)
	return c
}

cache_close :: proc(c: ^Cache) {
	for _, e in c.mem {
		delete_cache_entry(e)
		free(e)
	}
	delete(c.mem)
	for s in c.order {
		delete(s)
	}
	delete(c.order)
}

// FNV-128-ish content name: two seeded FNV-64a hashes, hex.
cache_body_name :: proc(key: string) -> string {
	h1 := hash.fnv64a(transmute([]u8)key)
	h2 := hash.fnv64a(transmute([]u8)key, 0x9e3779b97f4a7c15)
	return fmt.aprintf("%16x%16x.body", h1, h2)
}

cache_touch :: proc(c: ^Cache, key: string) {
	for s, i in c.order {
		if s == key {
			ordered_remove(&c.order, i)
			break
		}
	}
	append(&c.order, strings.clone(key))
}

cache_mem_evict :: proc(c: ^Cache) {
	for c.mem_use > MEM_CACHE_MAX && len(c.order) > 0 {
		old := c.order[0]
		ordered_remove(&c.order, 0)
		if e, ok := c.mem[old]; ok {
			c.mem_use -= e.size
			delete_cache_entry(e)
			free(e)
			delete_key(&c.mem, old)
		}
		delete(old)
	}
}

cache_mem_put :: proc(c: ^Cache, e: ^Cache_Entry) {
	// Map key aliases e.url (owned by the entry): remove the slot BEFORE
	// freeing any replaced entry so no dangling key remains.
	if old, ok := c.mem[e.url]; ok {
		c.mem_use -= old.size
		delete_key(&c.mem, e.url)
		delete_cache_entry(old)
		free(old)
	}
	c.mem[e.url] = e
	c.mem_use += e.size
	cache_touch(c, e.url)
	cache_mem_evict(c)
}

cache_mem_get :: proc(c: ^Cache, key: string, now: i64) -> ^Cache_Entry {
	e, ok := c.mem[key]
	if !ok {
		return nil
	}
	e.accessed = now
	cache_touch(c, key)
	return e
}

// ---- freshness ----

cache_directives :: proc(headers: [dynamic]Header) -> (no_store, no_cache: bool, max_age: i64) {
	max_age = -1
	for &h in headers {
		if h.name != "cache-control" {
			continue
		}
		for d in strings.split(h.value, ",", context.temp_allocator) {
			t := strings.trim_space(d)
			lower := strings.to_lower(t, context.temp_allocator)
			if lower == "no-store" {
				no_store = true
			} else if lower == "no-cache" {
				no_cache = true
			} else if strings.has_prefix(lower, "max-age=") {
				if n, ok := atoi_n(strings.trim_space(t[8:])); ok && n >= 0 {
					max_age = i64(n)
				}
			} else if strings.has_prefix(lower, "s-maxage=") {
				if n, ok := atoi_n(strings.trim_space(t[9:])); ok && n >= 0 {
					max_age = i64(n)
				}
			}
		}
	}
	return
}

cache_is_fresh :: proc(e: ^Cache_Entry, now: i64) -> bool {
	if e.max_age < 0 {
		return false
	}
	return now < e.fetched_at + e.max_age
}

cache_computable_age :: proc(headers: [dynamic]Header, now: i64) -> i64 {
	_, _, max_age := cache_directives(headers)
	if max_age >= 0 {
		return max_age
	}
	date_s, has_date := "", false
	exp_s, has_exp := "", false
	for &h in headers {
		if h.name == "date" && !has_date {
			date_s, has_date = h.value, true
		}
		if h.name == "expires" && !has_exp {
			exp_s, has_exp = h.value, true
		}
	}
	if has_exp {
		base := now
		if has_date {
			if t, ok := http_date_parse(date_s); ok {
				base = i64(t)
			}
		}
		if t, ok := http_date_parse(exp_s); ok && i64(t) > base {
			return i64(t) - base
		}
		return 0 // expired/invalid Expires with no max-age: immediately stale
	}
	return -1
}

// ---- vary ----

vary_key_of :: proc(vary: string, req: []string) -> string {
	// req: "Name: value" of the outgoing request.
	if len(vary) == 0 || strings.trim_space(vary) == "*" {
		return strings.clone(vary)
	}
	b: strings.Builder
	strings.builder_init(&b)
	defer delete(b.buf)
	for f in strings.split(vary, ",", context.temp_allocator) {
		name := strings.to_lower(strings.trim_space(f), context.temp_allocator)
		if len(name) == 0 {
			continue
		}
		val := ""
		for r in req {
			if i := strings.index_byte(r, ':'); i >= 0 {
				if strings.to_lower(strings.trim_space(r[:i]), context.temp_allocator) == name {
					val = strings.trim_space(r[i+1:])
					break
				}
			}
		}
		fmt.sbprintf(&b, "%s=%s\n", name, val)
	}
	return strings.clone(strings.to_string(b))
}

// ---- disk tier ----

cache_disk_load :: proc(c: ^Cache, key: string, now: i64) -> ^Cache_Entry {
	stmt := store_prepare(c.st, "SELECT status,headers,etag,lastmod,fetched_at,max_age,vary,req_vary,body_path,size FROM cache WHERE url=?")
	if stmt == nil {
		return nil
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, key)
	if sqlite3_step(stmt) != SQLITE_ROW {
		return nil
	}
	e := new(Cache_Entry)
	e.url = strings.clone(key)
	e.status = int(sqlite3_column_int64(stmt, 0))
	hdrs := col_text(stmt, 1)
	defer delete(hdrs)
	for line in strings.split_lines(hdrs) {
		if i := strings.index_byte(line, ':'); i >= 0 {
			append(&e.headers, Header{strings.clone(line[:i]), strings.clone(line[i+1:])})
		}
	}
	e.etag = col_text(stmt, 2)
	e.lastmod = col_text(stmt, 3)
	e.fetched_at = sqlite3_column_int64(stmt, 4)
	e.max_age = sqlite3_column_int64(stmt, 5)
	e.vary = col_text(stmt, 6)
	e.req_vary = col_text(stmt, 7)
	e.body_path = col_text(stmt, 8)
	e.size = int(sqlite3_column_int64(stmt, 9))
	e.accessed = now
	full := fmt.aprintf("%s/%s", c.st.dir, e.body_path)
	defer delete(full)
	body, err := os.read_entire_file_from_path(full, context.allocator)
	if err != nil {
		delete_cache_entry(e)
		free(e)
		return nil
	}
	e.body = body
	// Touch access time + promote small entries to memory.
	touch := store_prepare(c.st, "UPDATE cache SET accessed=? WHERE url=?")
	if touch != nil {
		sqlite3_bind_int64(touch, 1, now)
		bind_text_copy(touch, 2, key)
		sqlite3_step(touch)
		sqlite3_finalize(touch)
	}
	if e.size <= MEM_CACHE_MAX / 4 {
		cache_mem_put(c, e)
		return cache_mem_get(c, key, now)
	}
	return e
}

headers_serialize :: proc(headers: [dynamic]Header) -> string {
	b: strings.Builder
	strings.builder_init(&b)
	defer delete(b.buf)
	for &h in headers {
		fmt.sbprintf(&b, "%s:%s\n", h.name, h.value)
	}
	return strings.clone(strings.to_string(b))
}

cache_disk_store :: proc(c: ^Cache, key: string, status: int, headers: [dynamic]Header, etag, lastmod: string, fetched_at, max_age: i64, vary, req_vary: string, body: []u8, now: i64) {
	name := cache_body_name(key)
	defer delete(name)
	sub := fmt.aprintf("cache/%s/%s", name[:2], name)
	defer delete(sub)
	dir := fmt.aprintf("%s/cache/%s", c.st.dir, name[:2])
	defer delete(dir)
	os.make_directory(dir)
	full := fmt.aprintf("%s/%s", c.st.dir, sub)
	defer delete(full)
	if err := os.write_entire_file(full, body); err != nil {
		return
	}
	hdrs := headers_serialize(headers)
	defer delete(hdrs)
	stmt := store_prepare(c.st, "INSERT OR REPLACE INTO cache(url,status,headers,etag,lastmod,fetched_at,max_age,vary,req_vary,body_path,size,accessed) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")
	if stmt == nil {
		return
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, key)
	sqlite3_bind_int64(stmt, 2, i64(status))
	bind_text_copy(stmt, 3, hdrs)
	bind_text_copy(stmt, 4, etag)
	bind_text_copy(stmt, 5, lastmod)
	sqlite3_bind_int64(stmt, 6, i64(fetched_at))
	sqlite3_bind_int64(stmt, 7, i64(max_age))
	bind_text_copy(stmt, 8, vary)
	bind_text_copy(stmt, 9, req_vary)
	bind_text_copy(stmt, 10, sub)
	sqlite3_bind_int64(stmt, 11, i64(len(body)))
	sqlite3_bind_int64(stmt, 12, now)
	sqlite3_step(stmt)
	cache_enforce_disk_cap(c.st)
}

cache_enforce_disk_cap :: proc(st: ^Store) {
	for {
		s := store_prepare(st, "SELECT COALESCE(SUM(size),0) FROM cache")
		if s == nil {
			return
		}
		total := 0
		if sqlite3_step(s) == SQLITE_ROW {
			total = int(sqlite3_column_int64(s, 0))
		}
		sqlite3_finalize(s)
		if total <= DISK_CACHE_MAX {
			return
		}
		v := store_prepare(st, "SELECT url,body_path FROM cache ORDER BY accessed ASC LIMIT 8")
		if v == nil {
			return
		}
		victims: [dynamic]string
		paths: [dynamic]string
		for sqlite3_step(v) == SQLITE_ROW {
			append(&victims, col_text(v, 0))
			append(&paths, col_text(v, 1))
		}
		sqlite3_finalize(v)
		if len(victims) == 0 {
			break
		}
		for i in 0 ..< len(victims) {
			full := fmt.aprintf("%s/%s", st.dir, paths[i])
			os.remove(full)
			delete(full)
			d := store_prepare(st, "DELETE FROM cache WHERE url=?")
			if d != nil {
				bind_text_copy(d, 1, victims[i])
				sqlite3_step(d)
				sqlite3_finalize(d)
			}
			delete(victims[i])
			delete(paths[i])
		}
		delete(victims)
		delete(paths)
	}
}

// ---- integrated fetch: memory -> disk -> network (+revalidate/store) ----

Cached_Info :: struct {
	from_cache:  bool,
	revalidated: bool,
	hops:        int,
}

// GET through the cache. extra are outgoing "Name: value" headers.
// Returns an owned Response in all cases.
cached_fetch :: proc(
	c: ^Cache,
	fc: ^Fetch_Ctx,
	j: ^Jar,
	method, url: string,
	extra: []string,
	body: []u8,
	now: i64,
) -> (
	resp: Response,
	info: Cached_Info,
	ok: bool,
) {
	key := ""
	key_owned := false
	if method == "GET" {
		if u, uok := url_parse(url); uok {
			key = url_cache_key(&u)
			delete_parsed_url(&u)
			key_owned = true
		} else {
			return resp, info, false
		}
	}
	defer if key_owned {
		delete(key)
	}
	if len(key) > 0 {
		if hit, found := cache_lookup(c, key, extra, now); found {
			defer cache_release(c, &hit)
			if hit.fresh {
				resp.status = hit.entry.status
				for &h in hit.entry.headers {
					append(&resp.headers, Header{strings.clone(h.name), strings.clone(h.value)})
				}
				resp.body = make([]u8, len(hit.entry.body))
				copy(resp.body, hit.entry.body)
				resp.final_url = strings.clone(url)
				info.from_cache = true
				return resp, info, true
			}
			// Stale: revalidate when possible.
			if hit.entry.etag != "" || hit.entry.lastmod != "" {
				vextra := make([dynamic]string, context.temp_allocator)
				for e in extra {
					append(&vextra, e)
				}
				if hit.entry.etag != "" {
					append(&vextra, strings.concatenate([]string{"If-None-Match: ", hit.entry.etag}, context.temp_allocator))
				}
				if hit.entry.lastmod != "" {
					append(&vextra, strings.concatenate([]string{"If-Modified-Since: ", hit.entry.lastmod}, context.temp_allocator))
				}
				nr, nok := fetch_url(fc, j, c.st, method, url, vextra[:], body, now)
				if !nok {
					return resp, info, false
				}
				if nr.status == 304 {
					// Refresh stored freshness/headers, serve stored body.
					_, _, nm := cache_directives(nr.headers)
					if nm < 0 {
						nm = hit.entry.max_age
					}
					hit.entry.max_age = nm
					hit.entry.fetched_at = now
					for &h in nr.headers {
						set_header(&hit.entry.headers, h.name, h.value)
					}
					cache_refresh_entry(c, hit.entry, now)
					resp.status = hit.entry.status
					for &h in hit.entry.headers {
						append(&resp.headers, Header{strings.clone(h.name), strings.clone(h.value)})
					}
					resp.body = make([]u8, len(hit.entry.body))
					copy(resp.body, hit.entry.body)
					resp.final_url = strings.clone(url)
					info.from_cache = true
					info.revalidated = true
					info.hops = nr.hops
					delete_response(&nr)
					return resp, info, true
				}
				maybe_store(c, key, &nr, extra, body, now)
				info.hops = nr.hops
				return nr, info, true
			}
			// Stale without validators: fall through to plain network fetch.
		}
	}
	nr, nok := fetch_url(fc, j, c.st, method, url, extra, body, now)
	if !nok {
		return resp, info, false
	}
	maybe_store(c, key, &nr, extra, body, now)
	info.hops = nr.hops
	return nr, info, true
}

// Replace-or-add a header in place.
set_header :: proc(headers: ^[dynamic]Header, name, value: string) {
	for &h in headers {
		if h.name == name {
			delete(h.value)
			h.value = strings.clone(value)
			return
		}
	}
	append(headers, Header{strings.clone(name), strings.clone(value)})
}

// Persist a refreshed entry's headers/freshness to SQLite.
cache_refresh_entry :: proc(c: ^Cache, e: ^Cache_Entry, now: i64) {
	hdrs := headers_serialize(e.headers)
	defer delete(hdrs)
	stmt := store_prepare(c.st, "UPDATE cache SET headers=?,fetched_at=?,max_age=?,accessed=? WHERE url=?")
	if stmt == nil {
		return
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, hdrs)
	sqlite3_bind_int64(stmt, 2, e.fetched_at)
	sqlite3_bind_int64(stmt, 3, e.max_age)
	sqlite3_bind_int64(stmt, 4, now)
	bind_text_copy(stmt, 5, e.url)
	sqlite3_step(stmt)
	e.accessed = now
	if _, inmem := c.mem[e.url]; !inmem {
		cache_touch(c, e.url)
	}
}

// Store a network response when cacheable. Always takes copies.
maybe_store :: proc(c: ^Cache, key: string, r: ^Response, req_extra: []string, body: []u8, now: i64) {
	if len(key) == 0 || r.status != 200 {
		return
	}
	no_store, _, _ := cache_directives(r.headers)
	if no_store {
		return
	}
	etag, _ := headers_get_first(r, "etag")
	lastmod, _ := headers_get_first(r, "last-modified")
	vary, _ := headers_get_first(r, "vary")
	max_age := cache_computable_age(r.headers, now)
	req_vary := vary_key_of(vary, req_extra)
	defer delete(req_vary)
	// Memory tier for modest bodies.
	if len(r.body) <= MEM_CACHE_MAX / 4 {
		e := new(Cache_Entry)
		e.url = strings.clone(key)
		e.status = r.status
		for &h in r.headers {
			append(&e.headers, Header{strings.clone(h.name), strings.clone(h.value)})
		}
		e.etag = strings.clone(etag)
		e.lastmod = strings.clone(lastmod)
		e.fetched_at = now
		e.max_age = max_age
		e.vary = strings.clone(vary)
		e.req_vary = strings.clone(req_vary)
		e.body = make([]u8, len(r.body))
		copy(e.body, r.body)
		e.size = len(r.body)
		e.accessed = now
		cache_mem_put(c, e)
	}
	cache_disk_store(c, key, r.status, r.headers, etag, lastmod, now, max_age, vary, req_vary, r.body, now)
}

Cache_Hit :: struct {
	entry:   ^Cache_Entry,
	owned:   bool, // true when caller must free (disk-tier direct)
	fresh:   bool,
}

cache_lookup :: proc(c: ^Cache, key: string, req_headers: []string, now: i64) -> (Cache_Hit, bool) {
	hit: Cache_Hit
	e := cache_mem_get(c, key, now)
	owned := false
	if e == nil {
		e = cache_disk_load(c, key, now)
		if e == nil {
			return hit, false
		}
		// Disk tier returned a promoted (memory-owned) or direct entry.
		if _, inmem := c.mem[key]; inmem {
			e = cache_mem_get(c, key, now)
		} else {
			owned = true
		}
	}
	// Vary check.
	if strings.trim_space(e.vary) == "*" {
		if owned {
			delete_cache_entry(e)
			free(e)
		}
		return hit, false
	}
	if len(e.vary) > 0 {
		want := vary_key_of(e.vary, req_headers)
		defer delete(want)
		if want != e.req_vary {
			if owned {
				delete_cache_entry(e)
				free(e)
			}
			return hit, false
		}
	}
	hit.entry = e
	hit.owned = owned
	hit.fresh = cache_is_fresh(e, now)
	return hit, true
}

cache_release :: proc(c: ^Cache, hit: ^Cache_Hit) {
	if hit.owned {
		delete_cache_entry(hit.entry)
		free(hit.entry)
		hit.entry = nil
	}
}
