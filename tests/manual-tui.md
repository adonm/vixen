# Manual TUI checklist — Ghostty + Kitty

Required for the M4 exit (`ROADMAP.md`: "Real Ghostty and Kitty manual
checklists name tested terminal versions"). The PTY harness
(`tui_protocol.py`) proves protocol bytes and state transitions; it cannot
prove presentation — legibility, flicker, caret visibility, image placement,
or real mouse behavior. This checklist covers what only eyes can judge.

Do one full pass per terminal, on a fresh profile each time. Record the
environment first; a pass without versions recorded does not count.

## Setup

```sh
mise exec -- just build
./vixen version            # record below
python3 corpus/testserver.py .tmp/manual.port &
PORT=$(cat .tmp/manual.port)
PROF=$HOME/.vixen-manual   # fresh dir per terminal; delete before each pass
rm -rf "$PROF"
./vixen browse --profile "$PROF" "http://127.0.0.1:$PORT/relayout"
```

Pages: `/relayout` (long prose), `/fragments` (fragment jumps),
`/form` (fields), `/slowpage` (2s-delayed image + fast image),
`/imgpage` (attr-sized images), `/article` (links). Diagnostics land in
`$PROF/tui.log` — check it after every section.

## 0. Environment record

| Item | Ghostty pass | Kitty pass |
|---|---|---|
| `vixen version` output | | |
| Terminal version (`ghostty --version` / `kitty --version`) | | |
| OS + display (X11/Wayland/macOS) | | |
| Font + size | | |
| Window size at start (cols×rows) | | |
| Date + tester | | |

## 1. Capability probes (do first, both terminals)

In a bare shell (not in vixen), run:

```sh
printf '\e[?1000h\e[?1006h\e[?1016h'
```

then click the bottom-right corner of the window while reading stdin
(e.g. `xxd` or `cat -v`). Pixel mode shows large coordinates
(`<0;1600;900M`-ish); cell mode shows grid coordinates (`<0;80;24M`-ish).
Disable afterwards: `printf '\e[?1016l\e[?1006l\e[?1000l'`.

- [ ] Ghostty: reports **pixels** (confirms the 1016 upgrade path is live)
- [ ] Kitty: reports **pixels**
- [ ] DECRQM sanity: `printf '\e[?1016$p'` → reply contains `1016;1$y`
      or `1016;2$y` (either is a confirm; `0` means cells-only — record it)

## 2. First paint and chrome

Open `/relayout` at 80×24.

- [ ] Text paints promptly; no blank wait for images
- [ ] Page image fills rows 1..rows-2 edge to edge, no gaps or stale pixels
- [ ] Row `rows-1` shows the status line (percent, URL); last row shows the
      help line. Both stay put — no scrolling, no wrapped/truncated garbage
- [ ] Non-ASCII in chrome (if any) shows as `\u{...}` escapes, never mojibake
- [ ] `$PROF/tui.log` has no errors

## 3. Resize

- [ ] Grow to ~120×36: content reflows wider, no refetch symptoms
- [ ] Shrink to ~40×10: reflows narrower, nothing overflows the window
- [ ] Shrink to ~1×1 then back: bounded "too small" chrome, recovers cleanly
- [ ] Rapid drag-resize: settles on the final size, no torn frames
- [ ] Reading position survives reflow (same paragraph near the top, not
      jumped to top/bottom)

## 4. Scrolling and anchors

- [ ] `j`/`k`/arrows scroll smoothly; Space pages down; `g`/`G` jump top/bottom
- [ ] Scroll mid-page, follow a link, `b` back: position restored, not top
- [ ] Same after `r` reload

## 5. Links and hints

- [ ] Every visible link shows a legible number badge that doesn't cover text
- [ ] Type digits + Enter follows the right link; bad number says "no such link"
- [ ] On `/fragments`: `#sec` links jump without visible reload; `b`/`f`
      restore fragment positions

## 6. Forms (`/form`)

- [ ] Tab cycles fields with a visible focus ring; focused field scrolls
      into view
- [ ] Typing shows immediately with a visible caret; selection highlights
- [ ] Textarea: Enter inserts a newline (multi-row box, scrolls to caret);
      Tab to a button + Enter submits
- [ ] Submit button click submits; orphan `Nowhere` button reports "not in a form"
- [ ] Values survive a resize mid-edit

## 7. Find

- [ ] `/` opens the bar; typing highlights matches live and jumps to the first
- [ ] `n`/`N` cycle; Enter keeps matches; Esc clears; "no matches" state shows
- [ ] Highlights are legible against text (current match distinguishable)

## 8. Mouse

- [ ] Link click follows the link; empty-area click does nothing (no redraw
      flicker); click blurs a focused field
- [ ] Field click focuses and places the caret where you clicked (both ends
      of a short value; middle of a long one)
- [ ] Button click submits
- [ ] Wheel scrolls ~3 rows per tick, up and down; wheel at top/bottom is a
      silent no-op
- [ ] Move the pointer out of the window: no spurious action
- [ ] Middle/right/modified clicks: ignored, no action
- [ ] After `q`: Shift-select works normally again (mouse reporting is off —
      if clicks still reach the shell as escape codes, file a bug)

## 9. Images (`/slowpage`, `/imgpage`)

- [ ] `/slowpage`: text paints immediately (<1.5s), fast image pops in, slow
      image arrives ~2s later without moving text under attr-sized boxes
- [ ] Scrolling/typing during the slow fetch stays responsive
- [ ] `/imgpage`: sized images render at sensible sizes with dims in captions;
      the missing image shows a clean placeholder

## 10. Errors

- [ ] Nonexistent route: readable error page, still navigable (back works)
- [ ] Kill the fixture server mid-session, navigate: failure is explicit,
      terminal stays usable

## 11. Shutdown and terminal restoration

After each: cursor visible, shell prompt normal, Shift-select works,
scrollback intact.

- [ ] `q` quits cleanly
- [ ] Ctrl-C quits from a field editor and from the URL bar
- [ ] SIGTERM from another shell (`kill -TERM <pid>`): exits 143, terminal
      restored, Kitty image deleted (no ghost image left on screen)
- [ ] SIGHUP: same, exit 129
- [ ] Hangup (close the PTY/master side): exits, no busy loop

## 12. Soak (10 minutes, one terminal)

Browse, resize, edit, submit, find continuously. Then:

- [ ] No ghost images accumulating, no screen corruption
- [ ] Idle page uploads nothing new (no flicker when hands are off)
- [ ] Memory looks bounded (RSS stable between minute 2 and minute 10)
- [ ] `$PROF/tui.log` shows no errors or sanitizer output

## Failure report template

Paste one per failure:

```text
Terminal + version:
Section / check:
Expected:
Actual (what you saw):
`vixen version`:
tui.log tail (20 lines):
Repro steps:
```

When a full pass is green on both terminals, record the two version rows in
`ROADMAP.md` M4 and check off "Real Ghostty and Kitty manual checklists".
