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

## Current direction

The recent broad runtime surface is useful for CDP/headless compatibility, but
the next runtime work should deepen correctness:

- move backend-backed APIs from stubs/smoke shape to Rust-backed behavior,
- import focused WPT cases for each widened API family,
- keep generated WebIDL prototype inheritance intact,
- keep CDP and headless `--eval` consuming the same runtime path.

## Required proof for a host-family change

Each non-trivial host-family change should include:

- a focused runtime test in `vixen-engine` or `vixen-headless`,
- one user-visible seam check when applicable (`--eval`, CDP, or WPT fixture),
- a note in `COMPAT.md` only when support level or known gaps materially change,
- green `just gate-phase6` before push.
