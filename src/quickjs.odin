package vixen

// Minimal QuickJS bindings. JS_FreeValue / JS_IsException / JS_ToCString are
// static-inline in quickjs.h, so the link goes through shim.c wrappers.

foreign import qjs {
	"../quickjs-2024-01-13/libquickjs.a",
	"../.tmp/native/shim.o",
	"system:m",
	"system:dl",
	"system:pthread",
}

JS_Value   :: struct { u: u64, tag: i64 }
JS_Runtime :: struct {}
JS_Context :: struct {}

JS_EVAL_TYPE_GLOBAL :: 0

JS_Memory_Usage :: struct {
	malloc_size, malloc_limit, memory_used_size: i64,
	malloc_count:                              i64,
	memory_used_count:                         i64,
	atom_count, atom_size:                      i64,
	str_count, str_size:                        i64,
	obj_count, obj_size:                        i64,
	prop_count, prop_size:                      i64,
	shape_count, shape_size:                    i64,
	js_func_count, js_func_size, js_func_code_size:          i64,
	js_func_pc2line_count, js_func_pc2line_size:             i64,
	c_func_count, array_count:                               i64,
	fast_array_count, fast_array_elements:                  i64,
	binary_object_count, binary_object_size:                i64,
}

@(default_calling_convention = "c")
foreign qjs {
	JS_NewRuntime    :: proc() -> ^JS_Runtime ---
	JS_FreeRuntime   :: proc(rt: ^JS_Runtime) ---
	JS_NewContext    :: proc(rt: ^JS_Runtime) -> ^JS_Context ---
	JS_FreeContext   :: proc(ctx: ^JS_Context) ---
	JS_Eval          :: proc(ctx: ^JS_Context, input: cstring, input_len: uint, filename: cstring, eval_flags: i32) -> JS_Value ---
	JS_GetException  :: proc(ctx: ^JS_Context) -> JS_Value ---
	JS_ComputeMemoryUsage :: proc(rt: ^JS_Runtime, s: ^JS_Memory_Usage) ---

	spike_js_free         :: proc(ctx: ^JS_Context, v: JS_Value) ---
	spike_js_is_exception :: proc(v: JS_Value) -> i32 ---
	spike_js_to_cstring   :: proc(ctx: ^JS_Context, v: JS_Value, plen: ^uint) -> cstring ---
	spike_js_free_cstring :: proc(ctx: ^JS_Context, ptr: cstring) ---
	spike_js_dup          :: proc(ctx: ^JS_Context, v: JS_Value) -> JS_Value ---
	spike_js_new_bool     :: proc(ctx: ^JS_Context, b: i32) -> JS_Value ---
	spike_js_new_int32    :: proc(ctx: ^JS_Context, i: i32) -> JS_Value ---
	spike_js_new_int64    :: proc(ctx: ^JS_Context, i: i64) -> JS_Value ---
	spike_js_new_float64  :: proc(ctx: ^JS_Context, d: f64) -> JS_Value ---
	spike_js_null         :: proc() -> JS_Value ---
	spike_js_undefined    :: proc() -> JS_Value ---
	spike_js_is_null      :: proc(v: JS_Value) -> i32 ---
	spike_js_is_undefined :: proc(v: JS_Value) -> i32 ---
	spike_js_throw_type_error :: proc(ctx: ^JS_Context, msg: cstring) -> JS_Value ---

	JS_NewClassID      :: proc(class_id: ^u32) -> u32 ---
	JS_NewClass        :: proc(rt: ^JS_Runtime, class_id: u32, def: ^JS_Class_Def) -> i32 ---
	JS_NewObjectClass  :: proc(ctx: ^JS_Context, class_id: u32) -> JS_Value ---
	JS_SetOpaque       :: proc(obj: JS_Value, ptr: rawptr) ---
	JS_GetOpaque2      :: proc(ctx: ^JS_Context, obj: JS_Value, class_id: u32) -> rawptr ---
	JS_NewCFunction2   :: proc(ctx: ^JS_Context, func: rawptr, name: cstring, length: i32, cproto: i32, magic: i32) -> JS_Value ---
	JS_SetPropertyStr  :: proc(ctx: ^JS_Context, obj: JS_Value, prop: cstring, val: JS_Value) -> i32 ---
	JS_GetPropertyStr  :: proc(ctx: ^JS_Context, obj: JS_Value, prop: cstring) -> JS_Value ---
	JS_GetGlobalObject :: proc(ctx: ^JS_Context) -> JS_Value ---
	JS_DefinePropertyGetSet :: proc(ctx: ^JS_Context, obj: JS_Value, prop: u32, getter, setter: JS_Value, flags: i32) -> i32 ---
	JS_NewAtom         :: proc(ctx: ^JS_Context, str: cstring) -> u32 ---
	JS_FreeAtom        :: proc(ctx: ^JS_Context, atom: u32) ---
	JS_NewStringLen    :: proc(ctx: ^JS_Context, str: cstring, len: int) -> JS_Value ---
	JS_NewArray        :: proc(ctx: ^JS_Context) -> JS_Value ---
	JS_SetPropertyUint32 :: proc(ctx: ^JS_Context, obj: JS_Value, idx: u32, val: JS_Value) -> i32 ---
	JS_Call            :: proc(ctx: ^JS_Context, func, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value ---
	JS_ToBool          :: proc(ctx: ^JS_Context, v: JS_Value) -> i32 ---
	JS_ToInt32         :: proc(ctx: ^JS_Context, pres: ^i32, v: JS_Value) -> i32 ---
	JS_ToFloat64       :: proc(ctx: ^JS_Context, pres: ^f64, v: JS_Value) -> i32 ---
	JS_NewError        :: proc(ctx: ^JS_Context) -> JS_Value ---
}
