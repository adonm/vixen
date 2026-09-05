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

// Catchable shutdown: restore termios, delete the owned Kitty image, and
// leave the alternate screen even when killed by HUP/INT/TERM. The handler
// uses raw syscalls only (no allocation, no Odin runtime calls).
@(private) term_saved: Termios
@(private) term_saved_valid := false
@(private) term_in_alt := false

term_signal_handler :: proc "c" (sig: linux.Signal) {
	if term_saved_valid {
		_ = linux.ioctl(0, TCSETS, uintptr(rawptr(&term_saved)))
	}
	if term_in_alt {
		msg := "\x1b_Ga=d,d=I,i=1986623589,q=2;\x1b\\\x1b[0m\x1b[?2004l\x1b[?25h\x1b[?1049l"
		b := transmute([]u8)msg
		_, _ = linux.write(1, b)
	}
	code: i32 = 128
	#partial switch sig {
	case .SIGHUP:
		code = 129
	case .SIGINT:
		code = 130
	case .SIGTERM:
		code = 143
	}
	linux.exit_group(code)
}

term_install_signal_handlers :: proc(saved: ^Termios) {
	term_saved = saved^
	term_saved_valid = true
	term_in_alt = true
	sigs := [3]linux.Signal{linux.Signal.SIGHUP, linux.Signal.SIGINT, linux.Signal.SIGTERM}
	for sig in sigs {
		sa: linux.Sig_Action(u8)
		sa.handler = term_signal_handler
		old: linux.Sig_Action(u8)
		_ = linux.rt_sigaction(sig, &sa, &old)
	}
}

term_clear_signal_handlers :: proc() {
	term_in_alt = false
	term_saved_valid = false
	sigs := [3]linux.Signal{linux.Signal.SIGHUP, linux.Signal.SIGINT, linux.Signal.SIGTERM}
	for sig in sigs {
		sa: linux.Sig_Action(u8)
		sa.special = .SIG_DFL
		old: linux.Sig_Action(u8)
		_ = linux.rt_sigaction(sig, &sa, &old)
	}
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
