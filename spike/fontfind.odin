package spike

// Font discovery via fontconfig, dlopen'd at runtime: no link-time
// dependency, graceful degradation when the library is absent.
// Only the ~8 calls needed for family -> file resolution are bound.

import "core:strings"

foreign import dynlib {"system:c"}

@(default_calling_convention="c")
foreign dynlib {
	dlopen :: proc(filename: cstring, flags: i32) -> rawptr ---
	dlsym :: proc(handle: rawptr, symbol: cstring) -> rawptr ---
	dlclose :: proc(handle: rawptr) -> i32 ---
}

RTLD_NOW :: 2
FC_MATCH_PATTERN :: 0
FC_FILE :: "file"

// Function-pointer types matching fontconfig's C ABI.
Fc_Init_Cfg_Fn :: proc "c" () -> rawptr
Fc_Name_Parse_Fn :: proc "c" (name: cstring) -> rawptr
Fc_Substitute_Fn :: proc "c" (cfg, pat: rawptr, kind: i32) -> bool
Fc_Def_Subst_Fn :: proc "c" (pat: rawptr)
Fc_Font_Match_Fn :: proc "c" (cfg, pat: rawptr, result: ^i32) -> rawptr
Fc_Pat_Get_Str_Fn :: proc "c" (pat: rawptr, object: cstring, n: i32, s: ^cstring) -> i32
Fc_Pat_Destroy_Fn :: proc "c" (pat: rawptr)
Fc_Cfg_Destroy_Fn :: proc "c" (cfg: rawptr)

Font_Discovery :: struct {
	handle:      rawptr,
	cfg:         rawptr,
	init_cfg:    Fc_Init_Cfg_Fn,
	name_parse:  Fc_Name_Parse_Fn,
	substitute:  Fc_Substitute_Fn,
	def_subst:   Fc_Def_Subst_Fn,
	font_match:  Fc_Font_Match_Fn,
	pat_get_str: Fc_Pat_Get_Str_Fn,
	pat_destroy: Fc_Pat_Destroy_Fn,
	cfg_destroy: Fc_Cfg_Destroy_Fn,
}

fontfind_open :: proc() -> (Font_Discovery, bool) {
	d: Font_Discovery
	sons := []string{"libfontconfig.so.1", "libfontconfig.so"}
	for soname in sons {
		d.handle = dlopen(strings.clone_to_cstring(soname, context.temp_allocator), RTLD_NOW)
		if d.handle != nil {
			break
		}
	}
	if d.handle == nil {
		return d, false
	}
	load :: proc(d: ^Font_Discovery, sym: string) -> rawptr {
		return dlsym(d.handle, strings.clone_to_cstring(sym, context.temp_allocator))
	}
	d.init_cfg = transmute(Fc_Init_Cfg_Fn)load(&d, "FcInitLoadConfigAndFonts")
	d.name_parse = transmute(Fc_Name_Parse_Fn)load(&d, "FcNameParse")
	d.substitute = transmute(Fc_Substitute_Fn)load(&d, "FcConfigSubstitute")
	d.def_subst = transmute(Fc_Def_Subst_Fn)load(&d, "FcDefaultSubstitute")
	d.font_match = transmute(Fc_Font_Match_Fn)load(&d, "FcFontMatch")
	d.pat_get_str = transmute(Fc_Pat_Get_Str_Fn)load(&d, "FcPatternGetString")
	d.pat_destroy = transmute(Fc_Pat_Destroy_Fn)load(&d, "FcPatternDestroy")
	d.cfg_destroy = transmute(Fc_Cfg_Destroy_Fn)load(&d, "FcConfigDestroy")
	if d.init_cfg == nil || d.name_parse == nil || d.substitute == nil ||
	   d.def_subst == nil || d.font_match == nil || d.pat_get_str == nil ||
	   d.pat_destroy == nil || d.cfg_destroy == nil {
		dlclose(d.handle)
		d.handle = nil
		return d, false
	}
	d.cfg = d.init_cfg()
	if d.cfg == nil {
		dlclose(d.handle)
		d.handle = nil
		return d, false
	}
	return d, true
}

fontfind_close :: proc(d: ^Font_Discovery) {
	if d.cfg != nil {
		d.cfg_destroy(d.cfg)
		d.cfg = nil
	}
	if d.handle != nil {
		dlclose(d.handle)
		d.handle = nil
	}
}

// Resolve a family name ("DejaVu Sans") to a font file path. Owned result.
fontfind_file :: proc(d: ^Font_Discovery, family: string) -> (string, bool) {
	pat := d.name_parse(strings.clone_to_cstring(family, context.temp_allocator))
	if pat == nil {
		return "", false
	}
	defer d.pat_destroy(pat)
	d.substitute(d.cfg, pat, FC_MATCH_PATTERN)
	d.def_subst(pat)
	res: i32
	m := d.font_match(d.cfg, pat, &res)
	if m == nil || res != 0 {
		if m != nil {
			d.pat_destroy(m)
		}
		return "", false
	}
	defer d.pat_destroy(m)
	s: cstring
	if d.pat_get_str(m, FC_FILE, 0, &s) != 0 || s == nil {
		return "", false
	}
	return strings.clone(string(s)), true
}
