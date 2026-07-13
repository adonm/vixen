# Project direction

This is the short source of truth for product focus. Detailed implementation
notes live in the crate docs, `DECISIONS.md`, and code.

## North star

Vixen is a focused, cross-platform Firefox replacement with a Flutter GUI on
Linux, macOS, Windows, Android, and the Apple Silicon iOS Simulator, plus first-class CLI/CDP automation. It
is optimized for the most web capability per byte of binary and per MiB of
memory.

**Linux is the highest-priority GUI, integration, packaging, and release
target.** Product work should make the Linux Flutter browser useful and pass its
native gates before equivalent platform expansion. macOS, Windows, Android, and
the iOS Simulator remain committed targets, but they follow the shared contract
proven on Linux rather than competing with Linux convergence for priority.

The product should feel closer to Ghostty than to a kitchen-sink browser:
small, fast to build, efficient to run, easy to iterate on, and boringly
reliable.

The ambition is a real browser, not a demo shell. Vixen should first make a
measured corridor of everyday sites reliable, then keep widening toward ordinary
Firefox-replacement use: accessible documents and applications, media, offline
storage/workers, richer graphics and communications, automation, and a credible
security/release lifecycle. The constraint is not lower ambition; it is refusing
duplicate engines, duplicate renderers, broad unbacked API shape, and UI features
that do not move the browser toward daily usefulness.

The product bet is: one small Rust browser core behind a shared native Flutter
shell can become useful across desktop and mobile before it is universal, if it
is honest, inspectable, scriptable, and improving against a public compatibility
loop.

## Primary users

- Desktop and Android users who want a focused browser on Linux, macOS, Windows,
  or Android, plus developers exercising the shared GUI on iOS Simulator.
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
5. **Minimal Flutter shell, Linux first** — Linux is the highest-priority GUI and
   release target. Dart owns chrome and host-service presentation only; the
   current GTK/Relm4 shell is temporary. The same proven contract then expands
   to the other four committed native targets.
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

Recent work proved that shared fetch/storage/runtime pieces are valuable and that
component sharing alone is insufficient. BrowserCore now gives shell, headless,
CDP, and WPT one engine-owned lifecycle. The following lessons are requirements:

1. **One browser state graph.** Profile → browser → browsing context → document
   is the ownership hierarchy. BrowserCore is that owner and exposes it to JS,
   CDP, WPT, GUI, and headless. Parallel frontend navigation, history, runtime,
   permission, or profile coordinators are forbidden regressions.
2. **A component seam is not lifecycle integration.** Sharing `Page`, `Network`,
   or `JsRuntime` types is insufficient if frontends decide independently when to
   create, commit, cancel, persist, or destroy them. Those decisions remain in
   the production engine-owned lifecycle.
3. **Asynchrony needs identity.** Context, navigation, document, request, runtime,
   and download work carries stable ids/generations. Cancellation invalidates the
   generation, and late work cannot mutate current state or emit success.
4. **Trust boundaries are product features.** Validate URL/header/body/storage
   inputs near entry, fail closed, and apply response policy before exposure,
   execution, decode, cache insertion, persistence, download, or UI handoff.
5. **Automation must share the browser.** CDP events, waits, DOM queries, input,
   and screenshots observe the same lifecycle and network/rendering paths as the
   GUI. Protocol shape without independent live targets is not multi-page
   support.
6. **Profiles are durable, bounded browser state.** Cache, cookies, storage,
   history, sessions, permissions, downloads, and security state need one owner,
   partitioning, limits, recovery, and clear-data integration.
7. **Observability is an API.** Stable errors and bounded privacy-minimal traces
   are product contracts. They distinguish policy, transport, unsupported,
   cancellation, stale state, and resource exhaustion without leaking content.
8. **Measure before budgeting; reduce before claiming.** Size/performance limits
   need reproducible baselines. Every broad feature needs focused fixtures and a
   WPT path; every real-site bug becomes a reduction or an explicitly tracked
   unreduced failure.
9. **One shell contract, platform-specific proof.** Flutter is the primary GUI
    substrate, but each platform/ABI earns support through native BrowserCore,
    V8, WebRender, accessibility, host-service, package, size, and performance
    evidence on that target's latest stable major OS release. Exact versions are
    pinned per release; framework support is not Vixen support.
10. **Pixels need semantics.** Web content remains one WebRender texture path;
    BrowserCore also projects accessibility into Flutter Semantics. Dart may not
    infer browser meaning from pixels.

## Non-goals before alpha

- A kitchen-sink UI or clone of every Firefox chrome feature.
- WebKit fallback, runtime engine switching, or a generic JS-engine abstraction.
- A permanent second GUI shell; GTK/Relm4 exists only as the Linux compatibility
  baseline until Flutter parity.
- A CPU paint fallback that competes with WebRender.
- Media/WebGPU/WebRTC/service workers unless promoted by a later ADR.
- Full WPT/browser parity claims before measured profiles justify them.
- A full extension ecosystem before the browser core and five-platform shell are
  credible.
- Site isolation/OOPIF work before the single-process browser is measured enough
  to know what isolation architecture is actually needed.

## Alpha means

Alpha is not broad API completeness. Alpha means the architecture is frozen and
validated for full delivery:

- one JS runtime target (`deno_core`/V8),
- one target GUI path (Flutter) with a temporary GTK/Relm4 Linux baseline,
- one display list and one WebRender paint path,
- one layout architecture,
- one WPT/reporting workflow,
- hk-enforced git lifecycle gates,
- honest compatibility docs with measured local/imported fixture results.

Flutter alpha additionally requires the browser-scoped Rust bridge contract, a
Linux fake and real shell, bounded RGBA external-texture transport, input and
viewport routing, and the accessibility projection shape. The bridge, shell, and
RGBA texture, physical viewport, pointer/wheel/keyboard routing, and the first
bounded BrowserCore-to-Flutter Semantics shape are landed. Platform text-input
state, authored keyboard/action hints, and single-touch root dragging are also
landed. Native IME evidence, richer gesture/DOM event input, scale/lifecycle
recovery, and complete semantics/native AT behavior remain open.

Alpha also requires a production browser core: one profile service, one context
registry, one generational navigation/document lifecycle, and one command/event
path used by shell, headless, CDP, WPT, and page runtime. Two contexts must run
independently while sharing only intended profile state, active navigation must
be cancellable, and live DOM mutation must reach the visible render path. Narrow
surfaces are acceptable; duplicate models are not.

## Delivery horizon in one sentence

- **Beta**: a controlled real-site corridor is usable in the Linux Flutter GUI
  and headless, with measured compatibility/performance and known gaps; desktop
  expansion proceeds from the same bridge.
- **v1.0**: Vixen is an honest daily-driver minimum on every platform that has
  passed its declared gate and a useful Playwright/CDP automation target, with
  security/reliability limits documented instead of hidden.
- **Replacement horizon**: continue through accessibility, media, offline apps,
  richer graphics/communications, ecosystem support, and stronger isolation
  until ordinary browsing, not only a curated corridor, is credible on supported
  targets.

After alpha, API surface can still change, but architecture changes need a new
ADR and human approval.
