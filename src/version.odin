package vixen

// Build identity via -define (no generated files, no dirty tree).
// Justfile passes VIXEN_SHA/VIXEN_DATE/VIXEN_DIRTY; defaults mean a bare
// `odin build` without the recipe (still builds, clearly marked dev).
import "core:fmt"

VIXEN_SHA   :: #config(VIXEN_SHA, "dev")
VIXEN_DATE  :: #config(VIXEN_DATE, "unknown")
VIXEN_DIRTY :: #config(VIXEN_DIRTY, "unknown")

version_main :: proc() {
	fmt.printfln("vixen %s (%s%s)", VIXEN_SHA, VIXEN_DATE, "-dirty" if VIXEN_DIRTY == "dirty" else "")
	fmt.printfln("odin %s opt=%v debug=%v", ODIN_VERSION, ODIN_OPTIMIZATION_MODE, ODIN_DEBUG)
}
