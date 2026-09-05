package vixen

// File rendering through the shared browsing pipeline (session fonts, image
// cache, layout, raster). Used by `render` (PNG to file) and one-shot `tui`
// (PNG to terminal); `show` stays on the legacy direct path (manual demo).

import "core:fmt"
import "core:os"
import "core:strings"

RENDER_MAX_HEIGHT :: 4000

File_Meta :: struct {
	source:  string, // page path as passed (local file identity)
	base_url: string, // resource base as passed ("" when none)
	title:  string, // owned, from <title>
	width:  int,    // output PNG pixels
	height: int,    // output PNG pixels (capped)
	full_height: int, // uncapped layout height, px
	lines:  int,
	links:  int,
	fields: int,
	images: int,
	truncated: bool, // full_height exceeded the cap (top shown)
}

delete_file_meta :: proc(m: ^File_Meta) {
	delete(m.source)
	delete(m.base_url)
	delete(m.title)
}

// JSON string escaping (quotes/backslashes/controls; UTF-8 passes through).
json_escape :: proc(s: string) -> string {
	b: strings.Builder
	strings.builder_init(&b, context.temp_allocator)
	defer delete(b.buf)
	hex := "0123456789abcdef"
	for i in 0 ..< len(s) {
		c := s[i]
		switch c {
		case '"':
			strings.write_string(&b, "\\\"")
		case '\\':
			strings.write_string(&b, "\\\\")
		case '\n':
			strings.write_string(&b, "\\n")
		case '\r':
			strings.write_string(&b, "\\r")
		case '\t':
			strings.write_string(&b, "\\t")
		case 0x08:
			strings.write_string(&b, "\\b")
		case 0x0C:
			strings.write_string(&b, "\\f")
		case 0x00 ..= 0x1F:
			strings.write_string(&b, "\\u00")
			strings.write_byte(&b, hex[c >> 4])
			strings.write_byte(&b, hex[c & 0xF])
		case:
			strings.write_byte(&b, c)
		}
	}
	return strings.clone(strings.to_string(b))
}

file_meta_json :: proc(m: ^File_Meta) -> string {
	s := json_escape(m.source)
	defer delete(s)
	b := json_escape(m.base_url)
	defer delete(b)
	t := json_escape(m.title)
	defer delete(t)
	buf: strings.Builder
	strings.builder_init(&buf, context.temp_allocator)
	defer delete(buf.buf)
	strings.write_byte(&buf, '{')
	fmt.sbprintf(&buf, "\"source\":\"%s\",\"base_url\":\"%s\",\"title\":\"%s\",", s, b, t)
	fmt.sbprintf(&buf, "\"width\":%d,\"height\":%d,\"full_height\":%d,", m.width, m.height, m.full_height)
	fmt.sbprintf(&buf, "\"lines\":%d,\"links\":%d,\"fields\":%d,\"images\":%d,", m.lines, m.links, m.fields, m.images)
	fmt.sbprintf(&buf, "\"truncated\":%s", "true" if m.truncated else "false")
	strings.write_byte(&buf, '}')
	return strings.clone(strings.to_string(buf))
}

// Render a local HTML file: parse, optionally resolve links/images against
// base_url ("" skips both), raster capped full-page PNG pixels. Owns the
// returned frame and meta; the session is closed before returning.
file_render_page :: proc(prof, page_path: string, width: int, base_url: string) -> (Frame, File_Meta, bool) {
	fr: Frame
	meta: File_Meta
	sess, ok := browse_open(prof, width)
	if !ok {
		return fr, meta, false
	}
	defer browse_close(&sess)
	data, err := os.read_entire_file_from_path(page_path, context.allocator)
	if err != nil {
		fmt.eprintfln("render: cannot read %s", page_path)
		return fr, meta, false
	}
	defer delete(data)
	doc, dok := parse_document(data)
	if !dok {
		fmt.eprintfln("render: parse failed %s", page_path)
		return fr, meta, false
	}
	defer lxb_html_document_destroy(doc)
	refs: [dynamic]Image_Ref
	if len(base_url) > 0 {
		refs = collect_image_urls(doc, base_url, MAX_PAGE_IMAGES)
	}
	defer delete_image_refs(refs)
	imgs := page_load_images(&sess, refs[:])
	defer delete(imgs)
	rc := render_ctx_new(&sess.bank, 20, f32(width), base_url)
	for &im in imgs {
		append(&rc.images, im)
	}
	// page_load_images owns imgs array elements; moving structs shares pixel
	// backing, so clear the source array (keeps backing for deferred free).
	// NOTE: imgs elements moved one-by-one (append copies struct); clear the
	// array header only (backing freed by the deferred delete, elements now
	// owned by rc.images which frees pixels on render_ctx_free... wait, double
	// free risk: imgs array holds structs with px backing; rc.images holds
	// COPIES sharing backing. Deferred delete(imgs) frees array backing (struct
	// array), not pixel backing (pixels freed once via rc.images on ctx free).
	// Correct: array backing vs pixel backing are separate allocations.
	clear(&imgs)
	defer render_ctx_free(&rc)
	layout_html(&rc, doc)
	finalize_links(&rc)
	h := int(rc.margin * 2 + 20)
	if len(rc.lines) > 0 {
		last := rc.lines[len(rc.lines) - 1]
		h = int(last.baseline + rc.body_px * 0.6 + rc.margin)
	}
	full_h := h
	truncated := false
	if h > RENDER_MAX_HEIGHT {
		fmt.eprintfln("render: truncated to %dpx (full height %dpx)", RENDER_MAX_HEIGHT, h)
		h = RENDER_MAX_HEIGHT
		truncated = true
	}
	if h < 100 {
		h = 100
	}
	fr = frame_new(width, h, [4]u8{16, 16, 22, 255})
	raster_lines(&rc, &fr, [4]u8{232, 232, 238, 255})
	meta.source = strings.clone(page_path)
	meta.base_url = strings.clone(base_url)
	meta.title = strings.clone(rc.title)
	meta.width, meta.height = width, h
	meta.full_height = full_h
	meta.lines, meta.links = len(rc.lines), len(rc.links)
	meta.fields, meta.images = len(rc.fields), len(rc.images)
	meta.truncated = truncated
	fmt.eprintfln("render %-40s %dx%d lines=%d links=%d title=%q", page_path, width, h, len(rc.lines), len(rc.links), rc.title)
	return fr, meta, true
}
