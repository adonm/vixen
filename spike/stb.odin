package spike

// Native stb bindings. C sources compile straight into the spike:
//   stb_truetype.c  (glyph raster)
//   stb_image_write.c (PNG encode)
//   stb_image.c (image decode: PNG/JPEG/GIF-first-frame/BMP)
//   stb_image_resize.c (downscale to display size)

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
	stbi_load_from_memory       :: proc(buf: [^]u8, len: i32, w, h, comp: ^i32, req_comp: i32) -> [^]u8 ---
	stbi_image_free             :: proc(ptr: rawptr) ---
	stbir_resize_uint8          :: proc(input: [^]u8, in_w, in_h, in_stride: i32, output: [^]u8, out_w, out_h, out_stride: i32, num_channels: i32) -> i32 ---

	spike_stbtt_alloc :: proc() -> ^Stbtt_Info ---
	spike_stbtt_free  :: proc(info: ^Stbtt_Info) ---
	spike_free        :: proc(ptr: rawptr) ---
}
