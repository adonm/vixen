package spike

// Render core: lexbor DOM -> block flow layout -> kb shaping -> stb_truetype
// raster into an RGBA framebuffer. Backends (SDL3 window, PNG file, Kitty
// graphics) consume the framebuffer; see kitty.odin and the show/tui modes.

import c "core:c"
import "core:fmt"
import "core:os"
import "core:strings"
import "core:time"

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
	font:       int, // index into Font_Bank.fonts
	gid:        u16,
	x:          f32, // pen position, px from left margin
	off_x:      f32, // GPOS offset, px
	off_y:      f32, // GPOS offset, px
	adv:        f32, // advance, px
	seq:        int, // global glyph sequence for link spans
}

Line :: struct {
	glyphs:   [dynamic]Placed,
	text:     string, // plain-text fallback (words joined)
	baseline: f32,
	height:   f32,
}

// Hyperlink with a resolved absolute URL and a rendered bbox.
Link :: struct {
	url:        string, // owned absolute URL
	x0, y0:    f32,
	x1, y1:    f32,
}

// Open anchor while walking: resolved URL + starting glyph sequence.
Anchor_Open :: struct {
	url:      string, // owned
	seq0:     int,
}

Link_Span :: struct {
	url:        string, // owned
	seq0, seq1: int,
}

// Fonts + shaper, loaded once per session and borrowed by every page.
Font_Bank :: struct {
	fonts:    [dynamic]Loaded_Font,
	kb_ctx:   ^kbts.shape_context,
	kb_alloc: ^runtime.Allocator, // heap-held: kb calls back into it for ctx lifetime
}

Render_Ctx :: struct {
	bank:      ^Font_Bank,
	body_px:   f32,
	max_width: f32,
	margin:    f32,
	lines:     [dynamic]Line,
	indent:    f32, // current block indent (tables), px
	next_seq:  int,
	title:     string, // owned, from <title>
	in_title:  bool,
	page_url:  string, // owned base URL for link resolution ("" = skip links)
	anchors:   [dynamic]Anchor_Open,
	spans:     [dynamic]Link_Span,
	links:     [dynamic]Link, // finalized bboxes
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

FONT_SPECS := [?]struct {
	families: [2]string, // primary, fallback ("" = none)
	index:    int, // collection member (ttc)
	px:       f32,
} {
	// NOTE: kb resolves fallback last-pushed-first, so the widest-coverage
	// fallback (wqy) goes first and the preferred Latin font goes last.
	{{"WenQuanYi Micro Hei", ""}, 0, 20},
	{{"Noto Serif Thai", "Waree"}, 0, 20},
	{{"Noto Sans Devanagari", "Lohit Devanagari"}, 0, 20},
	{{"Noto Sans Hebrew", "Ezra SIL"}, 0, 20},
	{{"Noto Sans Arabic", ""}, 0, 20},
	{{"DejaVu Sans", ""}, 0, 20},
}

font_bank_load :: proc(body_px: f32) -> (Font_Bank, bool) {
	t0 := time.now()
	bank: Font_Bank
	fc, fok := fontfind_open()
	if !fok {
		fmt.eprintln("render: fontconfig unavailable")
		return bank, false
	}
	defer fontfind_close(&fc)
	alloc := new(runtime.Allocator)
	alloc^ = context.allocator
	alloc_fn, alloc_data := kbts.AllocatorFromOdinAllocator(alloc)
	bank.kb_ctx = kbts.CreateShapeContext(alloc_fn, alloc_data)
	if bank.kb_ctx == nil {
		free(alloc)
		return bank, false
	}
	bank.kb_alloc = alloc
	for spec in FONT_SPECS {
		path := ""
		for fam in spec.families {
			if len(fam) == 0 {
				continue
			}
			if p, ok := fontfind_file(&fc, fam); ok {
				path = p
				break
			}
		}
		if len(path) == 0 {
			fmt.eprintfln("render: no font for %q, skipping (tofu will show)", spec.families[0])
			continue
		}
		// Ownership of path transfers into Loaded_Font below; do not free here.
		data, err := os.read_entire_file_from_path(path, context.allocator)
		if err != nil {
			fmt.eprintfln("render: cannot read font %s", path)
			return bank, false
		}
		off := stbtt_GetFontOffsetForIndex(raw_data(data), i32(spec.index))
		if off < 0 {
			fmt.eprintfln("render: bad font offset %s", path)
			delete(data)
			return bank, false
		}
		stb := spike_stbtt_alloc()
		if stbtt_InitFont(stb, raw_data(data), off) == 0 {
			fmt.eprintfln("render: stbtt init failed %s", path)
			delete(data)
			return bank, false
		}
		upm := font_upm_at(data, int(off))
		if upm == 0 {
			fmt.eprintfln("render: no head table %s", path)
			delete(data)
			return bank, false
		}
		px := body_px if spec.px == 20 else spec.px
		scale := px / f32(upm)
		a, d, g: i32
		stbtt_GetFontVMetrics(stb, &a, &d, &g)
		kh := kbts.ShapePushFontFromMemory(bank.kb_ctx, data, c.int(spec.index))
		if kh == nil {
			fmt.eprintfln("render: kb push failed %s", path)
			delete(data)
			return bank, false
		}
		append(&bank.fonts, Loaded_Font{path, data, stb, kh, upm, px, scale, f32(a) * scale, f32(d) * scale, f32(g) * scale})
	}
	fmt.eprintfln("t+%-28s %8.1f ms", "font-bank-load", time.duration_milliseconds(time.since(t0)))
	return bank, true
}

font_bank_free :: proc(bank: ^Font_Bank) {
	// Tear down the shaper first: it borrows font bytes and the allocator.
	kbts.DestroyShapeContext(bank.kb_ctx)
	bank.kb_ctx = nil
	for &f in bank.fonts {
		spike_stbtt_free(f.stb)
		delete(f.data)
		delete(f.path) // owned (fontconfig discovery result)
	}
	delete(bank.fonts)
	free(bank.kb_alloc)
	bank.kb_alloc = nil
}

// Per-page context borrowing a session bank.
render_ctx_new :: proc(bank: ^Font_Bank, body_px, max_width: f32, page_url := "") -> Render_Ctx {
	rc: Render_Ctx
	rc.bank = bank
	rc.body_px = body_px
	rc.max_width = max_width
	rc.margin = 24
	rc.page_url = strings.clone(page_url)
	return rc
}

render_ctx_free :: proc(rc: ^Render_Ctx) {
	for &ln in rc.lines {
		delete(ln.glyphs)
		delete(ln.text)
	}
	delete(rc.lines)
	delete(rc.title)
	delete(rc.page_url)
	for &s in rc.spans {
		delete(s.url)
	}
	delete(rc.spans)
	for &a in rc.anchors {
		delete(a.url)
	}
	delete(rc.anchors)
	for &l in rc.links {
		delete(l.url)
	}
	delete(rc.links)
}

font_index_of :: proc(bank: ^Font_Bank, kh: ^kbts.font) -> int {
	for f, i in bank.fonts {
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
	x0:     f32, // indent origin for this line, px
}

// Shape one whitespace-free word and append its glyphs at the pen.
shape_word :: proc(rc: ^Render_Ctx, word: string, size_px: f32, line: ^Cur_Line) -> f32 {
	kbts.ShapeBegin(rc.bank.kb_ctx, .DONT_KNOW, .DONT_KNOW)
	kbts.ShapeUtf8(rc.bank.kb_ctx, word, .CODEPOINT_INDEX)
	kbts.ShapeEnd(rc.bank.kb_ctx)
	w := f32(0)
	for run, ok := kbts.ShapeRun(rc.bank.kb_ctx); ok; run, ok = kbts.ShapeRun(rc.bank.kb_ctx) {
		fi := font_index_of(rc.bank, run.Font)
		f := &rc.bank.fonts[fi]
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
		place := proc(rc: ^Render_Ctx, line: ^Cur_Line, fi: int, g: G, x: f32) {
			append(&line.glyphs, Placed{fi, g.gid, x, g.ox, g.oy, g.adv, rc.next_seq})
			rc.next_seq += 1
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
				place(rc, line, fi, g, x)
			}
			line.width += run_w
			w += run_w
		} else {
			for g in gs {
				place(rc, line, fi, g, line.width)
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
	case "p", "h1", "h2", "h3", "h4", "div", "ul", "ol", "li", "br", "hr", "blockquote", "tr", "title",
	     "table", "td", "th":
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
	case "head", "script", "style", "noscript", "svg", "canvas", "template",
	     "nav", "header", "footer":
		// nav/header/footer are landmark chrome; article content never lives
		// there per spec. Documented readability heuristic.
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

// Place one word with greedy wrap; x0 tracks the indent origin.
layout_place_word :: proc(rc: ^Render_Ctx, line: ^Cur_Line, y: ^f32, size_px: f32, word: string) {
	if len(line.glyphs) == 0 {
		line.x0 = rc.indent
	}
	probe: Cur_Line
	w := shape_word(rc, word, size_px, &probe)
	avail := rc.max_width - line.x0
	if line.width > 0 && line.width+w > avail {
		flush_line(rc, line, size_px, y, false)
		line.x0 = rc.indent
		avail = rc.max_width - line.x0
		_ = avail
	}
	for p in probe.glyphs {
		q := p
		q.x += line.width + line.x0
		append(&line.glyphs, q)
	}
	line.width += w
	for wd in probe.words {
		append(&line.words, wd)
	}
	delete(probe.glyphs)
	delete(probe.words)
	// Space advance after the word.
	line.width += rc.bank.fonts[0].scale * 280
}

// Anchor enter: always push (possibly empty); exit pops unconditionally.
// Symmetry keeps the stack balanced regardless of href validity.
anchor_enter :: proc(rc: ^Render_Ctx, node: ^Dom_Node) {
	url := ""
	if len(rc.page_url) > 0 {
		if href, ok := dom_attr_val(node, "href"); ok {
			defer delete(href)
			if len(href) > 0 && !strings.has_prefix(href, "#") && !strings.has_prefix(href, "javascript:") {
				if abs, rok := url_resolve(rc.page_url, href); rok {
					url = abs
				}
			}
		}
	}
	append(&rc.anchors, Anchor_Open{url, rc.next_seq})
}

anchor_exit :: proc(rc: ^Render_Ctx) {
	if len(rc.anchors) == 0 {
		return
	}
	a := pop(&rc.anchors)
	if len(a.url) == 0 {
		return // literal marker, nothing owned
	}
	if rc.next_seq > a.seq0 {
		append(&rc.spans, Link_Span{a.url, a.seq0, rc.next_seq})
	} else {
		delete(a.url)
	}
}

// Media placeholder as its own line.
img_placeholder :: proc(rc: ^Render_Ctx, line: ^Cur_Line, y: ^f32, size_px: f32, node: ^Dom_Node) {
	flush_line(rc, line, size_px, y, false)
	alt, ok := dom_attr_val(node, "alt")
	mark := "[image]"
	if ok && len(strings.trim_space(alt)) > 0 {
		mark = strings.concatenate([]string{"[image: ", strings.trim_space(alt), "]"}, context.temp_allocator)
	}
	if ok {
		delete(alt)
	}
	for seg in strings.split(mark, " ", context.temp_allocator) {
		if len(seg) == 0 {
			continue
		}
		layout_place_word(rc, line, y, size_px, seg)
	}
	flush_line(rc, line, size_px, y, true)
}

// Resolve link spans to rendered bboxes after layout.
finalize_links :: proc(rc: ^Render_Ctx) {
	for s in rc.spans {
		l: Link
		l.url = strings.clone(s.url)
		found := false
		for &ln in rc.lines {
			for &p in ln.glyphs {
				if p.seq < s.seq0 || p.seq >= s.seq1 {
					continue
				}
				gx0 := p.x
				gx1 := p.x + p.adv
				gy0 := ln.baseline - ln.height * 0.8
				gy1 := ln.baseline + ln.height * 0.2
				if !found {
					l.x0, l.y0, l.x1, l.y1 = gx0, gy0, gx1, gy1
					found = true
				} else {
					l.x0 = min(l.x0, gx0)
					l.y0 = min(l.y0, gy0)
					l.x1 = max(l.x1, gx1)
					l.y1 = max(l.y1, gy1)
				}
			}
		}
		if found {
			append(&rc.links, l)
		} else {
			delete(l.url)
		}
	}
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
				if tag == "title" {
					rc.in_title = true
				}
				if tag == "a" && skip_depth == 0 {
					anchor_enter(rc, node)
				}
				if is_skip_tag(tag) {
					skip_depth += 1
				} else if skip_depth == 0 {
					if tag == "br" || tag == "hr" {
						flush_line(rc, &line, size_px, &y, tag == "br")
						if tag == "hr" {
							y += 6
						}
					} else if tag == "img" {
						img_placeholder(rc, &line, &y, size_px, node)
					} else if tag == "table" {
						flush_line(rc, &line, size_px, &y, false)
						y += 4
					} else if tag == "tr" {
						flush_line(rc, &line, size_px, &y, false)
					} else if tag == "td" || tag == "th" {
						flush_line(rc, &line, size_px, &y, false)
						rc.indent += 26
					} else if is_block_tag(tag) {
						flush_line(rc, &line, size_px, &y, false)
						size_px = heading_px(rc, tag)
						if tag == "li" {
							layout_place_word(rc, &line, &y, size_px, "•")
						}
						if tag != "li" && tag != "title" && size_px != rc.body_px {
							y += 6
						}
					}
				}
			} else if t == NODE_TYPE_TEXT {
				tlen: uint
				ptr := lxb_dom_node_text_content(node, &tlen)
				if ptr != nil && tlen > 0 {
					text := strings.string_from_ptr(ptr, int(tlen))
					if rc.in_title {
						old := rc.title
						rc.title = strings.concatenate([]string{old, text})
						delete(old)
					} else if skip_depth == 0 {
					// Split on whitespace runs; shape word by word.
					for seg in strings.split_multi(text, []string{" ", "\t", "\n", "\r", "\f", "\v"}, context.temp_allocator) {
						word := strings.trim_space(seg)
						if len(word) == 0 {
							continue
						}
						layout_place_word(rc, &line, &y, size_px, word)
					}
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
				if tag == "title" {
					rc.in_title = false
				}
				if tag == "a" {
					anchor_exit(rc)
				}
				if is_skip_tag(tag) {
					skip_depth -= 1
				} else if skip_depth == 0 {
					if tag == "td" || tag == "th" {
						flush_line(rc, &line, size_px, &y, false)
						rc.indent -= 26
						if rc.indent < 0 {
							rc.indent = 0
						}
					} else if tag == "table" {
						flush_line(rc, &line, size_px, &y, false)
						y += 4
					} else if is_block_tag(tag) {
						flush_line(rc, &line, size_px, &y, false)
						size_px = rc.body_px
					}
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

// Blit one shaped glyph into an arbitrary RGBA buffer.
blit_glyph_px :: proc(buf: []u8, bw, bh: int, f: ^Loaded_Font, gid: u16, pen_x, baseline_y: f32, fg: [4]u8) {
	w, h, xoff, yoff: i32
	bmp := stbtt_GetGlyphBitmap(f.stb, f.scale, f.scale, i32(gid), &w, &h, &xoff, &yoff)
	if bmp == nil || w <= 0 || h <= 0 {
		return
	}
	dx := int(pen_x + f32(xoff) + 0.5)
	dy := int(baseline_y + f32(yoff) + 0.5)
	for row in 0 ..< int(h) {
		yy := dy + row
		if yy < 0 || yy >= bh {
			continue
		}
		for col in 0 ..< int(w) {
			xx := dx + col
			if xx < 0 || xx >= bw {
				continue
			}
			a := f32(bmp[row * int(w) + col]) / 255
			o := (yy * bw + xx) * 4
			buf[o + 0] = u8(f32(fg[0]) * a + f32(buf[o + 0]) * (1 - a) + 0.5)
			buf[o + 1] = u8(f32(fg[1]) * a + f32(buf[o + 1]) * (1 - a) + 0.5)
			buf[o + 2] = u8(f32(fg[2]) * a + f32(buf[o + 2]) * (1 - a) + 0.5)
		}
	}
	stbtt_FreeBitmap(bmp, nil)
}

// Rasterize the lines intersecting [y0, y1) into a fresh viewport frame.
// The TUI scrolls by re-slicing; whole pages are never rasterized.
raster_slice :: proc(bank: ^Font_Bank, lines: [dynamic]Line, width, y0, h: int) -> Frame {
	fr := frame_new(width, h, [4]u8{16, 16, 22, 255})
	fg := [4]u8{232, 232, 238, 255}
	for &ln in lines {
		top := ln.baseline - ln.height * 0.8
		bot := ln.baseline + ln.height * 0.2
		if bot < f32(y0) || top >= f32(y0 + h) {
			continue
		}
		dy := -f32(y0)
		for p in ln.glyphs {
			f := &bank.fonts[p.font]
			if p.gid == 0 {
				tofu_draw(&fr, p.x, ln.baseline + dy - f.ascent, f.ascent * 0.7, f.ascent * 0.7, fg)
				continue
			}
			blit_glyph_px(fr.px, fr.w, fr.h, f, p.gid, p.x + p.off_x, ln.baseline + dy + p.off_y, fg)
		}
	}
	return fr
}

// Rasterize laid-out lines into the framebuffer.
raster_lines :: proc(rc: ^Render_Ctx, fr: ^Frame, fg: [4]u8) {
	for ln in rc.lines {
		for p in ln.glyphs {
			f := &rc.bank.fonts[p.font]
			if p.gid == 0 {
				// .notdef tofu: hollow box at pen.
				tofu_draw(fr, p.x, ln.baseline - f.ascent, f.ascent * 0.7, f.ascent * 0.7, fg)
				continue
			}
			blit_glyph_px(fr.px, fr.w, fr.h, f, p.gid, p.x + p.off_x, ln.baseline + p.off_y, fg)
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

// Phase timer for the startup-to-rendered budget. Prints deltas to stderr.
Phase_Clock :: struct {
	t0:    time.Time,
	label: string,
}

phase_start :: proc(label: string) -> Phase_Clock {
	return Phase_Clock{time.now(), label}
}

phase_end :: proc(pc: ^Phase_Clock, what: string) {
	dt := time.since(pc.t0)
	fmt.eprintfln("t+%-28s %8.1f ms", what, time.duration_milliseconds(dt))
	pc.t0 = time.now()
}

// Full page: parse + layout over bytes. Caller owns the ctx.
layout_bytes :: proc(data: []u8, max_width: int, bank: ^Font_Bank, page_url := "") -> (Render_Ctx, bool) {
	empty: Render_Ctx
	doc := lxb_html_document_create()
	if doc == nil {
		return empty, false
	}
	defer lxb_html_document_destroy(doc)
	if lxb_html_document_parse(doc, raw_data(data), uint(len(data))) != LXB_STATUS_OK {
		return empty, false
	}
	rc := render_ctx_new(bank, 20, f32(max_width), page_url)
	layout_html(&rc, doc)
	finalize_links(&rc)
	return rc, true
}

// Full page: parse + layout. Caller owns the ctx (render_ctx_free).
layout_page :: proc(html_path: string, max_width: int, bank: ^Font_Bank) -> (Render_Ctx, bool) {
	empty: Render_Ctx
	data, err := os.read_entire_file_from_path(html_path, context.allocator)
	if err != nil {
		fmt.eprintfln("render: cannot read %s", html_path)
		return empty, false
	}
	defer delete(data)
	return layout_bytes(data, max_width, bank)
}

// Full page: parse -> layout -> raster. Returns frame owned by caller.
render_bytes :: proc(data: []u8, max_width: int, bank: ^Font_Bank, label: string, page_url := "") -> (Frame, bool) {
	fr: Frame
	pc := phase_start(label)
	rc, ok := layout_bytes(data, max_width, bank, page_url)
	if !ok {
		return fr, false
	}
	defer render_ctx_free(&rc)
	phase_end(&pc, "parse+layout")
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
	phase_end(&pc, "raster")
	fmt.eprintfln("render %-40s %dx%d lines=%d links=%d title=%q", label, fr.w, fr.h, len(rc.lines), len(rc.links), rc.title)
	return fr, true
}

// Full page: parse -> layout -> raster. Returns frame owned by caller.
render_page :: proc(html_path: string, max_width: int, bank: ^Font_Bank) -> (Frame, bool) {
	fr: Frame
	data, err := os.read_entire_file_from_path(html_path, context.allocator)
	if err != nil {
		fmt.eprintfln("render: cannot read %s", html_path)
		return fr, false
	}
	defer delete(data)
	return render_bytes(data, max_width, bank, html_path)
}

frame_encode_png :: proc(fr: ^Frame) -> ([]u8, bool) {
	return frame_encode_slice(fr.px, fr.w, fr.h)
}

// PNG-encode an arbitrary RGBA buffer.
frame_encode_slice :: proc(px: []u8, w, h: int) -> ([]u8, bool) {
	out_len: i32
	mem := stbi_write_png_to_mem(raw_data(px), i32(w * 4), i32(w), i32(h), 4, &out_len)
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
