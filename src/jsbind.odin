package vixen

// Binding DSL: table-declared JS classes, methods, properties.
// One framework (~here); each DOM interface becomes data, not glue.

import "core:strings"

JS_PROP_CONFIGURABLE :: 1

JS_Class_Def :: struct {
	class_name: cstring,
	finalizer:  rawptr,
	gc_mark:    rawptr,
	call:       rawptr,
	exotic:     rawptr,
}

Js_CFunc :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value
Js_Getter :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value
// NOTE: setters MUST use the generic form (argc/argv); a (ctx,this,val)
// shape reads the argv pointer as a value (see text/set failure).
Js_Setter :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value
Js_Finalizer :: proc "c" (rt: ^JS_Runtime, v: JS_Value)

Js_Method :: struct {
	name:   string,
	func:   Js_CFunc,
	length: i32,
}

Js_Prop :: struct {
	name: string,
	get:  Js_Getter, // nil = absent
	set:  Js_Setter, // nil = absent
}

// Register a class; returns its id. The def MUST be statically allocated:
// JS_NewClass references it (and class_name) for the runtime lifetime.
js_register_class :: proc(rt: ^JS_Runtime, def: ^JS_Class_Def) -> (u32, bool) {
	id: u32
	JS_NewClassID(&id)
	if JS_NewClass(rt, id, def) != 0 {
		return id, false
	}
	return id, true
}

// Wrap an opaque pointer in a fresh object of the class.
js_wrap :: proc(ctx: ^JS_Context, class_id: u32, ptr: rawptr) -> JS_Value {
	obj := JS_NewObjectClass(ctx, class_id)
	JS_SetOpaque(obj, ptr)
	return obj
}

js_unwrap :: proc(ctx: ^JS_Context, v: JS_Value, class_id: u32) -> rawptr {
	return JS_GetOpaque2(ctx, v, class_id)
}

js_install_methods :: proc(ctx: ^JS_Context, obj: JS_Value, methods: []Js_Method) {
	for m in methods {
		fn := JS_NewCFunction2(ctx, transmute(rawptr)m.func,
			strings.clone_to_cstring(m.name, context.temp_allocator), m.length, 0, 0)
		// SetPropertyStr consumes the function reference; do not free.
		JS_SetPropertyStr(ctx, obj, strings.clone_to_cstring(m.name, context.temp_allocator), fn)
	}
}

js_install_props :: proc(ctx: ^JS_Context, obj: JS_Value, props: []Js_Prop) {
	for p in props {
		getter := spike_js_undefined()
		setter := spike_js_undefined()
		if p.get != nil {
			getter = JS_NewCFunction2(ctx, transmute(rawptr)p.get,
				strings.clone_to_cstring(p.name, context.temp_allocator), 0, 0, 0)
		}
		if p.set != nil {
			setter = JS_NewCFunction2(ctx, transmute(rawptr)p.set,
				strings.clone_to_cstring(p.name, context.temp_allocator), 1, 0, 0)
		}
		atom := JS_NewAtom(ctx, strings.clone_to_cstring(p.name, context.temp_allocator))
		// DefinePropertyGetSet consumes getter/setter; the atom is ours to free.
		JS_DefinePropertyGetSet(ctx, obj, atom, getter, setter, JS_PROP_CONFIGURABLE)
		JS_FreeAtom(ctx, atom)
	}
}

// ---- value helpers ----

js_string :: proc(ctx: ^JS_Context, s: string) -> JS_Value {
	return JS_NewStringLen(ctx, strings.clone_to_cstring(s, context.temp_allocator), len(s))
}

js_int :: proc(ctx: ^JS_Context, v: i32) -> JS_Value {
	return spike_js_new_int32(ctx, v)
}

js_bool_val :: proc(ctx: ^JS_Context, b: bool) -> JS_Value {
	return spike_js_new_bool(ctx, 1 if b else 0)
}

js_to_string :: proc(ctx: ^JS_Context, v: JS_Value) -> (string, bool) {
	plen: uint
	cstr := spike_js_to_cstring(ctx, v, &plen)
	if cstr == nil {
		return "", false
	}
	defer spike_js_free_cstring(ctx, cstr)
	return strings.clone(string(cstr)[:plen]), true
}

js_to_int :: proc(ctx: ^JS_Context, v: JS_Value) -> (i32, bool) {
	n: i32
	if JS_ToInt32(ctx, &n, v) != 0 {
		return 0, false
	}
	return n, true
}

js_to_bool_arg :: proc(ctx: ^JS_Context, v: JS_Value) -> bool {
	return JS_ToBool(ctx, v) > 0
}

js_is_fn :: proc(v: JS_Value) -> bool {
	// Functions are objects (negative tag); exact class check is overkill here.
	return i64(v.tag) < 0
}

js_same :: proc(a, b: JS_Value) -> bool {
	return a.u == b.u && a.tag == b.tag
}

js_throw :: proc(ctx: ^JS_Context, msg: string) -> JS_Value {
	return spike_js_throw_type_error(ctx, strings.clone_to_cstring(msg, context.temp_allocator))
}

js_undefined_val :: proc(ctx: ^JS_Context) -> JS_Value {
	_ = ctx
	return spike_js_undefined()
}

// Set a value as a global property (consumes the value reference).
js_set_global :: proc(ctx: ^JS_Context, name: string, val: JS_Value) {
	global := JS_GetGlobalObject(ctx)
	defer spike_js_free(ctx, global)
	JS_SetPropertyStr(ctx, global, strings.clone_to_cstring(name, context.temp_allocator), val)
}
