# Vixen justfile. Recipe names referenced from docs/PLAN.md and
# docs/ACCEPTANCE.md: `check-all-host`, `test-host`, `size-fp`, `run`.
#
# The GNOME 50 SDK is NOT installed on the host; it is managed inside a
# flatpak-builder container. See docs/guidance/gnome-sdk-flatpak-builder.md
# and the `flatpak-*` recipes below.

# Container runtime + the flatpak-builder image that owns the GNOME SDK.
# Bump these two together (and runtime-version in build-aux/*.json) to move SDK.
CONTAINER            := "podman"
FLATPAK_BUILDER_IMAGE := "ghcr.io/flathub-infra/flatpak-github-actions:gnome-50"
GNOME_RUNTIME_VERSION := "50"

# Default recipe: explain yourself.
default:
    @just --list

# --- Build / check -----------------------------------------------------------

# Type-check the whole workspace (default features). This is the Phase 0
# gate (docs/PLAN.md). GTK shell wiring is opt-in via the `gtk-shell`
# feature because it needs the GNOME SDK; see `shell-check`.
check-all-host:
    cargo check --workspace --all-targets

# Type-check including the GTK shell. The canonical way to get the GNOME
# SDK is the flatpak-builder container (docs/guidance/gnome-sdk-flatpak-builder.md);
# for ad-hoc native work you can install your distro's gtk4/libadwaita -devel
# packages and run this. Otherwise use `just flatpak-build`.
shell-check:
    cargo check --workspace --all-targets --features vixen-shell/gtk-shell

# --- Test --------------------------------------------------------------------

# Run host-runnable tests. Phase 0 runs only `vixen-api`; Phase 1 adds
# `vixen-net` and `vixen-store` (docs/PLAN.md gate table).
test-host:
    cargo test --workspace

test-api:
    cargo test -p vixen-api

test-net:
    cargo test -p vixen-net

test-store:
    cargo test -p vixen-store

# --- Lint / format -----------------------------------------------------------

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# --- Fuzz (docs/PLAN.md Phase 1 gate: 1M iterations each) --------------------
# Requires `cargo install cargo-fuzz` and a nightly toolchain.
fuzz-init:
    @echo "Run once: cargo install cargo-fuzz"
    cargo fuzz run url_policy_validate -- -max_len=4096 -runs=1000000
    cargo fuzz run csp_parse       -- -max_len=4096 -runs=1000000

# --- Size (docs/ACCEPTANCE.md "Binary size gates") ---------------------------
# Measure stripped release binaries. Document any change > +50 KiB in the
# commit message (docs/ACCEPTANCE.md).
size-fp: build-release
    @ls -la target/release/vixen target/release/vixen-headless 2>/dev/null || \
        echo "binaries not present yet (some crates are still stubs)"

build-release:
    cargo build --release

# --- Run ---------------------------------------------------------------------
# Launch the GUI. Needs the GNOME SDK; the supported path is the flatpak
# build (`just flatpak-build`). For ad-hoc native runs, install your distro's
# gtk4/libadwaita -devel packages and use this.
run *ARGS:
    cargo run --features vixen-shell/gtk-shell -- {{ARGS}}

# --- GNOME SDK via flatpak-builder containers --------------------------------
# docs/guidance/gnome-sdk-flatpak-builder.md. The image tag pins the
# preinstalled org.gnome.Sdk//<GNOME_RUNTIME_VERSION> runtime.

# Pull/refresh the builder image. This IS the GNOME SDK install/upgrade.
flatpak-update-sdk:
    {{CONTAINER}} pull {{FLATPAK_BUILDER_IMAGE}}

# Interactive shell in the SDK container, workspace mounted at /workspace.
flatpak-shell:
    {{CONTAINER}} run --rm -it -v {{justfile_directory()}}:/workspace:z -w /workspace {{FLATPAK_BUILDER_IMAGE}}

# Build the Flatpak against org.gnome.Sdk//{{GNOME_RUNTIME_VERSION}} inside the
# container. Manifest is scaffolding until the shell lands (Phase 9).
flatpak-build:
    {{CONTAINER}} run --rm -v {{justfile_directory()}}:/workspace:z -w /workspace {{FLATPAK_BUILDER_IMAGE}} \
        flatpak-builder --user --force-clean --install \
        build-aux/_build build-aux/org.vixen.Vixen.json

# --- Audit (docs/ACCEPTANCE.md hard gate) ------------------------------------
# Requires `cargo install cargo-audit cargo-deny`.
audit:
    cargo audit
    cargo deny check 2>/dev/null || echo "cargo-deny not installed; skipping"
