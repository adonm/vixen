# Building against the GNOME SDK via flatpak-builder containers

**Compatibility-baseline workflow:** The GNOME SDK is not installed on the host.
The current GTK/Relm4 shell uses `org.gnome.Sdk//50` and
`org.gnome.Platform//50`, managed inside a `flatpak-builder` container image
rather than the host package manager. This keeps host pollution at zero and
makes that existing build reproducible: the SDK version is pinned by the image
tag.

ADR-018 supersedes the GNOME/Relm4 product direction. Flutter is the target GUI
on five platforms, but is not installed and has no build here. The target Linux
Flatpak will use pinned `flatpak-flutter` 0.15.0 preprocessing for an offline
Flutter+Rust source build. Until that workflow lands and reaches parity, this
page documents the executable compatibility shell only.

> Verified against `ghcr.io/flathub-infra/flatpak-github-actions:gnome-50`
> (flatpak 1.18.1, flatpak-builder 1.4.9, `org.gnome.Sdk//50` +
> `org.gnome.Platform//50` preinstalled, x86_64). The same workflow works in
> CI (this is the image Flathub's GitHub Action uses).

---

## Why a container?

- **Zero host churn.** No `libgtk-4-dev` / `libadwaita-1-dev` to install,
  version-skew, or distro-specific packages. `mise bootstrap` no longer
  installs GNOME packages (`.mise.toml`).
- **Reproducible.** The image tag *is* the SDK version. `gnome-50` today;
  bump the tag to move the SDK.
- **Matches the current compatibility build.** The checked-in Flatpak is built with
  `flatpak-builder` against this exact runtime, so dev and release go
  through the same SDK.

The image is **purpose-built for `flatpak-builder`**. It does *not* carry
`cargo`, `rustc`, or gtk4 at the container root — those live in the GNOME
SDK runtime and are consumed by `flatpak-builder` when it builds the app
inside the Flatpak sandbox. (For Rust, the manifest pulls the
`org.freedesktop.Sdk.Extension.rust-stable` SDK extension.) So the workflow
is "build a Flatpak", not "cargo-build against container-host gtk4".

---

## Prerequisites

You need a container runtime on the host — `podman` (preferred, rootless)
or `docker`. Neither needs the GNOME SDK installed.

```sh
podman --version    # or: docker --version
```

The recipes below live in the `justfile` and are run through `just`
(itself mise-managed — see the repo README). Container image and runtime
version are pinned as variables:

```
FLATPAK_BUILDER_IMAGE = "ghcr.io/flathub-infra/flatpak-github-actions:gnome-50"
GNOME_RUNTIME_VERSION = "50"
```

---

## First-time setup: pull the image (= install the GNOME 50 SDK)

```sh
just flatpak-update-sdk
# equivalent to: podman pull ghcr.io/flathub-infra/flatpak-github-actions:gnome-50
```

The image is ~5.8 GB (it carries the GNOME 50 SDK + Platform runtimes
preinstalled). Pull once; subsequent runs reuse it.

To **move the SDK** to a new GNOME version: bump `FLATPAK_BUILDER_IMAGE`
in the `justfile` (e.g. `gnome-51`), bump `GNOME_RUNTIME_VERSION`, update
`runtime-version` in the manifest (`build-aux/org.vixen.Vixen.json`), then
`just flatpak-update-sdk`.

---

## Interactive shell in the SDK container

Drop into a shell in the container with the workspace mounted at
`/workspace`:

```sh
just flatpak-shell
# equivalent to:
#   podman run --rm -it -v $PWD:/workspace:z -w /workspace \
#     ghcr.io/flathub-infra/flatpak-github-actions:gnome-50
```

From inside the shell you can inspect the managed SDK:

```sh
flatpak --version                       # flatpak 1.18.1
flatpak-builder --version               # flatpak-builder 1.4.9
flatpak list | grep gnome               # org.gnome.Sdk//50 + org.gnome.Platform//50
```

`flatpak-builder` is on `PATH`; `cargo`/`rustc` are **not** at the
container root (see "Why a container?" above) — to compile the app you use
`flatpak-builder`, which runs the build inside the GNOME SDK sandbox.

---

## Build the Flatpak (the GNOME-SDK-backed build path)

```sh
just flatpak-build
```

This runs, inside the container:

```sh
flatpak-builder --install-deps-from=flathub --disable-rofiles-fuse --force-clean --repo=build-aux/_repo \
  build-aux/_build build-aux/org.vixen.Vixen.json
```

The recipe launches the container with `podman run --privileged` because this
host path nests flatpak-builder's `bwrap` sandbox inside rootless Podman.
`--disable-rofiles-fuse` avoids needing a container dbus/fuse setup. These are
container-only privileges; the repo remains mounted at `/workspace`.

`flatpak-builder` resolves the manifest, builds each module **inside the
`org.gnome.Sdk//50` sandbox** (where gtk4, libadwaita, and the Rust extension
live), and exports the app to `build-aux/_repo`. The build output lands in
`build-aux/_build/`. For host GUI smoke, install the exported local repo with
`just flatpak-install-local`, then run `just flatpak-run`.

> **Status:** `build-aux/org.vixen.Vixen.json` builds the GTK shell vertical
> (`cargo build --release -p vixen --features vixen-shell/gtk-shell`) against
> the GNOME SDK. Cargo crate archives and the rusty_v8 static archive are
> checked Flatpak sources, so Cargo itself runs offline inside the build
> sandbox. The Rust SDK extension is installed from Flathub by
> `flatpak-builder --install-deps-from`.

### Validating the manifest without a full build

```sh
just flatpak-shell
# inside the container:
flatpak-builder --show-deps build-aux/org.vixen.Vixen.json   # resolve sources
flatpak-builder --dry-run build-aux/_build build-aux/org.vixen.Vixen.json
```

---

## Where the GNOME SDK actually lives

| Layer | What | Has gtk4 / cargo? |
|-------|------|-------------------|
| Host | your distro + mise-managed rust/just | **No** GNOME SDK (by design) |
| Container root | Freedesktop-SDK base, `flatpak`, `flatpak-builder`, `meson`, `ninja` | No |
| `org.gnome.Sdk//50` runtime | gtk4, libadwaita, Pango, HarfBuzz, fontconfig, … | **Yes** (consumed by flatpak-builder) |
| `org.freedesktop.Sdk.Extension.rust-stable` | cargo/rustc for the build | Pulled by the manifest |

So "managing the GNOME SDK" = **managing the container image tag** (which
pins the preinstalled `org.gnome.Sdk//50` runtime). Updating the SDK is
`just flatpak-update-sdk` with a bumped tag.

---

## Troubleshooting

- **`flatpak-builder` reports the runtime or Rust SDK extension is missing.**
  Use `just flatpak-build`; it passes `--install-deps-from=flathub` so
  flatpak-builder can install/update `org.gnome.Sdk//50`,
  `org.gnome.Platform//50`, and `org.freedesktop.Sdk.Extension.rust-stable`.
- **Permission denied on `/workspace`.** The `:z` mount flag relabels for
  SELinux (Fedora). On non-SELinux hosts it's harmless. With `docker`,
  swap `podman` for `docker` in the `justfile` if you prefer.
- **`bwrap: Can't mount proc on /newroot/proc`.** Run through
  `just flatpak-build`; the recipe uses `podman run --privileged` for nested
  flatpak-builder sandboxing.
- **`Failure spawning rofiles-fuse` / missing machine-id.** Run through
  `just flatpak-build`; the recipe passes `--disable-rofiles-fuse`.
- **"No cargo in the container."** Expected — cargo is provided to the
  build via the Rust SDK extension inside the manifest, not at the
  container root. Use `just flatpak-build`, not `cargo build` in the shell.
- **Host-side `cargo build --features vixen-shell/gtk-shell` fails.**
  Expected on a clean host — there is no GNOME SDK installed natively.
  Either use the container, or install your distro's `gtk4-devel` /
  `libadwaita-devel` yourself for ad-hoc native work (not the supported
  compatibility-shell path).

---

## Reference

- Image source: [`flathub-infra/actions-images`](https://github.com/flathub-infra/actions-images)
  (generates `ghcr.io/flathub-infra/flatpak-github-actions:<runtime>`).
- Flatpak building docs: <https://docs.flatpak.org/en/latest/building.html>
- Runtime/SDK concepts:
  [Available runtimes](https://docs.flatpak.org/en/latest/available-runtimes.html).
- ADR-018 and `docs/FLUTTER_SHELL.md` for target direction;
  `docs/ARCHITECTURE.md` "Crates and responsibilities" for the transitional
  boundary; `build-aux/` and `.mise.toml` for the current workflow.
