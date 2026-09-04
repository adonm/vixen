# Vixen guidance

How-to guides for specific workflows. The spec/architecture/plan docs say
*what* and *why*; these guides say *how, step by step*.

| Guide | When to read it |
|-------|-----------------|
| [`mise.md`](mise.md) | Activating the project-managed toolchain and using `just` recipes correctly. Start here for local setup. |
| [`cargo-home.md`](cargo-home.md) | Why `CARGO_HOME` points at `<workspace>/.cargo` and how recipe-installed Cargo tools stay local. |
| [`deno-core.md`](deno-core.md) | `deno_core`/V8 embedding, host-extension shape, and cache/pinning notes. |
| [`flatpark-release.md`](flatpark-release.md) | Building the official Linux release archive and publishing it through FlatPark. |

(Add new guides here as standalone files. Keep each guide focused on one
workflow, with copy-pasteable commands that have been verified to run.)
