# Linux release archive and FlatPark packaging

Vixen publishes one official x86_64 Linux archive on GitHub Releases. FlatPark
repackages those unchanged, checksum-pinned bytes as a signed Flatpak. Vixen no
longer builds or hosts its own OSTree repository.

> **Priority gate:** this document is a deferred release runbook. Keep the
> official archive reproducible, but do not submit, review, or publish through
> FlatPark until the Linux Flutter shell passes the basic-browser gate defined
> in `../ROADMAP.md` (visible navigation/rendering, scrolling, text/IME, core
> navigation controls, find/zoom, and bounded recovery).

This deliberately separates two responsibilities:

1. **Vixen CI** builds and tests `vixen-linux-x86_64.tar.gz` from the tagged
   source revision.
2. **FlatPark** pins that public release asset by URL, size, and SHA-256, adds
   the minimal wrapper/metadata/permissions, and signs and hosts the Flatpak.

FlatPark is an independent community repository, not Flathub. Its runtime
dependencies are supplied through Flathub.

## Pinned build inputs

The release archive uses:

- Flutter `3.47.0-0.1.pre` beta from the official Linux x64 archive declared in
  `.mise.toml`;
- Flutter revision `bd1e75d918605c91b411e8789fb911e6c9a84534`;
- engine revision `bbd15867c003dc66e678cb3c218649fa8bf914f2`;
- Rust `1.96.1`;
- `Cargo.lock` and `flutter/vixen_shell/pubspec.lock`; and
- rusty_v8 `v149.4.0` archive SHA-256
  `aa30f198b6e7be2188df6498f95053c4c052f212037a01f2c31414d7aca84b53`.

The Linux runner explicitly enables Impeller. A beta version alone is not
accepted as runtime evidence.

## Local build and smoke

Install the mise tools, pull the GNOME builder image, stage locked inputs, then
build the exact release archive:

```sh
mise install
just flutter-builder-update
just linux-release-prefetch
just linux-release-smoke
```

The host needs Cage with wlroots' headless backend; the packaged Linux GUI
supports native Wayland only.

`linux-release-smoke`:

- builds Flutter in release/AOT mode;
- builds the BrowserCore-backed `libvixen_ffi.so` through the Flutter runner;
- creates a deterministic archive with normalized ownership and timestamps;
- extracts that exact archive into a clean directory;
- launches it under Cage's headless Wayland backend on the Linux host;
- requires survival to the bounded timeout; and
- requires `Using the Impeller rendering backend (...)` in the engine log.

Generated assets are:

```text
.tmp/release/vixen-linux-x86_64.tar.gz
.tmp/release/vixen-linux-x86_64.tar.gz.sha256
```

The archive has one top-level `vixen/` directory containing the Flutter runner,
AOT app, Flutter engine, ICU/assets, and `libvixen_ffi.so`. It excludes source,
build tools, caches, JIT snapshots, and debug payloads.

## CI and tagged releases

The `Linux release archive` CI job performs the same logical build on Ubuntu
24.04 with the mise-managed Rust and Flutter beta. It creates the archive twice
and byte-compares the outputs, extracts and launch-smokes the exact archive,
and uploads it as a workflow artifact. On a tag, the release job attaches the
archive and checksum to the GitHub Release.

A release is not ready merely because the archive exists. The framework and
engine revisions, native layout, deterministic archive, bounded launch, and
Impeller log must all pass.

## FlatPark submission

This section is intentionally inactive while the basic-browser gate is open.

The FlatPark registry entry uses `extra-data` and an immutable GitHub Release
asset URL. Its update resolver reads Vixen's latest GitHub Release and selects
`vixen-linux-x86_64.tar.gz`; FlatPark computes and reviews the new size and
SHA-256 before publishing.

The package may install wrapper, desktop, icon, and AppStream files around the
archive, but it must not patch or replace Vixen's binaries. Permissions stay at
the minimum needed for browser operation: the Wayland socket (without X11 or
fallback-X11), GPU, IPC, network, and the explicit download directory grant.
Optional broader host access is not enabled.

Before submitting an update:

1. install and launch the release archive locally;
2. cut and verify the GitHub Release;
3. update the FlatPark registry entry to that immutable asset;
4. run FlatPark's descriptor validator and `publish.sh --verify`;
5. install from the isolated test repository and exercise startup/navigation;
6. record tested and untested behavior in the pull request.

FlatPark's publishing and review requirements are authoritative:
<https://flatpark.org/contributing/>.

## Evidence boundary

FlatPark simplifies packaging and signed repository maintenance; it does not
build Vixen or prove browser correctness. GitHub Releases remain the authoritative
upstream bytes. Platform parity, portals, complete accessibility/native AT,
IME, performance, and accepted size baselines remain separately gated.
