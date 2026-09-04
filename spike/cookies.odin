package spike

// Cookie jar (RFC 6265 subset): parse, domain/path/expiry match, public-suffix
// rejection via libpsl, SQLite persistence for non-session cookies.
// curl's cookie engine stays OFF: the jar owns policy and is unit-testable.

import "core:c"
import "core:fmt"
import "core:os"
import "core:strings"
import "core:time"

foreign import psl_lib {"system:psl"}

@(default_calling_convention = "c")
foreign psl_lib {
	// NULL ctx uses the builtin list. Returns 1 if domain is a public suffix.
	psl_is_public_suffix :: proc(psl: rawptr, domain: cstring) -> c.int ---
	// Returns 0 when the cookie domain may be set from hostname.
	psl_is_cookie_domain_acceptable :: proc(psl: rawptr, hostname: cstring, cookie_domain: cstring) -> c.int ---
	// Builtin public-suffix context (always available, no file needed).
	psl_builtin :: proc() -> rawptr ---
}

// Builtin PSL context for cookie checks.
psl_ctx :: proc() -> rawptr {
	return psl_builtin()
}

Cookie :: struct {
	host:      string, // canonical, no leading dot
	path:      string,
	name:      string,
	value:     string,
	expires:   i64, // unix; -1 = session
	created:   i64,
	secure:    bool,
	httponly:  bool,
	samesite:  string, // "", "Strict", "Lax", "None"
	host_only: bool,
}

delete_cookie :: proc(c: ^Cookie) {
	delete(c.host)
	delete(c.path)
	delete(c.name)
	delete(c.value)
	delete(c.samesite)
}

cookie_now :: proc() -> i64 {
	return time.to_unix_seconds(time.now())
}

// Domain match (RFC 6265 §5.1.3): identical, or host ends with "."+domain
// when the cookie is not host-only.
cookie_domain_match :: proc(host, cdomain: string, host_only: bool) -> bool {
	if host == cdomain {
		return true
	}
	if host_only {
		return false
	}
	if len(host) <= len(cdomain) + 1 {
		return false
	}
	return strings.has_suffix(host, cdomain) &&
		host[len(host)-len(cdomain)-1] == '.'
}

// Path match (RFC 6265 §5.1.4).
cookie_path_match :: proc(req_path, cpath: string) -> bool {
	if req_path == cpath {
		return true
	}
	if strings.has_prefix(req_path, cpath) {
		if strings.has_suffix(cpath, "/") {
			return true
		}
		if len(req_path) > len(cpath) && req_path[len(cpath)] == '/' {
			return true
		}
	}
	return false
}

psl_cstr :: proc(s: string) -> cstring {
	return strings.clone_to_cstring(s, context.temp_allocator)
}

// Parse one Set-Cookie header value from (scheme, host, req_path).
// Rejects domain mismatches and public-suffix domains. Caller owns result.
cookie_parse :: proc(set_cookie: string, scheme, host, req_path: string, now: i64) -> (Cookie, bool) {
	c: Cookie
	parts := strings.split(set_cookie, ";", context.temp_allocator)
	if len(parts) == 0 {
		return c, false
	}
	nv := strings.trim_space(parts[0])
	eq := strings.index_byte(nv, '=')
	if eq < 0 {
		return c, false // nameless cookie: ignore (spike policy)
	}
	c.name = strings.clone(strings.trim_space(nv[:eq]))
	c.value = strings.clone(strings.trim_space(nv[eq+1:]))
	if len(c.name) == 0 {
		delete_cookie(&c)
		return c, false
	}
	c.host = strings.clone(host)
	c.host_only = true
	c.path = url_default_cookie_path(req_path)
	c.expires = -1
	c.created = now
	c.samesite = strings.clone("")
	for a in parts[1:] {
		attr := strings.trim_space(a)
		aname, aval := attr, ""
		if i := strings.index_byte(attr, '='); i >= 0 {
			aname = strings.trim_space(attr[:i])
			aval = strings.trim_space(attr[i+1:])
		}
		lower := strings.to_lower(aname, context.temp_allocator)
		switch lower {
		case "expires":
			if t, ok := http_date_parse(aval); ok {
				c.expires = i64(t)
			}
		case "max-age":
			neg := len(aval) > 0 && aval[0] == '-'
			digits := aval[1:] if neg else aval
			if n, ok := atoi_n(digits); ok {
				c.expires = now - 1 if neg else now + i64(n)
			}
		case "domain":
			d := strings.to_lower(strings.trim_prefix(aval, "."), context.temp_allocator)
			if len(d) == 0 || !cookie_domain_match(host, d, false) {
				delete_cookie(&c)
				return c, false
			}
			// Public suffixes must never be set (supercookie defense).
			if psl_is_public_suffix(psl_ctx(), psl_cstr(d)) != 0 {
				delete_cookie(&c)
				return c, false
			}
			// libpsl: nonzero = acceptable, 0 = reject (public suffix etc).
			if psl_is_cookie_domain_acceptable(psl_ctx(), psl_cstr(host), psl_cstr(d)) == 0 {
				delete_cookie(&c)
				return c, false
			}
			delete(c.host)
			c.host = strings.clone(d)
			c.host_only = false
		case "path":
			if len(aval) > 0 && aval[0] == '/' {
				delete(c.path)
				c.path = strings.clone(aval)
			}
		case "secure":
			c.secure = true
		case "httponly":
			c.httponly = true
		case "samesite":
			v := strings.to_lower(aval, context.temp_allocator)
			delete(c.samesite)
			switch v {
			case "strict":
				c.samesite = strings.clone("Strict")
			case "lax":
				c.samesite = strings.clone("Lax")
			case "none":
				c.samesite = strings.clone("None")
			case:
				c.samesite = strings.clone("")
			}
		}
	}
	// SameSite=None requires Secure (modern baseline).
	if c.samesite == "None" && !c.secure {
		delete_cookie(&c)
		return c, false
	}
	if c.secure && scheme != "https" {
		// Secure cookies can be SET over http only from trustworthy contexts;
		// spike policy: accept but never send (send path enforces https).
	}
	return c, true
}

cookie_is_expired :: proc(c: ^Cookie, now: i64) -> bool {
	return c.expires >= 0 && c.expires <= now
}

cookie_sendable :: proc(c: ^Cookie, scheme, host, path: string, now: i64) -> bool {
	if cookie_is_expired(c, now) {
		return false
	}
	if c.secure && scheme != "https" {
		return false
	}
	if !cookie_domain_match(host, c.host, c.host_only) {
		return false
	}
	return cookie_path_match(path, c.path)
}

Jar :: struct {
	st:      ^Store,
	session: [dynamic]Cookie, // session cookies (memory only)
}

jar_open :: proc(st: ^Store) -> Jar {
	return Jar{st, nil}
}

jar_close :: proc(j: ^Jar) {
	for &c in j.session {
		delete_cookie(&c)
	}
	delete(j.session)
}

// All cookies visible to (scheme, host, path), longest-path-first.
jar_for_request :: proc(j: ^Jar, st: ^Store, scheme, host, path: string, now: i64) -> [dynamic]Cookie {
	out: [dynamic]Cookie
	collect := proc(out: ^[dynamic]Cookie, c: ^Cookie, scheme, host, path: string, now: i64) {
		if !cookie_sendable(c, scheme, host, path, now) {
			return
		}
		for &o in out {
			if o.name == c.name && o.host == c.host && o.path == c.path {
				return
			}
		}
		nc: Cookie
		nc.host = strings.clone(c.host)
		nc.path = strings.clone(c.path)
		nc.name = strings.clone(c.name)
		nc.value = strings.clone(c.value)
		nc.expires = c.expires
		nc.created = c.created
		nc.secure = c.secure
		nc.httponly = c.httponly
		nc.samesite = strings.clone(c.samesite)
		nc.host_only = c.host_only
		append(out, nc)
	}
	for &c in j.session {
		collect(&out, &c, scheme, host, path, now)
	}
	// Persistent cookies from SQLite.
	stmt := store_prepare(st, "SELECT host,path,name,value,expires,created,secure,httponly,samesite,host_only FROM cookies")
	if stmt != nil {
		defer sqlite3_finalize(stmt)
		for sqlite3_step(stmt) == SQLITE_ROW {
			c: Cookie
			c.host = col_text(stmt, 0)
			c.path = col_text(stmt, 1)
			c.name = col_text(stmt, 2)
			c.value = col_text(stmt, 3)
			c.expires = sqlite3_column_int64(stmt, 4)
			c.created = sqlite3_column_int64(stmt, 5)
			c.secure = sqlite3_column_int64(stmt, 6) != 0
			c.httponly = sqlite3_column_int64(stmt, 7) != 0
			c.samesite = col_text(stmt, 8)
			c.host_only = sqlite3_column_int64(stmt, 9) != 0
			collect(&out, &c, scheme, host, path, now)
			delete_cookie(&c)
		}
	}
	// RFC 6265 §5.4: longer paths first, then earlier creation.
	n := len(out)
	for i in 1 ..< n {
		for k := i; k > 0; k -= 1 {
			a, b := &out[k-1], &out[k]
			swap := len(b.path) > len(a.path) ||
				(len(b.path) == len(a.path) && b.created < a.created)
			if !swap {
				break
			}
			a^, b^ = b^, a^
		}
	}
	return out
}

col_text :: proc(stmt: ^Sqlite_Stmt, col: int) -> string {
	ptr := sqlite3_column_text(stmt, c.int(col))
	if ptr == nil {
		return strings.clone("")
	}
	n := int(sqlite3_column_bytes(stmt, c.int(col)))
	return strings.clone(string(ptr[:n]))
}

// Store one parsed cookie: session jar or SQLite; expired value deletes.
jar_store :: proc(j: ^Jar, st: ^Store, c: ^Cookie, now: i64) {
	if cookie_is_expired(c, now) {
		jar_delete(j, st, c.host, c.path, c.name)
		return
	}
	if c.expires < 0 {
		for &s in j.session {
			if s.host == c.host && s.path == c.path && s.name == c.name {
				delete_cookie(&s)
				s = c^
				// c now aliased into session; caller must not free.
				c.host, c.path, c.name, c.value, c.samesite = "", "", "", "", ""
				return
			}
		}
		append(&j.session, c^)
		c.host, c.path, c.name, c.value, c.samesite = "", "", "", "", ""
		return
	}
	stmt := store_prepare(st, "INSERT OR REPLACE INTO cookies(host,path,name,value,expires,created,secure,httponly,samesite,host_only) VALUES(?,?,?,?,?,?,?,?,?,?)")
	if stmt == nil {
		return
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, c.host)
	bind_text_copy(stmt, 2, c.path)
	bind_text_copy(stmt, 3, c.name)
	bind_text_copy(stmt, 4, c.value)
	sqlite3_bind_int64(stmt, 5, c.expires)
	sqlite3_bind_int64(stmt, 6, c.created)
	sqlite3_bind_int64(stmt, 7, 1 if c.secure else 0)
	sqlite3_bind_int64(stmt, 8, 1 if c.httponly else 0)
	bind_text_copy(stmt, 9, c.samesite)
	sqlite3_bind_int64(stmt, 10, 1 if c.host_only else 0)
	sqlite3_step(stmt)
}

jar_delete :: proc(j: ^Jar, st: ^Store, host, path, name: string) {
	for i := len(j.session) - 1; i >= 0; i -= 1 {
		s := &j.session[i]
		if s.host == host && s.path == path && s.name == name {
			delete_cookie(s)
			ordered_remove(&j.session, i)
		}
	}
	stmt := store_prepare(st, "DELETE FROM cookies WHERE host=? AND path=? AND name=?")
	if stmt == nil {
		return
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, host)
	bind_text_copy(stmt, 2, path)
	bind_text_copy(stmt, 3, name)
	sqlite3_step(stmt)
}

// Prune expired persistent cookies. Returns rows removed.
jar_prune :: proc(j: ^Jar, st: ^Store, now: i64) -> int {
	for i := len(j.session) - 1; i >= 0; i -= 1 {
		if cookie_is_expired(&j.session[i], now) {
			delete_cookie(&j.session[i])
			ordered_remove(&j.session, i)
		}
	}
	stmt := store_prepare(st, "DELETE FROM cookies WHERE expires >= 0 AND expires <= ?")
	if stmt == nil {
		return 0
	}
	defer sqlite3_finalize(stmt)
	sqlite3_bind_int64(stmt, 1, now)
	sqlite3_step(stmt)
	return int(sqlite3_changes(st.db))
}

// Build the Cookie header value for a request. Caller deletes.
jar_header :: proc(j: ^Jar, st: ^Store, scheme, host, path: string, now: i64) -> (string, bool) {
	cs := jar_for_request(j, st, scheme, host, path, now)
	defer {
		for &c in cs {
			delete_cookie(&c)
		}
		delete(cs)
	}
	if len(cs) == 0 {
		return "", false
	}
	b: strings.Builder
	strings.builder_init(&b)
	defer delete(b.buf)
	for c, i in cs {
		if i > 0 {
			strings.write_string(&b, "; ")
		}
		strings.write_string(&b, c.name)
		strings.write_byte(&b, '=')
		strings.write_string(&b, c.value)
	}
	return strings.clone(strings.to_string(b)), true
}
