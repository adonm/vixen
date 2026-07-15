# Vixen justfile. Recipe names referenced from docs/PLAN.md,
# docs/MILESTONES.md, and docs/ACCEPTANCE.md: `check-all-host`, `test-host`,
# `gate-*`, release, size, and Flutter run commands.
#
# Linux release bundles are built in the pinned GNOME builder image. FlatPark
# repackages the official GitHub Release archive as a signed Flatpak.
#
# Tool ownership is intentionally split: mise pins versions and environment;
# this justfile owns project actions. Prefer adding/updating a recipe here over
# duplicating `cargo ...` command lines in docs, mise tasks, or CI.

alias check := check-all-host
alias alpha := gate-alpha
alias smoke := gate-smoke
alias test := test-host
alias webidl := gate-webidl
alias hooks := hooks-install
alias docs := book-build

# Container runtime + GNOME 50 image used for local release builds.
CONTAINER             := env_var_or_default("CONTAINER", "podman")
FLUTTER_BUILDER_IMAGE := "ghcr.io/flathub-infra/flatpak-github-actions:gnome-50"
FLUTTER_VERSION       := "3.47.0-0.1.pre"
FLUTTER_REVISION      := "bd1e75d918605c91b411e8789fb911e6c9a84534"
FLUTTER_ENGINE        := "bbd15867c003dc66e678cb3c218649fa8bf914f2"
FLUTTER_HELLO         := "fixtures/artifact-size/flutter_hello"
RUSTY_V8_ARCHIVE      := ".tmp/linux-release/librusty_v8_simdutf_release_x86_64-unknown-linux-gnu.a.gz"
RUSTY_V8_SHA256       := "aa30f198b6e7be2188df6498f95053c4c052f212037a01f2c31414d7aca84b53"
LINUX_RELEASE_BUNDLE  := "flutter/vixen_shell/build/linux/x64/release/bundle"
LINUX_RELEASE_ARCHIVE := ".tmp/release/vixen-linux-x86_64.tar.gz"
WTYPE                 := env_var_or_default("WTYPE", "wtype")

# Default recipe: explain yourself.
default:
    @just --list

# --- Setup -------------------------------------------------------------------

# Full project setup after `mise install`: nightly for fuzzing, optional Cargo
# tools, then the cheap workspace build check. `mise bootstrap --yes` runs this.
setup: setup-rust setup-dev-tools check-all-host

# Install/update git hooks through hk. mise pins hk and exports HK_MISE=1, so
# hooks execute in the project tool environment even from a plain git command.
hooks-install:
    hk install --mise

# cargo-fuzz needs nightly even though normal development uses stable Rust.
setup-rust:
    rustup toolchain install nightly --profile minimal --component rust-src --allow-downgrade || true

# Optional developer tools used by `audit` and `fuzz-security`. Prefer the
# mise-managed cargo-binstall; fall back to `cargo install` where possible.
setup-dev-tools:
    cargo binstall --no-confirm cargo-audit || cargo install cargo-audit || true
    cargo binstall --no-confirm cargo-deny || cargo install cargo-deny || true
    cargo binstall --no-confirm cargo-fuzz || cargo install cargo-fuzz || true

# Install the exact official Flutter beta archive declared as a mise tool.
setup-flutter:
    mise install http:flutter-beta
    mise x -- flutter --version

_flutter-sdk-present:
    command -v flutter >/dev/null || { printf '%s\n' "mise Flutter beta missing from PATH; activate mise and run 'just setup-flutter'" >&2; exit 1; }
    test "$(flutter --version --machine | python3 -c 'import json, sys; value = json.load(sys.stdin); print(value["frameworkRevision"], value["engineRevision"])')" = "{{FLUTTER_REVISION}} {{FLUTTER_ENGINE}}" || { printf '%s\n' "Flutter beta revision mismatch; run 'mise install http:flutter-beta'" >&2; exit 1; }

# --- Build / check -----------------------------------------------------------

# Type-check every Rust workspace target and feature.
check-all-host:
    cargo check --workspace --all-targets --all-features

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

# Safe controller plus its native C ABI tests. This does not require a Flutter
# SDK and does not prove Dart bindings, a Flutter shell, or external textures.
test-flutter-controller:
    cargo test -p vixen-ffi

# Focused native ABI gate: build the rlib/cdylib/staticlib targets and exercise
# exported ownership, header/layout, bounded JSON wire, and panic containment.
# This is native C ABI evidence only, not Dart/Flutter/platform package proof.
gate-native-abi:
    cargo build -p vixen-ffi
    cc -std=c11 -Wall -Wextra -Werror -fsyntax-only crates/vixen-ffi/tests/header_smoke.c
    cargo test -p vixen-ffi c_abi::tests
    cargo test -p vixen-ffi render

# Test-only R3 Canvas/Paragraph/PNG formatter and exact scene evidence with the
# pinned engine asked to use Impeller. This does not change production frames.
test-flutter-formatter-impeller: _flutter-sdk-present
    cd flutter/vixen_shell && FLUTTER_TEST_IMPELLER=true flutter test --enable-impeller --dart-define=VIXEN_REQUIRE_IMPELLER=true test/formatter_test.dart test/renderer_broker_service_test.dart

# Dart/widget/native bridge evidence for the checked-in Linux Flutter shell.
# The native smoke test loads the exact cdylib built by gate-native-abi.
gate-flutter-shell: _flutter-sdk-present gate-native-abi test-flutter-formatter-impeller
    cd flutter/vixen_shell && dart format --output=none --set-exit-if-changed lib test
    cd flutter/vixen_shell && flutter analyze
    cd flutter/vixen_shell && VIXEN_FFI_LIBRARY="{{justfile_directory()}}/target/debug/libvixen_ffi.so" flutter test

# Build the relocatable Linux bundle, including libvixen_ffi.so. Requires the
# normal Flutter Linux prerequisites: CMake, Ninja, pkg-config, and GTK 3 headers.
build-flutter-linux: _flutter-sdk-present
    cd flutter/vixen_shell && flutter build linux --debug

# Launch the Linux shell through Flutter's desktop runner.
run-flutter: _flutter-sdk-present
    cd flutter/vixen_shell && GDK_BACKEND=wayland flutter run -d linux

# Launch the Linux shell in a local headless Wayland compositor.
run-flutter-cage: _flutter-sdk-present
    command -v cage >/dev/null || { printf '%s\n' "cage is required for local headless Wayland testing" >&2; exit 1; }
    rm -rf .tmp/wayland-run && mkdir -m 700 -p .tmp/wayland-run
    XDG_RUNTIME_DIR="{{justfile_directory()}}/.tmp/wayland-run" GDK_BACKEND=wayland \
        WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=gles2 \
        LIBGL_ALWAYS_SOFTWARE=1 cage -- sh -c 'cd {{justfile_directory()}}/flutter/vixen_shell && exec flutter run -d linux'

# Stage locked application/Cargo inputs and the exact rusty_v8 archive. This is
# network-capable setup, not release evidence.
linux-release-prefetch: _flutter-sdk-present
    mkdir -p "$(dirname {{RUSTY_V8_ARCHIVE}})"
    test -f {{RUSTY_V8_ARCHIVE}} || curl -L --fail --output {{RUSTY_V8_ARCHIVE}} https://github.com/denoland/rusty_v8/releases/download/v149.4.0/librusty_v8_simdutf_release_x86_64-unknown-linux-gnu.a.gz
    printf '%s  %s\n' {{RUSTY_V8_SHA256}} {{RUSTY_V8_ARCHIVE}} | sha256sum --check
    cargo fetch --locked
    cd {{FLUTTER_HELLO}} && flutter pub get --enforce-lockfile
    cd flutter/vixen_shell && flutter pub get --enforce-lockfile

# Backward-compatible staging name used by the size workflow.
flutter-size-prefetch: linux-release-prefetch

linux-release-check-inputs: _flutter-sdk-present
    {{CONTAINER}} image exists {{FLUTTER_BUILDER_IMAGE}} || { printf '%s\n' "GNOME builder image missing; run 'just flutter-builder-update'" >&2; exit 1; }
    test -d "$RUSTUP_HOME" && test -x "$HOME/.cargo/bin/rustc" && test -d "$HOME/.pub-cache" || { printf '%s\n' "mise Rust and Flutter tool caches are required" >&2; exit 1; }
    test -f {{RUSTY_V8_ARCHIVE}} || { printf '%s\n' "rusty_v8 archive missing; run 'just linux-release-prefetch'" >&2; exit 1; }
    printf '%s  %s\n' {{RUSTY_V8_SHA256}} {{RUSTY_V8_ARCHIVE}} | sha256sum --check

# Build the official release/AOT bundle consumed unchanged by FlatPark. Flutter
# does not strip bundled Linux plugin ELFs, so strip those beside the runner.
build-flutter-release-linux: linux-release-check-inputs
    cd flutter/vixen_shell && flutter clean
    mkdir -p .tmp/linux-release/bin && ln -sf /usr/sbin/g++ .tmp/linux-release/bin/clang++ && ln -sf /usr/sbin/gcc .tmp/linux-release/bin/clang
    {{CONTAINER}} run --rm --security-opt label=disable \
        -v {{justfile_directory()}}:/workspace \
        -v "$(readlink -f "$RUSTUP_HOME")":/host-rustup:ro \
        -v "$(readlink -f "$HOME/.cargo/bin")":/host-cargo-bin:ro \
        -v "$(readlink -f "$HOME/.pub-cache")":"$HOME/.pub-cache" \
        -v "$(readlink -f "$(mise where http:flutter-beta)")":/opt/flutter-tool \
        -w /workspace -e HOME="$HOME" -e RUSTUP_HOME=/host-rustup -e RUSTUP_TOOLCHAIN=1.96.1 \
        -e CARGO_HOME=/workspace/.cargo -e PUB_CACHE="$HOME/.pub-cache" \
        -e CARGO_NET_OFFLINE=true -e RUSTY_V8_ARCHIVE=/workspace/{{RUSTY_V8_ARCHIVE}} \
        -e FLUTTER_SUPPRESS_ANALYTICS=true {{FLUTTER_BUILDER_IMAGE}} sh -lc \
        'export PATH=/workspace/.tmp/linux-release/bin:/opt/flutter-tool/flutter/bin:/host-cargo-bin:/usr/sbin:/usr/bin; \
         cd /workspace/flutter/vixen_shell && flutter pub get --enforce-lockfile && flutter build linux --release --no-pub'
    cd flutter/vixen_shell && flutter pub get --offline --enforce-lockfile
    strip --strip-unneeded {{LINUX_RELEASE_BUNDLE}}/vixen_shell
    find {{LINUX_RELEASE_BUNDLE}}/lib -maxdepth 1 -type f -name '*_plugin.so' \
        -exec strip --strip-unneeded {} +

# Deterministic upstream archive for GitHub Releases and FlatPark extra-data.
linux-release-archive: build-flutter-release-linux
    rm -f {{LINUX_RELEASE_ARCHIVE}} {{LINUX_RELEASE_ARCHIVE}}.sha256
    python3 scripts/package-linux-release.py {{LINUX_RELEASE_BUNDLE}} {{LINUX_RELEASE_ARCHIVE}}
    cd "$(dirname {{LINUX_RELEASE_ARCHIVE}})" && sha256sum "$(basename {{LINUX_RELEASE_ARCHIVE}})" > "$(basename {{LINUX_RELEASE_ARCHIVE}}).sha256"

# Extract and launch the exact archive in a headless Wayland compositor. The
# runner must survive to the timeout and report an Impeller backend.
linux-release-smoke: linux-release-archive
    rm -rf .tmp/linux-release/smoke && mkdir -p .tmp/linux-release/smoke
    tar -xzf {{LINUX_RELEASE_ARCHIVE}} -C .tmp/linux-release/smoke
    command -v cage >/dev/null || { printf '%s\n' "cage is required for the Wayland release smoke" >&2; exit 1; }
    rm -rf .tmp/linux-release/wayland && mkdir -m 700 -p .tmp/linux-release/wayland
    sh -c 'set +e; XDG_RUNTIME_DIR="{{justfile_directory()}}/.tmp/linux-release/wayland" GDK_BACKEND=wayland WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=gles2 LIBGL_ALWAYS_SOFTWARE=1 timeout 15s cage -- .tmp/linux-release/smoke/vixen/vixen_shell > .tmp/linux-release-smoke.log 2>&1; status=$?; set -e; cat .tmp/linux-release-smoke.log; test "$status" -eq 124; grep -Eq "Using the Impeller rendering backend \\((Vulkan|OpenGLES|VulkanSDF|OpenGLESSDF)\\)\\." .tmp/linux-release-smoke.log'

# Launch the real release bundle on an isolated Wayland compositor and require
# BrowserCore-projected fixture semantics to appear through native AT-SPI.
linux-at-spi-smoke: build-flutter-release-linux
    command -v cage >/dev/null || { printf '%s\n' "cage is required for the Wayland AT-SPI smoke" >&2; exit 1; }
    test -n "$DBUS_SESSION_BUS_ADDRESS" || { printf '%s\n' "an active user D-Bus/AT-SPI session is required" >&2; exit 1; }
    python3 -c 'import gi; gi.require_version("Atspi", "2.0"); from gi.repository import Atspi'
    rm -rf .tmp/linux-at-spi-wayland && mkdir -m 700 -p .tmp/linux-at-spi-wayland
    XDG_RUNTIME_DIR="{{justfile_directory()}}/.tmp/linux-at-spi-wayland" GDK_BACKEND=wayland \
        WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=gles2 \
        LIBGL_ALWAYS_SOFTWARE=1 cage -- python3 scripts/flutter-at-spi-smoke.py \
        --app {{LINUX_RELEASE_BUNDLE}}/vixen_shell \
        --library {{LINUX_RELEASE_BUNDLE}}/lib/libvixen_ffi.so \
        --url file://{{justfile_directory()}}/fixtures/dom/basic.html \
        --expect "DOM Basic"

_build-wayland-virtual-pointer: linux-release-check-inputs
    rm -rf .tmp/wayland-virtual-pointer && mkdir -p .tmp/wayland-virtual-pointer
    {{CONTAINER}} run --rm --security-opt label=disable \
        -v {{justfile_directory()}}:/workspace -w /workspace \
        {{FLUTTER_BUILDER_IMAGE}} sh -lc \
        'set -eu; wayland-scanner client-header scripts/protocols/wlr-virtual-pointer-unstable-v1.xml .tmp/wayland-virtual-pointer/wlr-virtual-pointer-unstable-v1-client-protocol.h; \
         wayland-scanner private-code scripts/protocols/wlr-virtual-pointer-unstable-v1.xml .tmp/wayland-virtual-pointer/wlr-virtual-pointer-unstable-v1-protocol.c; \
         cc -std=c11 -D_POSIX_C_SOURCE=200809L -Wall -Wextra -Werror \
           -I.tmp/wayland-virtual-pointer scripts/wayland-virtual-pointer.c \
           .tmp/wayland-virtual-pointer/wlr-virtual-pointer-unstable-v1-protocol.c \
           $(pkg-config --cflags --libs wayland-client) -lm \
           -o .tmp/wayland-virtual-pointer/wayland-virtual-pointer'

# Real Wayland basic-navigation/input evidence: physical chrome URL entry,
# back/forward/reload/active stop with restored scrolling, IBus/GTK preedit+
# commit, and nested/root wheel routing in the release process.
linux-interaction-smoke: build-flutter-release-linux _build-wayland-virtual-pointer
    test -x "{{WTYPE}}" || command -v "{{WTYPE}}" >/dev/null || { printf '%s\n' "wtype is required for native Wayland keyboard input" >&2; exit 1; }
    command -v ibus >/dev/null && ibus list-engine | grep -q '^  anthy -' || { printf '%s\n' "IBus Anthy is required for native preedit evidence" >&2; exit 1; }
    test -n "$DBUS_SESSION_BUS_ADDRESS" || { printf '%s\n' "an active user D-Bus/AT-SPI session is required" >&2; exit 1; }
    python3 -c 'import gi; gi.require_version("Atspi", "2.0"); from gi.repository import Atspi'
    rm -rf .tmp/linux-interaction-wayland .tmp/interaction-profile && mkdir -m 700 -p .tmp/linux-interaction-wayland
    IBUS_ADDRESS="$(ibus address)" XDG_RUNTIME_DIR="{{justfile_directory()}}/.tmp/linux-interaction-wayland" \
        GDK_BACKEND=wayland WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
        WLR_RENDERER=gles2 LIBGL_ALWAYS_SOFTWARE=1 timeout 120s cage -- \
        python3 scripts/flutter-interaction-smoke.py \
        --app {{LINUX_RELEASE_BUNDLE}}/vixen_shell \
        --library {{LINUX_RELEASE_BUNDLE}}/lib/libvixen_ffi.so \
        --url file://{{justfile_directory()}}/fixtures/events/linux-interaction-qa.html \
        --wtype "{{WTYPE}}" \
        --pointer .tmp/wayland-virtual-pointer/wayland-virtual-pointer

# First R5 rendered-automation checkpoint: launch the same release bundle in
# page-only mode and capture exact presented Flutter scenes at two viewports.
linux-automation-smoke: build-flutter-release-linux
    command -v cage >/dev/null || { printf '%s\n' "cage is required for the Wayland automation smoke" >&2; exit 1; }
    rm -rf .tmp/linux-automation-wayland .tmp/linux-automation && mkdir -m 700 -p .tmp/linux-automation-wayland && mkdir -p .tmp/linux-automation
    XDG_RUNTIME_DIR="{{justfile_directory()}}/.tmp/linux-automation-wayland" \
        GDK_BACKEND=wayland WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
        WLR_RENDERER=gles2 LIBGL_ALWAYS_SOFTWARE=1 timeout 210s cage -- \
        python3 scripts/flutter-automation-smoke.py \
        --app {{LINUX_RELEASE_BUNDLE}}/vixen_shell \
        --library {{LINUX_RELEASE_BUNDLE}}/lib/libvixen_ffi.so \
        --url file://{{justfile_directory()}}/fixtures/dom/basic.html \
        --output-dir .tmp/linux-automation

flutter-size-check-inputs: linux-release-check-inputs

# Add the controlled hello peer for the raw release-bundle size comparison.
build-flutter-size-linux: build-flutter-release-linux
    cd {{FLUTTER_HELLO}} && flutter clean
    {{CONTAINER}} run --rm --security-opt label=disable \
        -v {{justfile_directory()}}:/workspace \
        -v "$(readlink -f "$(mise where http:flutter-beta)")":/opt/flutter-tool \
        -v "$(readlink -f "$HOME/.pub-cache")":"$HOME/.pub-cache" \
        -w /workspace -e HOME="$HOME" -e PUB_CACHE="$HOME/.pub-cache" \
        -e FLUTTER_SUPPRESS_ANALYTICS=true {{FLUTTER_BUILDER_IMAGE}} sh -lc \
        'export PATH=/workspace/.tmp/linux-release/bin:/opt/flutter-tool/flutter/bin:/usr/sbin:/usr/bin; \
         cd /workspace/{{FLUTTER_HELLO}} && flutter pub get --enforce-lockfile && flutter build linux --release --no-pub'
    cd {{FLUTTER_HELLO}} && flutter pub get --offline --enforce-lockfile

size-flutter-linux: build-flutter-size-linux
    node scripts/flutter-artifact-size.mjs --hello-bundle {{FLUTTER_HELLO}}/build/linux/x64/release/bundle --vixen-bundle flutter/vixen_shell/build/linux/x64/release/bundle

size-flutter-linux-json: build-flutter-size-linux
    node scripts/flutter-artifact-size.mjs --hello-bundle {{FLUTTER_HELLO}}/build/linux/x64/release/bundle --vixen-bundle flutter/vixen_shell/build/linux/x64/release/bundle --json

size-flutter-linux-existing:
    node scripts/flutter-artifact-size.mjs --hello-bundle {{FLUTTER_HELLO}}/build/linux/x64/release/bundle --vixen-bundle flutter/vixen_shell/build/linux/x64/release/bundle

# ADR-017 ownership vertical: production BrowserCore transport, context/runtime
# generations, bounded events, profile/session partitioning, and headless adapter.
test-browser-core: test-flutter-controller
    cargo test -p vixen-engine browser::tests -- --test-threads=1
    cargo test -p vixen-headless browser_adapter::tests -- --test-threads=1
    cargo test -p vixen-headless eval_gate_returns_three -- --test-threads=1
    cargo test -p vixen-headless interaction_flags_run_through_browser_core -- --test-threads=1

test-script:
    cargo test -p vixen-engine script

test-headless-runtime:
    cargo test -p vixen-headless focused_document_eval_uses_runtime_host_objects
    cargo test -p vixen-headless --test cdp_runtime

# Real Playwright client smoke over Vixen's CDP WebSocket. Requires mise-managed
# Node. Uses playwright-core only: no Chromium/browser binary download.
_node-deps:
    mise x node@24 -- npm ci

cdp-playwright-smoke: _node-deps
    mise x node@24 -- npm run cdp:playwright-smoke

# R5 rendered CDP gate: the release Flutter host owns the sole BrowserCore and
# CDP listener under Cage; Playwright drives commit geometry/input, two target
# viewports, before/after scene capture, and renderer-reset full resync.
flutter-cdp-playwright-smoke: build-flutter-release-linux _node-deps
    command -v cage >/dev/null || { printf '%s\n' "cage is required for rendered CDP smoke" >&2; exit 1; }
    rm -rf .tmp/flutter-cdp-wayland .tmp/flutter-cdp-profile && mkdir -m 700 -p .tmp/flutter-cdp-wayland .tmp/flutter-cdp-profile
    XDG_RUNTIME_DIR="{{justfile_directory()}}/.tmp/flutter-cdp-wayland" \
        GDK_BACKEND=wayland WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
        WLR_RENDERER=gles2 LIBGL_ALWAYS_SOFTWARE=1 \
        VIXEN_CDP_APP="{{justfile_directory()}}/{{LINUX_RELEASE_BUNDLE}}/vixen_shell" \
        VIXEN_FFI_LIBRARY="{{justfile_directory()}}/{{LINUX_RELEASE_BUNDLE}}/lib/libvixen_ffi.so" \
        VIXEN_PROFILE_PATH="{{justfile_directory()}}/.tmp/flutter-cdp-profile/profile.redb" \
        timeout 180s cage -- mise x node@24 -- npm run cdp:flutter-smoke

# Full R5 product gate: execute all manifest checks in order through one
# long-lived Flutter-owned BrowserCore. Text/runtime checks use BrowserCore's
# typed inspection seam; layout, hashes, and reftests use exact Flutter commits.
flutter-fixture-manifest: build-flutter-release-linux _node-deps
    command -v cage >/dev/null || { printf '%s\n' "cage is required for the Flutter fixture manifest" >&2; exit 1; }
    rm -rf .tmp/flutter-manifest-wayland .tmp/flutter-manifest-profile && mkdir -m 700 -p .tmp/flutter-manifest-wayland .tmp/flutter-manifest-profile
    XDG_RUNTIME_DIR="{{justfile_directory()}}/.tmp/flutter-manifest-wayland" \
        GDK_BACKEND=wayland WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
        WLR_RENDERER=gles2 LIBGL_ALWAYS_SOFTWARE=1 \
        VIXEN_CDP_APP="{{justfile_directory()}}/{{LINUX_RELEASE_BUNDLE}}/vixen_shell" \
        VIXEN_FFI_LIBRARY="{{justfile_directory()}}/{{LINUX_RELEASE_BUNDLE}}/lib/libvixen_ffi.so" \
        VIXEN_PROFILE_PATH="{{justfile_directory()}}/.tmp/flutter-manifest-profile/profile.redb" \
        timeout 420s cage -- mise x node@24 -- npm run fixtures:flutter-manifest

gate-r5: linux-automation-smoke flutter-cdp-playwright-smoke flutter-fixture-manifest

# Focused Alpha 6 automation product gate: dispatcher/runtime integration plus
# the real external Playwright client over CDP WebSocket.
gate-alpha6-cdp: cdp-playwright-smoke
    cargo test -p vixen-headless cdp::tests
    cargo test -p vixen-headless --test cdp_runtime

# --- Lint / format -----------------------------------------------------------

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# --- Documentation -----------------------------------------------------------

book-build:
    mdbook build

book-serve *ARGS:
    mdbook serve --open {{ARGS}}

# --- Executable gates --------------------------------------------------------
# These are current, runnable milestone gates. They complement (not replace)
# the broader release acceptance checks in docs/ACCEPTANCE.md.

# Fast alpha-slice gate: pair this with focused tests and the relevant phase
# gate. It is not a substitute for reviewer smoke before commit/push.
gate-alpha: fmt-check clippy check-all-host gate-webidl gate-architecture test-browser-core
    cargo test -p vixen-headless --test wpt_runner

# Stable crate-boundary allowlist. This also bans frontend direct composition
# of network, store, and WPT implementation crates.
gate-architecture:
    python3 scripts/check-vixen-deps.py

# Reviewer smoke: formatting, linting, and all host-runnable tests.
gate-smoke: fmt-check clippy check-all-host test-host

# Long gate invoked by hk pre-push. Keep long checks here instead of pre-commit
# so local iteration stays fast.
gate-push: gate-alpha gate-phase6 gate-smoke
    git diff --check
    git diff --cached --check

# Phase 0 gate: workspace builds and API DTO/trait tests pass.
gate-phase0: check-all-host test-api

# Phase 1 gate: networking/store tests, advisory/license audit, and security
# fuzz targets at their planned iteration count.
gate-phase1: test-net test-store audit fuzz-security

# Phase 2 vertical gate: engine tests plus deno_core eval through headless.
gate-phase2: test-engine gate-webidl
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

# Reproduce the compatibility counts published in docs/COMPAT.md.
compat-report:
    cargo test -p vixen-headless --test wpt_runner -- --nocapture

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
    cargo run -q -p vixen-headless -- --url file://{{justfile_directory()}}/fixtures/paint/display-list.html --viewport 160x120 --screenshot target/vixen-phase5-shot.png
    test -s target/vixen-phase5-shot.png

# WebIDL/runtime host gate: generated constructor/prototype coverage plus the
# user-visible headless/CDP seams that consume those bindings.
gate-webidl: test-script test-headless-runtime
    test "$(cargo run -q -p vixen-headless -- --url file://{{justfile_directory()}}/fixtures/dom/basic.html --eval 'globalThis.__vixenWebidl.interfaceNames().includes("HTMLDialogElement") && HTMLElement.prototype instanceof Element')" = "true"

# Phase 6 current gate: full engine host-family coverage plus WebIDL runtime
# seams. `test-engine` is intentionally cheaper and less fragile than a long
# list of filtered test invocations, while covering the same phase-6 modules.
gate-phase6: test-engine gate-webidl

# --- Fuzz (docs/PLAN.md Phase 1 gate: 1M iterations each) --------------------

_fuzz-tools-present:
    command -v cargo-fuzz >/dev/null || { printf '%s\n' "cargo-fuzz missing; run 'mise bootstrap --yes' or 'just setup-dev-tools'" >&2; exit 1; }

fuzz-security: _fuzz-tools-present
    cargo fuzz run url_policy_validate -- -max_len=4096 -runs=1000000
    cargo fuzz run csp_parse       -- -max_len=4096 -runs=1000000
    cargo fuzz run cookie_set_cookie -- -max_len=4096 -runs=1000000
    cargo fuzz run html5ever_parse -- -max_len=16384 -runs=1000000

# Backward-compatible name retained for older notes/scripts.
fuzz-init: fuzz-security

# --- Measurement (docs/BASELINES.md) -----------------------------------------

# Hermetic local scenarios. Example: `just baseline-headless 9 2`.
baseline-headless runs="5" warmups="1": build-release
    node scripts/headless-baseline.mjs --binary target/release/vixen-headless --suite fixtures/performance/headless-local.json --runs {{runs}} --warmups {{warmups}}

# Same scenarios as JSON for an accepted-report candidate.
baseline-headless-json runs="5" warmups="1": build-release
    node scripts/headless-baseline.mjs --binary target/release/vixen-headless --suite fixtures/performance/headless-local.json --runs {{runs}} --warmups {{warmups}} --json

# Temporary explicit profile; N controls repeated and unique visits (1-50).
baseline-profile-growth runs="5": build-release
    node scripts/profile-growth-baseline.mjs --binary target/release/vixen-headless --runs {{runs}}

# Headless-only structured artifact accounting; no Flatpak build required.
size-headless: build-release
    node scripts/artifact-size.mjs --headless target/release/vixen-headless

# Hermetic beta measurement foundation; intentionally not part of gate-push.
baseline-beta runs="5" warmups="1": build-release
    node scripts/headless-baseline.mjs --binary target/release/vixen-headless --suite fixtures/performance/headless-local.json --runs {{runs}} --warmups {{warmups}}
    node scripts/profile-growth-baseline.mjs --binary target/release/vixen-headless --runs {{runs}}
    node scripts/artifact-size.mjs --headless target/release/vixen-headless

build-release:
    cargo build --locked --release -p vixen-headless --bin vixen-headless

# --- Local GNOME release builder ---------------------------------------------

flutter-builder-update:
    {{CONTAINER}} pull {{FLUTTER_BUILDER_IMAGE}}

flutter-builder-shell:
    {{CONTAINER}} run --rm -it -v {{justfile_directory()}}:/workspace:z -w /workspace {{FLUTTER_BUILDER_IMAGE}}

# --- Audit (docs/ACCEPTANCE.md hard gate) ------------------------------------

_audit-tools-present:
    command -v cargo-audit >/dev/null || { printf '%s\n' "cargo-audit missing; run 'mise bootstrap --yes' or 'just setup-dev-tools'" >&2; exit 1; }
    command -v cargo-deny >/dev/null || { printf '%s\n' "cargo-deny missing; run 'mise bootstrap --yes' or 'just setup-dev-tools'" >&2; exit 1; }

audit: _audit-tools-present
    cargo audit
    cargo deny check
