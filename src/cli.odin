package vixen

// Strict CLI parsing: unknown flags, missing values, invalid widths, and
// extra positionals fail with usage on stderr and exit 2. Panics on missing
// option values (index out of range) are rejected.

import "core:fmt"
import "core:os"
import "core:strings"

cli_usage_error :: proc(msg: string) -> ! {
	fmt.eprintln(msg)
	os.exit(2)
}

cli_is_flag :: proc(arg: string) -> bool {
	if arg == "--" || len(arg) < 2 {
		return false
	}
	return arg[0] == '-'
}

// Value for --opt=value (val part) or --opt VALUE (next arg). Returns false
// when the value is missing or empty.
cli_take_value :: proc(args: []string, i: ^int, arg, val: string) -> (string, bool) {
	if strings.contains(arg, "=") {
		return val, len(val) > 0
	}
	i^ += 1
	if i^ >= len(args) {
		return "", false
	}
	return args[i^], len(args[i^]) > 0
}

// Width in px for headless layout. Bounded: tiny widths produce no useful
// layout, huge widths allocate unbounded framebuffers.
cli_parse_width :: proc(s: string) -> (int, bool) {
	if len(s) == 0 || len(s) > 5 {
		return 0, false
	}
	n := 0
	for c in s {
		if c < '0' || c > '9' {
			return 0, false
		}
		n = n * 10 + int(c - '0')
	}
	if n < 100 || n > 8192 {
		return 0, false
	}
	return n, true
}

CLI_BROWSE_USAGE :: "usage: vixen browse [--dump [--format text|json]] [--profile DIR] [--width N] <url>"
CLI_FETCH_USAGE  :: "usage: vixen fetch [--profile DIR] <url>"
CLI_RENDER_USAGE :: "usage: vixen render [--out PNG] [--meta JSON] [--width N] [--profile DIR] [--base-url URL] <page.html>"
CLI_TUI_USAGE    :: "usage: vixen tui [--width N] [--profile DIR] [--base-url URL] [--meta JSON] <page.html>"
CLI_SHOW_USAGE   :: "usage: vixen show <page.html>"

cli_help_requested :: proc(arg: string) -> bool {
	return arg == "--help" || arg == "-h" || arg == "help"
}
