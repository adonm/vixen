# mise workflow

Vixen uses `mise` as the only project tool manager. The intended workflow is an
activated shell where `cargo`, `rustfmt`, `clippy`, `rustup`, and `just` are all
on `PATH` from the versions pinned in `.mise.toml`.

## First setup

```sh
mise trust
mise install
```

For the full local setup, including optional Cargo tools:

```sh
mise bootstrap --yes
```

## Daily shell setup

Activate mise once per shell, then run normal commands directly:

```sh
eval "$(mise activate bash)"    # bash
# eval "$(mise activate zsh)"   # zsh
# mise activate fish | source   # fish

cargo --version
just --version
just test-host
```

Do **not** hard-code paths to Cargo, and do not wrap every build command in
`mise exec`. If `cargo` is missing, the shell is not activated or `mise install`
has not completed.

## One-shot commands

For automation that cannot keep an activated shell, prefer a single activated
subshell:

```sh
bash -lc 'eval "$(mise activate bash)" && cargo fmt --all && just test-host'
```

Avoid tool-specific invocations like `mise exec rust@... -- cargo ...`; Rust is
special in mise because the Rust backend delegates to `rustup`. In an activated
shell, mise sets `RUSTUP_TOOLCHAIN` and exposes Cargo through the workspace
`CARGO_HOME` (`.cargo/bin`).

## Verifying the active toolchain

```sh
eval "$(mise activate bash)"
mise ls --current
command -v cargo
cargo --version
command -v just
just --version
```

Expected properties:

- `cargo` resolves under `<workspace>/.cargo/bin`.
- `just` resolves under mise's install directory.
- `cargo --version` matches the Rust version pinned in `.mise.toml`.
- `CARGO_HOME` is `<workspace>/.cargo`.

## Updating versions

Update shared tool versions with `mise use` so `.mise.toml` remains the source
of truth:

```sh
mise use rust@<version>
mise use just@<version>
mise use cargo-binstall@<version>
```

Then verify in a freshly activated shell and run `just gate-smoke` before
committing the version change.
