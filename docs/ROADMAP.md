# Roadmap

This is the current delivery order. It intentionally avoids duplicating the
large historical phase notes in `PLAN.md`; keep this file focused on the next
browser-shaped outcomes.

## MVP to alpha

1. **Gate and report cleanup**
   - Enforce quick commit checks and long pre-push checks through hk.
   - Keep `just` recipes as implementation details invoked by hk.
   - Produce text-first summaries from WPT/fixture runs suitable for humans and
     LLM agents.
   - Proof: `hk validate`, `hk run pre-commit --check`, `hk run pre-push --check`.

2. **WebRender screenshot vertical**
   - Consume `Page::display_list` through the single WebRender path.
   - Make headless `--screenshot` produce PNGs through EGL surfaceless.
   - Keep GUI/headless rendering on the same display list and renderer.
   - Proof: `just gate-phase5`, visual/ref fixtures, size measurement.

3. **Minimal desktop browser vertical**
   - Relm4/libadwaita window with one visible page, URL entry, reload/stop,
     back/forward, and status diagnostics.
   - UI stays focused: no kitchen-sink chrome while engine architecture is still
     settling.
   - Current proof uses the GNOME SDK Flatpak builder container:
     `just flatpak-build`, manual GUI smoke, `just gate-smoke`.

4. **Runtime host APIs: deepen before broadening**
   - Keep WebIDL prototypes generated/adopted.
   - Move remaining Page string projections into `deno_core` extensions with ops
     or resources where state/backend access is required.
   - Prefer spec/WPT-correct useful subsets over broader shape-only stubs.
   - Proof: `just gate-phase6`, imported DOM/Web API fixture profiles.

5. **Real network/storage/history backing**
   - Wire fetch, cookies, storage, session history, and navigation state through
     existing Rust policy/storage modules.
   - Validate at trust boundaries and fail closed.
   - Proof: network/storage WPT fixtures plus `vixen-net`/`vixen-store` tests.

6. **CDP/Playwright MVP**
   - Grow CDP only along the paths needed by useful automation: navigation,
     runtime evaluation, screenshots, basic input, console/errors.
   - Preserve stable JSON/text diagnostics.
   - Proof: CDP integration tests and a documented Playwright smoke.

7. **Compatibility report loop**
   - Expand imported WPT profiles by priority area.
   - Publish measured pass counts in `COMPAT.md` after meaningful changes.
   - Use failures to choose the next implementation slice.

## Working rule

Every milestone should land as one coherent batch with:

- a browser-visible seam (`Page`, headless, CDP, GUI, or WPT profile),
- focused tests/fixtures,
- an honest compatibility or limitation update when behavior changes,
- green hk pre-push gates before push.

Architecture changes are the only routine reason to stop for human direction
before alpha.
