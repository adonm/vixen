package vixen

// Linux terminal I/O. Pure input decoding lives in terminal_input.odin.
import "core:fmt"
import "core:os"
import "core:unicode"
import "core:unicode/utf8"
import "core:sys/linux"

TCGETS    :: 0x5401
TCSETS    :: 0x5402
TIOCGWINSZ :: 0x5413

Termios :: struct {
	iflag, oflag, cflag, lflag: u32,
	line: u8,
	cc: [32]u8,
	ispeed, ospeed: u32,
}
Winsize :: struct {row, col, xpixel, ypixel: u16}

term_getattr :: proc() -> (Termios, bool) {
	t: Termios
	ok := int(linux.ioctl(0, TCGETS, uintptr(rawptr(&t)))) >= 0
	return t, ok
}

term_setattr :: proc(t: ^Termios) -> bool {
	return int(linux.ioctl(0, TCSETS, uintptr(rawptr(t)))) >= 0
}

term_raw_enable :: proc() -> (Termios, bool) {
	saved, ok := term_getattr()
	if !ok { return saved, false }
	raw := saved
	// Linux termios ABI: ignore input translations/parity marking and IXON.
	raw.iflag &= ~u32(0x0001 | 0x0002 | 0x0008 | 0x0010 | 0x0020 | 0x0040 | 0x0080 | 0x0100 | 0x0400)
	raw.oflag &= ~u32(0x0001) // OPOST: every output row must be positioned
	raw.cflag &= ~u32(0x30 | 0x100) // CSIZE, PARENB
	raw.cflag |= 0x30 // CS8
	raw.lflag &= ~u32(0x0008 | 0x0002 | 0x0001 | 0x8000) // ECHO, ICANON, ISIG, IEXTEN
	raw.cc[6], raw.cc[5] = 1, 0 // VMIN, VTIME
	return saved, term_setattr(&raw)
}

term_winsize :: proc() -> (Winsize, bool) {
	w: Winsize
	ok := int(linux.ioctl(1, TIOCGWINSZ, uintptr(rawptr(&w)))) >= 0
	return w, ok
}

// Positive count = input, zero = timeout/interruption, negative = EOF/error.
term_read_timeout :: proc(buf: []u8, timeout_ms: int) -> int {
	pfds := [1]linux.Poll_Fd{{fd = 0, events = {.IN}}}
	n, err_poll := linux.poll(pfds[:], i32(timeout_ms))
	if err_poll == .EINTR { return 0 }
	if n < 0 { return -1 }
	if n == 0 { return 0 }
	m, err := os.read(os.stdin, buf)
	return m if err == nil && m > 0 else -1
}

term_enter_alt :: proc() {
	os.write_string(os.stdout, "\x1b[?1049h\x1b[H\x1b[?25l\x1b[?2004h")
}
term_exit_alt :: proc() {
	os.write_string(os.stdout, "\x1b[0m\x1b[?2004l\x1b[?25h\x1b[?1049l")
}
term_move :: proc(col, row: int) {
	fmt.printf("\x1b[%d;%dH", row, col)
}

// Use the SDK's Unicode width data, not a handwritten partial range table.
// This remains a per-rune estimate: grapheme/terminal-specific widths need
// further work. Chrome uses ASCII escaping below for guaranteed row bounds.
term_char_width :: proc(r: rune) -> int {
	if unicode.is_grapheme_extend(r) { return 0 }
	return max(unicode.normalized_east_asian_width(r), 0)
}
term_str_width :: proc(s: string) -> int {
	w, i := 0, 0
	for i < len(s) {
		r, size := utf8.decode_rune(s[i:])
		w += term_char_width(r)
		i += max(size, 1)
	}
	return w
}
truncate_cells :: proc(s: string, max_cells: int) -> string {
	if max_cells <= 0 { return "" }
	w, i := 0, 0
	for i < len(s) {
		r, size := utf8.decode_rune(s[i:])
		w += term_char_width(r)
		if w > max_cells { return s[:i] }
		i += max(size, 1)
	}
	return s
}
