# Project direction

This is the short source of truth for product focus. Detailed implementation
notes live in the crate docs, `DECISIONS.md`, and code.

## North star

Vixen is a Firefox replacement for modern Linux: a focused desktop browser plus
first-class CLI/CDP automation, optimized for the most web capability per byte of
binary and per MiB of memory.

The product should feel closer to Ghostty than to a kitchen-sink browser:
small, fast to build, efficient to run, easy to iterate on, and boringly
reliable.

The ambition is a real browser, not a demo shell. Vixen should eventually load a
measured corridor of everyday sites, preserve profile state, handle
forms/navigation/storage securely, draw pages with a real layout/paint pipeline,
expose a useful Playwright/CDP surface, package cleanly for modern Linux, and
publish honest compatibility/performance numbers. The constraint is not lower
ambition; it is refusing duplicate engines, duplicate renderers, broad unbacked
API shape, and UI features that do not move the browser toward daily usefulness.

The product bet is: a small, integrated Linux-first browser can be useful before
it is universal if it is honest, inspectable, scriptable, and improving against a
public compatibility loop.

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

Correctness beats smallness at security/trust boundaries, data-loss boundaries,
and rendering invariants. “Small” means fewer duplicate models and less
framework gravity, not skipping the browser semantics users rely on.

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

## Design lessons now baked in

Recent implementation work moved storage, fetch, CORS, cache revalidation,
network events, XHR, and CDP lifecycle events onto shared paths. Keep following
that pattern:

1. **Authoritative state first.** Build or choose the state owner, then expose it
   to JS, CDP, WPT, GUI, and headless. Do not grow parallel “good enough” models.
2. **Trust boundaries are product features.** Validate URL/header/body/storage
   inputs at the JS → Rust or network/profile boundary, fail closed, and return
   diagnostics that separate policy from transport from unsupported behavior.
3. **Automation must share the browser.** CDP events, waits, DOM queries, and
   screenshots should observe the same page lifecycle and network/rendering paths
   as the GUI, even when the surface is initially narrow.
4. **Profiles are durable browser state.** Cache, cookies, storage, history,
   sessions, permissions, downloads, and security state need bounded persistence
   and clear-data integration as soon as the behavior exists.
5. **Compatibility needs reductions.** Every broad feature should land with
   focused fixtures and a path to WPT/imported coverage; every real-site bug
   should become a reduction or an explicitly tracked unreduced failure.

## Non-goals before alpha

- Cross-platform release targets beyond modern Linux.
- A kitchen-sink UI or clone of every Firefox chrome feature.
- WebKit fallback, runtime engine switching, or a generic JS-engine abstraction.
- A second desktop GUI toolkit path; Relm4/libadwaita is the GUI path.
- A CPU paint fallback that competes with WebRender.
- Media/WebGPU/WebRTC/service workers unless promoted by a later ADR.
- Full WPT/browser parity claims before measured profiles justify them.
- A full extension ecosystem, mobile port, or cross-platform packaging story.
- Site isolation/OOPIF work before the single-process browser is measured enough
  to know what isolation architecture is actually needed.

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

Alpha also requires that navigation, storage/profile, fetch/XHR, CDP, DOM/form
state, layout/paint, and shell chrome all use shared engine paths. Narrow
surfaces are acceptable; duplicate models are not.

## Beta and v1 in one sentence

- **Beta**: a controlled real-site corridor is usable in GUI and headless, with
  measured compatibility/performance and known gaps.
- **v1.0**: Vixen is an honest daily-driver minimum for focused Linux users and a
  useful Playwright/CDP automation target, with security/reliability limits
  documented instead of hidden.

After alpha, API surface can still change, but architecture changes need a new
ADR and human approval.
