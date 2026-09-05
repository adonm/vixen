package vixen

// Vixen command entrypoint. Browsing and explicit headless modes are the
// product interfaces; parse/js/rss and test commands are developer tools.

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
		fmt.eprintln("usage: vixen <command> [args]")
		print_usage()
		os.exit(2)
	}
	switch os.args[1] {
	case "help", "--help", "-h":
		print_usage()
	case "version", "--version", "-V":
		if len(os.args) != 2 {
			cli_usage_error("usage: vixen version")
		}
		version_main()
	case "rss":
		if len(os.args) != 2 {
			cli_usage_error("usage: vixen rss")
		}
		fmt.printfln("startup VmHWM=%d KB", vm_hwm_kb())
	case "parse":
		if len(os.args) < 3 {
			cli_usage_error("usage: vixen parse <html...>")
		}
		ok := true
		for path in os.args[2:] {
			if !do_parse(path) {
				ok = false
			}
		}
		fmt.printfln("final VmHWM=%d KB", vm_hwm_kb())
		if !ok {
			os.exit(1)
		}
	case "js":
		if len(os.args) < 3 {
			cli_usage_error("usage: vixen js <js...>")
		}
		ok := true
		for path in os.args[2:] {
			if !do_js(path) {
				ok = false
			}
		}
		fmt.printfln("final VmHWM=%d KB", vm_hwm_kb())
		if !ok {
			os.exit(1)
		}
	case "shapetest":
		if len(os.args) != 2 {
			cli_usage_error("usage: vixen shapetest")
		}
		os.exit(0 if run_shapetests() else 1)
	case "wasmtest":
		if len(os.args) != 3 {
			cli_usage_error("usage: vixen wasmtest <module.wasm>")
		}
		os.exit(0 if wasmtest_main(os.args[2]) else 1)
	case "nettest":
		if len(os.args) != 2 {
			cli_usage_error("usage: vixen nettest")
		}
		os.exit(0 if nettest_main() else 1)
	case "browse":
		// vixen browse [--dump [--format text|json]] [--profile DIR] [--width N] <url>
		dump := false
		format := "text"
		prof := ""
		width := 900
		url := ""
		flags_ended := false
		i := 2
		for i < len(os.args) {
			arg := os.args[i]
			if !flags_ended && arg == "--" {
				flags_ended = true
				i += 1
				continue
			}
			if !flags_ended && cli_is_flag(arg) {
				if cli_help_requested(arg) {
					fmt.println(CLI_BROWSE_USAGE)
					os.exit(0)
				}
				key, _, val := strings.partition(arg, "=")
				switch key {
				case "--dump":
					if strings.contains(arg, "=") {
						cli_usage_error(CLI_BROWSE_USAGE)
					}
					dump = true
				case "--format":
					v, ok := cli_take_value(os.args, &i, arg, val)
					if !ok || (v != "text" && v != "json") {
						cli_usage_error(CLI_BROWSE_USAGE)
					}
					format = v
				case "--profile":
					v, ok := cli_take_value(os.args, &i, arg, val)
					if !ok {
						cli_usage_error(CLI_BROWSE_USAGE)
					}
					prof = v
				case "--width":
					v, ok := cli_take_value(os.args, &i, arg, val)
					w, wok := cli_parse_width(v)
					if !ok || !wok {
						cli_usage_error(CLI_BROWSE_USAGE)
					}
					width = w
				case:
					cli_usage_error(CLI_BROWSE_USAGE)
				}
				i += 1
				continue
			}
			if len(url) > 0 {
				cli_usage_error(CLI_BROWSE_USAGE)
			}
			url = arg
			i += 1
		}
		if url == "" {
			cli_usage_error(CLI_BROWSE_USAGE)
		}
		if format != "text" && !dump {
			cli_usage_error(CLI_BROWSE_USAGE) // --format needs --dump
		}
		if !strings.contains(url, "://") {
			url = strings.concatenate([]string{"https://", url}, context.temp_allocator)
		}
		if dump {
			if format == "json" {
				os.exit(0 if browse_dump_json(prof, url, width) else 1)
			}
			os.exit(0 if browse_dump(prof, url, width) else 1)
		}
		os.exit(0 if browse_interactive(prof, url, width) else 1)
	case "domtest":
		if len(os.args) != 2 && len(os.args) != 3 {
			cli_usage_error("usage: vixen domtest [page.html]")
		}
		path := "corpus/domtest.html"
		if len(os.args) >= 3 {
			path = os.args[2]
		}
		os.exit(0 if domtest_main(path) else 1)
	case "browsetest", "tuitest": // tuitest is the legacy helper-suite alias
		if len(os.args) != 2 {
			cli_usage_error("usage: vixen browsetest")
		}
		os.exit(0 if tuitest_main() else 1)
	case "termtest":
		if len(os.args) != 2 {
			cli_usage_error("usage: vixen termtest")
		}
		os.exit(0 if termtest_main() else 1)
	case "fetch":
		// vixen fetch [--profile DIR] <url>
		prof := ""
		url := ""
		flags_ended := false
		i := 2
		for i < len(os.args) {
			arg := os.args[i]
			if !flags_ended && arg == "--" {
				flags_ended = true
				i += 1
				continue
			}
			if !flags_ended && cli_is_flag(arg) {
				if cli_help_requested(arg) {
					fmt.println(CLI_FETCH_USAGE)
					os.exit(0)
				}
				key, _, val := strings.partition(arg, "=")
				switch key {
				case "--profile":
					v, ok := cli_take_value(os.args, &i, arg, val)
					if !ok {
						cli_usage_error(CLI_FETCH_USAGE)
					}
					prof = v
				case:
					cli_usage_error(CLI_FETCH_USAGE)
				}
				i += 1
				continue
			}
			if len(url) > 0 {
				cli_usage_error(CLI_FETCH_USAGE)
			}
			url = arg
			i += 1
		}
		if url == "" {
			cli_usage_error(CLI_FETCH_USAGE)
		}
		os.exit(0 if fetch_main(prof, url) else 1)
	case "render":
		// vixen render [--out PNG] [--meta JSON] [--width N] [--profile DIR] [--base-url URL] <page.html>
		out := "vixen.png"
		metapath := ""
		width := 900
		prof := ""
		base := ""
		page := ""
		flags_ended := false
		i := 2
		for i < len(os.args) {
			arg := os.args[i]
			if !flags_ended && arg == "--" {
				flags_ended = true
				i += 1
				continue
			}
			if !flags_ended && cli_is_flag(arg) {
				if cli_help_requested(arg) {
					fmt.println(CLI_RENDER_USAGE)
					os.exit(0)
				}
				key, _, val := strings.partition(arg, "=")
				switch key {
				case "--out":
					v, ok := cli_take_value(os.args, &i, arg, val)
					if !ok {
						cli_usage_error(CLI_RENDER_USAGE)
					}
					out = v
				case "--meta":
					v, ok := cli_take_value(os.args, &i, arg, val)
					if !ok {
						cli_usage_error(CLI_RENDER_USAGE)
					}
					metapath = v
				case "--width":
					v, ok := cli_take_value(os.args, &i, arg, val)
					w, wok := cli_parse_width(v)
					if !ok || !wok {
						cli_usage_error(CLI_RENDER_USAGE)
					}
					width = w
				case "--profile":
					v, ok := cli_take_value(os.args, &i, arg, val)
					if !ok {
						cli_usage_error(CLI_RENDER_USAGE)
					}
					prof = v
				case "--base-url":
					v, ok := cli_take_value(os.args, &i, arg, val)
					if !ok {
						cli_usage_error(CLI_RENDER_USAGE)
					}
					base = v
				case:
					cli_usage_error(CLI_RENDER_USAGE)
				}
				i += 1
				continue
			}
			if len(page) > 0 {
				cli_usage_error(CLI_RENDER_USAGE)
			}
			page = arg
			i += 1
		}
		if page == "" {
			cli_usage_error(CLI_RENDER_USAGE)
		}
		fr, meta, ok := file_render_page(prof, page, width, base)
		if !ok {
			os.exit(1)
		}
		defer delete(fr.px)
		defer delete_file_meta(&meta)
		if !frame_write_png(&fr, out) {
			os.exit(1)
		}
		if len(metapath) > 0 {
			js := file_meta_json(&meta)
			defer delete(js)
			if err := os.write_entire_file(metapath, transmute([]u8)js); err != nil {
				fmt.eprintfln("render: cannot write %s", metapath)
				os.exit(1)
			}
		}
	case "show":
		// vixen show page.html — experimental static SDL3 window.
		if len(os.args) != 3 || cli_is_flag(os.args[2]) {
			cli_usage_error(CLI_SHOW_USAGE)
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
		if !sdl_show(&fr, os.args[2], 1500) {
			os.exit(1)
		}
	case "tui":
		// vixen tui [--width N] [--profile DIR] [--base-url URL] [--meta JSON] page.html
		// One-shot render to the terminal. Ghostty (Kitty graphics) assumed.
		width := 900
		prof := ""
		base := ""
		metapath := ""
		page := ""
		flags_ended := false
		i := 2
		for i < len(os.args) {
			arg := os.args[i]
			if !flags_ended && arg == "--" {
				flags_ended = true
				i += 1
				continue
			}
			if !flags_ended && cli_is_flag(arg) {
				if cli_help_requested(arg) {
					fmt.println(CLI_TUI_USAGE)
					os.exit(0)
				}
				key, _, val := strings.partition(arg, "=")
				switch key {
				case "--width":
					v, ok := cli_take_value(os.args, &i, arg, val)
					w, wok := cli_parse_width(v)
					if !ok || !wok {
						cli_usage_error(CLI_TUI_USAGE)
					}
					width = w
				case "--profile":
					v, ok := cli_take_value(os.args, &i, arg, val)
					if !ok {
						cli_usage_error(CLI_TUI_USAGE)
					}
					prof = v
				case "--base-url":
					v, ok := cli_take_value(os.args, &i, arg, val)
					if !ok {
						cli_usage_error(CLI_TUI_USAGE)
					}
					base = v
				case "--meta":
					v, ok := cli_take_value(os.args, &i, arg, val)
					if !ok {
						cli_usage_error(CLI_TUI_USAGE)
					}
					metapath = v
				case:
					cli_usage_error(CLI_TUI_USAGE)
				}
				i += 1
				continue
			}
			if len(page) > 0 {
				cli_usage_error(CLI_TUI_USAGE)
			}
			page = arg
			i += 1
		}
		if page == "" {
			cli_usage_error(CLI_TUI_USAGE)
		}
		fr, meta, ok := file_render_page(prof, page, width, base)
		if !ok {
			os.exit(1)
		}
		defer delete(fr.px)
		defer delete_file_meta(&meta)
		png, pok := frame_encode_png(&fr)
		if !pok {
			os.exit(1)
		}
		defer delete(png)
		if !kitty_transmit_png(png, fr.w, fr.h) {
			os.exit(1)
		}
		if len(metapath) > 0 {
			js := file_meta_json(&meta)
			defer delete(js)
			if err := os.write_entire_file(metapath, transmute([]u8)js); err != nil {
				fmt.eprintfln("tui: cannot write %s", metapath)
				os.exit(1)
			}
		}
	case:
		fmt.eprintln("unknown command:", os.args[1])
		print_usage()
		os.exit(2)
	}
}

print_usage :: proc() {
	fmt.println("Vixen — experimental reading browser")
	fmt.println("  vixen browse [--dump [--format text|json]] [--profile DIR] [--width N] <url>")
	fmt.println("  vixen render [--out PNG] [--meta JSON] [--width N] [--profile DIR] [--base-url URL] HTML")
	fmt.println("  vixen fetch [--profile DIR] <url>          response statistics")
	fmt.println("  vixen tui [--width N] [--profile DIR] [--base-url URL] [--meta JSON] HTML")
	fmt.println("  vixen show HTML                            static SDL demo")
	fmt.println("  vixen version                              build identity")
	fmt.println("Developer commands: parse, js, rss, shapetest, domtest, termtest, browsetest, nettest, wasmtest")
	fmt.println("Run 'vixen <command> --help' for command usage.")
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


// Headless dump: navigate once, print laid-out text. No display needed.
// Error pages print (for debugging) but fail, so scripts detect bad loads.
browse_dump :: proc(prof, url: string, width: int) -> bool {
	sess, ok := browse_open(prof, width)
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
	return !sess.page.is_error
}

// Headless JSON dump: same navigation, machine-readable stdout.
browse_dump_json :: proc(prof, url: string, width: int) -> bool {
	sess, ok := browse_open(prof, width)
	if !ok {
		return false
	}
	defer browse_close(&sess)
	if !browse_navigate(&sess, url, true) {
		return false
	}
	u := json_escape(sess.page.url)
	defer delete(u)
	t := json_escape(sess.page.title)
	defer delete(t)
	b: strings.Builder
	strings.write_byte(&b, '{')
	fmt.sbprintf(&b, "\"url\":\"%s\",\"title\":\"%s\",", u, t)
	fmt.sbprintf(&b, "\"width\":%d,\"height\":%d,\"is_error\":%s,\"lines\":[",
		sess.page.width, sess.page.height, "true" if sess.page.is_error else "false")
	for text, i in sess.page.text {
		e := json_escape(text)
		defer delete(e)
		fmt.sbprintf(&b, "%s\"%s\"", "," if i > 0 else "", e)
	}
	strings.write_string(&b, "],\"links\":[")
	for &l, i in sess.page.links {
		e := json_escape(l.url)
		defer delete(e)
		if i > 0 {
			strings.write_byte(&b, ',')
		}
		strings.write_byte(&b, '{')
		fmt.sbprintf(&b, "\"url\":\"%s\",", e)
		fmt.sbprintf(&b, "\"x0\":%.1f,\"y0\":%.1f,\"x1\":%.1f,\"y1\":%.1f",
			l.x0, l.y0, l.x1, l.y1)
		strings.write_byte(&b, '}')
	}
	strings.write_string(&b, "]}")
	fmt.println(strings.to_string(b))
	delete(b.buf)
	fmt.eprintfln("dump lines=%d links=%d", len(sess.page.text), len(sess.page.links))
	return !sess.page.is_error
}

browse_interactive :: proc(prof, url: string, width: int) -> bool {
	sess, ok := browse_open(prof, width)
	if !ok {
		return false
	}
	defer browse_close(&sess)
	return tui_loop(&sess, url)
}
