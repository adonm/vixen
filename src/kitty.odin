package vixen

// Kitty graphics protocol backend: transmit a PNG straight to the terminal.
// Interactive mode assumes capability; explicit one-shot output is separate.

import "core:fmt"
import "core:os"
import base64 "core:encoding/base64"

TUI_IMAGE_ID :: u32(0x76697865)

kitty_write :: proc(s: string) -> bool {
	pos := 0
	for pos < len(s) {
		n, err := os.write(os.stdout, transmute([]u8)s[pos:])
		if err != nil || n <= 0 { return false }
		pos += n
	}
	return true
}

kitty_delete_viewport :: proc() -> bool {
	return kitty_write(fmt.tprintf("\x1b_Ga=d,d=I,i=%d,q=2;\x1b\\", TUI_IMAGE_ID))
}

// Transmit PNG bytes as f=100 chunks of at most 4096 base64 chars.
kitty_transmit_png :: proc(png: []u8, w, h: int, image_id: u32 = 0, cols: int = 0, rows: int = 0) -> bool {
	b64, err := base64.encode(png)
	if err != nil {
		fmt.eprintln("kitty: base64 failed")
		return false
	}
	defer delete(b64)
	return kitty_transmit_chunks(b64, w, h, image_id, cols, rows)
}

kitty_transmit_chunks :: proc(b64: string, w, h: int, image_id: u32 = 0, cols: int = 0, rows: int = 0) -> bool {
	CHUNK :: 4096
	n := len(b64)
	pos := 0
	for pos < n {
		end := min(pos + CHUNK, n)
		more := 1 if end < n else 0
		head: string
		if pos == 0 {
			head = fmt.tprintf("\x1b_Ga=T,f=100,s=%d,v=%d,m=%d,q=2;", w, h, more)
			if image_id != 0 {
				// Re-transmission replaces this ID and its placement. C=1
				// prevents implicit cursor movement/scrolling; c/r also bound
				// placement when pixel metrics are unavailable or in flight.
				head = fmt.tprintf("\x1b_Ga=T,f=100,s=%d,v=%d,i=%d,p=1,C=1,c=%d,r=%d,m=%d,q=2;",
					w, h, image_id, cols, rows, more)
			}
		} else {
			head = fmt.tprintf("\x1b_Gm=%d;", more)
		}
		if !kitty_write(head) {
			return false
		}
		if !kitty_write(b64[pos:end]) {
			return false
		}
		if !kitty_write("\x1b\\") {
			return false
		}
		pos = end
	}
	return true
}
