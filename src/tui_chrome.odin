package vixen

import "core:fmt"
import "core:strings"
import "core:unicode/utf8"

// Terminal chrome is a bounded ASCII representation of untrusted text.
// Escape Unicode/control characters rather than relying on emulator-specific
// grapheme widths or allowing page data to inject terminal commands. The
// document remains fully rasterized and input retains its original UTF-8.
tui_chrome_text :: proc(s: string, budget: int) -> string {
	b: strings.Builder
	strings.builder_init(&b)
	defer strings.builder_destroy(&b)
	i := 0
	for i < len(s) && len(b.buf) < budget {
		r, n := utf8.decode_rune(s[i:])
		buf: [16]u8
		text := s[i:i+max(n, 1)]
		if r < 32 || r > 126 { text = fmt.bprintf(buf[:], "\\u{%x}", r) }
		if len(b.buf)+len(text) > budget { break }
		strings.write_string(&b, text)
		i += max(n, 1)
	}
	return strings.clone(strings.to_string(b))
}

tui_chrome_row :: proc(t: ^Tui, row: int, text: string, inverse: bool) {
	line := tui_chrome_text(text, max(t.cols-1, 0))
	defer delete(line)
	term_move(1, row)
	if inverse { fmt.print("\x1b[7m") }
	// Leave the last cell unused: no autowrap state to save or guess.
	fmt.printf("%s\x1b[0m\x1b[K", line)
}
