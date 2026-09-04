package spike

// SDL3 show backend: upload the framebuffer as a streaming texture.

import "core:fmt"
import "core:strings"

import sdl3 "vendor:sdl3"

sdl_show :: proc(fr: ^Frame, title: string, ms: u32) -> bool {
	if !sdl3.Init({.VIDEO}) {
		fmt.printfln("sdl: init failed: %s", sdl3.GetError())
		return false
	}
	defer sdl3.Quit()
	win := sdl3.CreateWindow(strings.clone_to_cstring(title, context.temp_allocator), i32(fr.w), i32(fr.h), {})
	if win == nil {
		fmt.printfln("sdl: window failed: %s", sdl3.GetError())
		return false
	}
	defer sdl3.DestroyWindow(win)
	ren := sdl3.CreateRenderer(win, nil)
	if ren == nil {
		fmt.printfln("sdl: renderer failed: %s", sdl3.GetError())
		return false
	}
	defer sdl3.DestroyRenderer(ren)
	tex := sdl3.CreateTexture(ren, .RGBA32, .STREAMING, i32(fr.w), i32(fr.h))
	if tex == nil {
		fmt.printfln("sdl: texture failed: %s", sdl3.GetError())
		return false
	}
	defer sdl3.DestroyTexture(tex)
	if !sdl3.UpdateTexture(tex, nil, raw_data(fr.px), i32(fr.w * 4)) {
		fmt.printfln("sdl: upload failed: %s", sdl3.GetError())
		return false
	}
	sdl3.RenderClear(ren)
	sdl3.RenderTexture(ren, tex, nil, nil)
	sdl3.RenderPresent(ren)
	sdl3.Delay(ms)
	return true
}
