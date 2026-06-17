# Pinned reference-browser revisions

Every implementation decision that touches CSS, DOM, JS, layout, or paint
semantics **must cite** a path in one of these trees, plus the pinned
revision below. The reference trees are large; pinning prevents
non-reproducible consultations ("latest main" drifts).

These pinned revisions are the canonical reference set for Vixen's
implementation work, captured once so every citation points at the same
tree state.

---

## Pin table (captured 2026-06-17)

| Reference      | Upstream                                                          | Pinned revision    | Branch | Used for |
|----------------|-------------------------------------------------------------------|--------------------|--------|----------|
| **Firefox**    | `https://github.com/mozilla-firefox/firefox.git`                  | `1d85bc4044b2`     | `main` | CSS property definitions, DOM API behavior, layout internals, WPT test selection. Also hosts the `servo/` subtree (see below). |
| **Servo** (under Firefox tree) | vendored at `firefox/servo/` @ `1d85bc4044b2`        | (same as Firefox)  | —      | **Primary rewrite reference.** Stylo (`components/style/`), `mozjs` host binding patterns (`components/script/bindings/`), layout crate (`components/layout_2020/` or `components/layout/`), WebRender consumer patterns. |
| **Ladybird**   | `https://github.com/LadybirdBrowser/ladybird.git`                 | `347ac79e7b7c`     | `master` | Engine subsystem seams (LibWeb/LibGfx), readable C++ implementation slices for sanity-checking Rust ports. |
| **GNOME Web (Epiphany)** | `https://gitlab.gnome.org/GNOME/epiphany.git`            | `cb66369c9ae3`     | `main` | GTK4/libadwaita shell patterns, WebKitGTK embedding, GSettings usage, Flatpak manifest conventions. |
| **Obscura**    | `https://github.com/h4ckf0r0day/obscura.git`                      | `cd889d56596d`     | `main` | Headless CLI design, CDP server patterns, single-binary distribution. |
| **Relm4**      | `https://github.com/Relm4/relm4.git`                              | `7b8251cbc109`     | `main` | Relm4 component patterns, factory widgets, async actions. The `examples/` (45) and `relm4-components/` directories are the primary value. |

---

## How to consult each

### Firefox / Servo subtree (`firefox/` checkout)

The Firefox checkout is large (~4 GB). Only the `servo/` subtree is
relevant to the rewrite — Firefox's own `dom/`, `layout/`, and `gfx/` are
the C++ originals; the Rust implementations under `servo/` are what we
actually depend on.

```
firefox/servo/components/style/                    ← Stylo. Read this for CSS.
firefox/servo/components/script/                   ← DOM. Big; consult for patterns, don't depend on the crate directly.
firefox/servo/components/script/bindings/          ← mozjs host-binding codegen patterns. Read before writing any new ClassBuilder wrapper.
firefox/servo/components/layout_2020/              ← modern layout crate (preferred over components/layout/).
firefox/servo/components/layout/                   ← legacy layout crate; richer feature coverage if layout_2020 is too sparse.
firefox/servo/components/gfx/                      ← font + image helper types.
firefox/servo/components/net/                      ← network trait shapes (we keep our own reqwest stack).
firefox/servo/ports/script_bindings/               ← additional mozjs glue.
```

When in doubt about a CSS computed value, search
`firefox/servo/components/style/properties/` for the property name —
longhands, shorthands, and computed-value logic all live there.

### Ladybird (`ladybird/`)

Use Ladybird when a question is **architectural** ("how do other engines
seam X from Y?") rather than **specification-level**. Its C++ is unusually
readable for a browser engine.

```
ladybird/Libraries/LibWeb/                         ← DOM, CSS, layout, paint (cleanly seamed)
ladybird/Libraries/LibWeb/CSS/                     ← cascade + stylesheet model
ladybird/Libraries/LibWeb/Layout/                  ← formatting contexts
ladybird/Libraries/LibWeb/Painting/                ← display-list construction
ladybird/Libraries/LibGfx/                         ← rasteriser fallback
```

### GNOME Web (`gnome-web/`)

Consult for **shell-side** questions: how to embed a webview in libadwaita,
how to structure preferences, how to write the Flatpak manifest, how to
manage profile data per app-ID. This is the closest production analog to
what Vixen wants to be at the shell layer.

```
gnome-web/src/                                     ← shell source
gnome-web/data/                                    ← gschema, metainfo, desktop
gnome-web/flatpak/                                 ← manifest conventions (we keep our own in build-aux/)
```

### Obscura (`obscura/`)

Consult for **headless tooling**: CDP server implementation, CLI flag
ergonomics, single-binary packaging for automation. Obscura is the
design source for the headless CLI surface, which Vixen inherits
verbatim.

### Relm4 (`relm4/`)

Consult before writing any new shell widget. The `examples/` directory is
curated and the `relm4-components/` directory has reusable widgets
(`relm4-components::alert`, `::simple_adw_combo_box`, etc.).

```
relm4/examples/                                    ← 45 component-pattern examples
relm4/relm4-components/                            ← reusable widgets
relm4/relm4/src/                                   ← factory, actions, message passing
```

---

## Re-cloning fresh

If the `reference-browsers/` directory is unavailable, clone each at the
pinned revision:

```sh
mkdir -p reference-browsers && cd reference-browsers

git clone https://github.com/mozilla-firefox/firefox.git
git -C firefox checkout 1d85bc4044b2

git clone https://github.com/LadybirdBrowser/ladybird.git
git -C ladybird checkout 347ac79e7b7c

git clone https://gitlab.gnome.org/GNOME/epiphany.git gnome-web
git -C gnome-web checkout cb66369c9ae3

git clone https://github.com/h4ckf0r0day/obscura.git
git -C obscura checkout cd889d56596d

git clone https://github.com/Relm4/relm4.git
git -C relm4 checkout 7b8251cbc109
```

Disk budget: ~4 GB for Firefox (only `firefox/servo/` is needed; the rest
can be `rm -rf`'d after checkout to reclaim space), ~600 MB Ladybird,
~150 MB GNOME Web, ~30 MB each for Obscura and Relm4.

---

## Citation discipline

Vixen's tick-tock rules (each phase is a *tick* — capability lands; the
post-phase cleanup is a *tock* — dead-code removal, ≤ 1 kLOC modules,
reference citations):

- **Every implementation commit** cites at least one path + commit hash from
  a reference tree explaining *why* the behaviour is correct.
- **Every tock** (post-phase hardening) cites at least four reference paths.
- Commit hashes are the **short form of the pin above** (`1d85bc4044`,
  `347ac79e7b`, etc.), never `HEAD` or `main`.
