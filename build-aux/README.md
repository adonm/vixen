# Build auxiliary

Flatpak manifests and distribution packaging live here
(docs/ARCHITECTURE.md, docs/DECISIONS.md).

Contents:

- `org.vixen.Vixen.json` — production Flatpak manifest (**scaffolding**: the
  structure is correct but the shell is not wired yet; full release build +
  Cargo vendoring land at Phase 9). Build it against the GNOME 50 SDK inside
  the flatpak-builder container: `just flatpak-build`
  (see [../docs/guidance/gnome-sdk-flatpak-builder.md](../docs/guidance/gnome-sdk-flatpak-builder.md)).
- `org.vixen.Vixen.Devel.json` — devel manifest (TODO)
- `modules/` — vendored Flatpak modules (runtime deps as needed; TODO)
- `_build/`, `_repo/` — flatpak-builder outputs (gitignored)

ADR-014 makes `deno_core`/V8 the JS runtime; release sizing must be measured
against the current V8-backed binaries.

Build artifacts (`_build/`, `_repo/`) are generated — keep them out of git.
