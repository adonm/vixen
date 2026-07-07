# Vixen guidance

How-to guides for specific workflows. The spec/architecture/plan docs say
*what* and *why*; these guides say *how, step by step*.

| Guide | When to read it |
|-------|-----------------|
| [`mise.md`](mise.md) | Activating the project-managed toolchain and using `just` recipes correctly. Start here for local setup. |
| [`cargo-home.md`](cargo-home.md) | Why `CARGO_HOME` points at `<workspace>/.cargo` and how recipe-installed Cargo tools stay local. |
| [`gnome-sdk-flatpak-builder.md`](gnome-sdk-flatpak-builder.md) | Building against the GNOME 50 SDK. The GNOME SDK is **not** installed on the host; it is managed inside a `flatpak-builder` container image. |
| [`mozjs.md`](mozjs.md) | Acquiring SpiderMonkey. The `mozjs` crate downloads a prebuilt by default — we don't build it ourselves. |

(Add new guides here as standalone files. Keep each guide focused on one
workflow, with copy-pasteable commands that have been verified to run.)
