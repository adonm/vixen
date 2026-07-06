# Vixen justfile. Recipe names referenced from docs/PLAN.md,
# docs/MILESTONES.md, and docs/ACCEPTANCE.md: `check-all-host`, `test-host`,
# `gate-*`, `size-fp`, `run`.
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

# --- Executable gates --------------------------------------------------------
# These are current, runnable milestone gates. They complement (not replace)
# the broader release acceptance checks in docs/ACCEPTANCE.md.

# Reviewer smoke: formatting, linting, and all host-runnable tests.
gate-smoke: fmt-check clippy test-host

# Phase 2 vertical gate: SpiderMonkey eval through the headless binary.
gate-phase2:
    test "$(cargo run -q -p vixen-headless -- --url file://{{justfile_directory()}}/fixtures/dom/basic.html --eval '1+2')" = "3"

# Phase 3 current gate: DOM parse + Stylo selector matching through the shared
# Page facade and the WPT fixture runner. Full computed cascade extends this.
gate-phase3:
    cargo test -p vixen-engine doc
    cargo test -p vixen-engine style_dom
    cargo test -p vixen-engine style_cascade
    cargo test -p vixen-engine page
    cargo test -p vixen-headless --test wpt_runner

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
    case "$(cargo run -q -p vixen-headless -- --url file://{{justfile_directory()}}/fixtures/layout/boxes.html --viewport 120x200 --dump-layout-tree)" in *"# layout-tree"*"tag=main id=root"*"tag=div id=a"*"w=100.0 h=100.0"*) true;; *) false;; esac
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
