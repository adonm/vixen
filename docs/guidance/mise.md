# mise + just workflow

Vixen uses two tools with separate jobs:

- `mise` pins tool versions and exports the project environment from
  `.mise.toml` (`RUSTUP_TOOLCHAIN`, `CARGO_HOME`, `HK_MISE`, and `PATH`).
- `just` owns repository actions. Add or update a `justfile` recipe instead of
  copying `cargo ...` command lines into docs, scripts, or CI.
- `hk` owns git lifecycle hooks. The project config is `hk.pkl`; install hooks
  with `just hooks-install` or through the mise postinstall hook.

The intended workflow is an activated shell where `cargo`, `rustfmt`, `clippy`,
`rustup`, `cargo-binstall`, `hk`, and `just` come from the versions pinned in
`.mise.toml`.

## Linux environment

Use an x86_64 Ubuntu 24.04 Distrobox as the primary Linux development
environment:

```sh
distrobox create \
  --name flutter-dev \
  --image quay.io/toolbx/ubuntu-toolbox:24.04
distrobox enter flutter-dev
```

The versioned image matches Vixen's explicit Ubuntu 24.04 host CI jobs. Do not
replace it with `latest`; Ubuntu 26.04 and Fedora are additional compatibility
checks rather than the development gate. Distrobox shares the host home by
default, so an existing Mise installation and this checkout remain available.
Do not add project activation blindly to the shared shell startup files.

Install the native Linux build and host-smoke dependencies once inside the box:

```sh
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  at-spi2-core binutils build-essential cage clang cmake curl dbus-daemon git \
  gir1.2-atspi-2.0 ibus ibus-gtk4 ibus-mozc libegl-dev libgl-dev \
  libgtk-4-dev libsecret-1-dev libwayland-dev mesa-vulkan-drivers ninja-build \
  pkg-config python3 python3-gi ripgrep wayland-protocols wtype
```

## First setup

```sh
mise trust
mise bootstrap --yes
```

`mise bootstrap` installs pinned tools, then runs `just setup`, which installs
the optional Cargo tools used by `just audit` / `just fuzz-security`, installs a
nightly Rust toolchain for cargo-fuzz, and finishes with `just check-all-host`.
mise's postinstall hook also runs `hk install --mise` so git hooks execute in
the project tool environment.

For tools-only CI images, `mise install` is enough; run project checks through
`just` after activating the shell.

## Daily shell setup

Activate mise once per shell, then run recipes directly:

```sh
eval "$(mise activate bash)"    # bash
# eval "$(mise activate zsh)"   # zsh
# mise activate fish | source   # fish

just check
just test
just smoke
just hooks-install
```

Do **not** hard-code paths to Cargo, and do not wrap every build command in
`mise exec`. If `cargo` is missing, the shell is not activated or `mise install`
has not completed.

## Common recipes

| Recipe | What it does |
|--------|--------------|
| `just setup` | Optional dev tools + nightly for fuzzing + `check-all-host` |
| `just hooks-install` | Install/update hk hooks through `hk install --mise` |
| `just check` / `just check-all-host` | Type-check the host-runnable workspace |
| `just test` / `just test-host` | Run host-runnable tests |
| `just smoke` / `just gate-smoke` | Formatting check, clippy, check, tests |
| `just gate-push` | Long hk pre-push gate |
| `just audit` | `cargo audit` and `cargo deny check` |
| `just fuzz-security` | Phase 1 fuzz targets at 1 M iterations |
| `just linux-release-prefetch` / `just linux-release-smoke` | Build and verify the official Linux archive consumed by FlatPark |

Use `just --list` for the full recipe list.

## One-shot commands

For automation that cannot keep an activated shell, prefer a single activated
subshell and still call `just` recipes:

```sh
bash -lc 'eval "$(mise activate bash)" && just smoke'
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
command -v hk
hk --version
printenv CARGO_HOME
```

Expected properties:

- `cargo` resolves under `<workspace>/.cargo/bin`.
- `just` resolves under mise's install directory.
- `hk` resolves under mise's install directory.
- `cargo --version` matches the Rust version pinned in `.mise.toml`.
- `CARGO_HOME` is `<workspace>/.cargo`.

## Updating versions

Update shared tool versions with `mise use` so `.mise.toml` remains the source
of truth:

```sh
mise use rust@<version>
mise use just@<version>
mise use hk@<version>
mise use cargo-binstall@<version>
```

Then verify in a freshly activated shell and run `just smoke` before committing
the version change.
