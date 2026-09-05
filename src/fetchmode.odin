package vixen

// vixen fetch: one URL through the full pipeline (jar + cache + network).

import "core:fmt"
import "core:time"

fetch_main :: proc(prof, url: string) -> bool {
	st, ok := store_open(prof)
	if !ok {
		return false
	}
	defer store_close(&st)
	fc, fok := fetch_ctx_new()
	if !fok {
		return false
	}
	defer fetch_ctx_free(&fc)
	j := jar_open(&st)
	defer jar_close(&j)
	c := cache_open(&st)
	defer cache_close(&c)
	t0 := time.now()
	r, info, rok := cached_fetch(&c, &fc, &j, "GET", url, nil, nil, tnow())
	dt := time.since(t0)
	if !rok {
		fmt.eprintfln("fetch: failed %s", url)
		return false
	}
	defer delete_response(&r)
	fmt.printfln("status=%d bytes=%d cached=%v revalidated=%v hops=%d ms=%.0f",
		r.status, len(r.body), info.from_cache, info.revalidated, info.hops,
		time.duration_milliseconds(dt))
	ct, _ := headers_get_first(&r, "content-type")
	fmt.printfln("content-type: %s", ct)
	fmt.printfln("VmHWM=%d KB", vm_hwm_kb())
	return r.status < 400
}
