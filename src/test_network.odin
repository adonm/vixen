package vixen

// Network test suite: deterministic local HTTP server + unit tests.
//   vixen nettest   (spawns corpus/testserver.py itself)

import "core:fmt"
import "core:os"
import "core:strings"
import "core:time"

Test_Env :: struct {
	base:  string, // http://127.0.0.1:PORT
	st:    Store,
	fc:    Fetch_Ctx,
	jar:   Jar,
	cache: Cache,
	ss:    Session_Storage,
	fails: int,
}

tcheck :: proc(env: ^Test_Env, name: string, cond: bool, detail: string = "") {
	if cond {
		fmt.printfln("PASS %-22s %s", name, detail)
	} else {
		fmt.printfln("FAIL %-22s %s", name, detail)
		env.fails += 1
	}
}

tnow :: proc() -> i64 {
	return cookie_now()
}

// ---------- URL suite ----------

test_url :: proc(env: ^Test_Env) {
	res, ok := url_resolve("http://a/b/c/d;p?q", "g:h")
	tcheck(env, "url/abs", ok && res == "g:h", res)
	delete(res)
	cases := [?][2]string{
		{"g", "http://a/b/c/g"},
		{"./g", "http://a/b/c/g"},
		{"g/", "http://a/b/c/g/"},
		{"/g", "http://a/g"},
		{"//g", "http://g/"},
		{"?y", "http://a/b/c/d;p?y"},
		{"g?y", "http://a/b/c/g?y"},
		{"g/../h", "http://a/b/c/h"},
		{"g/./h", "http://a/b/c/g/h"},
		{"../h", "http://a/b/h"},
		{"../../h", "http://a/h"},
		{"#frag", "http://a/b/c/d;p?q#frag"},
		{"g#frag", "http://a/b/c/g#frag"},
		{"/g#frag", "http://a/g#frag"},
	}
	for c in cases {
		r, rok := url_resolve("http://a/b/c/d;p?q", c[0])
		tcheck(env, "url/resolve", rok && r == c[1], fmt.tprintf("%s -> %s", c[0], r))
		delete(r)
	}
	r, rok := url_resolve("http://a/b/c#old", "g")
	tcheck(env, "url/frag-base", rok && r == "http://a/b/g", r)
	delete(r)
	r, rok = url_resolve("http://a/b/c#old", "#new")
	tcheck(env, "url/frag-ref", rok && r == "http://a/b/c#new", r)
	delete(r)
	u, uok := url_parse("HTTP://Example.COM:80/a/./b/../c?x=1#frag")
	tcheck(env, "url/parse", uok, "")
	if uok {
		s := url_serialize(&u)
		k := url_cache_key(&u)
		tcheck(env, "url/normal", s == "http://example.com/a/./b/../c?x=1" && k == s, s)
		delete(s)
		delete(k)
		delete_parsed_url(&u)
	}
	_, bad := url_parse("ftp://x/y")
	tcheck(env, "url/reject-scheme", !bad, "")
	dp := url_default_cookie_path("/a/b/c")
	tcheck(env, "url/cookie-path", dp == "/a/b", dp)
	delete(dp)
}

// ---------- cookie unit suite ----------

test_cookies :: proc(env: ^Test_Env) {
	now := tnow()
	// Basic parse + send.
	c, ok := cookie_parse("sess=abc; Path=/; HttpOnly", "http", "example.com", "/", now)
	tcheck(env, "cookie/parse", ok && c.name == "sess" && c.value == "abc" && c.httponly && c.host_only, "")
	if ok {
		tcheck(env, "cookie/send-self", cookie_sendable(&c, "http", "example.com", "/", now), "")
		tcheck(env, "cookie/no-subdomain", !cookie_sendable(&c, "http", "sub.example.com", "/", now), "")
		delete_cookie(&c)
	}
	// Supercookie rejection.
	_, ok2 := cookie_parse("evil=x; Domain=com; Path=/", "http", "example.com", "/", now)
	tcheck(env, "cookie/supercookie", !ok2, "Domain=com rejected")
	_, ok3 := cookie_parse("evil=x; Domain=other.com; Path=/", "http", "example.com", "/", now)
	tcheck(env, "cookie/domain-mismatch", !ok3, "")
	// Subdomain cookie OK, parent invisible to sibling.
	c2, ok4 := cookie_parse("w=1; Domain=example.com; Path=/", "http", "sub.example.com", "/", now)
	tcheck(env, "cookie/subdomain-set", ok4 && !c2.host_only && c2.host == "example.com", "")
	if ok4 {
		tcheck(env, "cookie/subdomain-send", cookie_sendable(&c2, "http", "other.example.com", "/", now), "")
		delete_cookie(&c2)
	}
	// Expiry.
	ce, _ := cookie_parse("old=gone; Max-Age=-5", "http", "example.com", "/", now)
	tcheck(env, "cookie/expired", cookie_is_expired(&ce, now), "")
	delete_cookie(&ce)
	// Secure.
	cs, _ := cookie_parse("s=1; Secure; Path=/", "https", "example.com", "/", now)
	tcheck(env, "cookie/secure-blocked-http", !cookie_sendable(&cs, "http", "example.com", "/", now), "")
	tcheck(env, "cookie/secure-ok-https", cookie_sendable(&cs, "https", "example.com", "/", now), "")
	delete_cookie(&cs)
	// Path rules.
	cp, _ := cookie_parse("p=1; Path=/prefs", "http", "example.com", "/prefs/a", now)
	tcheck(env, "cookie/path-ok", cookie_sendable(&cp, "http", "example.com", "/prefs/a", now), "")
	tcheck(env, "cookie/path-block", !cookie_sendable(&cp, "http", "example.com", "/", now), "")
	tcheck(env, "cookie/path-prefix", !cookie_sendable(&cp, "http", "example.com", "/prefsx", now), "")
	delete_cookie(&cp)
	// SameSite=None requires Secure.
	_, okn := cookie_parse("n=1; SameSite=None; Path=/", "http", "example.com", "/", now)
	tcheck(env, "cookie/samesite-none", !okn, "rejected without Secure")
	// Header ordering: longest path first.
	j := jar_open(&env.st)
	defer jar_close(&j)
	now2 := tnow()
	a1, _ := cookie_parse("a=1; Path=/", "http", "example.com", "/", now2)
	a2, _ := cookie_parse("b=2; Path=/deep", "http", "example.com", "/deep", now2)
	jar_store(&j, &env.st, &a1, now2)
	delete_cookie(&a1)
	jar_store(&j, &env.st, &a2, now2)
	delete_cookie(&a2)
	hdr, has := jar_header(&j, &env.st, "http", "example.com", "/deep/x", now2)
	tcheck(env, "cookie/order", has && hdr == "b=2; a=1", hdr)
	if has {
		delete(hdr)
	}
}

// ---------- helpers ----------

stats_count :: proc(env: ^Test_Env, path: string) -> int {
	r, _, ok := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", fmt.tprintf("%s/stats", env.base), nil, nil, tnow())
	if !ok {
		return -1
	}
	defer delete_response(&r)
	body := string(r.body)
	// crude JSON scan: "path": N
	needle := fmt.aprintf("\"%s\": ", path)
	defer delete(needle)
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

// ---------- network suites ----------

test_fetch_basic :: proc(env: ^Test_Env) {
	now := tnow()
	r, ok := fetch_url(&env.fc, &env.jar, &env.st, "GET", fmt.tprintf("%s/echo", env.base), nil, nil, now)
	tcheck(env, "fetch/echo", ok && r.status == 200, fmt.tprintf("status=%d", r.status))
	delete_response(&r)
	// POST echo.
	p, pok := fetch_url(&env.fc, &env.jar, &env.st, "POST", fmt.tprintf("%s/post", env.base), nil, transmute([]u8)string("hello=1"), now)
	body_ok := pok && string(p.body) == "POST:hello=1"
	tcheck(env, "fetch/post", body_ok, string(p.body))
	delete_response(&p)
	// 404 passes through.
	nf, nfok := fetch_url(&env.fc, &env.jar, &env.st, "GET", fmt.tprintf("%s/nope", env.base), nil, nil, now)
	tcheck(env, "fetch/404", nfok && nf.status == 404, "")
	delete_response(&nf)
}

test_server_cookies :: proc(env: ^Test_Env) {
	now := tnow()
	r, ok := fetch_url(&env.fc, &env.jar, &env.st, "GET", fmt.tprintf("%s/setcookies", env.base), nil, nil, now)
	tcheck(env, "cookies/server-set", ok && r.status == 200, "")
	delete_response(&r)
	e, eok := fetch_url(&env.fc, &env.jar, &env.st, "GET", fmt.tprintf("%s/echo", env.base), nil, nil, now)
	ck_ok := eok && strings.contains(string(e.body), "sess=abc123")
	tcheck(env, "cookies/sent", ck_ok, string(e.body))
	delete_response(&e)
	// Evil Domain=com cookie from server must not be stored.
	hdr, has := jar_header(&env.jar, &env.st, "http", "127.0.0.1", "/", now)
	evil_ok := !has || !strings.contains(hdr, "evil=")
	tcheck(env, "cookies/evil-rejected", evil_ok, hdr if has else "-")
	if has {
		delete(hdr)
	}
}

test_cache :: proc(env: ^Test_Env) {
	now := tnow()
	mu := fmt.aprintf("%s/maxage?n=60", env.base)
	defer delete(mu)
	r1, i1, ok1 := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", mu, nil, nil, now)
	tcheck(env, "cache/miss", ok1 && !i1.from_cache && string(r1.body) == "maxage-body", "")
	delete_response(&r1)
	r2, i2, ok2 := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", mu, nil, nil, now)
	tcheck(env, "cache/hit", ok2 && i2.from_cache && !i2.revalidated && string(r2.body) == "maxage-body", "")
	delete_response(&r2)
	tcheck(env, "cache/one-fetch", stats_count(env, "/maxage") == 1, "")
	// Expiry: max-age=1 then sleep.
	m1 := fmt.aprintf("%s/maxage?n=1", env.base)
	defer delete(m1)
	a, _, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", m1, nil, nil, tnow())
	delete_response(&a)
	time.sleep(2100 * time.Millisecond)
	b, ib, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", m1, nil, nil, tnow())
	// No validators on this entry -> plain refetch (network again).
	tcheck(env, "cache/expired", !ib.from_cache, "")
	delete_response(&b)
	// ETag revalidation: 2 fetches, 2 hits on server, 1 body.
	eu := fmt.aprintf("%s/etag", env.base)
	defer delete(eu)
	e1, ei1, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", eu, nil, nil, tnow())
	delete_response(&e1)
	e2, ei2, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", eu, nil, nil, tnow())
	tcheck(env, "cache/etag-304", ei2.from_cache && ei2.revalidated && string(e2.body) == "etag-body", "")
	delete_response(&e2)
	tcheck(env, "cache/etag-count", stats_count(env, "/etag") == 2, "")
	// Last-Modified revalidation.
	lu := fmt.aprintf("%s/lastmod", env.base)
	defer delete(lu)
	l1, _, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", lu, nil, nil, tnow())
	delete_response(&l1)
	l2, li2, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", lu, nil, nil, tnow())
	tcheck(env, "cache/lastmod-304", li2.from_cache && li2.revalidated && string(l2.body) == "lm-body", "")
	delete_response(&l2)
	// no-store bypass.
	nu := fmt.aprintf("%s/nostore", env.base)
	defer delete(nu)
	n1, ni1, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", nu, nil, nil, tnow())
	delete_response(&n1)
	n2, ni2, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", nu, nil, nil, tnow())
	delete_response(&n2)
	tcheck(env, "cache/nostore", !ni1.from_cache && !ni2.from_cache && stats_count(env, "/nostore") == 2, "")
}

test_vary :: proc(env: ^Test_Env) {
	now := tnow()
	vu := fmt.aprintf("%s/vary", env.base)
	defer delete(vu)
	fa := []string{"X-Foo: a"}
	fb := []string{"X-Foo: b"}
	a1, _, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", vu, fa, nil, now)
	delete_response(&a1)
	a2, ai2, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", vu, fa, nil, now)
	tcheck(env, "vary/same-hit", ai2.from_cache, "")
	delete_response(&a2)
	b1, bi1, _ := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", vu, fb, nil, now)
	tcheck(env, "vary/differ-miss", !bi1.from_cache && string(b1.body) == "foo=b", string(b1.body))
	delete_response(&b1)
	tcheck(env, "vary/count", stats_count(env, "/vary") == 2, "")
}

test_redirect_cookies :: proc(env: ^Test_Env) {
	now := tnow()
	ru := fmt.aprintf("%s/redir/3", env.base)
	defer delete(ru)
	r, info, ok := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", ru, nil, nil, now)
	tcheck(env, "redir/lands", ok && r.status == 200 && info.hops == 3, fmt.tprintf("status=%d hops=%d", r.status, info.hops))
	all_hops := ok && strings.contains(string(r.body), "hop1=1") &&
		strings.contains(string(r.body), "hop2=1") && strings.contains(string(r.body), "hop3=1")
	tcheck(env, "redir/cookies", all_hops, string(r.body))
	delete_response(&r)
	// Jar isolation: 127.0.0.1 cookies must not leak to localhost.
	host := strings.clone(env.base)
	defer delete(host)
	if strings.contains(host, "127.0.0.1") {
		lo, _ := strings.replace(host, "127.0.0.1", "localhost", 1)
		defer delete(lo)
		eu := fmt.aprintf("%s/echo", lo)
		defer delete(eu)
		e, eok := fetch_url(&env.fc, &env.jar, &env.st, "GET", eu, nil, nil, now)
		iso_ok := eok && !strings.contains(string(e.body), "hop1=1")
		tcheck(env, "cookies/host-isolation", iso_ok, string(e.body) if eok else "no-localhost")
		if eok {
			delete_response(&e)
		}
	}
}

test_limits_schemes :: proc(env: ^Test_Env) {
	now := tnow()
	// Rejected schemes never hit the network.
	for bad in ([]string{"ftp://127.0.0.1/x", "javascript:alert(1)", "data:text/plain,hi", "file:///etc/passwd"}) {
		_, _, ok := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", bad, nil, nil, now)
		tcheck(env, "scheme/reject", !ok, bad)
	}
	// Redirect loop and bad-scheme Location fail instead of hanging.
	loop := fmt.aprintf("%s/redir-loop", env.base)
	defer delete(loop)
	_, _, lok := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", loop, nil, nil, now)
	tcheck(env, "redir/loop", !lok, "")
	badloc := fmt.aprintf("%s/redir-badscheme", env.base)
	defer delete(badloc)
	_, _, bok := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", badloc, nil, nil, now)
	tcheck(env, "redir/bad-scheme", !bok, "")
	// 40MB body aborts mid-transfer (cap, not OOM).
	big := fmt.aprintf("%s/big-body", env.base)
	defer delete(big)
	_, _, bigok := cached_fetch(&env.cache, &env.fc, &env.jar, "GET", big, nil, nil, now)
	tcheck(env, "limit/body-cap", !bigok, "")
}

test_storage :: proc(env: ^Test_Env) {
	ls := local_storage_open(&env.st, "http", "127.0.0.1", -1)
	defer local_storage_close(&ls)
	tcheck(env, "ls/set-get", local_storage_set(&ls, "theme", "dark") &&
		(local_storage_get(&ls, "theme") or_else "") == "dark", "")
	v, _ := local_storage_get(&ls, "theme")
	delete(v)
	// Quota: 6 MB write must fail, usage unchanged.
	big := make([]u8, 6*1024*1024)
	defer delete(big)
	for &b in big {
		b = 'x'
	}
	qok := !local_storage_set(&ls, "big", string(big))
	tcheck(env, "ls/quota", qok, "")
	// Origin isolation.
	ls2 := local_storage_open(&env.st, "http", "other.test", -1)
	defer local_storage_close(&ls2)
	_, has := local_storage_get(&ls2, "theme")
	tcheck(env, "ls/origin-isolation", !has, "")
	// Session storage isolation + clear.
	session_storage_set(&env.ss, 1, "k", "v1")
	session_storage_set(&env.ss, 2, "k", "v2")
	s1, _ := session_storage_get(&env.ss, 1, "k")
	tcheck(env, "sess/isolated", s1 == "v1", s1)
	delete(s1)
	session_storage_clear_ctx(&env.ss, 1)
	_, has1 := session_storage_get(&env.ss, 1, "k")
	s2, _ := session_storage_get(&env.ss, 2, "k")
	tcheck(env, "sess/clear", !has1 && s2 == "v2", "")
	delete(s2)
}

// ---------- server lifecycle ----------

// Every test owns a fresh directory. Never delete a shared profile or
// port file: separate gate processes must be safe to run concurrently.
test_directory :: proc() -> (string, bool) {
	os.make_directory(".tmp")
	dir, err := os.make_directory_temp(".tmp", "vixen-test-*", context.allocator)
	if err != nil {
		fmt.eprintfln("test: cannot create temporary directory: %v", err)
		return "", false
	}
	return dir, true
}

server_start :: proc() -> (port: string, proc_handle: os.Process, ok: bool) {
	dir, dok := test_directory()
	if !dok {
		return "", {}, false
	}
	defer {
		os.remove_all(dir)
		delete(dir)
	}
	pf := fmt.aprintf("%s/port", dir)
	defer delete(pf)
	cmd := []string{"python3", "corpus/testserver.py", pf}
	p, err := os.process_start(os.Process_Desc{command = cmd})
	if err != nil {
		fmt.eprintfln("nettest: cannot start server: %v", err)
		return "", {}, false
	}
	for _ in 0 ..< 100 {
		if data, rerr := os.read_entire_file_from_path(pf, context.temp_allocator); rerr == nil {
			text := strings.trim_space(string(data))
			if len(text) > 0 && len(text) <= 5 {
				n := parse_int_or(text, 0)
				if n > 0 && n <= 65535 {
					port = strings.clone(text)
					break
				}
			}
		}
		time.sleep(100 * time.Millisecond)
	}
	if port == "" {
		fmt.eprintfln("nettest: server never wrote portfile")
		_ = os.process_kill(p)
		_, _ = os.process_wait(p)
		return "", {}, false
	}
	return port, p, true
}

nettest_main :: proc() -> bool {
	env: Test_Env
	port, srv, ok := server_start()
	if !ok {
		return false
	}
	defer {
		_ = os.process_kill(srv)
		_, _ = os.process_wait(srv)
	}
	defer delete(port)
	env.base = fmt.aprintf("http://127.0.0.1:%s", port)
	defer delete(env.base)
	prof, pok := test_directory()
	if !pok {
		return false
	}
	defer {
		os.remove_all(prof)
		delete(prof)
	}
	st, sok := store_open(prof)
	if !sok {
		return false
	}
	env.st = st
	defer store_close(&env.st)
	fc, fok := fetch_ctx_new()
	if !fok {
		return false
	}
	env.fc = fc
	defer fetch_ctx_free(&env.fc)
	env.jar = jar_open(&env.st)
	defer jar_close(&env.jar)
	env.cache = cache_open(&env.st)
	defer cache_close(&env.cache)
	env.ss = session_storage_open()
	defer session_storage_close(&env.ss)

	test_url(&env)
	test_cookies(&env)
	test_fetch_basic(&env)
	test_server_cookies(&env)
	test_cache(&env)
	test_vary(&env)
	test_redirect_cookies(&env)
	test_limits_schemes(&env)
	test_storage(&env)

	// Persistence across reopen: pref cookie + localStorage survive.
	env2st, reopened := store_open(prof)
	tcheck(&env, "persist/reopen", reopened, "")
	if !reopened { return false }
	hdr, has := jar_header(&env.jar, &env2st, "http", "127.0.0.1", "/prefs/x", tnow())
	tcheck(&env, "persist/cookies", has && strings.contains(hdr, "pref=dark"), hdr if has else "-")
	if has {
		delete(hdr)
	}
	ls := local_storage_open(&env2st, "http", "127.0.0.1", -1)
	lv, lok := local_storage_get(&ls, "theme")
	tcheck(&env, "persist/localstorage", lok && lv == "dark", lv if lok else "-")
	if lok {
		delete(lv)
	}
	local_storage_close(&ls)
	store_close(&env2st)

	fmt.printfln("nettest: %d failures", env.fails)
	return env.fails == 0
}
