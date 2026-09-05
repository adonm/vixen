package vixen

// wasmtest: native -> wasm round trip through WAMR.
//   vixen wasmtest <module.wasm>
// Exercises: exported call, imported Odin callback, error reporting.

import "core:fmt"
import "core:os"
import "core:strings"

wasm_native_add100 :: proc "c" (env: ^Wasm_ExecEnv, x: i32) -> i32 {
	context = {}
	return x + 100
}

wasm_test_symbols := [1]Native_Symbol{
	{"call_imported", transmute(rawptr)wasm_native_add100, "(i)i", nil},
}

wasm_call :: proc(env: ^Wasm_ExecEnv, inst: ^Wasm_Inst, name: string, args: ..u32) -> (u32, bool) {
	fn := wasm_runtime_lookup_function(inst, temp_cstr(name))
	if fn == nil {
		fmt.eprintfln("wasmtest: missing export %s", name)
		return 0, false
	}
	argv := make([dynamic]u32, context.temp_allocator)
	for a in args {
		append(&argv, a)
	}
	if !wasm_runtime_call_wasm(env, fn, u32(len(argv)), raw_data(argv)) {
		if ex := wasm_runtime_get_exception(inst); ex != nil {
			fmt.eprintfln("wasmtest: trap in %s: %s", name, string(ex))
		}
		return 0, false
	}
	if len(argv) == 0 {
		return 0, true
	}
	return argv[0], true
}

temp_cstr :: proc(s: string) -> cstring {
	return strings.clone_to_cstring(s, context.temp_allocator)
}

wasmtest_main :: proc(path: string) -> bool {
	data, err := os.read_entire_file_from_path(path, context.allocator)
	if err != nil {
		fmt.eprintfln("wasmtest: cannot read %s", path)
		return false
	}
	defer delete(data)
	if !wasm_runtime_init() {
		fmt.eprintfln("wasmtest: runtime init failed")
		return false
	}
	defer wasm_runtime_destroy()
	if !wasm_runtime_register_natives("env", raw_data(wasm_test_symbols[:]), 1) {
		fmt.eprintfln("wasmtest: native registration failed")
		return false
	}
	ebuf: [256]u8
	mod := wasm_runtime_load(raw_data(data), u32(len(data)), raw_data(ebuf[:]), 256)
	if mod == nil {
		fmt.eprintfln("wasmtest: load failed: %s", string(ebuf[:]))
		return false
	}
	defer wasm_runtime_unload(mod)
	inst := wasm_runtime_instantiate(mod, 16 * 1024, 16 * 1024, raw_data(ebuf[:]), 256)
	if inst == nil {
		fmt.eprintfln("wasmtest: instantiate failed: %s", string(ebuf[:]))
		return false
	}
	defer wasm_runtime_deinstantiate(inst)
	env := wasm_runtime_create_exec_env(inst, 16 * 1024)
	if env == nil {
		fmt.eprintfln("wasmtest: exec env failed")
		return false
	}
	defer wasm_runtime_destroy_exec_env(env)
	ok := true
	if v, cok := wasm_call(env, inst, "add", 40, 2); !cok || v != 42 {
		fmt.eprintfln("wasmtest: add(40,2) = %d, want 42", v)
		ok = false
	} else {
		fmt.printfln("wasmtest add(40,2) = %d", v)
	}
	// via_import(5) = call_imported(5)*2 = 105*2 = 210.
	if v, cok := wasm_call(env, inst, "via_import", 5); !cok || v != 210 {
		fmt.eprintfln("wasmtest: via_import(5) = %d, want 210", v)
		ok = false
	} else {
		fmt.printfln("wasmtest via_import(5) = %d", v)
	}
	fmt.printfln("wasmtest VmHWM=%d KB", vm_hwm_kb())
	return ok
}
