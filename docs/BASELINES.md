# Measurement baselines

Vixen's current baseline suite is a dependency-light Linux measurement
foundation built with Node.js built-ins. It records observations; it does not
enforce budgets or claim complete real-site behavior. The repository now has a
checked-in hello-Flutter peer plus controlled Linux release/AOT raw-bundle build
and comparison commands. Clean post-R7 exact-commit renderer and post-Yaru raw-
bundle reports plus the historical pre-R7 raw-bundle report are checked in for
reproduction. None is an accepted budget or FlatPark package baseline.

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

The suite in `fixtures/performance/headless-local.json` measures native process
startup/version and local navigation plus runtime evaluation. Rendered layout,
commit, and capture performance belongs to the Flutter GUI/chrome-less baselines.
Each scenario has its own output validation; warmups are discarded.

Measure profile growth through the release headless binary and its public
`--profile-dir` seam:

```sh
just baseline-profile-growth       # 5 repeated and 5 unique visits
just baseline-profile-growth 12
```

The command creates a temporary explicit profile under workspace-local `.tmp/`,
closes each headless process before sizing, and records checkpoints after
initialization, repeated local-file visits, unique `data:` URL visits, and a
deterministic localStorage payload. A fresh process must reopen and read the
payload before the last checkpoint. The script treats the profile as an opaque
directory and does not depend on redb files, tables, or allocation internals.

[`baselines/profile-growth-2026-07-16.json`](baselines/profile-growth-2026-07-16.json)
records the first post-R7 reproduction from clean revision `6a61897`: five
repeated local visits, five unique `data:` visits, a 65,536-byte localStorage
payload, and a fresh-process persistence read all exited successfully. The
opaque profile's 3,686,400-byte logical file size remained constant; allocated
storage was 1,622,016 bytes after initialization, grew 8,192 bytes across
repeated visits, did not grow across unique visits, and grew 139,264 bytes for
the persisted payload to 1,769,472 bytes. This single-host measurement is not a
growth budget or a broad history/cache/storage workload.

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

The accepted report below is historical GTK3 evidence from the exact pinned
Flutter 3.47.0-0.1.pre beta. The current release path uses the immutable
flutter-dev `3.47.0-1.0.pre-160` GTK4 SDK, Dart 3.14, and a digest-pinned GNOME
50 builder. A 2026-07-18 migration check measured 21,384,740 logical bytes for
the hello bundle and 85,283,280 for Vixen, a 63,898,540-byte delta with no
native plugin ELFs; it remains an unaccepted dirty-worktree candidate until a
clean revision report is checked in. `fixtures/artifact-size/flutter_hello`
uses Material plus the standard GTK4 Linux runner without Vixen code. Both
controlled runners are stripped with the same policy.

The analyzer requires release bundle structure (`libapp.so`, Flutter engine,
and ICU), requires exactly one `libvixen_ffi.so` only in Vixen, rejects debug and
build artifacts, verifies byte-identical shared Flutter engine/ICU files, and
reports every file plus component and Vixen-minus-hello logical/allocated deltas.
The native Vixen library remains an aggregate because stripped static
BrowserCore/V8 attribution needs separate linker-map evidence. The recorded
pre-R7 artifact also includes now-deleted renderer code.

## Historical post-R7/Yaru GTK3 raw-bundle reference

[`baselines/flutter-linux-x64-raw-2026-07-16.json`](baselines/flutter-linux-x64-raw-2026-07-16.json)
was produced from clean revision `4a12d26` with `just
build-flutter-size-linux` followed by the JSON analyzer used by `just
size-flutter-linux-json`. Both release/AOT builds ran in the GNOME 50 builder
container; the analyzer verifies byte-identical shared Flutter engine and ICU
files.

| Artifact | Logical bytes | Allocated bytes | Files |
|----------|--------------:|----------------:|------:|
| hello-Flutter | 21,398,668 | 21,434,368 | 12 |
| Flutter+Vixen | 85,377,960 | 85,430,272 | 27 |
| Vixen minus hello | 63,979,292 | 63,995,904 | 15 |

The current logical delta attributes 58,184,992 bytes to aggregate stripped
BrowserCore/Rust/V8 native code, 3,096,576 bytes to Dart AOT, 2,576,028 bytes to
Flutter assets, 121,624 bytes to four native plugins, and 72 bytes to the runner.
The deterministic release archive made from the same bundle is 31,913,890 bytes
with SHA-256
`3eef1bbed0e8e79dd8a85602837d4a9217dfbb82193cfcb93b62ca8730bc7879`.
The archive observation is documented separately because the raw-bundle schema
correctly leaves compressed download size null. Clean extraction and the bounded
`linux-release-smoke` Cage launch steps reported Impeller and presented a Flutter
commit from this exact archive; that is one controlled release launch, not
sustained host/GPU evidence.

Against the historical 2026-07-12 report, the Vixen bundle is 131,560 bytes
smaller overall and the aggregate native library is 2,076,976 bytes smaller
after R7 deletion, while Yaru/fonts/assets add a 2,576,026-byte delta, native
plugins add 121,624 bytes, and Dart AOT adds 638,976 bytes. These are net
component changes, not isolated causal attribution: the comparison also moves
from Flutter 3.44 to 3.47 and normalizes runner stripping. The hello control is
1,380,082 bytes smaller, so Vixen-minus-hello grows by 1,248,522 bytes even
though the Vixen bundle itself shrinks. No value is a budget, and this report
has not yet been independently reproduced.

## Historical pre-R7 Flutter raw-bundle reference

[`baselines/flutter-linux-x64-raw-2026-07-12.json`](baselines/flutter-linux-x64-raw-2026-07-12.json)
was produced from clean revision `5b1d0af` with `just
size-flutter-linux-json`. Both release/AOT builds ran in the GNOME 50 builder
container with `--network=none`; shared Flutter engine and ICU hashes match.

| Artifact | Logical bytes | Allocated bytes | Files |
|----------|--------------:|----------------:|------:|
| hello-Flutter | 22,778,750 | 22,814,720 | 12 |
| Flutter+Vixen | 85,509,520 | 85,540,864 | 13 |
| Vixen minus hello | 62,730,770 | 62,726,144 | 1 |

The historical logical delta attributes 60,261,968 bytes to the aggregate stripped
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
resources, packaging metadata, and symbols. R7 reports must verify the deleted
WebRender/EGL/frame dependencies and symbols are absent.
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

Measure the final release/AOT Flutter renderer under one Cage headless-Wayland
session in text or JSON form:

```sh
just baseline-flutter-linux
just baseline-flutter-linux-json 5 1
just baseline-flutter-linux-hardware
just baseline-flutter-linux-hardware-json 5 1
```

Every warmup and measured sample starts a fresh `vixen_shell` CDP-automation
process and profile at 320×240, loads `fixtures/dom/basic.html`, requires
Impeller and the renderer-specific pinned exact Flutter-scene PNG, and shuts
down cleanly. Version 2 then loads `fixtures/cdp/playwright-smoke.html` and
serializes eight direct attribute mutations plus one mouse release. Each
operation requests synchronous Flutter geometry and joins its exact commit,
coordinator acknowledgement, engine frame number, and `FrameTiming` raster
finish. The report records app-spawn → CDP-ready, app-spawn → first exact
presentation, capture dispatch/client latency, app-process Linux memory,
mutation/input → exact presented-commit frame endpoints, and build/raster/total
frame spans. Cage and Node are excluded; BrowserCore, V8, Flutter, and Dart AOT
remain inside the app process.

Software mode forces Mesa software rendering. Hardware mode removes that
override and fails closed unless `eglinfo -B` identifies a non-software OpenGL
ES renderer on the same Wayland display. Neither mode accepts a budget.
`FrameTiming.rasterFinishWallTime` is not compositor acceptance, scanout, or a
physical input timestamp, and Cage exposed no refresh rate in these runs, so
over-refresh-interval counts are correctly `null`.

## Recorded Flutter renderer references

[`baselines/flutter-linux-renderer-2026-07-16.json`](baselines/flutter-linux-renderer-2026-07-16.json)
and
[`baselines/flutter-linux-renderer-hardware-2026-07-16.json`](baselines/flutter-linux-renderer-hardware-2026-07-16.json)
were produced from clean revision `cddcb09` with five measured runs after one
discarded warmup. The release bundle had already been built by the integration
recipe; the recorded repetitions used the same Cage environment and direct
script invocation with `--renderer software` and `--renderer hardware`.

| Metric | Software median | Software p95 | AMD/Mesa median | AMD/Mesa p95 |
|--------|----------------:|-------------:|----------------:|-------------:|
| CDP ready | 225.223 ms | 317.256 ms | 176.528 ms | 252.007 ms |
| first exact presented commit | 324.232 ms | 412.192 ms | 258.415 ms | 330.925 ms |
| exact-scene capture dispatch | 50.317 ms | 57.148 ms | 38.360 ms | 42.243 ms |
| capture client round trip | 50.642 ms | 57.567 ms | 38.699 ms | 42.543 ms |
| app-process `VmHWM` | 300,240,896 B | 302,569,882 B | 208,400,384 B | 208,770,662 B |
| direct mutation → commit frame | 15.402 ms | 28.812 ms | 14.527 ms | 29.248 ms |
| mouse release → commit frame | 26.364 ms | 29.746 ms | 25.269 ms | 29.185 ms |
| exact-frame build span | 71 µs | 91 µs | 70 µs | 89 µs |
| exact-frame raster span | 492 µs | 561 µs | 350 µs | 442 µs |
| exact-frame total span | 2,587 µs | 3,461 µs | 2,590 µs | 3,338 µs |

Each report contains 45 measured interaction frames, five successful clean
exits, exact commit/frame identity, bounded diagnostics, artifact/fixture/lock
hashes, and renderer-specific repeated PNG hashes. The hardware probe identifies
`AMD Ryzen 7 7700X ... (radeonsi, raphael_mendocino, ACO, DRM 3.64)` with Mesa
26.0.4 and OpenGL ES 3.2. This is one integrated physical GPU/driver
reproduction, not a supported matrix. Version 2's longer interaction workload
also makes its memory samples unlike the earlier version-1 startup/capture-only
report. None of these observations is a warning or failure threshold.

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
| Flutter Linux renderer | `vixen.flutter-linux-renderer-baseline-report` version 2 |
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
environment variables. Hardware Flutter reports additionally include the
fail-closed Wayland EGL vendor/renderer/version probe. Optional metadata is null
when unavailable. A host
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
headless-path, historical native-shell artifact-size, Flutter raw-release-bundle
comparison, exact-commit Flutter startup/capture, controlled exact-frame spans,
synthetic mutation/input-to-commit endpoints, and one physical GPU/driver
reproduction. It does not yet measure:

- representative external sites or complete external-site compatibility;
- an accepted/reproduced Flutter GUI size baseline or FlatPark package artifact;
- the GUI/FlatPark path across a supported Linux, GPU, driver, and renderer matrix;
- native macOS, Windows, Android, or iOS Simulator BrowserCore/V8/Flutter-renderer behavior;
- animation cadence/smoothness, dropped vsyncs, compositor/scanout presentation,
  isolated GPU raster cost, or physical-device input-to-paint latency;
- V8/JavaScript heap usage separately from process memory;
- HTTP transfer or download throughput; or
- installed GNOME runtime size and shared-system storage attribution.

Those remain beta measurement work. Reports must keep these gaps explicit.
