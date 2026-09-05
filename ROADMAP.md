# Vixen roadmap to beta

Status: alpha. This is the development contract, not a statement that the
features or gates below already exist. Checklists remain open until their
exit criteria have reproducible evidence.

## Scope and order

**Shared-core stabilization → headless beta → Kitty TUI beta → desktop beta.**
No desktop implementation until the headless/TUI gates are credible and M4
has passed. Architecture planning and toolkit criteria can be documented
earlier; the current `show` demo is not a desktop milestone.

First beta scope: Linux (Debian/Ubuntu first), Wikipedia, documentation, and
ordinary HTML forms with a declared reader-style rendering subset. Ghostty
is the primary interactive terminal; Kitty is a compatibility check. This
does not promise Chromium-level website compatibility or a hardened sandbox.

Full author-CSS compatibility, JS-heavy applications, JS↔WASM integration,
media, extensions, and additional operating systems are a subsequent track.
Do not quietly change the beta definition to count unsupported workflows as
passing. Broader compatibility needs its own requirements and gates.

Mise owns tooling/system dependencies via `mise bootstrap`; Just owns repo
recipes. Preserve the public repository and `flutter/` archive. Do not resume
parent-repository/submodule maintenance.

## M0 — Trustworthy baseline

- [x] Correct README/architecture claims, command examples, feature status,
  profile behavior, dependency requirements, and benchmark qualifications.
- [x] Replace the obsolete `tui --kitty=off` smoke with explicit one-shot
  Kitty output; label it as encoder smoke, not a terminal test.
- [ ] Reject invalid/missing CLI arguments
  and propagate load/evaluation/export failures as nonzero exit statuses.
- [ ] Separate core/headless tests from terminal protocol tests; name what
  each test actually proves rather than counting PASS lines.
- [x] Isolate helper-test profiles and server port files in freshly created
  directories. Reap owned server children on startup failure as well as normal
  cleanup; concurrent helper suites must not delete each other's state.
- [ ] Isolate gate output artifacts and add a committed concurrency regression.
- [ ] Identify release/debug binaries with commit/build information and
  establish repeatable benchmark inputs and measurement scripts.

**Exit:** a fresh `mise bootstrap --yes` plus documented Just recipes work;
intentional fixture/command failures fail the gate; known defects have
reproducers; published evidence names the tested build. Baseline gaps are
explicit, not silently reported as frontend success.

## M1 — Lifecycle and terminal correctness

- [x] Build replacement layouts before publishing them. Repeated resize
  preserves source, URL/title, live state, images, form submission values, and
  history; performs no network requests; frees old state exactly once.
- [ ] Edit controls visibly, not only in submission data. Keep focused input
  and selection coherent across layout changes.
- [ ] Respect actual terminal rows/columns and cell/pixel metrics. Gracefully
  handle tiny windows rather than inventing larger minimum dimensions.
- [ ] Parse fragmented/coalesced terminal replies and UTF-8 input without
  dropping typeahead. Cover reverse tab, escapes, malformed input, and EOF.
- [ ] Bound chrome rows, use explicit positioning, and prevent implicit
  newline/wrap scrolling. Keep terminal diagnostics out of fullscreen output.
- [ ] Own Kitty image IDs/placements, replacement and cleanup. Handle short
  writes/errors without silently corrupting protocol output.
- [ ] React to resize while idle and restore terminal modes/cursor/alternate
  screen on normal exit, handled signals, and recoverable failures.

**Exit:** committed navigation–resize–edit–submit regressions and PTY tests
pass repeatedly. Sanitizer/lifetime checks find no attributable memory
errors. Assert network/history invariants and terminal bounds, not just a
clean child exit. Retain real-terminal checks for emulator behavior.

## M2 — Useful shared reading engine

- [ ] Reliable paragraphs/headings/lists/inline text, code whitespace, and
  readable tables. Define intentional contained overflow separately from
  accidental document overflow; support narrow and wide viewports.
- [ ] Tested Unicode segmentation/line breaking and paragraph-direction
  behavior; do not split grapheme clusters or silently reorder/drop text.
- [ ] Working fragment links, Wikipedia references, find-in-page, and shared
  text-to-geometry mappings for hit testing and selection/copy.
- [ ] Complete the declared form subset, including usable multiline editing,
  visible values, and correct successful-control/submission behavior.
- [ ] Final-URL-aware relative links/resources, history and scroll restoration,
  recoverable error documents, and explicit unsupported-content handling.
- [ ] Bounded, cancellable resource scheduling; ignore stale completions.
  First text/placeholder paint must not wait for optional images. Prioritize
  visible images and preserve the reading anchor as they arrive.
- [ ] Enforce response/header/decompressed-byte, document/depth, image-pixel,
  viewport and cache limits before expensive allocations, with overflow checks.
- [ ] Verify TLS, scheme restrictions, redirect/cookie/origin boundaries.
  Keep live scripting disabled until its own limits/policy/lifecycle gates exist.

**Exit:** a versioned local Wikipedia/documentation corpus stays readable and
operable across widths, scripts, missing resources, redirects, malformed
content, and slow responses. Compare content/geometry and fixed-font visual
fixtures. Live-site checks supplement rather than destabilize local CI.

## M3 — Headless beta

- [ ] URL/file dump and render use the shared document/resource pipeline.
- [ ] Predictable text, PNG, and machine-readable metadata outputs with
  documented viewport, timeout, profile, and error semantics.
- [ ] Clean stdout for results; diagnostics elsewhere; no terminal/display
  initialization. Failed loads/exports produce meaningful exit codes.
- [ ] Bounded full-page export (limits or tiled/streamed output), not an
  unrestricted tall framebuffer allocation.
- [ ] Consistent profile selection and isolated/ephemeral test usage, plus
  execution outside the source checkout without repository runtime assets.

**Exit:** CLI end-to-end tests run with no terminal/display server, assert
content/dimensions/status and failure paths, and reproduce outputs under a
documented system-font configuration. Helpers alone do not satisfy this gate.

**Deliverable:** independently usable headless beta, even while other
frontends remain alpha.

## M4 — Kitty TUI beta

- [ ] Complete navigation, scrolling/page movement, hints, URL editing,
  focus/edit/submit, find, zoom, and copy workflows. Keep focused controls
  visible and restore logical reading position through reflow/history.
- [ ] Coalesce rapid input/resizes and track layout/page/chrome invalidation
  independently. No layout/raster/PNG work for unchanged state.
- [ ] PTY harness covers multiple geometries, cell metrics, delayed replies,
  fragmented Unicode, paste, repeated resize, long URLs, load cancellation,
  forms, errors, and terminal restoration.
- [ ] Independently decode Kitty transfers and terminal operations to assert
  image sizes/counts/placements, cursor/row bounds, and lack of unwanted scroll.
- [ ] Real Ghostty and Kitty manual checklists name tested terminal versions.

**Exit:** tests and real-terminal journeys pass; no accumulating images or
spurious screen scrolling; no image uploads during an unchanged idle page;
navigation/resize soak memory stays within documented bounds. Begin desktop
implementation only after M3 and M4 gates pass.

**Deliverable:** TUI beta.

## M5 — Functional desktop alpha

- [ ] Evaluate SDL3 plus a small chrome toolkit with a measured prototype:
  text input/selection, IME, clipboard, focus/accessibility, startup/memory,
  integration cost. Document the decision; do not assume DIY is cheaper.
- [ ] Replace the static timed screenshot with a persistent event loop using
  the shared browser session/actions.
- [ ] Live navigation/history/reload, responsive loading/cancel/error states,
  scrolling, forms, address bar, mouse hit-testing, and visible focus.
- [ ] Selection/copy, clipboard, shortcuts, IME composition, resizing, display
  scaling and scale changes.

**Exit:** shared browsing journeys pass through actual desktop input, with
action assertions and screenshots. A window-opening smoke is insufficient.

## M6 — Desktop beta and release hardening

- [ ] Everyday workflows: basic tabs, history/bookmarks, session restoration,
  save/download, failed-load recovery. Expose shared actions in TUI as applicable.
- [ ] Test supported Linux display environments/scales; establish keyboard
  accessibility and explicitly document remaining accessibility limitations.
- [ ] Validate profile upgrades/interrupted writes, packaging/ABI dependencies,
  system-font behavior, and relocation outside the checkout.
- [ ] Produce identifiable release artifacts, dependency/license information,
  supported-platform instructions, and accurate release notes/known limitations.
- [ ] Run all frontend gates, sanitizer checks, malformed-resource tests,
  navigation/resize soak, and performance checks against the release build.

**Exit:** all three suites pass; no known release-blocking crashes, state loss,
terminal corruption, or unbounded resource behavior in the supported scope.
Publish measured evidence and remaining compatibility/security limitations.

**Deliverable:** desktop beta and a coordinated release of all three modes.

## Performance acceptance

Initial targets to validate on a named reference machine, **not current
measurements or promised timings for the public network**:

- Cached Wikipedia first usable frame under 1 second.
- Local input-to-frame latency p95 under 50 ms.
- No unnecessary frame uploads while state is unchanged.
- Memory settles within declared document/cache/resource budgets during soak.

Measure process launch through font loading, layout, rasterization, encoding,
and backend submission; distinguish Kitty transfer completion from actual
terminal presentation. Record first desktop present. Publish p50/p95, peak
RSS, frame bytes, commit, toolchain/build flags, font versions, fixture hash,
viewport, and cache state. Test slow optional images to establish that first
paint does not depend on them. Set numerical resource budgets from M0
measurements and enforce them before declaring a beta gate passed.

## Initial progress — 2026-09-05

M0 and M1 are **in progress**, not complete. The initial lifecycle slice adds
repeated-resize/source/value/history checks, image pixel-allocation reuse and
server-request counts, reload/back/forward after reflow, borrowed-URL
navigation, and idempotent page destruction to `tuitest`. The regression
failed on the previous implementation at its second image-page resize.

`corpus/relayout.html` provides actual long text: narrow/wide/narrow layout
must change and restore line counts. The old short-fixture test could mistake
stale lines appended from the previous page for evidence of wrapping.

Verification recipes: `mise exec -- just test` and
`mise exec -- just test-sanitize`. Concurrent release/ASan helper runs were
also exercised using independent profiles/port files. These do not establish
frontend geometry correctness or fully instrument separately built C libraries.
Next: CLI failure semantics and committed terminal parser/geometry regressions,
followed by visible field editing/focus preservation and repaint scheduling.
