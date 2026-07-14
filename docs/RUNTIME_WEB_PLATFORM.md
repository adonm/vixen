# Runtime Web Platform strategy

Vixen exposes the browser runtime through `deno_core`/V8. This document defines
where Web API code should live so the runtime stays fast, small, and spec-driven.

## Fixed constraints

- `deno_core`/V8 is the only JS runtime target.
- Do not add a generic JS-engine abstraction.
- Generated WebIDL substrate stays in `crates/vixen-engine/src/script/webidl.rs`.
- Host-family extensions adopt generated interfaces with
  `webidl.adoptInterface(...)`.
- DOM, CSS cascade/computed styles, network policy, and storage remain
  BrowserCore source of truth. Flutter owns commit-bound CSS formatting/layout.
- Security-sensitive behavior validates near the host boundary and fails closed.
- Flutter/Dart is the web formatter/renderer and browser chrome. It does not
  implement page JavaScript, DOM/Web APIs, navigation, storage, policy, or a
  fallback runtime.
- The same BrowserCore/`deno_core` runtime is the target on all five GUI
  platforms. Platform support is evidence-gated, not inferred from Flutter.

## Fidelity ladder

Use this ladder when adding or reviewing an API:

1. **Shape** — constructor/prototype exists because WebIDL requires it.
2. **Pure value behavior** — JS-only implementation is acceptable when it has no
   privileged state, I/O, persistence, origin policy, or layout dependency.
3. **BrowserCore op/resource backing** — required for page DOM, CSSOM, network,
   storage, history, permissions, timers, and anything security-sensitive;
   layout-dependent APIs use the bounded Flutter renderer broker.
4. **Spec/WPT correctness** — useful subset covered by local or imported WPT
   fixtures; this is the target for committed behavior.

Shape-only APIs are temporary compatibility scaffolding. Do not keep widening
shape if the MVP needs deeper behavior in an already-exposed family.

## JS bootstrap vs Rust ops

Choose the fastest/smallest correct implementation:

- Keep **pure value objects** in JS bootstrap when doing so avoids Rust/V8 glue
  and does not duplicate an authoritative source of truth. Examples: event objects,
  geometry value wrappers, iterator ergonomics, small serialization helpers.
- Use **Rust ops/resources** when behavior touches parsed page state, CSS cascade,
  network, storage, origin/security policy, long-lived handles, or mutable browser
  state. Layout-dependent ops query exact accepted Flutter commits rather than
  implementing layout in Rust.
- Avoid two authoritative paths. The obsolete Page string-expression evaluator
  is deleted; do not reintroduce expression classifiers or fallback eval paths.

## Lessons from the first host-object migrations

- **One eval path beats clever fallbacks.** Headless `--eval`, CDP
  `Runtime.evaluate`, and WPT `js-eval` all use BrowserCore/`JsRuntime`.
- **Expose only after behavior exists.** Replace generated WebIDL placeholders
  with host behavior before claiming support; there is no legacy evaluator to
  conceal `unsupportedMember`.
- **Small vertical slices are safer than broad shape.** A narrow family such as
  document metadata, collections, or form reflections should include the op/data
  source, JS bootstrap member, headless routing, and CDP/WPT-visible proof in the
  same change.
- **BrowserCore remains authoritative for browser state.** JS bootstrap may cache
  and compose objects inside one realm, but page mutations, navigation actions,
  storage, cookies, and fetch policy commit through Rust-backed ops/resources.
  Layout geometry comes only from an exact accepted Flutter commit surfaced
  through those ops.
- **Fail-closed errors are part of compatibility.** Unsupported selectors, bad
  storage keys, private-network fetches, malformed host operations, and missing
  elements should produce deterministic errors instead of silently widening the
  smoke surface.

## Single-path evaluation rules

- New API families start in a `script::<family>` extension or in a JS-only value
  bootstrap adopted onto generated WebIDL prototypes.
- A supported behavior requires a focused `vixen-engine` runtime test and one
  user-visible seam (`--eval`, CDP, or WPT fixture).
- Never add expression classifiers or fallback evaluators. Converge transitional
  document snapshots with live page-backed resources instead.

## Current direction

The broad runtime surface is useful for CDP/headless compatibility, but the next
runtime work should deepen correctness and converge state:

- move backend-backed APIs from stubs/smoke shape to Rust-backed behavior,
- replace transitional runtime/document snapshots with live page-backed resources,
- import focused WPT cases for each widened API family,
- keep generated WebIDL prototype inheritance intact,
- keep CDP and headless `--eval` consuming the same runtime path.

Android requires a pinned rusty_v8/V8 source archive and toolchain with a proved
source cross-build for each shipped ABI. The iOS target is Apple Silicon Simulator
only and builds rusty_v8 for `aarch64-apple-ios-sim`, retaining the same V8
JavaScript and WebAssembly path. There is no JavaScriptCore, WKWebView, WebKit,
alternate Wasm runtime, or physical-device fallback.

WebAssembly remains V8-backed on every declared target. Widen it with identical
module validation, memory/table limits, deadline cancellation, host-call policy,
and conformance fixtures rather than adding a platform-specific implementation.

## Required proof for a host-family change

Each non-trivial host-family change should include:

- a focused runtime test in `vixen-engine` or `vixen-headless`,
- one user-visible seam check when applicable (`--eval`, CDP, or WPT fixture),
- a note in `COMPAT.md` only when support level or known gaps materially change,
- green `just gate-phase6` before push.
