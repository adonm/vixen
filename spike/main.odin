package spike

// Memory-floor spike: lexbor HTML parse + QuickJS eval + VmHWM self-report.
// Usage:
//   spike rss                  print own peak RSS (cold-start baseline)
//   spike parse <html...>      parse each file, report nodes/ms/peak RSS
//   spike js <js...>           eval each file, report result/memory/ms/peak RSS

import "core:fmt"
import "core:os"
import "core:strings"
import "core:time"

vm_hwm_kb :: proc() -> int {
	data, err := os.read_entire_file_from_path("/proc/self/status", context.allocator)
	if err != nil {
		return -1
	}
	defer delete(data)
	for line in strings.split_lines(string(data)) {
		if strings.has_prefix(line, "VmHWM:") {
			fields := strings.fields(line)
			if len(fields) >= 2 {
				n := 0
				for c in fields[1] {
					n = n * 10 + int(c - '0')
				}
				return n
			}
		}
	}
	return -1
}

Parse_Stats :: struct {
	elements, text_nodes, comments, other: int,
	text_bytes:                           int,
	max_depth:                            int,
}

walk_count :: proc(root: ^Dom_Node, stats: ^Parse_Stats) {
	// Iterative depth-first walk with an explicit stack (no recursion).
	stack: [dynamic]^Dom_Node
	defer delete(stack)
	depth_stack: [dynamic]int
	defer delete(depth_stack)
	append(&stack, root)
	append(&depth_stack, 0)
	for len(stack) > 0 {
		node := pop(&stack)
		depth := pop(&depth_stack)
		if depth > stats.max_depth {
			stats.max_depth = depth
		}
		t := node_type(node)
		if t == NODE_TYPE_ELEMENT {
			stats.elements += 1
		} else if t == NODE_TYPE_TEXT {
			stats.text_nodes += 1
			// Document-owned allocation; freed with the document.
			tlen: uint
			_ = lxb_dom_node_text_content(node, &tlen)
			stats.text_bytes += int(tlen)
		} else if t == NODE_TYPE_COMMENT {
			stats.comments += 1
		} else {
			stats.other += 1
		}
		// Push children (siblings via next-link chain).
		child := node_field(node, NODE_OFF_FIRST_CHILD)
		for child != nil {
			next := node_field(child, NODE_OFF_NEXT)
			append(&stack, child)
			append(&depth_stack, depth + 1)
			child = next
		}
	}
}

do_parse :: proc(path: string) -> bool {
	data, err := os.read_entire_file_from_path(path, context.allocator)
	if err != nil {
		fmt.printfln("parse: cannot read %s", path)
		return false
	}
	defer delete(data)
	doc := lxb_html_document_create()
	if doc == nil {
		fmt.println("parse: document create failed")
		return false
	}
	t0 := time.now()
	status := lxb_html_document_parse(doc, raw_data(data), uint(len(data)))
	dt := time.since(t0)
	if status != LXB_STATUS_OK {
		fmt.printfln("parse: %s failed status=%d", path, status)
		lxb_html_document_destroy(doc)
		return false
	}
	stats: Parse_Stats
	walk_count((^Dom_Node)(doc), &stats)
	lxb_html_document_destroy(doc)
	fmt.printfln(
		"parse %-40s %7d KB html  el=%6d text=%6d comments=%5d textbytes=%8d depth=%4d  %8.1f ms  VmHWM=%6d KB",
		path, len(data) / 1024, stats.elements, stats.text_nodes, stats.comments,
		stats.text_bytes, stats.max_depth, time.duration_milliseconds(dt),
		vm_hwm_kb(),
	)
	return true
}

do_js :: proc(path: string) -> bool {
	data, err := os.read_entire_file_from_path(path, context.allocator)
	if err != nil {
		fmt.printfln("js: cannot read %s", path)
		return false
	}
	defer delete(data)
	src := string(data)
	rt := JS_NewRuntime()
	if rt == nil {
		fmt.println("js: runtime create failed")
		return false
	}
	ctx := JS_NewContext(rt)
	if ctx == nil {
		fmt.println("js: context create failed")
		JS_FreeRuntime(rt)
		return false
	}
	t0 := time.now()
	v := JS_Eval(ctx, strings.clone_to_cstring(src, context.temp_allocator), uint(len(data)), "<input>", JS_EVAL_TYPE_GLOBAL)
	dt := time.since(t0)
	mem: JS_Memory_Usage
	JS_ComputeMemoryUsage(rt, &mem)
	if spike_js_is_exception(v) != 0 {
		ex := JS_GetException(ctx)
		plen: uint
		cstr := spike_js_to_cstring(ctx, ex, &plen)
		out := string(cstr)[:min(int(plen), 120)]
		fmt.printfln("js   %-40s EXCEPTION: %q  %8.1f ms  qjs_heap=%8d KB  VmHWM=%6d KB",
			path, out, time.duration_milliseconds(dt), mem.memory_used_size / 1024, vm_hwm_kb())
		spike_js_free_cstring(ctx, cstr)
		spike_js_free(ctx, ex)
	} else {
		plen: uint
		cstr := spike_js_to_cstring(ctx, v, &plen)
		out := string(cstr)[:min(int(plen), 80)]
		fmt.printfln("js   %-40s => %-30q %8.1f ms  qjs_heap=%8d KB  VmHWM=%6d KB",
			path, out, time.duration_milliseconds(dt), mem.memory_used_size / 1024, vm_hwm_kb())
		spike_js_free_cstring(ctx, cstr)
	}
	spike_js_free(ctx, v)
	JS_FreeContext(ctx)
	JS_FreeRuntime(rt)
	return true
}

main :: proc() {
	if len(os.args) < 2 {
		fmt.println("usage: vixen (rss|parse <html...>|js <js...>|shapetest)")
		os.exit(2)
	}
	switch os.args[1] {
	case "rss":
		fmt.printfln("startup VmHWM=%d KB", vm_hwm_kb())
	case "parse":
		if len(os.args) < 3 {
			fmt.println("usage: vixen parse <html...>")
			os.exit(2)
		}
		for path in os.args[2:] {
			do_parse(path)
		}
		fmt.printfln("final VmHWM=%d KB", vm_hwm_kb())
	case "js":
		if len(os.args) < 3 {
			fmt.println("usage: vixen js <js...>")
			os.exit(2)
		}
		for path in os.args[2:] {
			do_js(path)
		}
		fmt.printfln("final VmHWM=%d KB", vm_hwm_kb())
	case "shapetest":
		os.exit(0 if run_shapetests() else 1)
	case "wasmtest":
		if len(os.args) < 3 {
			fmt.eprintln("usage: vixen wasmtest <module.wasm>")
			os.exit(2)
		}
		os.exit(0 if wasmtest_main(os.args[2]) else 1)
	case "nettest":
		os.exit(0 if nettest_main() else 1)
	case "browse":
		// vixen browse [--dump] [--profile DIR] [--width N] <url>
		dump := false
		prof := ""
		width := 900
		url := ""
		i := 2
		for i < len(os.args) {
			arg := os.args[i]
			key, _, val := strings.partition(arg, "=")
			switch key {
			case "--dump":
				dump = true
			case "--profile":
				if val != "" {
					prof = val
				} else {
					i += 1
					prof = os.args[i]
				}
			case "--width":
				if val != "" {
					width = parse_int_or(val, 900)
				} else {
					i += 1
					width = parse_int_or(os.args[i], 900)
				}
			case:
				url = arg
			}
			i += 1
		}
		if url == "" {
			fmt.eprintln("usage: vixen browse [--dump] [--profile DIR] [--width N] <url>")
			os.exit(2)
		}
		if !strings.contains(url, "://") {
			url = strings.concatenate([]string{"https://", url}, context.temp_allocator)
		}
		if dump {
			os.exit(0 if browse_dump(prof, url, width) else 1)
		}
		os.exit(0 if browse_interactive(prof, url, width) else 1)
	case "domtest":
		path := "corpus/domtest.html"
		if len(os.args) >= 3 {
			path = os.args[2]
		}
		os.exit(0 if domtest_main(path) else 1)
	case "tuitest":
		os.exit(0 if tuitest_main() else 1)
	case "fetch":
		// spike fetch [--profile DIR] <url>
		prof := ""
		url := ""
		i := 2
		for i < len(os.args) {
			arg := os.args[i]
			key, _, val := strings.partition(arg, "=")
			switch key {
			case "--profile":
				if val != "" {
					prof = val
				} else {
					i += 1
					prof = os.args[i]
				}
			case:
				url = arg
			}
			i += 1
		}
		if url == "" {
			fmt.eprintln("usage: vixen fetch [--profile DIR] <url>")
			os.exit(2)
		}
		os.exit(0 if fetch_main(prof, url) else 1)
	case "render":
		// spike render --out f.png [--width N] page.html
		out := "spike/out.png"
		width := 900
		page := ""
		i := 2
		for i < len(os.args) {
			arg := os.args[i]
			key, _, val := strings.partition(arg, "=")
			switch key {
			case "--out":
				if val != "" {
					out = val
				} else {
					i += 1
					out = os.args[i]
				}
			case "--width":
				if val != "" {
					width = parse_int_or(val, 900)
				} else {
					i += 1
					width = parse_int_or(os.args[i], 900)
				}
			case:
				page = arg
			}
			i += 1
		}
		if page == "" {
			fmt.eprintln("usage: vixen render --out f.png [--width N] page.html")
			os.exit(2)
		}
		bank, bok := font_bank_load(20)
		if !bok {
			os.exit(1)
		}
		defer font_bank_free(&bank)
		fr, ok := render_page(page, width, &bank)
		if !ok {
			os.exit(1)
		}
		defer delete(fr.px)
		if !frame_write_png(&fr, out) {
			os.exit(1)
		}
	case "show":
		// spike show page.html — SDL3 window plus PNG dump.
		if len(os.args) < 3 {
			fmt.eprintln("usage: vixen show page.html")
			os.exit(2)
		}
		bank, bok := font_bank_load(20)
		if !bok {
			os.exit(1)
		}
		defer font_bank_free(&bank)
		fr, ok := render_page(os.args[2], 900, &bank)
		if !ok {
			os.exit(1)
		}
		defer delete(fr.px)
		frame_write_png(&fr, "spike/show.png")
		if !sdl_show(&fr, os.args[2], 1500) {
			os.exit(1)
		}
	case "tui":
		// spike tui [--kitty auto|force|off] [--width N] page.html
		kitty_mode := "auto"
		width := 900
		page := ""
		i := 2
		for i < len(os.args) {
			arg := os.args[i]
			key, _, val := strings.partition(arg, "=")
			switch key {
			case "--kitty":
				if val != "" {
					kitty_mode = val
				} else {
					i += 1
					kitty_mode = os.args[i]
				}
			case "--width":
				if val != "" {
					width = parse_int_or(val, 900)
				} else {
					i += 1
					width = parse_int_or(os.args[i], 900)
				}
			case:
				page = arg
			}
			i += 1
		}
		if page == "" {
			fmt.eprintln("usage: vixen tui [--kitty auto|force|off] [--width N] page.html")
			os.exit(2)
		}
		bank, bok := font_bank_load(20)
		if !bok {
			os.exit(1)
		}
		defer font_bank_free(&bank)
		fr, ok := render_page(page, width, &bank)
		if !ok {
			os.exit(1)
		}
		defer delete(fr.px)
		use_kitty := kitty_mode == "force" || (kitty_mode == "auto" && kitty_env_supported())
		if use_kitty {
			png, pok := frame_encode_png(&fr)
			if !pok {
				os.exit(1)
			}
			defer delete(png)
			if !kitty_transmit_png(png, fr.w, fr.h) {
				os.exit(1)
			}
		} else {
			// Plain-text fallback: the laid-out line texts.
			tui_print_text(page, width)
		}
	case:
		fmt.println("usage: vixen (rss|parse <html...>|js <js...>|shapetest)")
		os.exit(2)
	}
}

parse_int_or :: proc(s: string, dflt: int) -> int {
	n := 0
	if len(s) == 0 {
		return dflt
	}
	for c in s {
		if c < '0' || c > '9' {
			return dflt
		}
		n = n * 10 + int(c - '0')
	}
	return n
}

// Plain-text TUI fallback: laid-out line texts, no graphics.
tui_print_text :: proc(page: string, width: int) {
	bank, bok := font_bank_load(20)
	if !bok {
		return
	}
	defer font_bank_free(&bank)
	rc, ok := layout_page(page, width, &bank)
	if !ok {
		return
	}
	defer render_ctx_free(&rc)
	for ln in rc.lines {
		if len(ln.text) == 0 {
			fmt.println()
		} else {
			fmt.println(ln.text)
		}
	}
}

// Headless dump: navigate once, print laid-out text. No display needed.
browse_dump :: proc(prof, url: string, width: int) -> bool {
	sess, ok := browse_open(prof == "" ? "/tmp/opencode/vixen-profile" : prof, width)
	if !ok {
		return false
	}
	defer browse_close(&sess)
	if !browse_navigate(&sess, url, true) {
		return false
	}
	fmt.printfln("# %s", sess.page.title)
	fmt.printfln("# %s", sess.page.url)
	for t in sess.page.text {
		fmt.println(t)
	}
	fmt.eprintfln("dump lines=%d links=%d", len(sess.page.text), len(sess.page.links))
	return true
}

browse_interactive :: proc(prof, url: string, width: int) -> bool {
	sess, ok := browse_open(prof == "" ? "/tmp/opencode/vixen-profile" : prof, width)
	if !ok {
		return false
	}
	defer browse_close(&sess)
	tui_loop(&sess, url)
	return true
}
