# Acquiring SpiderMonkey (`mozjs`)

Short answer: **we don't build SpiderMonkey ourselves.** The `mozjs` crate
fetches a prebuilt static library by default. This doc records exactly how
that works and how to override it (offline, mirror, or the production
Flatpak shared-lib path).

> Verified against `mozjs_sys v140.11.0-1` / `mozjs v0.16.3`. The crate
> logged, on a clean build:
>
> ```
> [mozjs_sys] Trying to download prebuilt mozjs static library from Github Releases
> [mozjs_sys] Successfully downloaded mozjs archive in 749 ms
> ```

---

## The default (do nothing)

`mozjs_sys`'s build script, before it ever considers compiling SpiderMonkey,
tries to **download a prebuilt `libmozjs-<target><features>.tar.gz`** from
the `servo/mozjs` GitHub Releases
(`https://github.com/servo/mozjs/releases/download/mozjs-sys-v<version>/…`).
For `x86_64-unknown-linux-gnu` with the default `jit,intl,libz-sys`
features, a prebuilt exists and is fetched in well under a second. Only if
the download fails does it fall back to a from-source SpiderMonkey build.

So adding `mozjs` to `vixen-engine` does **not** compile SpiderMonkey — it
downloads it. The notable first-build cost (~tens of seconds) is the Rust
dependency tree (ICU, etc.) and linking the large static lib, not
SpiderMonkey compilation. `ccache` (if present) further speeds any fallback
source build.

> Note: the GNOME/Flatpak SDK **does not** ship mozjs. "Use mozjs from
> Flatpak" therefore means building mozjs once as a Flatpak *module* (see
> below), not pulling it from the runtime.

## Overriding the source

`mozjs_sys` honors three environment variables (see its `build.rs`):

| Variable | Effect |
|----------|--------|
| `MOZJS_ARCHIVE=<url-or-path>` | Use a specific prebuilt archive. A URL is treated as a GitHub-Releases-style base (`<base>/download/mozjs-sys-v<ver>/<archive>`); a path is used directly. **No SpiderMonkey build.** |
| `MOZJS_CREATE_ARCHIVE=1` | Build SpiderMonkey from source *and* emit `libmozjs-<target><features>.tar.gz` into the cargo target dir (for caching/hosting). |
| `MOZJS_FROM_SOURCE=1` | Force a from-source build (no download). |

Use cases:
- **Offline / air-gapped / reproducible:** `MOZJS_ARCHIVE=/path/to/libmozjs-x86_64-unknown-linux-gnu.tar.gz`.
- **Self-hosted mirror:** `MOZJS_ARCHIVE=https://your-mirror.example/servo-mozjs/`.
- **Build once, reuse across `cargo clean` / CI:** build with
  `MOZJS_CREATE_ARCHIVE=1`, cache the resulting tarball, then point
  `MOZJS_ARCHIVE` at it on every subsequent build.

The archive is keyed by Rust target **and** feature set, so match the
features you depend on (Vixen uses the mozjs defaults).

## Production Flatpak (ADR-005)

ADR-005 splits the strategy:

- **Dev / CI**: static mozjs via the crate, downloaded as above (default) or
  via a cached `MOZJS_ARCHIVE`. No system dependency.
- **Production Flatpak** (`org.vixen.Vixen`): build mozjs once as a Flatpak
  *module* (`build-aux/modules/mozjs.json`, to land at Phase 9) and link the
  app against the shared `libmozjs-140.so`. flatpak-builder caches the
  module in `.flatpak-builder/`, so it is built once and reused — neither
  the app nor `cargo` rebuilds it.

The shared-lib link saves ~3–5 MiB on the production binary (ADR-005). It is
a Phase 9 (release-hardening) concern; until then dev builds use the static
download.

## References

- `mozjs_sys` build script: `download_archive` / `compress_static_lib`
  (`MOZJS_ARCHIVE`, `MOZJS_CREATE_ARCHIVE`, `MOZJS_FROM_SOURCE`).
- Prebuilt releases: <https://github.com/servo/mozjs/releases>
- ADR-005 (System mozjs by default, static fallback for distribution).
