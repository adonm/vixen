package vixen

// Pure decoder/geometry tests. tests/tui_protocol.py separately exercises real
// terminal file descriptors and independently parses the output protocol.
import "core:fmt"
import "core:strings"

termtest_main :: proc() -> bool {
	fails := 0
	check :: proc(fails: ^int, name: string, ok: bool) {
		if !ok { fails^ += 1; fmt.printfln("FAIL term/%s", name) }
	}
	Case :: struct {text: string, want: Term_Event}
	cases := [?]Case{
		{"x", Term_Key('x')},
		{"é", Term_Key('é')},
		{"日", Term_Key('日')},
		{"😀", Term_Key('😀')},
		{"\x03", Term_Key(Key_Special.CtrlC)},
		{"\x04", Term_Key(Key_Special.CtrlD)},
		{"\t", Term_Key(Key_Special.Tab)},
		{"\x7f", Term_Key(Key_Special.Backspace)},
		{"\r", Term_Key(Key_Special.Enter)},
		{"\x1b[A", Term_Key(Key_Special.Up)},
		{"\x1bOB", Term_Key(Key_Special.Down)},
		{"\x1b[C", Term_Key(Key_Special.Right)},
		{"\x1b[D", Term_Key(Key_Special.Left)},
		{"\x1b[Z", Term_Key(Key_Special.ShiftTab)},
		{"\x1b[6;20;10t", Term_Metrics{6, 20, 10}},
		{"\x1b[4;600;800t", Term_Metrics{4, 600, 800}},
		{"\x1b[8;30;80t", Term_Metrics{8, 30, 80}},
		{"\x1b[200~", Term_Paste{true}},
		{"\x1b[201~", Term_Paste{false}},
		{"\x1b[99~", Term_Key(Key_Special.Unknown)},
		{"\x1b_Gi=1;OK\x1b\\", Term_Key(Key_Special.Unknown)},
		{"\x1b]0;title\x07", Term_Key(Key_Special.Unknown)},
	}
	for c in cases {
		for split in 0 ..< len(c.text) {
			_, n := term_decode(transmute([]u8)c.text[:split])
			check(&fails, "fragment-retained", n == 0)
		}
		joined := strings.concatenate([]string{c.text, "q"})
		defer delete(joined)
		e, n := term_decode(transmute([]u8)joined)
		check(&fails, "decode", e == c.want && n == len(c.text))
		e, n = term_decode(transmute([]u8)joined[n:])
		check(&fails, "typeahead", e == Term_Event(Term_Key('q')) && n == 1)
	}
	e, n := term_decode(transmute([]u8)string("\x1b"), true)
	check(&fails, "escape-timeout", n == 1 && e == Term_Event(Term_Key(Key_Special.Esc)))
	e, n = term_decode(transmute([]u8)string("\xe6"), true)
	check(&fails, "utf8-slow-fragment", n == 0)
	e, n = term_decode(transmute([]u8)string("\xe6q"))
	check(&fails, "utf8-invalid-tail", n == 1 && e == Term_Event(Term_Key(rune(0xfffd))))
	for bad in ([]string{"6;0;8", "6;20;0", "6;999999999999999999999;1", "6;65536;1", "6;20;8;1", "6;;8", "6;-1;8"}) {
		_, ok := term_parse_metrics(bad)
		check(&fails, "reject-metrics", !ok)
	}
	_, n = term_decode(transmute([]u8)string("\x1b[12;"), true)
	check(&fails, "csi-slow-fragment", n == 0)
	_, n = term_decode(transmute([]u8)string("\x1b[\x03"))
	check(&fails, "control-recovery", n == 2)
	t := Tui{cols = 80, rows = 24, cell_w = 8, cell_h = 16, focus = -1}
	tui_metrics_reply(&t, Term_Metrics{6, 20, 10})
	check(&fails, "cell-order", t.cell_w == 10 && t.cell_h == 20)
	tui_metrics_reply(&t, Term_Metrics{4, 576, 960})
	check(&fails, "pixel-order", t.cell_w == 12 && t.cell_h == 24)
	tui_cell_metrics(&t, 99999, 99999)
	check(&fails, "cell-bound", t.cell_w == 12 && t.cell_h == 24)
	check(&fails, "viewport", tui_drawable(&t) && tui_view_height(&t) == 22*24)
	t.rows = 1
	check(&fails, "tiny-viewport", !tui_drawable(&t) && tui_view_height(&t) == 0)
	t.cols, t.rows = 65535, 65535
	check(&fails, "huge-viewport", !tui_drawable(&t))
	for budget in 0 ..< 32 {
		s := tui_chrome_text("a日😀\x1b[2J\r\nx", budget)
		check(&fails, "chrome-budget", len(s) <= budget)
		for c in s { check(&fails, "chrome-safe", c >= 32 && c <= 126) }
		delete(s)
	}
	sess: Browse_Session
	sess.page.height = 1000
	t.sess = &sess
	t.cols, t.rows, t.cell_h = 80, 24, 16
	t.page_dirty, t.chrome_dirty = false, false
	tui_handle_key(&t, Term_Key('x'))
	tui_handle_key(&t, Term_Key(Key_Special.Unknown))
	tui_handle_key(&t, Term_Key('g'))
	check(&fails, "no-op-clean", !t.page_dirty && !t.chrome_dirty)
	tui_handle_key(&t, Term_Key('j'))
	check(&fails, "scroll-dirty", t.page_dirty && t.chrome_dirty)
	t.url_active = true
	check(&fails, "quit-url-editor", !tui_handle_key(&t, Term_Key(Key_Special.CtrlC)))
	fmt.printfln("termtest: %d failures", fails)
	return fails == 0
}
