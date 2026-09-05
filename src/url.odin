package vixen

// Minimal URL parser/normalizer/resolver (RFC 3986 §5). Owned (not curl's
// URL API) because cookie domains, cache keys, history display, and link
// following all need the same normalization. Absolute http(s) only for fetch.

import "core:fmt"
import "core:strings"

Parsed_Url :: struct {
	scheme:   string, // lowercased
	userinfo: string,
	host:     string, // lowercased, no brackets
	port:     int,    // -1 = default
	path:     string, // always starts with /
	query:    string,
	fragment: string,
}

delete_parsed_url :: proc(u: ^Parsed_Url) {
	delete(u.scheme)
	delete(u.userinfo)
	delete(u.host)
	delete(u.path)
	delete(u.query)
	delete(u.fragment)
}

url_default_port :: proc(scheme: string) -> int {
	switch scheme {
	case "http":
		return 80
	case "https":
		return 443
	}
	return -1
}

url_parse :: proc(raw: string) -> (Parsed_Url, bool) {
	u: Parsed_Url
	rest := raw
	// Scheme.
	if i := strings.index(rest, "://"); i >= 0 {
		u.scheme = strings.to_lower(rest[:i])
		rest = rest[i+3:]
	} else {
		return u, false
	}
	if u.scheme != "http" && u.scheme != "https" {
		delete_parsed_url(&u)
		return u, false
	}
	// Fragment.
	if i := strings.index_byte(rest, '#'); i >= 0 {
		u.fragment = strings.clone(rest[i+1:])
		rest = rest[:i]
	}
	// Query.
	if i := strings.index_byte(rest, '?'); i >= 0 {
		u.query = strings.clone(rest[i+1:])
		rest = rest[:i]
	}
	// Authority + path.
	path_i := strings.index_byte(rest, '/')
	authority := rest
	if path_i >= 0 {
		authority = rest[:path_i]
		u.path = strings.clone(rest[path_i:])
	} else {
		u.path = strings.clone("/")
	}
	// Userinfo.
	if i := strings.index_byte(authority, '@'); i >= 0 {
		u.userinfo = strings.clone(authority[:i])
		authority = authority[i+1:]
	}
	// Host + port (bracketed IPv6 or last-colon split).
	u.port = -1
	host := authority
	if strings.has_prefix(host, "[") {
		if end := strings.index_byte(host, ']'); end >= 0 {
			u.host = strings.to_lower(host[1:end])
			restp := host[end+1:]
			if strings.has_prefix(restp, ":") {
				u.port = parse_int_or(restp[1:], -1)
			}
		} else {
			delete_parsed_url(&u)
			return u, false
		}
	} else if i := strings.last_index_byte(host, ':'); i >= 0 {
		// Single colon (or host:port with no other colons; unbracketed
		// IPv6 is rejected to keep parsing total).
		if strings.index_byte(host, ':') == i {
			u.host = strings.to_lower(host[:i])
			u.port = parse_int_or(host[i+1:], -1)
			if u.port < 0 {
				delete_parsed_url(&u)
				return u, false
			}
		} else {
			delete_parsed_url(&u)
			return u, false
		}
	} else {
		u.host = strings.to_lower(host)
	}
	if len(u.host) == 0 {
		delete_parsed_url(&u)
		return u, false
	}
	if u.port == url_default_port(u.scheme) {
		u.port = -1
	}
	return u, true
}

// Serialize without fragment (network form).
url_serialize :: proc(u: ^Parsed_Url) -> string {
	b: strings.Builder
	strings.builder_init(&b)
	defer delete(b.buf)
	fmt.sbprintf(&b, "%s://", u.scheme)
	if len(u.userinfo) > 0 {
		fmt.sbprintf(&b, "%s@", u.userinfo)
	}
	if strings.index_byte(u.host, ':') >= 0 {
		fmt.sbprintf(&b, "[%s]", u.host)
	} else {
		strings.write_string(&b, u.host)
	}
	if u.port >= 0 {
		fmt.sbprintf(&b, ":%d", u.port)
	}
	strings.write_string(&b, u.path)
	if len(u.query) > 0 {
		fmt.sbprintf(&b, "?%s", u.query)
	}
	return strings.clone(strings.to_string(b))
}

// Cache/key identity: scheme://host[:port]/path?query (no userinfo/fragment).
url_cache_key :: proc(u: ^Parsed_Url) -> string {
	b: strings.Builder
	strings.builder_init(&b)
	defer delete(b.buf)
	fmt.sbprintf(&b, "%s://%s", u.scheme, u.host)
	if u.port >= 0 {
		fmt.sbprintf(&b, ":%d", u.port)
	}
	strings.write_string(&b, u.path)
	if len(u.query) > 0 {
		fmt.sbprintf(&b, "?%s", u.query)
	}
	return strings.clone(strings.to_string(b))
}

// RFC 3986 §5.2 reference resolution. Returns an owned absolute URL string.
url_resolve :: proc(base, ref: string) -> (string, bool) {
	// Absolute URI: scheme prefix (strict) or network-path reference.
	if is_scheme_prefix(ref) {
		return strings.clone(ref), true
	}
	if strings.has_prefix(ref, "//") {
		b, ok := url_parse(base)
		if !ok {
			return "", false
		}
		defer delete_parsed_url(&b)
		abs := strings.concatenate([]string{b.scheme, "://", ref[2:]}, context.temp_allocator)
		nb, nok := url_parse(abs)
		if !nok {
			return "", false
		}
		defer delete_parsed_url(&nb)
		return url_serialize(&nb), true
	}
	nb, ok := url_parse(base)
	if !ok {
		return "", false
	}
	defer delete_parsed_url(&nb)
	path, query, frag := "", "", ""
	// Split ref into path/query/fragment.
	r := ref
	if i := strings.index_byte(r, '#'); i >= 0 {
		frag = strings.clone(r[i+1:])
		r = r[:i]
	}
	rpath := r
	has_query := false
	if i := strings.index_byte(r, '?'); i >= 0 {
		query = strings.clone(r[i+1:])
		has_query = true
		rpath = r[:i]
	}
	if rpath == "" {
		path = strings.clone(nb.path)
		if !has_query {
			query = strings.clone(nb.query)
		}
	} else if strings.has_prefix(rpath, "/") {
		path = remove_dot_segments(rpath)
	} else {
		merged := merge_paths(nb.path, rpath)
		defer delete(merged)
		path = remove_dot_segments(merged)
	}
	res := Parsed_Url{
		strings.clone(nb.scheme),
		strings.clone(nb.userinfo),
		strings.clone(nb.host),
		nb.port,
		path,
		query,
		frag,
	}
	defer delete_parsed_url(&res)
	return url_serialize(&res), true
}

merge_paths :: proc(base_path, ref_path: string) -> string {
	if i := strings.last_index_byte(base_path, '/'); i >= 0 {
		return strings.concatenate([]string{base_path[:i+1], ref_path})
	}
	return strings.clone(ref_path)
}

// RFC 3986 §5.2.4 dot-segment removal.
remove_dot_segments :: proc(path: string) -> string {
	segs := strings.split(path, "/", context.temp_allocator)
	out: [dynamic]string
	defer delete(out)
	leading_slash := strings.has_prefix(path, "/")
	for s in segs {
		switch s {
		case "", ".":
			continue
		case "..":
			if len(out) > 0 {
				pop(&out)
			}
		case:
			append(&out, s)
		}
	}
	b: strings.Builder
	strings.builder_init(&b, context.temp_allocator)
	if leading_slash {
		strings.write_byte(&b, '/')
	}
	for s, i in out {
		if i > 0 {
			strings.write_byte(&b, '/')
		}
		strings.write_string(&b, s)
	}
	if strings.has_suffix(path, "/") && (len(out) > 0 || !leading_slash) {
		strings.write_byte(&b, '/')
	}
	return strings.clone(strings.to_string(b))
}

// scheme ":" — strict absolute-URI detection (RFC 3986 §5.2.2).
is_scheme_prefix :: proc(s: string) -> bool {
	i := 0
	if len(s) == 0 {
		return false
	}
	c := s[0]
	if !((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')) {
		return false
	}
	for i = 1; i < len(s); i += 1 {
		c = s[i]
		if c == ':' {
			return true
		}
		if !((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '+' || c == '-' || c == '.') {
			return false
		}
	}
	return false
}

// Default cookie path (RFC 6265 §5.1.4): directory of the request path.
url_default_cookie_path :: proc(path: string) -> string {
	if len(path) == 0 || path[0] != '/' {
		return strings.clone("/")
	}
	if path == "/" {
		return strings.clone("/")
	}
	if i := strings.last_index_byte(path, '/'); i > 0 {
		return strings.clone(path[:i])
	}
	return strings.clone("/")
}
