# Vixen justfile. Recipe names referenced from docs/PLAN.md,
# docs/MILESTONES.md, and docs/ACCEPTANCE.md: `check-all-host`, `test-host`,
# `gate-*`, `size-fp`, `run`.
#
# The GNOME 50 SDK is NOT installed on the host; it is managed inside a
# flatpak-builder container. See docs/guidance/gnome-sdk-flatpak-builder.md
# and the `flatpak-*` recipes below.
#
# Tool ownership is intentionally split: mise pins versions and environment;
# this justfile owns project actions. Prefer adding/updating a recipe here over
# duplicating `cargo ...` command lines in docs, mise tasks, or CI.

alias check := check-all-host
alias smoke := gate-smoke
alias test := test-host

# Container runtime + the flatpak-builder image that owns the GNOME SDK.
# Bump these two together (and runtime-version in build-aux/*.json) to move SDK.
CONTAINER            := "podman"
FLATPAK_BUILDER_IMAGE := "ghcr.io/flathub-infra/flatpak-github-actions:gnome-50"
GNOME_RUNTIME_VERSION := "50"

# Default recipe: explain yourself.
default:
    @just --list

# --- Setup -------------------------------------------------------------------

# Full project setup after `mise install`: nightly for fuzzing, optional Cargo
# tools, then the cheap workspace build check. `mise bootstrap --yes` runs this.
setup: setup-rust setup-dev-tools check-all-host

# cargo-fuzz needs nightly even though normal development uses stable Rust.
setup-rust:
    rustup toolchain install nightly --profile minimal --component rust-src --allow-downgrade || true

# Optional developer tools used by `audit` and `fuzz-security`. Prefer the
# mise-managed cargo-binstall; fall back to `cargo install` where possible.
setup-dev-tools:
    cargo binstall --no-confirm cargo-audit || cargo install cargo-audit || true
    cargo binstall --no-confirm cargo-deny || cargo install cargo-deny || true
    cargo binstall --no-confirm cargo-fuzz || cargo install cargo-fuzz || true

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

test-engine:
    cargo test -p vixen-engine

# --- Lint / format -----------------------------------------------------------

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# --- Executable gates --------------------------------------------------------
# These are current, runnable milestone gates. They complement (not replace)
# the broader release acceptance checks in docs/ACCEPTANCE.md.

# Reviewer smoke: formatting, linting, and all host-runnable tests.
gate-smoke: fmt-check clippy check-all-host test-host

# Phase 0 gate: workspace builds and API DTO/trait tests pass.
gate-phase0: check-all-host test-api

# Phase 1 gate: networking/store tests, advisory/license audit, and security
# fuzz targets at their planned iteration count.
gate-phase1: test-net test-store audit fuzz-security

# Phase 2 vertical gate: engine tests plus SpiderMonkey eval through headless.
gate-phase2: test-engine
    test "$(cargo run -q -p vixen-headless -- --url file://{{justfile_directory()}}/fixtures/dom/basic.html --eval '1+2')" = "3"

# Phase 3 current gate: DOM parse + Stylo selector matching through the shared
# Page facade and the WPT fixture runner. Full computed cascade extends this.
gate-phase3:
    cargo test -p vixen-engine doc
    cargo test -p vixen-engine style_dom
    cargo test -p vixen-engine style_cascade
    cargo test -p vixen-engine page
    cargo test -p vixen-headless --test wpt_runner

# Run a committed external-WPT profile against an ignored upstream checkout.
# Example: `just wpt-profile fixtures/wpt-profiles/layout.json .tmp/wpt`.
wpt-profile profile root=".tmp/wpt":
    VIXEN_WPT_PROFILE="{{profile}}" VIXEN_WPT_ROOT="{{root}}" cargo test -p vixen-headless --test wpt_profile_runner -- --nocapture

# Phase 4 current gate: pure layout-resolution prep plus the first executable
# Page-backed Vixen layout-tree / line-layout slices.
gate-phase4:
    cargo test -p vixen-engine layout_tree
    cargo test -p vixen-engine line_layout
    cargo test -p vixen-engine box_model
    cargo test -p vixen-engine flex_resolve
    cargo test -p vixen-engine grid_resolve
    cargo test -p vixen-engine writing_modes
    cargo test -p vixen-engine multicol
    cargo test -p vixen-engine scroll_snap
    case "$(cargo run -q -p vixen-headless -- --url file://{{justfile_directory()}}/fixtures/layout/flex-row.html --viewport 360x200 --dump-layout-tree)" in *"# layout-tree"*"tag=section id=flex"*"tag=div id=grow2"*"w=153.3 h=40.0"*) true;; *) false;; esac
    case "$(cargo run -q -p vixen-headless -- --url file://{{justfile_directory()}}/fixtures/layout/boxes.html --viewport 120x200 --dump-lines)" in *"line 1:"*) true;; *) false;; esac

# Phase 5 current gate: display-list contract + paint-geometry/compositing prep,
# plus the first executable Page-backed display-list dump.
gate-phase5:
    cargo test -p vixen-engine display_list
    cargo test -p vixen-engine page
    cargo test -p vixen-engine transform
    cargo test -p vixen-engine border_radius
    cargo test -p vixen-engine gradient
    cargo test -p vixen-engine radial_gradient
    cargo test -p vixen-engine conic_gradient
    cargo test -p vixen-engine box_shadow
    cargo test -p vixen-engine background_position
    cargo test -p vixen-engine stacking_context
    cargo test -p vixen-engine blend
    cargo test -p vixen-engine filter
    cargo test -p vixen-engine border_image
    cargo test -p vixen-engine clip_path
    cargo test -p vixen-engine mask
    cargo test -p vixen-engine animation
    cargo test -p vixen-engine geometry
    case "$(cargo run -q -p vixen-headless -- --url file://{{justfile_directory()}}/fixtures/paint/display-list.html --viewport 160x120 --dump-display-list)" in *"cmd 0: background"*"cmd 1: text"*) true;; *) false;; esac
    case "$(cargo run -q -p vixen-headless -- --url file://{{justfile_directory()}}/fixtures/paint/display-list.html --viewport 160x120 --paint-stats)" in *"# paint-stats"*"text-runs="*) true;; *) false;; esac

# Phase 6 current gate: DOM/forms/network-host pure prep + responsive images.
gate-phase6:
    cargo test -p vixen-engine forms
    cargo test -p vixen-engine form_submission
    cargo test -p vixen-engine dataset
    cargo test -p vixen-engine storage_key
    cargo test -p vixen-engine url_search_params
    cargo test -p vixen-engine mime
    cargo test -p vixen-engine text_codec
    cargo test -p vixen-engine html_serialize
    cargo test -p vixen-engine class_list
    cargo test -p vixen-engine calc
    cargo test -p vixen-engine easing
    cargo test -p vixen-engine media_query
    cargo test -p vixen-engine source_size
    cargo test -p vixen-engine responsive_select
    cargo test -p vixen-engine structured_clone
    cargo test -p vixen-engine message_port
    cargo test -p vixen-engine range
    cargo test -p vixen-engine history
    cargo test -p vixen-engine mutation_observer
    cargo test -p vixen-engine traversal
    cargo test -p vixen-engine whatwg_url

# --- Fuzz (docs/PLAN.md Phase 1 gate: 1M iterations each) --------------------

_fuzz-tools-present:
    command -v cargo-fuzz >/dev/null || { printf '%s\n' "cargo-fuzz missing; run 'mise bootstrap --yes' or 'just setup-dev-tools'" >&2; exit 1; }

fuzz-security: _fuzz-tools-present
    cargo fuzz run url_policy_validate -- -max_len=4096 -runs=1000000
    cargo fuzz run csp_parse       -- -max_len=4096 -runs=1000000

# Backward-compatible name retained for older notes/scripts.
fuzz-init: fuzz-security

# --- Size (docs/ACCEPTANCE.md "Binary size gates") ---------------------------
# Measure stripped release binaries. Document any change > +50 KiB in the
# commit message (docs/ACCEPTANCE.md).
size-fp: build-release
    @set -eu; \
        gui="target/release/vixen"; \
        headless="target/release/vixen-headless"; \
        test -x "$gui" || { echo "missing $gui" >&2; exit 1; }; \
        test -x "$headless" || { echo "missing $headless" >&2; exit 1; }; \
        gui_bytes=$(stat -c '%s' "$gui"); \
        headless_bytes=$(stat -c '%s' "$headless"); \
        gui_limit=$((14 * 1024 * 1024)); \
        headless_limit=$((14 * 1024 * 1024)); \
        printf '%s %s bytes\n' "$gui" "$gui_bytes"; \
        printf '%s %s bytes\n' "$headless" "$headless_bytes"; \
        failed=0; \
        if [ "$gui_bytes" -gt "$gui_limit" ]; then \
            echo "vixen exceeds static mozjs size target ($gui_bytes > $gui_limit bytes)" >&2; \
            failed=1; \
        fi; \
        if [ "$headless_bytes" -gt "$headless_limit" ]; then \
            echo "vixen-headless exceeds static mozjs size target ($headless_bytes > $headless_limit bytes)" >&2; \
            failed=1; \
        fi; \
        exit "$failed"

build-release:
    cargo build --release -p vixen --bin vixen -p vixen-headless --bin vixen-headless

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

_audit-tools-present:
    command -v cargo-audit >/dev/null || { printf '%s\n' "cargo-audit missing; run 'mise bootstrap --yes' or 'just setup-dev-tools'" >&2; exit 1; }
    command -v cargo-deny >/dev/null || { printf '%s\n' "cargo-deny missing; run 'mise bootstrap --yes' or 'just setup-dev-tools'" >&2; exit 1; }

audit: _audit-tools-present
    cargo audit
    cargo deny check
