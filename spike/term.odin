package spike

// Terminal driver: raw mode, window size, cell metrics, key decoding.
// termios constants are the stable Linux ABI (no libc dependency).

import "core:fmt"
import "core:os"
import "core:strings"
import "core:unicode/utf8"

import "core:sys/linux"

TCGETS    :: 0x5401
TCSETS    :: 0x5402
TIOCGWINSZ :: 0x5413

T_IFLAG_BRKINT :: 0x0002
T_IFLAG_ICRNL  :: 0x0100
T_IFLAG_INPCK  :: 0x0010
T_IFLAG_ISTRIP :: 0x0020
T_IFLAG_IXON   :: 0x0400
T_OFLAG_OPOST  :: 0x0001
T_LFLAG_ECHO   :: 0x0008
T_LFLAG_ICANON :: 0x0002
T_LFLAG_ISIG   :: 0x0001
T_LFLAG_IEXTEN :: 0x8000
T_VMIN :: 6
T_VTIME :: 5

Termios :: struct {
	iflag: u32,
	oflag: u32,
	cflag: u32,
	lflag: u32,
	line:  u8,
	cc:    [32]u8,
	ispeed: u32,
	ospeed: u32,
}

Winsize :: struct {
	row, col: u16,
	xpixel, ypixel: u16,
}

term_getattr :: proc() -> (Termios, bool) {
	t: Termios
	r := linux.ioctl(0, TCGETS, uintptr(rawptr(&t)))
	if int(r) < 0 {
		return t, false
	}
	return t, true
}

term_setattr :: proc(t: ^Termios) -> bool {
	return int(linux.ioctl(0, TCSETS, uintptr(rawptr(t)))) >= 0
}

term_raw_enable :: proc() -> (Termios, bool) {
	saved, ok := term_getattr()
	if !ok {
		return saved, false
	}
	raw := saved
	raw.iflag &= ~u32(T_IFLAG_BRKINT | T_IFLAG_ICRNL | T_IFLAG_INPCK | T_IFLAG_ISTRIP | T_IFLAG_IXON)
	raw.oflag &= ~u32(T_OFLAG_OPOST)
	raw.cflag |= 0x30 // CS8
	raw.lflag &= ~u32(T_LFLAG_ECHO | T_LFLAG_ICANON | T_LFLAG_IEXTEN | T_LFLAG_ISIG)
	raw.cc[T_VMIN] = 1
	raw.cc[T_VTIME] = 0
	if !term_setattr(&raw) {
		return saved, false
	}
	return saved, true
}

term_size :: proc() -> (cols, rows: int, ok: bool) {
	w: Winsize
	if int(linux.ioctl(1, TIOCGWINSZ, uintptr(rawptr(&w)))) < 0 || w.col == 0 {
		return 80, 24, false
	}
	return int(w.col), int(w.row), true
}

// Query pixel cell size: CSI 14 t (pixels) + CSI 18 t (cells).
term_cell_size :: proc() -> (cw, ch: int, ok: bool) {
	os.write_string(os.stdout, "\x1b[14t\x1b[18t")
	pw, ph, ook1 := term_csi_reply('4', 300)
	cr, cc, ook2 := term_csi_reply('8', 300)
	if !ook1 || !ook2 || cr == 0 || cc == 0 {
		return 8, 16, false
	}
	return pw / cc, ph / cr, true
}

// Read a "CSI num ; ... <final>" reply: returns first two numbers.
term_csi_reply :: proc(kind: byte, timeout_ms: int) -> (a, b: int, ok: bool) {
	buf: [128]u8
	n := term_read_timeout(buf[:], timeout_ms)
	if n <= 0 {
		return 0, 0, false
	}
	s := string(buf[:n])
	// Expect ESC [ kind ; a ; b <final>.
	i := strings.index_byte(s, '[')
	if i < 0 {
		return 0, 0, false
	}
	rest := s[i+1:]
	parts := strings.split(rest, ";", context.temp_allocator)
	if len(parts) < 3 {
		return 0, 0, false
	}
	if len(parts[0]) != 1 || parts[0][0] != kind {
		return 0, 0, false
	}
	return parse_int_or(strings.trim_space(parts[1]), -1), parse_int_or(strings.trim_space(parts[2]), -1), true
}

// Read up to len(buf) with timeout; 0 = timeout.
term_read_timeout :: proc(buf: []u8, timeout_ms: int) -> int {
	pfd := linux.Poll_Fd{fd = 0, events = {.IN}}
	pfds := [1]linux.Poll_Fd{pfd}
	n, _ := linux.poll(pfds[:], i32(timeout_ms))
	if n <= 0 {
		return 0
	}
	m, err := os.read(os.stdin, buf)
	if err != nil {
		return 0
	}
	return m
}

Term_Key :: union {
	rune, // printable char / ctrl mapped below
	Key_Special,
}

Key_Special :: enum {
	Up, Down, Left, Right,
	Enter, Backspace, Tab, Esc,
	CtrlC, CtrlD,
	Unknown,
}

// Decode one keypress (blocking). Escape sequences need a short follow-up
// window; a lone ESC would block up to 25ms (accepted TUI tradeoff).
term_read_key :: proc() -> Term_Key {
	b: [16]u8
	m, err := os.read(os.stdin, b[:])
	if err != nil || m <= 0 {
		return Key_Special.Unknown
	}
	if b[0] != 0x1b {
		switch b[0] {
		case '\r', '\n':
			return Key_Special.Enter
		case 127:
			return Key_Special.Backspace
		case '\t':
			return Key_Special.Tab
		case 3:
			return Key_Special.CtrlC
		case 4:
			return Key_Special.CtrlD
		}
		r, _ := utf8.decode_rune(string(b[:m]))
		return r
	}
	// ESC: read the rest with a short timeout.
	n := term_read_timeout(b[m:], 25)
	m += n
	if m == 1 {
		return Key_Special.Esc
	}
	if m >= 3 && b[1] == '[' {
		switch b[2] {
		case 'A':
			return Key_Special.Up
		case 'B':
			return Key_Special.Down
		case 'C':
			return Key_Special.Right
		case 'D':
			return Key_Special.Left
		}
	}
	return Key_Special.Unknown
}

term_enter_alt :: proc() {
	os.write_string(os.stdout, "\x1b[?1049h\x1b[H\x1b[?25l")
}

term_exit_alt :: proc() {
	os.write_string(os.stdout, "\x1b[?25h\x1b[?1049l")
}

term_move :: proc(col, row: int) {
	fmt.printf("\x1b[%d;%dH", row, col)
}
