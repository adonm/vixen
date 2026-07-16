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

The current report uses the exact pinned Flutter 3.47.0-0.1.pre beta.
`fixtures/artifact-size/flutter_hello` uses Material plus the standard Linux
runner without Vixen code. The local build uses the GNOME 50 builder image, its
CMake/Ninja/GTK toolchain, the mise Rust/Flutter toolchains, locked Cargo/Pub
dependencies, and the SHA-256-pinned rusty_v8 archive. The mutable builder-image
tag remains a limitation until the release path pins an immutable digest.
The Vixen dependency graph now also includes locked Yaru 10.2.0 and its native
window plugins.
Both controlled runners and any bundled plugin ELFs are stripped with the same
policy before comparison.

The analyzer requires release bundle structure (`libapp.so`, Flutter engine,
and ICU), requires exactly one `libvixen_ffi.so` only in Vixen, rejects debug and
build artifacts, verifies byte-identical shared Flutter engine/ICU files, and
reports every file plus component and Vixen-minus-hello logical/allocated deltas.
The native Vixen library remains an aggregate because stripped static
BrowserCore/V8 attribution needs separate linker-map evidence. The recorded
pre-R7 artifact also includes now-deleted renderer code.

## Recorded post-R7/Yaru Flutter raw-bundle reference

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
just baseline-flutter-linux-json 9 2
```

Every warmup and measured sample starts a fresh `vixen_shell` CDP-automation
process and profile at 320×240, loads `fixtures/dom/basic.html`, requires
Impeller and the pinned exact Flutter-scene PNG, and shuts down cleanly. The
report records app-spawn → CDP-ready, app-spawn → the first exact presented
commit induced by the controlled capture, the Rust monotonic
`Page.captureScreenshot` dispatch, client round-trip capture latency, and the
app process's Linux `VmHWM`/sampled `VmRSS`/`VmSize`. Cage and the Node client are
excluded; BrowserCore, V8, Flutter, and Dart AOT all remain inside the measured
app process. This is a controlled software-renderer observation, not a physical
GPU matrix or accepted budget.

## Recorded Flutter renderer reference

[`baselines/flutter-linux-renderer-2026-07-16.json`](baselines/flutter-linux-renderer-2026-07-16.json)
was produced from clean revision `e18550a` with five measured runs after one
discarded warmup. The release bundle had already been built by `just
baseline-flutter-linux-json 1 0`; the recorded repetition used the same recipe's
Cage environment and direct script invocation:

```sh
XDG_RUNTIME_DIR="$PWD/.tmp/flutter-baseline-wayland" \
GDK_BACKEND=wayland WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
WLR_RENDERER=gles2 LIBGL_ALWAYS_SOFTWARE=1 cage -- \
mise x node@24 -- node scripts/flutter-linux-baseline.mjs \
  --runs 5 --warmups 1 --json
```

| Metric | Median | p95 |
|--------|-------:|----:|
| CDP ready | 204.399 ms | 207.986 ms |
| first exact presented commit | 281.438 ms | 306.990 ms |
| exact-scene capture dispatch | 45.688 ms | 57.314 ms |
| capture client round trip | 46.087 ms | 57.690 ms |
| app-process `VmHWM` | 281,800,704 bytes | 281,935,053 bytes |

All five samples produced the pinned 7,721-byte PNG with SHA-256
`34ff6e88553c9396587d64131f48fa8e7d4579eccdc252398aa20d54472a42fb`
and exited cleanly. The report names the artifact/lock/fixture hashes and host.
Its memory includes BrowserCore/V8 and Flutter/Dart in one process; its capture
duration is the exact CDP dispatch, not isolated GPU raster or frame-stability
evidence. The sample uses Mesa software rendering and has not been independently
reproduced, so these observations are not warning or failure thresholds.

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
| Flutter Linux renderer | `vixen.flutter-linux-renderer-baseline-report` version 1 |
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
headless-path, historical native-shell artifact-size, Flutter raw-release-bundle
comparison, and first exact-commit Flutter startup/capture foundations. It does
not yet measure:

- representative external sites or complete external-site compatibility;
- an accepted/reproduced Flutter GUI size baseline or FlatPark package artifact;
- the GUI/FlatPark path across a supported Linux, GPU, driver, and renderer matrix;
- native macOS, Windows, Android, or iOS Simulator BrowserCore/V8/Flutter-renderer behavior;
- frame time, frame stability, animation smoothness, isolated GPU raster time,
  or input-to-paint latency;
- V8/JavaScript heap usage separately from process memory;
- HTTP transfer or download throughput; or
- installed GNOME runtime size and shared-system storage attribution.

Those remain beta measurement work. Reports must keep these gaps explicit.
