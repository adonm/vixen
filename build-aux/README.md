# Build auxiliary

Current and target Flatpak manifests and distribution packaging live here
(docs/ARCHITECTURE.md, docs/DECISIONS.md).

The checked-in manifest builds the GTK/Relm4 Linux compatibility shell. It is
not a Flutter manifest and `just flatpak-build` is not Flutter evidence. Flutter
is not installed in this workspace.

Contents:

- `org.vixen.Vixen.json` — current compatibility-shell Flatpak manifest. Build
  it against the GNOME 50 SDK inside the flatpak-builder container:
  `just flatpak-build`
  (see [../docs/guidance/gnome-sdk-flatpak-builder.md](../docs/guidance/gnome-sdk-flatpak-builder.md)).
- `cargo-sources.json` — generated checked-source list for offline Cargo builds
  inside the Flatpak sandbox. Refresh with `just flatpak-cargo-sources` after
  `Cargo.lock` changes.
- `write-vendor-checksums.py` — emits Cargo's per-file vendor checksum metadata
  after flatpak-builder extracts checked crate archives into `cargo/vendor`.
- The manifest also checks and stages the rusty_v8 prebuilt archive referenced
  by `RUSTY_V8_ARCHIVE`, so the Cargo build itself does not fetch from GitHub.
- `org.vixen.Vixen.Devel.json` — devel manifest (TODO)
- `modules/` — vendored Flatpak modules (runtime deps as needed; TODO)
- `_build/`, `_repo/` — flatpak-builder outputs (gitignored)

ADR-014 makes `deno_core`/V8 the JS runtime; release sizing must be measured
against the current V8-backed binaries.

## Target Flutter Flatpak

The Linux product target is a pinned offline source build of Flutter plus Rust.
Pin Flutter 3.44.x and `TheAppgineer/flatpak-flutter` 0.15.0, then preprocess the
app manifest so Flutter/pub sources, `Cargo.lock` sources, the rusty_v8 source or
pinned archive, and declared foreign dependencies are available to
`flatpak-builder` without network access. The generated manifest must pass a
sandboxed offline build and remain reviewable alongside its locks and hashes.

Do not retrofit unverified Flutter claims into the current JSON manifest. Land
the template, generated manifest policy, and executable recipes with the first
Flutter Linux build. Measure hello-Flutter and Flutter+Vixen separately and
attribute components per [`../docs/FLUTTER_SHELL.md`](../docs/FLUTTER_SHELL.md).
Flutter's Linux embedder uses GTK; replacing Relm4/libadwaita/custom GLArea does
not necessarily remove GTK runtime dependencies.

Build artifacts (`_build/`, `_repo/`) are generated — keep them out of git.
