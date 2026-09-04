package spike

// WAMR embedding (interpreter build, libiwasm.a): load, instantiate,
// call exported functions, serve imported native callbacks.

foreign import wamr {"../thirdparty/wasm-micro-runtime-WAMR-2.4.4/build/libiwasm.a"}

Wasm_Module  :: struct{}
Wasm_Inst    :: struct{}
Wasm_Func    :: struct{}
Wasm_ExecEnv :: struct{}

Native_Symbol :: struct {
	symbol:    cstring,
	func_ptr:  rawptr,
	signature: cstring,
	attachment: rawptr,
}

@(default_calling_convention = "c")
foreign wamr {
	wasm_runtime_init              :: proc() -> bool ---
	wasm_runtime_destroy           :: proc() ---
	wasm_runtime_load              :: proc(buf: [^]u8, size: u32, error_buf: [^]u8, error_buf_size: u32) -> ^Wasm_Module ---
	wasm_runtime_unload            :: proc(module: ^Wasm_Module) ---
	wasm_runtime_register_natives  :: proc(module_name: cstring, symbols: [^]Native_Symbol, n: u32) -> bool ---
	wasm_runtime_instantiate       :: proc(module: ^Wasm_Module, stack_size: u32, heap_size: u32, error_buf: [^]u8, error_buf_size: u32) -> ^Wasm_Inst ---
	wasm_runtime_deinstantiate     :: proc(inst: ^Wasm_Inst) ---
	wasm_runtime_lookup_function   :: proc(inst: ^Wasm_Inst, name: cstring) -> ^Wasm_Func ---
	wasm_runtime_create_exec_env   :: proc(inst: ^Wasm_Inst, stack_size: u32) -> ^Wasm_ExecEnv ---
	wasm_runtime_destroy_exec_env  :: proc(env: ^Wasm_ExecEnv) ---
	wasm_runtime_call_wasm         :: proc(env: ^Wasm_ExecEnv, function: ^Wasm_Func, argc: u32, argv: [^]u32) -> bool ---
	wasm_runtime_get_exception     :: proc(inst: ^Wasm_Inst) -> cstring ---
}
