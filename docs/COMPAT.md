# Vixen compatibility target

This is the honest v1.0 target matrix. It is not a claim of full Firefox or full
WPT compatibility. Vixen delegates CSS cascade/selectors, HTML parsing, JS
execution, and paint where credible upstream Rust/Firefox-family components
exist; layout is Vixen-owned Rust code per ADR-013 and therefore WPT-gated by
feature.

---

## Current measured local fixture baseline

As of 2026-07-06, `fixtures/manifest.json` contains:

| Category | Fixtures |
|----------|---------:|
| css      | 16 |
| dom      | 10 |
| forms    | 2 |
| layout   | 1 |
| network  | 2 |
| paint    | 1 |
| security | 8 |
| **Total** | **40** |

Total manifest checks: **500**.

Current check mix:

| Check type | Count |
|------------|------:|
| `selector-count` | 231 |
| `element-attribute` | 127 |
| `selectors-exact` | 49 |
| `title` | 39 |
| `body-contains` | 38 |
| `computed-style` | 11 |
| `no-critical-diagnostics` | 4 |
| `selector-match` | 1 |

This local fixture set is release-blocking and must remain **100 % green**.

---

## WPT target profile

Full upstream WPT is too broad to summarize honestly with one percentage at
v1.0. The release contract is a curated, imported WPT profile with measured
pass counts by category. Imported WPT fixtures should live beside Vixen fixtures
and be recorded in `fixtures/manifest.json` so `vixen-wpt` reports the same
check types for local and upstream-derived tests.

| Area | v1.0 target | Expected achievability | Notes |
|------|-------------|------------------------|-------|
| HTML parsing/tree construction | Broad smoke subset green | High | `html5ever` carries parser behavior; Vixen must preserve node ids/tree shape. |
| Selectors | Modern selector subset green | High | Backed by Stylo/`selectors`; include combinators, attributes, `:is`, `:where`, `:has`, form/link pseudos. |
| CSS cascade/computed values | Author stylesheet + inline subset green | High after full Stylo slice | Compact cascade is temporary; full Stylo should unlock wider WPT coverage. |
| CSS layout: block/inline | v1 visual/ref subset green | Medium | Vixen-owned layout; start with normal flow, margin/border/padding, inline line boxes. |
| CSS layout: flex/grid | Useful common-case subset green | Medium | Pure helpers exist; full WPT edge coverage is post-v1. |
| CSS layout: tables/floats/fragmentation | Not v1 release-blocking | Low for v1 | Document as unsupported/partial until implemented. |
| DOM Core | Traversal, attributes, token lists, ranges, mutation observer subset green | Medium | Vixen-owned Web APIs over SpiderMonkey bindings. |
| Events/forms/history/storage | Selected behavioral subset green | Medium | Gate by fixtures from SPEC invariants and imported WPT cases. |
| JS language | Use SpiderMonkey/test262, not WPT percentage | High for language | Web API exposure remains Vixen-owned and separately gated. |
| Paint/ref tests | Display-list + WebRender visual subset green | Medium | One paint path; correctness depends on layout fragments and WebRender mapping. |
| Media/WebGPU/WebRTC/service workers | Out of scope for v1 | Not targeted | Deferred by ADRs / acceptance post-v1 scope. |

---

## Release-blocking WPT goals

For v1.0, Vixen should be able to claim:

1. **100 % pass** on local `fixtures/manifest.json`.
2. **Green imported WPT smoke profile** for parser, selectors, cascade, DOM core,
   forms, and the v1 layout subset.
3. **Measured pass counts published here** for every imported category.
4. **No global full-WPT percentage claim** until the harness imports and runs a
   representative upstream WPT checkout.

Initial import targets before v1.0:

| Imported WPT area | Minimum useful target |
|-------------------|----------------------:|
| selectors/css-scoping/css-nesting selector behavior | 50 fixtures |
| css-cascade / css-values computed-value behavior | 50 fixtures |
| dom/nodes + traversal + ranges | 50 fixtures |
| html/semantics/forms basics | 25 fixtures |
| css/css-display + css-box + css-position normal-flow layout | 40 fixtures |
| css-flexbox common cases | 25 fixtures |
| css-grid common cases | 25 fixtures |
| paint/ref-equivalent smoke | 20 fixtures |

These are minimum profile sizes, not final compatibility claims. The measured
pass table below must be filled from `vixen-wpt` output as the fixtures land.

| Imported WPT area | Fixtures run | Checks run | Passed | Pass rate | Notes |
|-------------------|-------------:|-----------:|-------:|----------:|-------|
| selectors | 0 | 0 | 0 | n/a | Not imported yet. |
| css-cascade/css-values | 0 | 0 | 0 | n/a | Not imported yet. |
| dom-core | 0 | 0 | 0 | n/a | Not imported yet. |
| forms | 0 | 0 | 0 | n/a | Not imported yet. |
| layout block/inline/position | 0 | 0 | 0 | n/a | Not imported yet. |
| flexbox | 0 | 0 | 0 | n/a | Not imported yet. |
| grid | 0 | 0 | 0 | n/a | Not imported yet. |
| paint/ref-equivalent | 0 | 0 | 0 | n/a | Not imported yet. |

---

## Known v1.0 layout gaps

Expected unsupported or partial areas unless promoted by WPT/real-site evidence:

- table layout
- floats and float avoidance
- full vertical writing modes / vertical text shaping
- page fragmentation / pagination / print layout
- advanced intrinsic sizing cycles (`min-content` / `max-content` edge cases)
- complete absolute/fixed/sticky interaction matrix
- full SVG layout integration

Each gap should fail closed where possible, emit diagnostics when visible to
users/tests, and receive a WPT fixture before being marked supported.
