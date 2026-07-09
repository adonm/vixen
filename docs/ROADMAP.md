# Roadmap

This is the current delivery order toward the full project goal: a credible
modern-Linux Firefox replacement with a focused desktop shell, first-class
headless/CDP automation, and maximum web capability per byte. Historical phase
notes stay in `PLAN.md`; this file should describe what to build next, not what
already landed.

## Current baseline

These items are live enough to stop treating them as roadmap milestones:

- hk owns the developer lifecycle: quick pre-commit checks, full pre-push gate,
  and `just` as implementation detail.
- WPT/fixture reports are text-first and include actionable detail.
- `Page::display_list` feeds the single WebRender path; headless screenshots,
  CDP screenshot capture, and GUI/headless rendering share that path.
- CDP/Playwright smoke covers navigation, runtime evaluation, console/exception
  events, screenshot capture, flattened-session response echo, exposed bindings,
  init scripts, dialogs, and basic mouse/keyboard input.
- The `deno_core` runtime host is the eval path for WPT, headless, and CDP;
  `Page::evaluate_dom_expression` is only a fail-closed compatibility shim.

## Alpha to beta: make one small browser real

1. **Full page lifecycle over real backing stores**
   - Connect fetch, cookies, local/session storage, history, form submission,
     redirects, CSP, referrer policy, permissions, and session restore through
     `vixen-net`/`vixen-store` instead of in-memory runtime-only state.
   - Treat every crossing from JS/page content to Rust/network/storage as a trust
     boundary: validate near the op, fail closed, emit stable diagnostics.
   - Make navigation/update invalidation explicit: script mutation, history
     traversal, form submit, document.write, and network navigation should all
     update the same authoritative `Page`/engine state.
   - Proof: network/storage/history WPT profiles, `vixen-net`/`vixen-store` tests,
     and CDP navigation/runtime integration tests.

2. **Real layout broadening, not more projection tricks**
   - Replace deterministic text metrics and compact layout shortcuts with the
     owned layout pipeline: block, inline, positioned, overflow/scroll, flex, grid,
     and enough intrinsic sizing for real sites.
   - Keep one styled-DOM → layout-tree → display-list flow; no post-pass geometry
     fixups that hide bad layout data.
   - Prioritize WPT/ref fixtures that affect visible pages: forms, navigation UI,
     common app layouts, overflow clipping, sticky/fixed positioning, and nested
     flex/grid.
   - Proof: `just gate-phase4`, imported layout profile pass counts in
     `COMPAT.md`, visual/ref fixtures, and realworld fixture screenshots.

3. **Desktop shell becomes daily-smoke usable**
   - Move from “window can display a page” to a tight browser vertical: tabs,
     URL/search entry, reload/stop, back/forward, find, zoom, downloads/status,
     permission prompts, and clear error states.
   - Keep the UI small and fast; add chrome only when it supports daily browsing
     or debugging the engine.
   - Persist profile state through `vixen-store`: history, cookies, sessions,
     settings, and restore-on-start.
   - Proof: `just flatpak-build`, manual GNOME smoke, `just gate-smoke`, and a
     realworld fixture checklist documented in `COMPAT.md` or a smoke report.

4. **Automation becomes a product surface**
   - Grow CDP toward the Playwright MVP: target/session lifecycle, runtime object
     handles/properties, DOM querying, input, navigation waits, screenshots,
     downloads, dialogs, console/network events, and stable error responses.
   - Keep headless CLI, CDP, and WPT using the same engine/runtime paths; no
     automation-only DOM model.
   - Add enough protocol coverage for common Playwright smoke suites against
     local files and controlled HTTP fixtures.
   - Proof: `docs/CDP_PLAYWRIGHT_SMOKE.md`, CDP integration tests, and at least
     one external Playwright smoke script recorded as a repeatable command.

5. **Compatibility loop scales up**
   - Expand imported WPT profiles by user-visible risk, not by easy pass counts:
     layout, DOM/events/forms, storage/history/network, CSS cascade/values, then
     paint/ref tests.
   - Publish measured fixture/check pass counts in `COMPAT.md` after meaningful
     behavior changes.
   - Use failing WPTs to choose the next implementation slice; do not claim broad
     parity until the harness runs representative upstream profiles.
   - Proof: `vixen-wpt` profile reports with local/imported split and green local
     release-blocking fixtures.

## Beta to v1.0: credible browser claim

1. **Real-site corridor** — choose a small, public, reproducible set of static
   and app-like sites. Load them in GUI and headless, publish screenshots,
   diagnostics, and known gaps.
2. **Performance and footprint budget** — track binary size, startup time,
   navigation latency, memory after first page, and screenshot time. Regressions
   need an explicit tradeoff.
3. **Security hardening** — complete audit/deny gates, fuzz URL/CSP/cookie/HTML
   boundaries, and keep private-network fetch blocking and CSP fail-closed.
4. **Release discipline** — every v1 capability in `ACCEPTANCE.md` has a gate,
   a fixture/profile, and an honest compatibility entry.

## Working rule

Every milestone should land as one coherent batch with:

- a browser-visible seam (`Page`, headless, CDP, GUI, or WPT profile),
- focused tests/fixtures,
- an honest compatibility or limitation update when behavior changes,
- green hk pre-push gates before push.

Architecture changes are the only routine reason to stop for human direction
before alpha. After alpha, architecture changes need a new ADR.
