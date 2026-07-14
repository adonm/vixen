# Measurement baselines

Vixen's current baseline suite is a dependency-light Linux measurement
foundation built with Node.js built-ins. It records observations; it does not
enforce budgets or claim complete real-site behavior. The repository now has a
checked-in hello-Flutter peer plus controlled Linux release/AOT raw-bundle build
and comparison commands. No accepted Flutter report or FlatPark package
size/performance baseline has been recorded yet; one clean
measurement-only raw-bundle reference is checked in for reproduction. That
reference predates the Yaru chrome/titlebar dependency added on 2026-07-14 and
is historical until a clean post-Yaru report is reviewed.

## Commands

Build inputs are locked to `Cargo.lock`:

```sh
just build-release
```

Run the committed hermetic headless scenario suite in text or JSON form:

```sh
just baseline-headless
just baseline-headless-json 9 2  # 9 measured runs, 2 warmups per scenario
```

The suite in `fixtures/performance/headless-local.json` currently measures the
transitional native renderer's
process startup/version, local navigation plus runtime evaluation, layout-tree
output, display-list plus paint-stat output, and a temporary PNG screenshot. Each
scenario has its own output validation. Warmups are discarded; temporary outputs
are removed.

Measure profile growth through the release headless binary and its public
`--profile-dir` seam:

```sh
just baseline-profile-growth       # 5 repeated and 5 unique visits
just baseline-profile-growth 12
```

The command creates a temporary explicit profile, closes each headless process
before sizing, and records checkpoints after initialization, repeated local-file
visits, unique `data:` URL visits, and a deterministic localStorage payload. A
fresh process must reopen and read the payload before the last checkpoint. The
script treats the profile as an opaque directory and does not depend on redb
files, tables, or allocation internals.

Measure the headless binary and create the official compressed Linux archive:

```sh
just size-headless
just linux-release-archive
stat --format='%s' .tmp/release/vixen-linux-x86_64.tar.gz
```

Reports produced for the former native Flatpak remain historical GTK/Relm4
evidence and must not be relabeled. The FlatPark package needs its own reviewed
compressed/install observation after registry publication; the GitHub Release
archive does not include the separately supplied GNOME runtime.

Stage the pinned Linux Flutter/rusty_v8 inputs, then build and compare clean raw
release bundles:

```sh
just flutter-size-prefetch       # network-capable staging; never evidence
just flutter-size-check-inputs   # revision/archive/namespace checks
just size-flutter-linux          # controlled build and text report
just size-flutter-linux-json     # controlled build and JSON report
just size-flutter-linux-existing # analyze existing release bundles only
```

The recorded report below used Flutter 3.44 and remains historical evidence.
`fixtures/artifact-size/flutter_hello` now tracks the exact pinned Flutter
3.47.0-0.1.pre beta and uses Material plus the standard Linux runner without
Vixen code. The current local build uses the GNOME 50 builder image, its
CMake/Ninja/GTK toolchain, the mise Rust/Flutter toolchains, locked Cargo/Pub
dependencies, and the SHA-256-pinned rusty_v8 archive. The mutable builder-image
tag remains a limitation until the release path pins an immutable digest.
The Vixen dependency graph now also includes locked Yaru 10.2.0 and its native
window plugins; do not use the recorded pre-Yaru delta as a current size claim.

The analyzer requires release bundle structure (`libapp.so`, Flutter engine,
and ICU), requires exactly one `libvixen_ffi.so` only in Vixen, rejects debug and
build artifacts, verifies byte-identical shared Flutter engine/ICU files, and
reports every file plus component and Vixen-minus-hello logical/allocated deltas.
The native Vixen library remains an honest aggregate because stripped static
BrowserCore/V8/transitional-WebRender attribution needs separate linker-map
evidence.

## Recorded Flutter raw-bundle reference

[`baselines/flutter-linux-x64-raw-2026-07-12.json`](baselines/flutter-linux-x64-raw-2026-07-12.json)
was produced from clean revision `5b1d0af` with `just
size-flutter-linux-json`. Both release/AOT builds ran in the GNOME 50 builder
container with `--network=none`; shared Flutter engine and ICU hashes match.

| Artifact | Logical bytes | Allocated bytes | Files |
|----------|--------------:|----------------:|------:|
| hello-Flutter | 22,778,750 | 22,814,720 | 12 |
| Flutter+Vixen | 85,509,520 | 85,540,864 | 13 |
| Vixen minus hello | 62,730,770 | 62,726,144 | 1 |

The logical delta attributes 60,261,968 bytes to the aggregate stripped
`libvixen_ffi.so`, 2,457,600 bytes to Dart AOT, 11,200 bytes to the native
runner, and 2 bytes to Flutter assets. These observations are not a budget and
have not yet been independently reproduced. Compressed download, installation,
Flatpak payload/runtime, symbols, and static native subcomponents remain null or
unattributed as recorded in the report.

## Flutter renderer baseline protocol

For every target platform and shipped ABI/architecture, produce three controlled
artifacts with the same Flutter version, build mode, runner configuration,
plugins, architecture, signing mode where practical, and package format:

1. **hello-Flutter:** the smallest representative native Flutter application;
2. **Flutter+Vixen GUI:** release renderer/chrome plus BrowserCore; and
3. **chrome-less rendered host:** the same formatter/commit path without chrome.

Both use Flutter release/AOT mode, Rust release mode with strip/LTO, and native
dead-code stripping where reproducible. Record compressed download, unpacked or
installed size, native executables/libraries, assets, and separately supplied
runtime/shared-system costs. Attribute at least Flutter engine/ICU, Dart AOT
formatter/assets, runner/plugins, BrowserCore/Rust, V8/ICU/snapshots, Vixen
resources, packaging metadata, symbols, and any transitional WebRender/EGL/frame
cost still present. R7 reports must show those transitional costs removed.
Report both the hello-Flutter delta and the delta from the prior accepted Vixen
artifact.

Each report names platform, OS/toolchain, ABI/architecture, Flutter/Dart/Rust/V8
and lock/source revisions, exact command, clean revision, hashes, AOT/strip/LTO
settings, package split strategy, and exclusions. Android reports each split ABI
rather than hiding duplication in a universal package. macOS reports universal
and per-architecture attribution when both are distributed.

GUI bundles are inspected for accidental debug Flutter engines, symbols,
duplicate ABIs, development snapshots, test data, headless/CDP/WPT executables,
source archives, build tools, and caches. Required symbols are stored separately.
Warnings may be proposed only after representative reports are reproduced. A
hard budget follows only after warnings establish normal variance, component
ownership, comparison statistics, platform/ABI scope, and an explicit override
policy in `ACCEPTANCE.md`. There is no accepted numeric Flutter budget today.

Run the complete hermetic local batch with:

```sh
just baseline-beta
```

This runs the headless scenarios, profile growth, and headless artifact size. It
is intentionally not part of `gate-push`.

The underlying scripts accept `--help`. Paths relative to the workspace are
resolved from the repository rather than the caller's current directory where
practical.

## Report schemas

JSON reports are versioned independently:

| Report | Schema |
|--------|--------|
| Headless scenarios | `vixen.headless-baseline-report` version 1 |
| Profile growth | `vixen.profile-growth-baseline-report` version 1 |
| Artifact size | `vixen.artifact-size-report` version 1 |
| Flutter Linux raw bundles | `vixen.flutter-linux-artifact-size-report` version 1 |
| Scenario input | `vixen.headless-scenario-suite` version 1 |

Every report says `measurement_only: true`. Headless scenario reports include
per-scenario wall-time samples and min/median/p95/max/mean summaries, sampled
`VmHWM`/`VmRSS`/`VmSize` peaks where Linux exposes them, exit status, and bounded
stdout/stderr byte counts. Profile reports include logical and allocated bytes,
growth from the preceding checkpoint, file counts, process samples, and the
storage reopen result. Artifact reports include logical and allocated bytes,
file counts, SHA-256, presence state, and the runtime-exclusion method.

`VmHWM` is the kernel high-water resident set for the measured process. `VmRSS`
and `VmSize` are maxima observed by polling `/proc/<pid>/status`; very short
processes can exit before a field is sampled, in which case the field is null.
These values do not include separate descendant processes.

Artifact SHA-256 is the file digest for files. For directories it is a stable
manifest digest over sorted relative paths, entry types, sizes, symlink targets,
and file digests; it is not a Flatpak/Ostree commit checksum.

## Host fingerprint

Reports include the binary and `Cargo.lock` hashes where applicable, git revision
and dirty state, Node/rustc/Cargo versions, kernel and distro, architecture, CPU
model and logical CPU count, total host memory, page size, and renderer-related
environment variables. Optional metadata is null when unavailable. A host
fingerprint supports comparison; it does not make unlike hosts equivalent.

## Controlled and live inputs

`fixtures/realworld/` contains small static-document, form-workflow, and app-shell
controls. They are deterministic, committed, site-shaped inputs used to exercise
production paths. They are not captures of external sites and do not establish a
real-site compatibility corridor. The headless suite performs no external
network access. Live-site findings still require named URLs, dates, host details,
and preferably reduced local or pinned WPT cases.

## Accepted reports

No numerical report or regression budget is currently accepted. To accept one,
a maintainer must publish the complete JSON report, exact command, clean git
revision, artifact hashes, supported host class, run/warmup counts, and relevant
renderer environment. The report must be reproduced on the declared host class
and reviewed for workload validity and noise before a warning or failure
threshold is proposed. Thresholds belong in `ACCEPTANCE.md` and must state their
comparison statistic and product override policy. A convenient sample, an
unreviewed CI run, or a value copied from another dependency graph is not a
budget.

## Current limits

This batch completes the local latency, Linux process-memory, profile-growth,
headless-path, historical native-shell artifact-size, and Flutter raw-release-
bundle comparison foundations. It does not yet measure:

- representative external sites or complete external-site compatibility;
- an accepted/reproduced Flutter GUI size baseline or FlatPark package artifact;
- the GUI/FlatPark path across a supported Linux, GPU, driver, and renderer matrix;
- native macOS, Windows, Android, or iOS Simulator BrowserCore/V8/Flutter-renderer behavior;
- frame time, frame stability, animation smoothness, or input-to-paint latency;
- V8/JavaScript heap usage separately from process memory;
- HTTP transfer or download throughput; or
- installed GNOME runtime size and shared-system storage attribution.

Those remain beta measurement work. Reports must keep these gaps explicit.
