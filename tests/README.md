# Frontend regression tests

Run from the repository root after `mise bootstrap --yes`:

```sh
mise exec -- just test          # helpers, smoke, PTY, profile tests
mise exec -- just test-tui      # terminal decoder + PTY protocol harness
mise exec -- just test-sanitize # instrumented Odin helpers and frontend runs
```

`src/test_*.odin` contains in-package helper suites. `vixen browsetest`
exercises browser/session helpers (the old `tuitest` spelling is an alias).
`vixen termtest` checks the pure incremental decoder, metrics, and chrome
bounds. These are separate from the frontend harnesses below.

## `tui_protocol.py`

Uses real PTY descriptors and the standard-library fixture server. Every run
owns its profile, port file, server, and child processes. The independent
output model checks cursor/row bounds, Kitty chunk sizes and PNG dimensions,
image/placement identity, replacement/deletion, and restored termios/modes.

Journeys cover tiny/oversized windows, idle resize, cell-pixel changes, no-op
input, URL-only redraws, fragmented/missing metrics with typeahead, bracketed paste,
slow/fragmented UTF-8, reverse tab, focus/value preservation through resize,
form submission, EOF/hangup, and refusing a non-terminal interactive invocation.
An empty focused field is deliberately painted before completing its first
input rune: this catches the native empty-caret shaping crash.

The harness intentionally sets `TERM=xterm-256color` without a Kitty window
ID. Capability is an interactive-mode contract, not an environment-name test.
`VIXEN_TEST_TERM` can override that for regression checks against older builds.

This is **not Ghostty or Kitty**. It does not validate actual terminal
presentation, font rendering, compositor behavior, multiplexer passthrough,
or catchable OS-signal restoration. Those remain separate manual/automated
milestone requirements.

## `profiles.py`

Runs fetch and headless browsing from outside the checkout against a local
HTTP server. Checks explicit/Vixen/legacy/XDG/HOME profile precedence,
directory creation and errors, preservation of old user data, and the user
agent. No external website or default user profile is used.

## Sanitizer scope

`test-sanitize` instruments the Odin executable and enables leak detection.
It does not rebuild every separately compiled native dependency with
instrumentation. A passing gate is evidence for these cases, not proof of
memory safety or beta readiness. See `ROADMAP.md` for remaining gates.
