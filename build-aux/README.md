# Build auxiliary

Flatpak manifests and distribution packaging live here
(docs/ARCHITECTURE.md, docs/DECISIONS.md ADR-005).

Contents:

- `org.vixen.Vixen.json` — production Flatpak manifest (**scaffolding**: the
  structure is correct but the shell is not wired yet; full release build +
  Cargo vendoring land at Phase 9). Build it against the GNOME 50 SDK inside
  the flatpak-builder container: `just flatpak-build`
  (see [../docs/guidance/gnome-sdk-flatpak-builder.md](../docs/guidance/gnome-sdk-flatpak-builder.md)).
- `org.vixen.Vixen.Devel.json` — devel manifest (TODO)
- `modules/` — vendored Flatpak modules (mozjs per ADR-005; TODO)
- `_build/`, `_repo/` — flatpak-builder outputs (gitignored)

Per ADR-005, the production Flatpak links a shared libmozjs module vendored
into the manifest; devel/CI builds use static mozjs.

Build artifacts (`_build/`, `_repo/`) are generated — keep them out of git.

