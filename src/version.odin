package vixen

// Build identity via -define (no generated files, no dirty tree).
// Justfile passes VIXEN_SHA/VIXEN_DATE/VIXEN_DIRTY quoted: Odin parses an
// unquoted numeric-looking SHA (e.g. HEAD 6e14534) as a float and fails the
// build. One quote layer is stripped here; bare `odin build` without the
// recipe falls back to the unquoted defaults below.
import "core:fmt"

VIXEN_SHA   :: #config(VIXEN_SHA, "dev")
VIXEN_DATE  :: #config(VIXEN_DATE, "unknown")
VIXEN_DIRTY :: #config(VIXEN_DIRTY, "unknown")

_unq :: proc(s: string) -> string {
	if len(s) >= 2 && s[0] == '"' && s[len(s)-1] == '"' {
		return s[1:len(s)-1]
	}
	return s
}

version_main :: proc() {
	sha, date, dirty := _unq(VIXEN_SHA), _unq(VIXEN_DATE), _unq(VIXEN_DIRTY)
	fmt.printfln("vixen %s (%s%s)", sha, date, "-dirty" if dirty == "dirty" else "")
	fmt.printfln("odin %s opt=%v debug=%v", ODIN_VERSION, ODIN_OPTIMIZATION_MODE, ODIN_DEBUG)
}
