# Vixen executable milestones

This is the short delivery plan for turning the current pure-prep modules into
browser-visible slices. Rule: every large browser milestone extends
`vixen_engine::page::Page` and proves itself with a `just gate-*` command plus a
fixture in `fixtures/manifest.json`.

## Gates

| Command | Proves today | Extends next |
|---------|--------------|--------------|
| `just gate-smoke` | fmt, clippy, all host tests | reviewer baseline before commit/push |
| `just gate-phase2` | `vixen-headless --eval '1+2'` through SpiderMonkey | DOM/document host bindings |
| `just gate-phase3` | HTML parse + Stylo selector matching + author/inline computed-style cascade through `Page` + WPT fixtures | full Stylo `Stylist`/computed values behind `Page::computed_style` |
| `just gate-phase4` | layout pure-logic prep + Page-backed text line boxes through `vixen-headless --dump-lines` | real box tree via layout adapter |
| `just gate-phase5` | display-list + paint prep + Page-backed text display list/stats through `vixen-headless --dump-display-list` + `--paint-stats` | WebRender screenshot path through `Page` |
| `just gate-phase6` | DOM/forms/network-host pure prep | actual host hooks, events, forms, history, responsive images |

## Next vertical slices

1. **Cascade slice** — author `<style>` blocks and inline `style` attributes now
   flow through `Page::computed_style(node_id)` with Stylo selector matching,
   specificity, source order, and `!important`. Next: replace the compact
   projection with Stylo `Stylist` computed values. Proof:
   `just gate-phase3 && just gate-smoke`.
2. **Layout slice** — `Page::dump_lines` now emits deterministic body-text line
   boxes for `vixen-headless --dump-lines`. Next: feed computed styles into the
   layout adapter and replace the text-only boxes with the positioned box tree.
   Proof: `just gate-phase4 && just gate-smoke`.
3. **Display-list slice** — `Page::display_list` now converts the first line
   layout into invariant-enforced paint commands and exposes
   `vixen-headless --dump-display-list`; `--paint-stats` reports command counts
   and painted area from the same stream. Next: replace text-rect commands with
   real glyph/display items from the full layout tree. Proof:
   `just gate-phase5 && just gate-smoke`.
4. **Renderer slice** — render the display list via WebRender over
   `vixen_api::GlContext`, then wire headless `--screenshot`. Proof:
   `just gate-phase5`, a screenshot fixture/visual hash, and `just gate-smoke`.
5. **Host-bindings slice** — bind document/query/forms/events/history/fetch to
   SpiderMonkey against the same `Page`. Proof: `just gate-phase6`, relevant WPT
   fixtures, and `just gate-smoke`.

Keep adapters thin: `vixen-api` owns DTOs/traits, `vixen-engine::page` owns the
pipeline state, `vixen-headless` and `vixen-wpt` only drive the facade.
