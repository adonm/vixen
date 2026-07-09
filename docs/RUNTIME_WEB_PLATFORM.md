# Runtime Web Platform strategy

Vixen exposes the browser runtime through `deno_core`/V8. This document defines
where Web API code should live so the runtime stays fast, small, and spec-driven.

## Fixed constraints

- `deno_core`/V8 is the only JS runtime target.
- Do not add a generic JS-engine abstraction.
- Generated WebIDL substrate stays in `crates/vixen-engine/src/script/webidl.rs`.
- Host-family extensions adopt generated interfaces with
  `webidl.adoptInterface(...)`.
- DOM, CSS, layout, network policy, and storage state remain Rust source of
  truth.
- Security-sensitive behavior validates near the host boundary and fails closed.

## Fidelity ladder

Use this ladder when adding or reviewing an API:

1. **Shape** — constructor/prototype exists because WebIDL requires it.
2. **Pure value behavior** — JS-only implementation is acceptable when it has no
   privileged state, I/O, persistence, origin policy, or layout dependency.
3. **Rust op/resource backing** — required for page DOM, CSSOM, layout,
   network, storage, history, permissions, timers, and anything security-sensitive.
4. **Spec/WPT correctness** — useful subset covered by local or imported WPT
   fixtures; this is the target for committed behavior.

Shape-only APIs are temporary compatibility scaffolding. Do not keep widening
shape if the MVP needs deeper behavior in an already-exposed family.

## JS bootstrap vs Rust ops

Choose the fastest/smallest correct implementation:

- Keep **pure value objects** in JS bootstrap when doing so avoids Rust/V8 glue
  and does not duplicate a Rust source of truth. Examples: simple event objects,
  geometry value wrappers, iterator ergonomics, small serialization helpers.
- Use **Rust ops/resources** when behavior touches parsed page state, layout,
  CSS cascade, network, storage, origin/security policy, long-lived handles, or
  mutable browser state.
- Avoid two authoritative paths. Transitional `Page::evaluate_dom_expression`
  projections must be retired as equivalent `deno_core` host objects land.

## Lessons from the first host-object migrations

- **One eval path beats clever fallbacks.** Headless `--eval`, CDP
  `Runtime.evaluate`, and WPT `js-eval` should all try the page runtime host
  first. Legacy Page string projections are compatibility debt, not a second
  product API.
- **Route only after behavior exists.** Moving a string expression to the runtime
  whitelist is safe only when the generated WebIDL placeholder has been replaced
  by an implementation. If a routed expression would hit `unsupportedMember`, add
  the host behavior first or leave it on the legacy path temporarily.
- **Small vertical slices are safer than broad shape.** A narrow family such as
  document metadata, collections, or form reflections should include the op/data
  source, JS bootstrap member, headless routing, and CDP/WPT-visible proof in the
  same change.
- **Rust remains authoritative for browser state.** JS bootstrap may cache and
  compose objects inside one realm, but page mutations, layout geometry,
  navigation actions, storage, cookies, and fetch policy must commit back through
  Rust-backed ops/resources.
- **Fail-closed errors are part of compatibility.** Unsupported selectors, bad
  storage keys, private-network fetches, malformed host operations, and missing
  elements should produce deterministic errors instead of silently widening the
  smoke surface.

## Legacy projection retirement rules

- Do not add new cases to `Page::evaluate_dom_expression` unless the change is a
  short-lived compatibility guard for deleting a larger legacy branch.
- New API families start in a `script::<family>` extension or in a JS-only value
  bootstrap adopted onto generated WebIDL prototypes.
- A legacy expression may be deleted when the equivalent runtime path is covered
  by a focused `vixen-engine` runtime test and one user-visible seam (`--eval`,
  CDP, or WPT fixture).
- Keep the fallback easy to audit: prefer removing whole helper families over
  growing more pattern-matching branches.

## Current direction

The broad runtime surface is useful for CDP/headless compatibility, but the next
runtime work should deepen correctness and remove duplicate authority:

- move backend-backed APIs from stubs/smoke shape to Rust-backed behavior,
- delete legacy Page projections as equivalent host objects land,
- import focused WPT cases for each widened API family,
- keep generated WebIDL prototype inheritance intact,
- keep CDP and headless `--eval` consuming the same runtime path.

## Required proof for a host-family change

Each non-trivial host-family change should include:

- a focused runtime test in `vixen-engine` or `vixen-headless`,
- one user-visible seam check when applicable (`--eval`, CDP, or WPT fixture),
- a note in `COMPAT.md` only when support level or known gaps materially change,
- green `just gate-phase6` before push.
