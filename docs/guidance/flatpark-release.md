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

- Flutter `3.47.0-1.0.pre-160` from the checksum-pinned flutter-dev Linux x64
  archive declared in `.mise.toml`;
- Flutter revision `328b829d35a3a5d7a00e0c2f0e97eb8cc0d97188`;
- engine revision `fc1ad955f16467c959e3cd8079b760d5af0984aa` and immutable
  engine content hash `469f2b34de41cab5f677ba84d6e9099c0e682d1e`;
- SDK archive SHA-256
  `b6e95c97348bebd1f129db1f1cbfb7a4a8f6481839ebe80d3eb746e102336bb9`;
- GNOME 50 Linux builder image digest
  `sha256:a2b78890f165cd5b5c6a8629c5f6cb293e64d1bf523ca6662fac8ca8e247f8b0`;
- Rust `1.96.1`;
- `Cargo.lock` and `flutter/vixen_shell/pubspec.lock`; and
- rusty_v8 `v149.4.0` archive SHA-256
  `aa30f198b6e7be2188df6498f95053c4c052f212037a01f2c31414d7aca84b53`.

The Linux runner explicitly enables Impeller and links
`libflutter_linux_gtk4.so`; archive validation rejects GTK3 and checks the exact
engine library hash. A version string alone is not runtime evidence.

## Local build and smoke

Install the mise tools, pull the GNOME builder image, stage locked inputs, then
build the exact release archive:

```sh
mise install
just docker-builder-pull
just linux-release-prefetch
just linux-release-smoke
```

The host needs Docker plus Cage with wlroots' headless backend; the packaged
Linux GUI supports native Wayland only. The script pulls/inspects/runs the
existing image and does not build a custom image. Prefetch is network-capable;
the release build runs with `--network=none` from workspace-local caches.

`linux-release-smoke`:

- builds Flutter in release/AOT mode;
- builds the BrowserCore-backed `libvixen_ffi.so` through the Flutter runner;
- verifies GTK4 linkage, absence of GTK3 linkage, and the immutable GTK4 engine
  hash;
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

The `Linux release archive` CI job invokes the same digest-pinned, offline
Docker build recipe as local development. Ubuntu 24.04 owns only the host smoke
environment. CI creates the archive twice and byte-compares the outputs,
extracts and launch-smokes the exact archive, and uploads it as a workflow
artifact. On a tag, the release job attaches the archive and checksum to the
GitHub Release.

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
