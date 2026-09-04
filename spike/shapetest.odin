package spike

// Shaping conformance suite: popular scripts through kb_text_shape.
// Each case asserts structural shaping facts (ligatures, run direction,
// conjuncts, advance uniformity) that are font-independent in spirit but
// pinned to the corpus fonts in fonts/.
//
//   spike shapetest   # exit 0 iff every case passes

import "core:c"
import "core:fmt"
import "core:os"
import "core:strings"

import kbts "vendor:kb_text_shape"

Shape_Result :: struct {
	glyphs:   int,
	advances: [dynamic]i32,
	dirs:     [dynamic]direction,
}

delete_shape_result :: proc(r: ^Shape_Result) {
	delete(r.advances)
	delete(r.dirs)
}

// direction is kbts.direction; alias for brevity.
direction :: kbts.direction

shape_collect :: proc(ctx: ^kbts.shape_context, text: string) -> (Shape_Result, bool) {
	r: Shape_Result
	kbts.ShapeBegin(ctx, .DONT_KNOW, .DONT_KNOW)
	kbts.ShapeUtf8(ctx, text, .CODEPOINT_INDEX)
	kbts.ShapeEnd(ctx)
	if kbts.ShapeError(ctx) != .NONE {
		return r, false
	}
	for run, ok := kbts.ShapeRun(ctx); ok; run, ok = kbts.ShapeRun(ctx) {
		append(&r.dirs, run.Direction)
		it := run.Glyphs
		for g, gok := kbts.GlyphIteratorNext(&it); gok; g, gok = kbts.GlyphIteratorNext(&it) {
			r.glyphs += 1
			append(&r.advances, g.AdvanceX)
		}
	}
	return r, true
}

with_font :: proc(font_path: string, font_index: int, fn: proc(ctx: ^kbts.shape_context) -> bool) -> bool {
	data, err := os.read_entire_file_from_path(font_path, context.allocator)
	if err != nil {
		fmt.printfln("FAIL font load %s", font_path)
		return false
	}
	defer delete(data)
	alloc := context.allocator
	alloc_fn, alloc_data := kbts.AllocatorFromOdinAllocator(&alloc)
	ctx := kbts.CreateShapeContext(alloc_fn, alloc_data)
	if ctx == nil {
		fmt.println("FAIL context create")
		return false
	}
	defer kbts.DestroyShapeContext(ctx)
	if kbts.ShapePushFontFromMemory(ctx, data, c.int(font_index)) == nil {
		fmt.printfln("FAIL font push %s", font_path)
		return false
	}
	return fn(ctx)
}

check :: proc(name: string, cond: bool, detail: string) -> bool {
	fmt.printfln("%s %-18s %s", "PASS" if cond else "FAIL", name, detail)
	return cond
}

run_shapetests :: proc() -> bool {
	all_ok := true

	// 1. Latin baseline.
	all_ok = all_ok && with_font("fonts/DejaVuSans.ttf", 0, proc(ctx: ^kbts.shape_context) -> bool {
		r, ok := shape_collect(ctx, "Hello, world!")
		defer delete_shape_result(&r)
		if !ok {
			return check("latin/hello", false, "shape error")
		}
		positive := true
		for a in r.advances {
			positive &= a > 0
		}
		return check("latin/hello", r.glyphs == 13 && positive && len(r.dirs) == 1 && r.dirs[0] == .KBTS_DIRECTION_LTR,
			fmt.tprintf("glyphs=%d runs=%v", r.glyphs, r.dirs))
	})

	// 2. Arabic lam-alef ligature: 6 chars -> 5 glyphs.
	all_ok = all_ok && with_font("fonts/NotoSansArabic-Regular.ttf", 0, proc(ctx: ^kbts.shape_context) -> bool {
		r, ok := shape_collect(ctx, "السلام")
		defer delete_shape_result(&r)
		if !ok {
			return check("arabic/lamalef", false, "shape error")
		}
		return check("arabic/lamalef", r.glyphs == 5 && len(r.dirs) == 1 && r.dirs[0] == .KBTS_DIRECTION_RTL,
			fmt.tprintf("glyphs=%d runs=%v", r.glyphs, r.dirs))
	})

	// 3. Arabic plain word: 5 chars -> 5 glyphs, RTL.
	all_ok = all_ok && with_font("fonts/NotoSansArabic-Regular.ttf", 0, proc(ctx: ^kbts.shape_context) -> bool {
		r, ok := shape_collect(ctx, "مرحبا")
		defer delete_shape_result(&r)
		if !ok {
			return check("arabic/word", false, "shape error")
		}
		return check("arabic/word", r.glyphs == 5 && len(r.dirs) == 1 && r.dirs[0] == .KBTS_DIRECTION_RTL,
			fmt.tprintf("glyphs=%d runs=%v", r.glyphs, r.dirs))
	})

	// 4. Hebrew: 4 chars -> 4 glyphs, RTL.
	all_ok = all_ok && with_font("fonts/NotoSansHebrew-Regular.ttf", 0, proc(ctx: ^kbts.shape_context) -> bool {
		r, ok := shape_collect(ctx, "שלום")
		defer delete_shape_result(&r)
		if !ok {
			return check("hebrew/word", false, "shape error")
		}
		return check("hebrew/word", r.glyphs == 4 && len(r.dirs) == 1 && r.dirs[0] == .KBTS_DIRECTION_RTL,
			fmt.tprintf("glyphs=%d runs=%v", r.glyphs, r.dirs))
	})

	// 5. Devanagari conjunct k+halant+ssa -> fewer glyphs than chars.
	all_ok = all_ok && with_font("fonts/NotoSansDevanagari-Regular.ttf", 0, proc(ctx: ^kbts.shape_context) -> bool {
		r, ok := shape_collect(ctx, "क्ष")
		defer delete_shape_result(&r)
		if !ok {
			return check("deva/conjunct", false, "shape error")
		}
		return check("deva/conjunct", r.glyphs <= 2,
			fmt.tprintf("glyphs=%d (3 chars) runs=%v", r.glyphs, r.dirs))
	})

	// 6. Thai with above/below marks: shapes cleanly, sane advances.
	all_ok = all_ok && with_font("fonts/Waree.ttf", 0, proc(ctx: ^kbts.shape_context) -> bool {
		r, ok := shape_collect(ctx, "สวัสดี")
		defer delete_shape_result(&r)
		if !ok {
			return check("thai/word", false, "shape error")
		}
		sane := true
		for a in r.advances {
			sane &= a >= 0
		}
		return check("thai/word", r.glyphs >= 4 && r.glyphs <= 7 && sane,
			fmt.tprintf("glyphs=%d (6 chars) runs=%v", r.glyphs, r.dirs))
	})

	// 7. CJK fullwidth: uniform advances.
	all_ok = all_ok && with_font("fonts/wqy-microhei.ttc", 0, proc(ctx: ^kbts.shape_context) -> bool {
		r, ok := shape_collect(ctx, "日本語")
		defer delete_shape_result(&r)
		if !ok {
			return check("cjk/word", false, "shape error")
		}
		uniform := len(r.advances) == 3 && r.advances[0] > 0
		for a in r.advances {
			uniform &= a == r.advances[0]
		}
		return check("cjk/word", uniform, fmt.tprintf("glyphs=%d adv=%v", r.glyphs, r.advances))
	})

	// 8. Mixed bidi splits runs by direction (coverage-independent).
	all_ok = all_ok && with_font("fonts/NotoSansHebrew-Regular.ttf", 0, proc(ctx: ^kbts.shape_context) -> bool {
		r, ok := shape_collect(ctx, "abc שלום")
		defer delete_shape_result(&r)
		if !ok {
			return check("bidi/split", false, "shape error")
		}
		split := len(r.dirs) == 2 && r.dirs[0] != r.dirs[1]
		return check("bidi/split", split, fmt.tprintf("runs=%v", r.dirs))
	})

	_ = strings.has_prefix
	return all_ok
}
