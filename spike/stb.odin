package spike

// Native stb bindings. C sources compile straight into the spike:
//   stb_truetype.c  (glyph raster)
//   stb_image_write.c (PNG encode)

foreign import stb {"stb_native.a", "system:m"}

Stbtt_Info :: struct {}

@(default_calling_convention = "c")
foreign stb {
	stbtt_GetFontOffsetForIndex :: proc(data: [^]u8, index: i32) -> i32 ---
	stbtt_InitFont              :: proc(info: ^Stbtt_Info, data: [^]u8, offset: i32) -> i32 ---
	stbtt_ScaleForPixelHeight   :: proc(info: ^Stbtt_Info, pixels: f32) -> f32 ---
	stbtt_GetFontVMetrics       :: proc(info: ^Stbtt_Info, ascent, descent, line_gap: ^i32) ---
	stbtt_GetGlyphBitmap        :: proc(info: ^Stbtt_Info, scale_x, scale_y: f32, glyph: i32, w, h, xoff, yoff: ^i32) -> [^]u8 ---
	stbtt_FreeBitmap            :: proc(bitmap: [^]u8, userdata: rawptr) ---
	stbi_write_png_to_mem       :: proc(pixels: [^]u8, stride: i32, w, h, comp: i32, out_len: ^i32) -> [^]u8 ---

	spike_stbtt_alloc :: proc() -> ^Stbtt_Info ---
	spike_stbtt_free  :: proc(info: ^Stbtt_Info) ---
	spike_free        :: proc(ptr: rawptr) ---
}
