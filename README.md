# Vixen

A small, GNOME-native web browser built in Rust on Firefox-family
components. Targets Firefox-grade compatibility with the smallest credible
binary.

The hard, spec-heavy, easy-to-get-wrong subsystems (CSS cascade, HTML
parsing, JS, selector matching, GPU paint) are delegated to the same
Mozilla crates Firefox and Servo use — **Stylo** for CSS, **SpiderMonkey**
for JS, **WebRender** for paint, **html5ever** for HTML. Vixen itself is
the product glue: a libadwaita shell, a networking/security layer, a
persistence layer, headless tooling, and the integration code that wires
the upstream crates together.

---

## Status

Pre-v1.0. This repository contains the specification, architecture, plan,
and reference material, plus:
- **Phase 0** — scaffolding (workspace + 7 crates).
- **Phase 1** — networking/security "crown jewels" (`vixen-net`, `vixen-store`).
- **Phase 2** — the SpiderMonkey runtime (`vixen-engine::script`) and the
  `vixen-headless` CLI; the gate `vixen-headless --url <file> --eval '1+2'` →
  `3` passes.
- **Phase 3 (in progress)** — HTML parsing (`vixen-engine::doc`,
  html5ever → RcDom) with `--dump-dom`/`--extract-text`; **selector matching
  via Stylo** (`vixen-engine::style_dom` implementing `selectors::Element` over
  the RcDom), driving `--extract-selector` and the WPT selector fixtures;
  the shared `vixen-engine::page::Page` facade; inline `style` declarations now
  project through `Page::computed_style(node_id)` and WPT `computed-style`
  checks; and the **WPT harness** (`vixen-wpt`: manifest + runner + all 13
  check types). The full Stylo cascade (`TNode`/`TElement`/`TDocument` +
  `Stylist::update_stylist` + `computed_values_for(node_id)`) is the next
  slice; Stylo arrives via the crates.io-published `stylo` crate per
  ADR-011 (no Servo git dep).
- **Phase 4 prep** — `vixen-engine::box_model` implements the CSS2 § 10.3.3
  block-level horizontal-constraint solve (`auto`-width leftover absorption,
  one/two `auto`-margin centering, `box-sizing: border-box` content
  subtraction) and the four-box nesting. `vixen-engine::flex_resolve`
  implements CSS Flexbox 1 § 9.7 main-axis distribution (grow/shrink factor
  selection, inflexible-item freezing, min/max violation clamping, iterative
  free-space distribution).   `vixen-engine::grid_resolve` implements CSS Grid
  1 § 12.5 fr-factor distribution + § 11.7 track maximization (the natural
  complement to `flex_resolve` for grid columns/rows, with the iterative
  growth-limit clamp-and-redistribute pattern). `vixen-engine::writing_modes`
  implements CSS Writing Modes 3 § 3 + CSS Logical Properties 1 — the
  `writing-mode` / `direction` → block + inline axis + the logical → physical
  side mapping (`block/inline-start/end` → `top/right/bottom/left`, the
  `inline-size`/`block-size` → `width`/`height` swap for vertical modes) the
  box model, the logical insets, and the flex/grid main-axis selection
  resolve against. `vixen-engine::multicol` implements CSS Multi-column
  Layout 1 § 3 — the `column-width` / `column-count` / `column-gap` § 3.4
  resolution (the four-branch pseudo-algorithm + the single-column overflow
  clamp) the column-row distribution reduces to. `vixen-engine::scroll_snap`
  implements CSS Scroll Snap 1 § 5 — the snap-position computation
  (`start`/`end`/`center` per axis, clamped to the scrollable range) + the
  `scroll-snap-type` axis/strictness + `scroll-snap-align`/`scroll-snap-stop`
  model the scroll layer's snap targeting reduces to. All six ready for
  `layout_2020` to feed off. The first vertical layout surface is live:
  `vixen-engine::line_layout` + `Page::dump_lines` power
  `vixen-headless --dump-lines` with deterministic body-text line boxes until
  the full positioned box tree replaces the text-width estimate.
- **Phase 5 prep** — `vixen-engine::display_list` (all eight `SPEC.md`
  display-list invariants) + the paint-geometry family it will consume:
  `vixen-engine::transform` (CSS Transforms 1 § 13 2D affine algebra +
  list parser), `vixen-engine::border_radius` (CSS Backgrounds 3 § 5.5
  corner shaping), `vixen-engine::gradient` (CSS Images 4 § 4.5
  linear-gradient colour-stop resolution + linear-sRGB sampling, with the
  `repeating-linear-gradient()` wrap), `vixen-engine::radial_gradient`
  (CSS Images 4 § 4.2.3–§ 4.2.4 `radial-gradient` colour sampling — the
  four size keywords `closest-side`/`farthest-side`/`closest-corner`/
  `farthest-corner` + the circle/ellipse distance projection), and
  `vixen-engine::conic_gradient` (CSS Images 4 § 4.3.3 `conic-gradient`
  colour sampling — the per-pixel angle → `t` projection, the
  `from <angle>` start offset, and the `repeating-conic-gradient()` wrap),
  `vixen-engine::box_shadow` (CSS
  Backgrounds 3 § 7.2 outer/inset shadow geometry + the `<shadow>#`
  parser), `vixen-engine::background_position` (CSS Backgrounds 3 § 3.6 +
  § 4.2 `<position>` resolution: keyword/length/percentage mix, the 1–4
  value forms, the keyword-axis swap rule), and `vixen-engine::stacking_context`
  (CSS 2.1 § 9.9.1 + Positioned Layout 3 § 6 stacking-context formation +
  the seven-layer § App. E.2.1 paint-order classification). The paint
  compositing family: `vixen-engine::blend` (CSS Compositing 1 § 5 + § 10 —
  the 13 Porter-Duff operators + the 16 blend modes operating in linear
  sRGB, with the § 5.2 combined isolation-blend pipeline `mix-blend-mode`
  runs), `vixen-engine::filter` (CSS Filter Effects 1 § 5 — the
  `<filter-function-list>` grammar + the per-pixel `feColorMatrix`-shaped
  4×5 matrix family the paint path folds into one multiply), and
  `vixen-engine::border_image` (CSS Backgrounds 3 § 6 — the four longhands
  `border-image-slice`/`-width`/`-outset`/`-repeat`, the 3×3 nine-region
  carving, and the `stretch`/`repeat`/`round`/`space` edge tiling). The
  clip-path + mask family: `vixen-engine::clip_path` (CSS Masking 1 § 5
  `clip-path` basic shapes — `inset`/`circle`/`ellipse`/`polygon` with the
  per-pixel point-in-shape test + the polygon nonzero/evenodd winding
  rules) + `vixen-engine::mask` (CSS Masking 1 § 6 `mask` shorthand
  per-layer model — `mask-mode`/`mask-repeat`/`mask-clip`/`mask-origin` +
  the paren-aware comma-separated layer parse). The animation timing model:
  `vixen-engine::animation` (Web Animations § 5 — the phase classification,
  the simple iteration progress + current iteration, the `direction`-aware
  directed progress, the easing-transformed progress via `easing::Easing`,
  and the `fill`-mode before/after resolution the transition/animation
  drivers reduce to). The geometry-interfaces surface: `vixen-engine::geometry`
  (CSS Geometry Interfaces L1 — `DOMPoint`/`DOMRect`/`DOMQuad`/`DOMMatrix`
  with the full 4×4 matrix algebra + the perspective divide
  `Element.getBoundingClientRect()`/`IntersectionObserver`/`DOMMatrixReadOnly`
  reduce to). All
  `#![forbid(unsafe_code)]` and Rust-unit-tested.
- **Phase 6 prep** — pure form-constraint validation in `vixen-engine::forms`
  (email/URL formats, step arithmetic, range/length flags) ready for the
  script-layer host hooks; `vixen-engine::form_submission` (the three WHATWG
  HTML § 4.10.21 encoders: `application/x-www-form-urlencoded`,
  `multipart/form-data`, `text/plain`); `vixen-engine::dataset` (WHATWG HTML
  § 3.2.6.9 `data-*` ↔ `dataset` property-name bidirectional mapping, with
  the anti-collision rule); `vixen-engine::storage_key` (Web Storage key/value
  validation + origin-partitioned redb keys + the 5 MiB quota); the network
  host-hook family: `vixen-engine::url_search_params` (WHATWG URL Standard
  `URLSearchParams` parse/serialize + the full mutating surface),
  `vixen-engine::mime` (WHATWG MIME Sniffing § 2.1/§ 2.2 parse/serialize +
  `essence()`), and   `vixen-engine::text_codec` (WHATWG Encoding API
  `TextEncoder`/`TextDecoder` with the `fatal` flag, BOM sniff, and § 7.1
  line-break normalisation). The DOM-serialisation surface:
  `vixen-engine::html_serialize` (WHATWG HTML § 13.2.9 fragment serialisation
  — the `Element.innerHTML` / `outerHTML` / `document.write` getter pipeline,
  with the void-element + raw-text + text-escape + attribute-escape tables).
  The `vixen-engine::class_list` (WHATWG HTML
  § 4.6.4 `DOMTokenList` + § 2.7.3 ordered-set parser: `add`/`remove`/
  `toggle`/`replace`/`contains` with the spec's atomic validate-then-mutate
  rule, the supported-tokens surface for `<link>.relList`) backs every
  `element.classList` / `relList` / `sandbox` host-hook reflection. The CSS
  Values 4 dimension family (`length`,
  `color`, `angle`, `time`, `resolution`) — the value primitives the
  cascade/layout/paint resolves against — is now complete for v1.0; `<length>`
  includes logical viewport units plus the small/large/dynamic viewport
  families (`sv*`/`lv*`/`dv*`), pure sRGB colour arithmetic + interpolation,
  premultiplied alpha, hue/unit normalisation, and dots-per-pixel conversion
  are all Rust-unit-tested and ready for the cascade + WebRender to consume.
  The responsive-image
  selection family (`media_query`, `source_size`, `responsive_select`)
  completes the WHATWG § 4.8.4.6–§ 4.8.4.8 pipeline end-to-end: CSS Media
  Queries 4 condition evaluation against a `Viewport` (including `screen` /
  `print` output contexts and `any-hover` / `any-pointer` aggregate input
  devices), the `<img sizes>` source-size-list parser, and the § 4.8.4.8
  density-based source selection (incl. the `<picture>`/`<source media>`
  art-direction walk). The
  value-resolution primitives `calc` (CSS Values 4 § 10 `calc()`/`min()`/
  `max()`/`clamp()` with full § 10.7 dimension type-checking) and `easing`
  (CSS Easing 1 `cubic-bezier`/`steps`/`linear` timing functions) cover the
  cascade's `calc()` reduction and the transition/animation driver surface.
  The structured-clone + MessagePort family (`structured_clone`,
  `message_port`) models the HTML § 2.7.5 serialisation algorithm +
  § 9.5.2 entangled port pair `postMessage()` / `new MessageChannel()` /
  worker messaging reduce to, with the transfer-list validation
  (duplicate/ unreachable/detached rejection) and the `SharedArrayBuffer`
  cross-origin-isolation gate.   The Range/Selection family (`range`)
  models the DOM § 5.2 boundary-point pair + § 5.4 direction-aware
  selection (`add_range`/`collapse_to`/`extend_to`, the forward/backward
  direction) the editing commands and user-selection reflection reduce to.
  The session-history family (`history`) models the HTML § 7.1 entry-stack
  + the `history.pushState`/`replaceState`/`back`/`forward`/`go` surface +
  the `scrollRestoration` mode the `History` host hook + the navigation
  layer reduce to.   The MutationObserver family (`mutation_observer`)
  models the DOM § 4.3 mutation-queue + the § 4.3.1 match predicate
  (childList/attributes/characterData + the subtree/attributeFilter
  options) + the microtask-delivery batch the `MutationObserver` host
  hook reduces to.   The traversal family (`traversal`) models the DOM § 6
  `TreeWalker` + `NodeIterator` filtered preorder traversal (`whatToShow`
  bitmask + the `FILTER_ACCEPT`/`REJECT`/`SKIP` distinction — REJECT skips a
  subtree for TreeWalker, REJECT == SKIP for NodeIterator) + the
  node-removal reference adjustment, over a `Tree` trait the host hook
  implements on the real DOM. The WHATWG URL parser (`whatwg_url`) models
  the URL Standard § 4 parse + serialize + relative-resolution + the
  § 4.5 origin tuple the `new URL()` host hook + the fetch / navigation /
  storage layers consult.
- **Phase 7 prep** — CSP enforcement at the script execution boundary
  (`vixen-engine::script`); `vixen-net::referrer_policy` (Fetch § 3.4/§ 4.3.7
  `Referrer-Policy` parsing + `Referer` resolution); `vixen-net::strict_transport_security`
  (RFC 6795 HSTS parsing + § 8.2 host match); `vixen-net::cors` (Fetch
  § 3.2.1 `Access-Control-*` response-header parsing + § 4.1.5 CORS check
  with credentials-mode tightening + § 4.1.6 CORS-filtered response with
  the `Set-Cookie`/`Set-Cookie2` forbidden headers);
  `vixen-net::mixed_content` (W3C Mixed Content L1 § 3 verdict —
  `NotMixed`/`Block`/`Upgrade` — the fetch layer consults at every
  subresource out of a secure context); and `vixen-net::sandboxing`
  (WHATWG HTML § 4.8.5 `<iframe sandbox>` flag parser + the
  `implies_unique_origin` / `is_dangerous_scripts_plus_same_origin`
  predicates the script/navigation/storage layers consult when loading
  framed content); `vixen-net::sec_fetch` (Fetch § 3.1 `Sec-Fetch-*`
  request-metadata parsing + the § 3.2.4 site classifier); and
  `vixen-net::permissions_policy` (Permissions Policy 1 § 3.3
  `Permissions-Policy` header + `<iframe allow>` parser + the § 4
  per-feature allowlist evaluation) — ready for the network layer to
  consult at every fetch. The cross-origin-isolation gate:
  `vixen-net::coop` (HTML § 7.8 `Cross-Origin-Opener-Policy` parser +
  the opener-isolation predicate) + `vixen-net::coep` (Fetch § 3.2
  `Cross-Origin-Embedder-Policy` parser + the combined
  `is_cross_origin_isolated` gate the `performance.now()` coarsening and
  `SharedArrayBuffer` exposure consult).   The SRI + nosniff response-header
  family: `vixen-net::integrity` (W3C SRI `<script integrity>`/`<link
  integrity>` metadata parse + the constant-time hash verify, SHA-2 family
  only, any-match-passes) + `vixen-net::nosniff` (Fetch § 2
  `X-Content-Type-Options: nosniff` enforcement — the script/style MIME
  block) — ready for the fetch layer to consult at every subresource fetch.
  The CORP family: `vixen-net::corp` (Fetch § 4.5.3
  `Cross-Origin-Resource-Policy` parse + the combined COEP + CORP gate —
  `require-corp` cross-origin no-CORP block, `credentialless`
  cross-origin no-credentials allow, CORS the alternative opt-in) — ready
  for the fetch layer to consult before applying a no-cors subresource
  into a COEP-hardened document. The Trusted Types family:
  `vixen-net::trusted_types` (W3C Trusted Types `trusted-types` +
  `require-trusted-types-for` CSP directive parse + the
  `createPolicy(name)` gate + the injection-sink decision — a Trusted\*
  value ⇒ Allow, a string at a TT-requiring sink ⇒ `default`-policy or
  Block) — ready for the DOM injection-sink host hooks to consult before
  accepting a string.
- **Phase 8 (partial)** — the CDP WebSocket server (`vixen-headless::cdp`)
  responds to the six required methods (`Browser.getVersion`,
  `Target.createTarget`, `Target.attachToTarget`, `Page.navigate`,
  `Page.loadEventFired`, `Runtime.evaluate`) with stable error codes.

Source for later phases lands per [`docs/PLAN.md`](docs/PLAN.md).

---

## Setup

Workspace setup is managed by [mise](https://mise.jdx.dev) for the Rust
toolchain, `just`, and project tooling:

```sh
mise trust
mise bootstrap --yes     # Rust toolchain + just + dev tooling + build check
just check-all-host      # type-check the workspace
just test-host           # run host-runnable tests
```

`mise bootstrap` also points `CARGO_HOME` at `<workspace>/.cargo` so the
Cargo registry cache and `cargo-binstall`-ed tooling stay inside the
workspace (see [`docs/guidance/cargo-home.md`](docs/guidance/cargo-home.md)).

**The GNOME 50 SDK is not installed on the host** — it is managed inside a
`flatpak-builder` container, so host churn stays at zero and the build is
reproducible. To build against the SDK (the shell, or the Flatpak):

```sh
just flatpak-update-sdk  # pull the image (= install the GNOME 50 SDK)
just flatpak-build       # flatpak-builder against org.gnome.Sdk//50 in the container
```

See [`docs/guidance/gnome-sdk-flatpak-builder.md`](docs/guidance/gnome-sdk-flatpak-builder.md)
for the full workflow. Headless/CI hosts that only build `vixen-api` /
`vixen-net` / `vixen-store` need neither the SDK nor the container —
`mise install` + `just check-all-host` is enough.

See [`.mise.toml`](.mise.toml) and the
[mise bootstrap guide](https://mise.jdx.dev/bootstrap.html). The library
MSRV is 1.88 (let-chains); the dev toolchain floats to latest stable.

---

## Repository map

| Path                                        | Purpose                                                       |
|---------------------------------------------|---------------------------------------------------------------|
| [`docs/SPEC.md`](docs/SPEC.md)              | **What Vixen must do.** Capabilities, CLI, behaviour contracts. |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | **How Vixen is structured.** Crates, data flow, trust boundaries, trait APIs. |
| [`docs/DECISIONS.md`](docs/DECISIONS.md)    | **Why these choices.** ADR-style records for the major decisions. |
| [`docs/PLAN.md`](docs/PLAN.md)              | **How to build it.** Phased execution runbook with phase gates. |
| [`docs/REFERENCES.md`](docs/REFERENCES.md)  | **Where to look for truth.** Pinned reference trees + how to consult each. |
| [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md)  | **When it's done.** Release gates per capability. |
| [`docs/guidance/`](docs/guidance)           | **How to do specific tasks.** e.g. the GNOME SDK via flatpak-builder containers. |
| `LICENSE`                                   | Apache 2.0 (lands at Phase 0). |

---

## Reading order

If executing the build:

1. `docs/SPEC.md` — the contract
2. `docs/ARCHITECTURE.md` — the shape
3. `docs/DECISIONS.md` — confirm the choices
4. `docs/PLAN.md` — the runbook
5. `docs/REFERENCES.md` — consult as integration questions arise
6. `docs/ACCEPTANCE.md` — check against, every phase

If evaluating the project: read `docs/SPEC.md` and
`docs/DECISIONS.md`, then sample `docs/PLAN.md`.

When a doc and a decision record disagree, the **decision record wins**.
Update both when resolving.

---

## Working assumptions

- Target platform: **Linux + GNOME 50 SDK**, distributed via Flatpak.
  Other platforms are best-effort, not a release blocker.
- Build profile is optimal already and carries forward unchanged:
  `strip = true`, `lto = "thin"`, `codegen-units = 1`, `panic = "abort"`.
- App IDs: `org.vixen.Vixen` (production), `org.vixen.Vixen.Devel` (devel).

## License

Apache 2.0 — see [`LICENSE`](LICENSE).
