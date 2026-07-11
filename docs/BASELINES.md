# Measurement baselines

Vixen's current baseline suite is a dependency-light Linux/headless measurement
foundation built with Node.js built-ins. It records observations; it does not
enforce budgets, measure a Flutter build, or claim complete real-site behavior.
The Linux Flutter debug bundle exists, but no controlled hello-Flutter versus
Flutter+Vixen release/AOT size or performance baseline has been recorded yet.

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

The suite in `fixtures/performance/headless-local.json` separately measures
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

Measure artifacts without requiring Flatpak, or build and measure the exported
Flatpak payload:

```sh
just size-headless
just size-fp
node scripts/artifact-size.mjs \
  --headless target/release/vixen-headless \
  --flatpak-payload build-aux/_build/files \
  --flatpak-bundle build-aux/vixen.flatpak \
  --json
```

`size-fp` still builds Flatpak first. Optional payload or bundle paths are
reported as absent rather than failing; the required headless binary remains a
hard error. Logical and allocated sizes deduplicate hardlinks by device and
inode. Flatpak payload and bundle numbers exclude the separately supplied GNOME
runtime.

That Flatpak contains the current GTK/Relm4 compatibility shell. Its report
remains useful historical/current evidence, but it is not a Flutter Linux
baseline and must not be relabeled as one.

## Flutter GUI baseline protocol

For every target platform and shipped ABI/architecture, produce two controlled
artifacts with the same Flutter version, build mode, runner configuration,
plugins, architecture, signing mode where practical, and package format:

1. **hello-Flutter:** the smallest representative native Flutter application;
2. **Flutter+Vixen:** the release Vixen shell plus BrowserCore.

Both use Flutter release/AOT mode, Rust release mode with strip/LTO, and native
dead-code stripping where reproducible. Record compressed download, unpacked or
installed size, native executables/libraries, assets, and separately supplied
runtime/shared-system costs. Attribute at least Flutter engine/ICU, Dart AOT and
assets, runner/plugins, BrowserCore/Rust dependencies, V8/ICU/snapshots,
WebRender/GPU dependencies, Vixen resources, packaging metadata, and symbols.
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
headless-path, and compatibility-shell artifact-size measurement foundation. It
does not yet measure:

- representative external sites or complete external-site compatibility;
- any Flutter GUI or hello-Flutter versus Flutter+Vixen artifact;
- the GUI/Flatpak path across a supported Linux, GPU, driver, and renderer matrix;
- native macOS, Windows, Android, or iOS Simulator BrowserCore/V8/WebRender behavior;
- frame time, frame stability, animation smoothness, or input-to-paint latency;
- V8/JavaScript heap usage separately from process memory;
- HTTP transfer or download throughput; or
- installed GNOME runtime size and shared-system storage attribution.

Those remain beta measurement work. Reports must keep these gaps explicit.
