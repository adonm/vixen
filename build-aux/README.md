# Build auxiliary

Flatpak manifests and distribution packaging live here
(docs/ARCHITECTURE.md, docs/DECISIONS.md).

Contents:

- `org.vixen.Vixen.json` — production Flatpak manifest. Build it against the
  GNOME 50 SDK inside the flatpak-builder container: `just flatpak-build`
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

Build artifacts (`_build/`, `_repo/`) are generated — keep them out of git.
