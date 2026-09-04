package spike

// Render core: lexbor DOM -> block flow layout -> kb shaping -> stb_truetype
// raster into an RGBA framebuffer. Backends (SDL3 window, PNG file, Kitty
// graphics) consume the framebuffer; see kitty.odin and the show/tui modes.

import c "core:c"
import "core:fmt"
import "core:os"
import "core:strings"

import "base:runtime"

import kbts "vendor:kb_text_shape"



Loaded_Font :: struct {
	path:     string,
	data:     []u8,
	stb:      ^Stbtt_Info,
	kb:       ^kbts.font,
	upm:      int,
	px:       f32,
	scale:    f32, // px per font unit
	ascent:   f32, // px, scaled
	descent:  f32, // px, scaled (negative)
	line_gap: f32, // px, scaled
}

Placed :: struct {
	font:       int, // index into Render_Ctx.fonts
	gid:        u16,
	x:          f32, // pen position, px from left margin
	off_x:      f32, // GPOS offset, px
	off_y:      f32, // GPOS offset, px
	adv:        f32, // advance, px
}

Line :: struct {
	glyphs:   [dynamic]Placed,
	text:     string, // plain-text fallback (words joined)
	baseline: f32,
	height:   f32,
}

Render_Ctx :: struct {
	fonts:      [dynamic]Loaded_Font,
	kb_ctx:     ^kbts.shape_context,
	kb_alloc:   ^runtime.Allocator, // heap-held: kb calls back into it for ctx lifetime
	body_px:    f32,
	max_width:  f32,
	margin:     f32,
	lines:      [dynamic]Line,
}

delete_render_ctx :: proc(rc: ^Render_Ctx) {
	render_ctx_free(rc)
}

u16be :: proc(d: []u8, o: int) -> int {
	return int(d[o]) << 8 | int(d[o + 1])
}

u32be :: proc(d: []u8, o: int) -> int {
	return int(d[o]) << 24 | int(d[o + 1]) << 16 | int(d[o + 2]) << 8 | int(d[o + 3])
}

// unitsPerEm from the TTF 'head' table at a font offset (0 for plain TTF,
// the collection member offset for TTC); 0 on failure.
font_upm_at :: proc(data: []u8, base: int) -> int {
	if base + 12 > len(data) {
		return 0
	}
	n := u16be(data, base + 4)
	for i in 0 ..< n {
		off := base + 12 + i * 16
		if off + 16 > len(data) {
			return 0
		}
		if u32be(data, off) == 0x68656164 { // 'head'
			// Table offsets are absolute even inside a TTC member.
			toff := u32be(data, off + 8)
			if toff + 20 > len(data) {
				return 0
			}
			return u16be(data, toff + 18)
		}
	}
	return 0
}

FONT_PATHS := [?]struct {
	path:  string,
	index: int,
	px:    f32,
} {
	// NOTE: kb resolves fallback last-pushed-first, so the widest-coverage
	// fallback (wqy) goes first and the preferred Latin font goes last.
	{"fonts/wqy-microhei.ttc", 0, 20},
	{"fonts/Waree.ttf", 0, 20},
	{"fonts/NotoSansDevanagari-Regular.ttf", 0, 20},
	{"fonts/NotoSansHebrew-Regular.ttf", 0, 20},
	{"fonts/NotoSansArabic-Regular.ttf", 0, 20},
	{"fonts/DejaVuSans.ttf", 0, 20},
}

render_ctx_new :: proc(body_px: f32, max_width: f32) -> (Render_Ctx, bool) {
	rc: Render_Ctx
	rc.body_px = body_px
	rc.max_width = max_width
	rc.margin = 24
	alloc := new(runtime.Allocator)
	alloc^ = context.allocator
	alloc_fn, alloc_data := kbts.AllocatorFromOdinAllocator(alloc)
	rc.kb_ctx = kbts.CreateShapeContext(alloc_fn, alloc_data)
	if rc.kb_ctx == nil {
		return rc, false
	}
	rc.kb_alloc = alloc
	for fp in FONT_PATHS {
	data, err := os.read_entire_file_from_path(fp.path, context.allocator)
		if err != nil {
			fmt.eprintfln("render: cannot read font %s", fp.path)
			return rc, false
		}
		off := stbtt_GetFontOffsetForIndex(raw_data(data), i32(fp.index))
		if off < 0 {
			fmt.eprintfln("render: bad font offset %s", fp.path)
			return rc, false
		}
		stb := spike_stbtt_alloc()
		if stbtt_InitFont(stb, raw_data(data), off) == 0 {
			fmt.eprintfln("render: stbtt init failed %s", fp.path)
			return rc, false
		}
		upm := font_upm_at(data, int(off))
		if upm == 0 {
			fmt.eprintfln("render: no head table %s", fp.path)
			return rc, false
		}
		px := body_px if fp.px == 20 else fp.px
		scale := px / f32(upm)
		a, d, g: i32
		stbtt_GetFontVMetrics(stb, &a, &d, &g)
		kh := kbts.ShapePushFontFromMemory(rc.kb_ctx, data, c.int(fp.index))
		if kh == nil {
			fmt.eprintfln("render: kb push failed %s", fp.path)
			return rc, false
		}
		append(&rc.fonts, Loaded_Font{fp.path, data, stb, kh, upm, px, scale, f32(a) * scale, f32(d) * scale, f32(g) * scale})
	}
	return rc, true
}

render_ctx_free :: proc(rc: ^Render_Ctx) {
	// Tear down the shaper first: it borrows font bytes and the allocator.
	kbts.DestroyShapeContext(rc.kb_ctx)
	rc.kb_ctx = nil
	for &f in rc.fonts {
		spike_stbtt_free(f.stb)
		delete(f.data)
		// NOTE: f.path aliases the static FONT_PATHS table; must not free.
	}
	delete(rc.fonts)
	for &ln in rc.lines {
		delete(ln.glyphs)
		delete(ln.text)
	}
	delete(rc.lines)
	free(rc.kb_alloc)
	rc.kb_alloc = nil
}

font_index_of :: proc(rc: ^Render_Ctx, kh: ^kbts.font) -> int {
	for f, i in rc.fonts {
		if f.kb == kh {
			return i
		}
	}
	return 0
}

Cur_Line :: struct {
	glyphs: [dynamic]Placed,
	words:  [dynamic]string, // owned copies for the text fallback
	width:  f32,
}

// Shape one whitespace-free word and append its glyphs at the pen.
shape_word :: proc(rc: ^Render_Ctx, word: string, size_px: f32, line: ^Cur_Line) -> f32 {
	kbts.ShapeBegin(rc.kb_ctx, .DONT_KNOW, .DONT_KNOW)
	kbts.ShapeUtf8(rc.kb_ctx, word, .CODEPOINT_INDEX)
	kbts.ShapeEnd(rc.kb_ctx)
	w := f32(0)
	for run, ok := kbts.ShapeRun(rc.kb_ctx); ok; run, ok = kbts.ShapeRun(rc.kb_ctx) {
		fi := font_index_of(rc, run.Font)
		f := &rc.fonts[fi]
		k := size_px / f.px // rescale advances/offsets for heading sizes
		rtl := run.Direction == .KBTS_DIRECTION_RTL
		// Collect run glyphs first (needed for RTL pen order).
		G :: struct {
			gid:        u16,
			adv, ox, oy: f32,
		}
		gs: [dynamic]G
		defer delete(gs)
		it := run.Glyphs
		for g, gok := kbts.GlyphIteratorNext(&it); gok; g, gok = kbts.GlyphIteratorNext(&it) {
			append(&gs, G{g.Id, f32(g.AdvanceX) * f.scale * k, f32(g.OffsetX) * f.scale * k, f32(g.OffsetY) * f.scale * k})
		}
		if rtl {
			// Lay right-to-left: reserve the run width, then place.
			run_w := f32(0)
			for g in gs {
				run_w += g.adv
			}
			x := line.width + run_w
			for g in gs {
				x -= g.adv
				append(&line.glyphs, Placed{fi, g.gid, x, g.ox, g.oy, g.adv})
			}
			line.width += run_w
			w += run_w
		} else {
			for g in gs {
				append(&line.glyphs, Placed{fi, g.gid, line.width, g.ox, g.oy, g.adv})
				line.width += g.adv
			}
			for g in gs {
				w += g.adv
			}
		}
	}
	append(&line.words, strings.clone(word))
	return w
}

flush_line :: proc(rc: ^Render_Ctx, line: ^Cur_Line, size_px: f32, y: ^f32, force: bool) {
	if len(line.glyphs) == 0 && !force {
		return
	}
	lh := size_px * 1.35
	ln: Line
	ln.glyphs = line.glyphs
	ln.text = strings.join(line.words[:], " ")
	for w in line.words {
		delete(w)
	}
	ln.baseline = y^ + lh * 0.8
	ln.height = lh
	append(&rc.lines, ln)
	y^ += lh
	line.glyphs = nil
	delete(line.words)
	line.words = nil
	line.width = 0
}

is_block_tag :: proc(tag: string) -> bool {
	switch tag {
	case "p", "h1", "h2", "h3", "h4", "div", "ul", "ol", "li", "br", "hr", "blockquote", "tr", "title":
		return true
	}
	return false
}

heading_px :: proc(rc: ^Render_Ctx, tag: string) -> f32 {
	switch tag {
	case "h1":
		return 32
	case "h2":
		return 27
	case "h3":
		return 23
	}
	return rc.body_px
}

is_skip_tag :: proc(tag: string) -> bool {
	switch tag {
	case "head", "script", "style", "noscript", "svg", "canvas", "template":
		return true
	}
	return false
}

tag_name_of :: proc(node: ^Dom_Node) -> string {
	tid := lxb_dom_node_tag_id_noi(node)
	tlen: uint
	ptr := lxb_tag_name_by_id_noi(tid, &tlen)
	if ptr == nil {
		return strings.clone("")
	}
	return strings.clone(strings.string_from_ptr(ptr, int(tlen)))
}

// Block flow layout over the lexbor DOM. Words are shaped individually:
// spaces never join across in any script, so this is shaping-safe.
layout_html :: proc(rc: ^Render_Ctx, doc: ^Html_Document) {
	y := rc.margin
	size_px := rc.body_px
	line: Cur_Line
	defer {
		flush_line(rc, &line, size_px, &y, false)
		delete(line.glyphs)
		delete(line.words)
	}
	// Iterative walk with enter/exit events. Index-based: the stack may
	// reallocate on append, so never hold &stack[i] across one.
	Walk :: struct {
		node:    ^Dom_Node,
		child:   ^Dom_Node, // next child to visit
		entered: bool,
	}
	stack: [dynamic]Walk
	defer delete(stack)
	append(&stack, Walk{(^Dom_Node)(doc), nil, false})
	skip_depth := 0
	for len(stack) > 0 {
		top := len(stack) - 1
		if !stack[top].entered {
			// Enter node.
			stack[top].entered = true
			node := stack[top].node
			stack[top].child = node_field(node, NODE_OFF_FIRST_CHILD)
			t := node_u32(node, NODE_OFF_TYPE)
			if t == NODE_TYPE_ELEMENT {
				tag := tag_name_of(node)
				defer delete(tag)
				if is_skip_tag(tag) {
					skip_depth += 1
				} else if skip_depth == 0 {
					if tag == "br" || tag == "hr" {
						flush_line(rc, &line, size_px, &y, tag == "br")
						if tag == "hr" {
							y += 6
						}
					} else if is_block_tag(tag) {
						flush_line(rc, &line, size_px, &y, false)
						size_px = heading_px(rc, tag)
						if tag == "li" {
							shape_word(rc, "•", size_px, &line)
							shape_word(rc, " ", size_px, &line)
						}
						if tag != "li" && tag != "title" && size_px != rc.body_px {
							y += 6
						}
					}
				}
			} else if t == NODE_TYPE_TEXT && skip_depth == 0 {
				tlen: uint
				ptr := lxb_dom_node_text_content(node, &tlen)
				if ptr != nil && tlen > 0 {
					text := strings.string_from_ptr(ptr, int(tlen))
					// Split on whitespace runs; shape word by word.
					for seg in strings.split_multi(text, []string{" ", "\t", "\n", "\r", "\f", "\v"}, context.temp_allocator) {
						word := strings.trim_space(seg)
						if len(word) == 0 {
							continue
						}
						// Greedy wrap: measure first with a scratch pen.
						probe: Cur_Line
						w := shape_word(rc, word, size_px, &probe)
						// Reclaim probe glyphs into the real line on fit.
						if line.width > 0 && line.width+w > rc.max_width {
							flush_line(rc, &line, size_px, &y, false)
						}
						for p in probe.glyphs {
							q := p
							q.x += line.width
							append(&line.glyphs, q)
						}
						line.width += w
						for wd in probe.words {
							append(&line.words, wd)
						}
						delete(probe.glyphs)
						delete(probe.words)
						// Space advance after the word.
						line.width += rc.fonts[0].scale * 280
					}
				}
			}
		}
		// Descend or exit.
		kid := stack[len(stack) - 1].child
		if kid != nil {
			stack[len(stack) - 1].child = node_field(kid, NODE_OFF_NEXT)
			append(&stack, Walk{kid, nil, false})
		} else {
			xnode := stack[len(stack) - 1].node
			t := node_u32(xnode, NODE_OFF_TYPE)
			if t == NODE_TYPE_ELEMENT {
				tag := tag_name_of(xnode)
				defer delete(tag)
				if is_skip_tag(tag) {
					skip_depth -= 1
				} else if skip_depth == 0 && is_block_tag(tag) {
					flush_line(rc, &line, size_px, &y, false)
					size_px = rc.body_px
				}
			}
			pop(&stack)
		}
	}
}

Frame :: struct {
	w, h:  int,
	px:    []u8, // RGBA
}

frame_new :: proc(w, h: int, bg: [4]u8) -> Frame {
	fr: Frame
	fr.w, fr.h = w, h
	fr.px = make([]u8, w * h * 4)
	for i := 0; i < w * h; i += 1 {
		o := i * 4
		fr.px[o + 0], fr.px[o + 1], fr.px[o + 2], fr.px[o + 3] = bg[0], bg[1], bg[2], bg[3]
	}
	return fr
}

// Rasterize laid-out lines into the framebuffer.
raster_lines :: proc(rc: ^Render_Ctx, fr: ^Frame, fg: [4]u8) {
	for ln in rc.lines {
		for p in ln.glyphs {
			f := &rc.fonts[p.font]
			if p.gid == 0 {
				// .notdef tofu: hollow box at pen.
				tofu_draw(fr, p.x, ln.baseline - f.ascent, f.ascent * 0.7, f.ascent * 0.7, fg)
				continue
			}
			w, h, xoff, yoff: i32
			bmp := stbtt_GetGlyphBitmap(f.stb, f.scale, f.scale, i32(p.gid), &w, &h, &xoff, &yoff)
			if bmp == nil || w <= 0 || h <= 0 {
				continue
			}
			dx := int(p.x + f32(xoff) + p.off_x + 0.5)
			dy := int(ln.baseline + f32(yoff) + p.off_y + 0.5)
			for row in 0 ..< int(h) {
				yy := dy + row
				if yy < 0 || yy >= fr.h {
					continue
				}
				for col in 0 ..< int(w) {
					xx := dx + col
					if xx < 0 || xx >= fr.w {
						continue
					}
					a := f32(bmp[row * int(w) + col]) / 255
					o := (yy * fr.w + xx) * 4
					fr.px[o + 0] = u8(f32(fg[0]) * a + f32(fr.px[o + 0]) * (1 - a) + 0.5)
					fr.px[o + 1] = u8(f32(fg[1]) * a + f32(fr.px[o + 1]) * (1 - a) + 0.5)
					fr.px[o + 2] = u8(f32(fg[2]) * a + f32(fr.px[o + 2]) * (1 - a) + 0.5)
				}
			}
			stbtt_FreeBitmap(bmp, nil)
		}
	}
}

tofu_draw :: proc(fr: ^Frame, x, y, w, h: f32, fg: [4]u8) {
	x0, y0 := int(x + 0.5), int(y + 0.5)
	x1, y1 := x0 + int(w + 0.5), y0 + int(h + 0.5)
	for yy in y0 ..= y1 {
		if yy < 0 || yy >= fr.h {
			continue
		}
		for xx in x0 ..= x1 {
			if xx < 0 || xx >= fr.w {
				continue
			}
			if xx == x0 || xx == x1 || yy == y0 || yy == y1 {
				o := (yy * fr.w + xx) * 4
				fr.px[o + 0], fr.px[o + 1], fr.px[o + 2], fr.px[o + 3] = fg[0], fg[1], fg[2], fg[3]
			}
		}
	}
}

// Full page: parse + layout. Caller owns the ctx (render_ctx_free).
layout_page :: proc(html_path: string, max_width: int) -> (Render_Ctx, bool) {
	empty: Render_Ctx
	data, err := os.read_entire_file_from_path(html_path, context.allocator)
	if err != nil {
		fmt.eprintfln("render: cannot read %s", html_path)
		return empty, false
	}
	defer delete(data)
	doc := lxb_html_document_create()
	if doc == nil {
		return empty, false
	}
	defer lxb_html_document_destroy(doc)
	if lxb_html_document_parse(doc, raw_data(data), uint(len(data))) != LXB_STATUS_OK {
		fmt.eprintfln("render: parse failed %s", html_path)
		return empty, false
	}
	rc, ok := render_ctx_new(20, f32(max_width))
	if !ok {
		return empty, false
	}
layout_html(&rc, doc)
	return rc, true
}

// Full page: parse -> layout -> raster. Returns frame owned by caller.
render_page :: proc(html_path: string, max_width: int) -> (Frame, bool) {
	fr: Frame
	rc, ok := layout_page(html_path, max_width)
	if !ok {
		return fr, false
	}
	defer render_ctx_free(&rc)
	h := int(rc.margin * 2 + 20)
	if len(rc.lines) > 0 {
		last := rc.lines[len(rc.lines) - 1]
		h = int(last.baseline + rc.body_px * 0.6 + rc.margin)
	}
	if h < 100 {
		h = 100
	}
	if h > 4000 {
		h = 4000
	}
	fr = frame_new(max_width, h, [4]u8{16, 16, 22, 255})
	raster_lines(&rc, &fr, [4]u8{232, 232, 238, 255})
	fmt.eprintfln("render %-40s %dx%d lines=%d", html_path, fr.w, fr.h, len(rc.lines))
	return fr, true
}

frame_encode_png :: proc(fr: ^Frame) -> ([]u8, bool) {
	out_len: i32
	mem := stbi_write_png_to_mem(raw_data(fr.px), i32(fr.w * 4), i32(fr.w), i32(fr.h), 4, &out_len)
	if mem == nil || out_len <= 0 {
		return nil, false
	}
	png := make([]u8, out_len)
	copy(png, mem[:out_len])
	spike_free(mem)
	return png, true
}

frame_write_png :: proc(fr: ^Frame, path: string) -> bool {
	png, ok := frame_encode_png(fr)
	if !ok {
		fmt.eprintln("render: png encode failed")
		return false
	}
	defer delete(png)
	if err := os.write_entire_file(path, png); err != nil {
		fmt.eprintfln("render: cannot write %s", path)
		return false
	}
	fmt.eprintfln("wrote %s (%d KB)", path, len(png) / 1024)
	return true
}
