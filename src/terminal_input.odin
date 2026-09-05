package vixen

// Incremental, I/O-free terminal decoder. Zero consumed bytes means the
// caller must buffer more input or expire a lone escape. UTF-8 and identified
// control packets may be arbitrarily fragmented; a timer must not eat them.
import "core:unicode/utf8"

Term_Key :: union {rune, Key_Special}
Key_Special :: enum {
	Up, Down, Left, Right,
	Enter, Backspace, Tab, Esc, ShiftTab,
	CtrlC, CtrlD, Unknown,
}
Term_Metrics :: struct {kind, a, b: int}
Term_Paste :: struct {start: bool}
Term_Event :: union {Term_Key, Term_Metrics, Term_Paste}

term_decode :: proc(data: []u8, expired := false) -> (Term_Event, int) {
	unknown := Term_Event(Term_Key(Key_Special.Unknown))
	if len(data) == 0 { return unknown, 0 }
	b := data[0]
	if b != 0x1b {
		switch b {
		case 3: return Term_Key(Key_Special.CtrlC), 1
		case 4: return Term_Key(Key_Special.CtrlD), 1
		case '\r', '\n': return Term_Key(Key_Special.Enter), 1
		case 8, 127: return Term_Key(Key_Special.Backspace), 1
		case '\t': return Term_Key(Key_Special.Tab), 1
		}
		if b < 32 { return unknown, 1 }
		if b < 0x80 { return Term_Key(rune(b)), 1 }
		n := 1
		if b >= 0xc2 && b <= 0xdf { n = 2 }
		if b >= 0xe0 && b <= 0xef { n = 3 }
		if b >= 0xf0 && b <= 0xf4 { n = 4 }
		for i in 1 ..< min(n, len(data)) {
			if data[i] & 0xc0 != 0x80 { return Term_Key(rune(0xfffd)), 1 }
		}
		if len(data) < n { return unknown, 0 }
		r, size := utf8.decode_rune(string(data))
		return Term_Key(r), max(size, 1)
	}
	if len(data) == 1 {
		return Term_Key(Key_Special.Esc), 1 if expired else 0
	}
	if data[1] == '[' || data[1] == 'O' {
		for i in 2 ..< len(data) {
			fin := data[i]
			if fin >= 0x40 && fin <= 0x7e {
				params := string(data[2:i])
				if data[1] == '[' && fin == 't' {
					if m, ok := term_parse_metrics(params); ok { return m, i + 1 }
				} else if fin == '~' && params == "200" {
					return Term_Paste{true}, i + 1
				} else if fin == '~' && params == "201" {
					return Term_Paste{false}, i + 1
				} else if params == "" || params == "1" {
					switch fin {
					case 'A': return Term_Key(Key_Special.Up), i + 1
					case 'B': return Term_Key(Key_Special.Down), i + 1
					case 'C': return Term_Key(Key_Special.Right), i + 1
					case 'D': return Term_Key(Key_Special.Left), i + 1
					case 'Z': return Term_Key(Key_Special.ShiftTab), i + 1
					}
				}
				return unknown, i + 1
			}
			if fin < 0x20 || fin > 0x3f {
				return unknown, i // preserve a new ESC/control byte
			}
		}
		return unknown, len(data) if len(data) >= 256 else 0
	}
	// Consume OSC/DCS/APC replies as packets, never as browser shortcuts.
	if data[1] == ']' || data[1] == 'P' || data[1] == '_' {
		for i in 2 ..< len(data) {
			if data[1] == ']' && data[i] == 7 { return unknown, i + 1 }
			if data[i] == '\\' && data[i-1] == 0x1b { return unknown, i + 1 }
		}
		return unknown, len(data) if len(data) >= 4096 else 0
	}
	return unknown, 2 // unsupported Alt-key pair
}

term_parse_metrics :: proc(s: string) -> (Term_Metrics, bool) {
	values: [3]int
	part, digits := 0, 0
	for c in s {
		if c == ';' {
			if digits == 0 || part == 2 { return {}, false }
			part += 1
			digits = 0
		} else {
			if c < '0' || c > '9' || digits >= 5 { return {}, false }
			values[part] = values[part] * 10 + int(c - '0')
			digits += 1
		}
	}
	if part != 2 || digits == 0 || values[1] <= 0 || values[2] <= 0 ||
	   values[1] > 65535 || values[2] > 65535 { return {}, false }
	if values[0] != 4 && values[0] != 6 && values[0] != 8 { return {}, false }
	return Term_Metrics{values[0], values[1], values[2]}, true
}
