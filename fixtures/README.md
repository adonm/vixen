# Vixen compatibility fixtures

`fixtures/manifest.json` is the hermetic, release-blocking compatibility suite
consumed by `vixen-wpt`. Each manifest entry points at HTML plus typed assertions
defined in [`../docs/SPEC.md`](../docs/SPEC.md). Current measured counts and
limitations live in [`../docs/COMPAT.md`](../docs/COMPAT.md); reproduce them with
`just compat-report`.

## Layout

```text
fixtures/
├── cdp/           # controlled pages used by CDP/Playwright integration
├── css/           # selectors, cascade, computed values
├── dom/           # DOM, runtime host objects, storage/history projections
├── events/        # event dispatch, focus, and interaction ordering
├── forms/         # controls, validation, submission, reset, encoding
├── imported/      # minimized upstream-derived smoke cases with provenance
├── layout/        # layout tree, boxes, fragments, flex/grid/positioning
├── network/       # fetch/XHR and network-observable behavior
├── paint/         # display-list, visual-hash, and reference equivalence
├── security/      # policy and fail-closed browser behavior
└── wpt-profiles/  # descriptors for pinned external WPT checkout runs
```

## Rules

- Keep local fixtures small and deterministic; use controlled HTTP servers for
  behavior that cannot be represented by a file fixture.
- Record upstream provenance for minimized/imported cases. Larger upstream suites
  stay in an ignored pinned checkout and are selected by `wpt-profiles/` JSON.
- A real-site screenshot is triage, not a regression test. Reduce failures here
  or into a pinned external WPT profile when practical.
- New behavior uses the production/shared engine path. Do not add a harness-only
  parser, DOM, layout, network, or runtime semantic.
- Fixture checks in one manifest entry share one BrowserCore context/runtime;
  tests that need independent mutable host state must use distinct elements or
  distinct fixtures rather than relying on a fresh realm per assertion.
- Update measured documentation only from runner output. The suite must remain
  100% green; unsupported breadth belongs in explicit external profiles and
  `COMPAT.md`, not weakened local assertions.
