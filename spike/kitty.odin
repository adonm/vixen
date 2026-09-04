package spike

// Kitty graphics protocol backend: transmit a PNG straight to the terminal.
// Detection is environmental (no interactive query, keeps stdout pipe-safe);
// --kitty=force overrides, --kitty=off disables.

import "core:fmt"
import "core:os"
import base64 "core:encoding/base64"

kitty_env_supported :: proc() -> bool {
	term := os.get_env("TERM", context.temp_allocator)
	if term == "xterm-kitty" {
		return true
	}
	if os.get_env("KITTY_WINDOW_ID", context.temp_allocator) != "" {
		return true
	}
	return false
}

kitty_write :: proc(s: string) -> bool {
	n, err := os.write(os.stdout, transmute([]u8)s)
	return err == nil && n == len(s)
}

// Transmit PNG bytes as f=100 chunks of at most 4096 base64 chars.
kitty_transmit_png :: proc(png: []u8, w, h: int) -> bool {
	b64, err := base64.encode(png)
	if err != nil {
		fmt.println("kitty: base64 failed")
		return false
	}
	defer delete(b64)
	return kitty_transmit_chunks(b64, w, h)
}

kitty_transmit_chunks :: proc(b64: string, w, h: int) -> bool {
	CHUNK :: 4096
	n := len(b64)
	pos := 0
	for pos < n {
		end := min(pos + CHUNK, n)
		more := 1 if end < n else 0
		head: string
		if pos == 0 {
			head = fmt.tprintf("\x1b_Ga=T,f=100,s=%d,v=%d,m=%d,q=2;", w, h, more)
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
