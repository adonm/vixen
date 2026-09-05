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
// Raw SGR mouse report (1-based units; Tui.mouse_pixels selects cells vs
// pixels). Button keeps wire bits (0-2 buttons, 64/65 wheel, 4/8/16 mods,
// 32 motion); leave pre-decodes Kitty's bit-8 window-leave extension.
Term_Mouse :: struct {button, x, y: int, release, leave: bool}
// DECRPM mode report (reply to our DECRQM probes, e.g. 1016 pixels).
Term_Decrpm :: struct {mode, ps: int}
Term_Event :: union {Term_Key, Term_Metrics, Term_Paste, Term_Mouse, Term_Decrpm}

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
				} else if data[1] == '[' && (fin == 'M' || fin == 'm') && len(params) > 0 && params[0] == '<' {
					if m, ok := term_parse_mouse(params[1:], fin == 'm'); ok { return m, i + 1 }
				} else if data[1] == '[' && fin == 'M' && params == "" {
					// X10 mouse (ESC [ M b1 b2 b3): never sent while 1006 is
					// on, but consume the full packet so coord bytes can't
					// decode as keypresses if a terminal ignores 1006.
					end := min(6, len(data))
					for j in 3 ..< end {
						if data[j] < 32 { return unknown, j }
					}
					if len(data) < 6 {
						return unknown, 0 if !expired else len(data)
					}
					return unknown, 6
				} else if data[1] == '[' && fin == 'y' {
					if m, ok := term_parse_decrpm(params); ok { return m, i + 1 }
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

// Parse SGR mouse params "Cb;Cx;Cy" (leading '<' stripped by the caller).
// Coordinates accept the full pixel range; the Tui resolves units.
term_parse_mouse :: proc(s: string, release: bool) -> (Term_Mouse, bool) {
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
	if part != 2 || digits == 0 || values[0] > 255 ||
	   values[1] <= 0 || values[2] <= 0 ||
	   values[1] > 65535 || values[2] > 65535 { return {}, false }
	m := Term_Mouse{values[0], values[1], values[2], release, false}
	if values[0] & 128 != 0 {
		m.leave = true // Kitty window-leave extension: coords meaningless
	}
	return m, true
}

// Parse DECRPM "?<mode>;<Ps>$" (final 'y' stripped by the caller).
term_parse_decrpm :: proc(s: string) -> (Term_Decrpm, bool) {
	if len(s) < 5 || s[0] != '?' || s[len(s)-1] != '$' {
		return {}, false
	}
	body := s[1:len(s)-1]
	semi := -1
	for i in 0 ..< len(body) {
		if body[i] == ';' {
			if semi >= 0 { return {}, false }
			semi = i
		} else if body[i] < '0' || body[i] > '9' {
			return {}, false
		}
	}
	if semi <= 0 || semi != len(body) - 2 {
		return {}, false
	}
	mode := 0
	for c in body[:semi] {
		mode = mode * 10 + int(c - '0')
		if mode > 9999 { return {}, false }
	}
	ps := int(body[semi+1] - '0')
	if mode <= 0 || ps < 0 || ps > 4 { return {}, false }
	return Term_Decrpm{mode, ps}, true
}
