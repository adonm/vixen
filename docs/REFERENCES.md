# Pinned reference-browser revisions

Every implementation decision that touches CSS, DOM, JS, layout, or paint
semantics **must cite** a path in one of these trees, plus the pinned
revision below. The reference trees are large; pinning prevents
non-reproducible consultations ("latest main" drifts).

These pinned revisions are the canonical reference set for Vixen's
implementation work, captured once so every citation points at the same
tree state.

---

## Pin table (captured 2026-07-06 from branch HEADs)

| Reference      | Upstream                                                          | Pinned revision    | Branch | Used for |
|----------------|-------------------------------------------------------------------|--------------------|--------|----------|
| **Firefox**    | `https://github.com/mozilla-firefox/firefox.git`                  | `46e9f12a8f9b`     | `main` | CSS property definitions, DOM API behavior, JS/realm/rooting discipline, WebRender internals, WPT test selection. Also hosts the `servo/` Stylo subtree (see below). |
| **Servo Stylo** (under Firefox tree) | vendored at `firefox/servo/` @ `46e9f12a8f9b` | (same as Firefox) | —      | **Primary CSS reference.** Stylo (`components/style/`), selectors (`components/selectors/`), and supporting Servo crates. Current Firefox HEAD does **not** carry the old Servo script/layout crates. |
| **Ladybird**   | `https://github.com/LadybirdBrowser/ladybird.git`                 | `0de15a5dd2a9`     | `master` | **Primary layout architecture reference.** LibWeb DOM/style/layout/paint seams, TreeBuilder, formatting contexts, display-list construction. |
| **Flutter**    | `https://github.com/flutter/flutter.git`                          | `677d472756f8`     | `beta` | **Primary GUI reference.** Widget, Semantics, platform-channel, texture, Linux runner-template, and test patterns for the pinned SDK. |
| **GNOME Web (Epiphany)** | `https://gitlab.gnome.org/GNOME/epiphany.git`            | `21e02b9a272d`     | `main` | Linux browser AppStream/desktop metadata, portal behavior, and Flatpak manifest conventions. |
| **Obscura**    | `https://github.com/h4ckf0r0day/obscura.git`                      | `ca71ce3c2da9`     | `main` | Headless CLI design, CDP server patterns, single-binary distribution. |
| **Deno / deno_core** | `https://github.com/denoland/deno.git`                     | `83c50b1da61e`     | `main` | **Primary JS runtime packaging reference.** `deno_core` embedding, extension/op boundaries, bootstrap JS packaging, resource tables, permissions, and test layout. |

---

## How to consult each

### Firefox / Servo Stylo subtree (`firefox/` checkout)

The Firefox checkout is large. For Vixen, use a sparse checkout containing
the Rust-facing pieces we can cite directly plus the Firefox C++ seams that
show API contracts:

```
firefox/servo/components/style/                    ← Stylo. Read this for CSS cascade/computed values.
firefox/servo/components/selectors/                ← selector engine used by Stylo.
firefox/gfx/wr/webrender_api/src/                  ← WebRender display-list API.
firefox/gfx/webrender_bindings/                    ← Firefox ↔ WebRender transaction/builder bridge.
firefox/dom/bindings/                              ← WebIDL binding and wrapping discipline.
firefox/dom/webidl/                                ← DOM API surface contracts.
firefox/dom/base/                                  ← DOM API behavior and selector delegation.
```

Current Firefox HEAD (`46e9f12a8f9b`) does **not** include
`servo/components/layout_2020/`, `servo/components/layout/`, or
`servo/components/script/`. Do not cite those removed historical paths.
Vixen-owned layout uses Ladybird as the architecture reference per ADR-013.

When in doubt about a CSS computed value, search
`firefox/servo/components/style/properties/` for the property name —
longhands, shorthands, and computed-value logic all live there.

### Ladybird (`ladybird/`)

Use Ladybird when a question is **architectural** ("how do other engines
seam X from Y?") rather than **specification-level**. Per ADR-013, Vixen's
Rust layout engine follows Ladybird's layout architecture, not its C++
ownership model.

```
ladybird/Libraries/LibWeb/                         ← DOM, CSS, layout, paint (cleanly seamed)
ladybird/Libraries/LibWeb/CSS/                     ← cascade + stylesheet model
ladybird/Libraries/LibWeb/Layout/TreeBuilder.cpp   ← styled DOM → layout tree seam
ladybird/Libraries/LibWeb/Layout/                  ← formatting contexts
ladybird/Libraries/LibWeb/Painting/                ← display-list construction
ladybird/Libraries/LibGfx/                         ← rasteriser fallback
```

### Flutter (`flutter/`)

Consult for GUI widget, Semantics, texture, platform-channel, native-runner, and
test behavior. The mise-managed SDK is pinned to this framework revision; Vixen
still requires its own platform evidence rather than treating Flutter support as
BrowserCore support.

```text
flutter/packages/flutter/lib/                     ← widgets, services, Semantics
flutter/packages/flutter_test/                    ← widget/test harness patterns
flutter/packages/flutter_tools/templates/app/linux.tmpl/ ← Linux runner boundary
flutter/examples/                                 ← focused framework examples
```

### GNOME Web (`gnome-web/`)

Consult only for Linux browser metadata, portal expectations, and Flatpak
conventions. It is not a GUI architecture reference; Vixen renders its sole GUI
with Flutter.

```
gnome-web/data/                                    ← gschema, metainfo, desktop
gnome-web/flatpak/                                 ← runtime/portal conventions useful for the FlatPark submission
```

### Obscura (`obscura/`)

Consult for **headless tooling**: CDP server implementation, CLI flag
ergonomics, single-binary packaging for automation. Obscura is the
design source for the headless CLI surface, which Vixen inherits
verbatim.

### Deno (`deno/`)

Consult Deno for **JS runtime embedding and Rust host packaging**, per ADR-014.
The target crate is [`deno_core`](https://crates.io/crates/deno_core). Use this
tree for extension/op organization, resource-table shape, permission checks near
host boundaries, bootstrap script packaging, and feature-family test layout. Do
not cite Deno for DOM/Web API semantics over Firefox/specs; Deno is the runtime
substrate reference, while Web-facing behavior remains WPT/spec-gated.

```
deno/core/                                         ← op/extension/runtime core patterns
deno/runtime/                                      ← permissions, workers, bootstrap packaging
deno/ext/                                          ← feature-family JS/Rust extension layout
deno/cli/                                          ← integration tests and permission plumbing examples
```

---

## Re-cloning fresh

If `.tmp/ref/` is unavailable, clone each at the pinned revision:

```sh
mkdir -p .tmp/ref && cd .tmp/ref

git clone --depth 1 --filter=blob:none --sparse --branch main https://github.com/mozilla-firefox/firefox.git
git -C firefox sparse-checkout set servo gfx/wr gfx/layers/wr gfx/webrender_bindings dom/webidl dom/base dom/bindings js/public
git -C firefox checkout 46e9f12a8f9b

git clone --depth 1 --filter=blob:none --sparse --branch master https://github.com/LadybirdBrowser/ladybird.git
git -C ladybird sparse-checkout set Libraries/LibWeb Libraries/LibGfx
git -C ladybird checkout 0de15a5dd2a9

git clone --depth 1 --filter=blob:none --sparse --branch beta https://github.com/flutter/flutter.git
git -C flutter sparse-checkout set packages/flutter packages/flutter_test packages/flutter_tools/templates/app/linux.tmpl examples
git -C flutter checkout 677d472756f83c14371dd8cc624387065f3d32a7

git clone --depth 1 --filter=blob:none --sparse --branch main https://gitlab.gnome.org/GNOME/epiphany.git gnome-web
git -C gnome-web sparse-checkout set data flatpak
git -C gnome-web checkout 21e02b9a272d

git clone --depth 1 --filter=blob:none --branch main https://github.com/h4ckf0r0day/obscura.git
git -C obscura checkout ca71ce3c2da9

git clone --depth 1 --filter=blob:none --sparse --branch main https://github.com/denoland/deno.git
git -C deno sparse-checkout set core runtime ext cli
git -C deno checkout 83c50b1da61e
```

Disk budget depends on sparse settings. Keep the checkouts in `.tmp/ref/`
or another ignored workspace; avoid committing reference trees.

---

## Citation discipline

Vixen's tick-tock rules (each phase is a *tick* — capability lands; the
post-phase cleanup is a *tock* — dead-code removal, ≤ 1 kLOC modules,
reference citations):

- **Every implementation commit** cites at least one path + commit hash from
  a reference tree explaining *why* the behaviour is correct.
- **Every tock** (post-phase hardening) cites at least four reference paths.
- Commit hashes are the **short form of the pin above** (`46e9f12a8f`,
  `0de15a5dd2`, etc.), never `HEAD` or `main`.
- When a reference path goes stale, refresh the affected checkout to the
  current branch HEAD and update this file in the same change; do not leave
  implementation comments pointing at historical paths that no longer exist.
