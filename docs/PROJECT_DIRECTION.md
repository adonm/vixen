# Project direction

This is the short source of truth for product focus. Detailed implementation
notes live in the crate docs, `DECISIONS.md`, and code.

## North star

Vixen is a Firefox replacement for modern Linux: a minimal, focused desktop
browser plus first-class CLI/CDP automation, optimized for the most web
capability per byte of binary and per MiB of memory.

The product should feel closer to Ghostty than to a kitchen-sink browser:
small, fast to build, efficient to run, easy to iterate on, and boringly
reliable.

## Primary users

- Desktop Linux users who want a focused daily browser.
- CLI/CDP users running headless workflows, Playwright-style automation, and
  terminal-oriented apps such as <https://adonm.github.io/zuko/app.html>.
- Maintainers and agents using text reports to drive rapid, high-quality
  iteration.

## Product metric

The leading metric is **maximum capability for the smallest binary**. When two
solutions are both correct enough for the target WPT/spec surface, prefer the
one with:

1. smaller runtime/binary footprint,
2. lower memory use,
3. faster local builds,
4. fewer moving parts,
5. clearer text output for automation and review.

## Priority ranking

The user-facing rank is:

1. **CSS cascade, layout, and rendering** — a Firefox replacement must draw real
   pages. Vixen owns layout; keep it WPT-driven and small.
2. **DOM/WebIDL/Web API runtime** — modern pages need correct host APIs over
   `deno_core`/V8.
3. **Network/security/fetch/cookies** — real browsing needs safe, fail-closed
   loading before breadth.
4. **Storage/history/session** — required for real browsing and app-like sites.
5. **Minimal Relm4 desktop shell** — focused browser UI, not a feature buffet.
6. **Headless CLI + CDP/Playwright-compatible seams** — automation and text
   reports are product features, not test-only scaffolding.
7. **WPT/imported fixture coverage and reports** — correctness driver for every
   item above. Treat it as cross-cutting, not optional polish.
8. **HTML parsing/serialization** — essential but mostly delegated to
   `html5ever`; Vixen must preserve tree shape and integration semantics.
9. **CLI ergonomics** — keep commands stable, scriptable, and useful.
10. **Embeddable Rust API** — important as an internal seam, but not a separate
    product until the browser is credible.

## Non-goals before alpha

- Cross-platform release targets beyond modern Linux.
- A kitchen-sink UI or clone of every Firefox chrome feature.
- WebKit fallback, runtime engine switching, or a generic JS-engine abstraction.
- A second desktop GUI toolkit path; Relm4/libadwaita is the GUI path.
- A CPU paint fallback that competes with WebRender.
- Media/WebGPU/WebRTC/service workers unless promoted by a later ADR.
- Full WPT/browser parity claims before measured profiles justify them.

## Alpha means

Alpha is not broad API completeness. Alpha means the architecture is frozen and
validated for full delivery:

- one JS runtime target (`deno_core`/V8),
- one desktop GUI path (Relm4/libadwaita),
- one display list and one WebRender paint path,
- one layout architecture,
- one WPT/reporting workflow,
- hk-enforced git lifecycle gates,
- honest compatibility docs with measured local/imported fixture results.

After alpha, API surface can still change, but architecture changes need a new
ADR and human approval.
