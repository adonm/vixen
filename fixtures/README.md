# WPT fixtures

The acceptance suite (docs/ACCEPTANCE.md). Each fixture is an HTML file
plus an assertion manifest consumed by `vixen-wpt`. The check types
are defined in docs/SPEC.md "WPT harness — check types".

Layout (per docs/PLAN.md):

```
fixtures/
├── css/            # cascade + layout + paint (visual-hash, ref-equivalent)
├── dom/            # DOM core
├── layout/         # Page-backed layout and display-list coordinate fixtures
├── events/         # composed event dispatch (focus-order.html)
├── forms/          # form validation edge cases (docs/SPEC.md)
├── storage/        # localStorage / sessionStorage
├── network/        # fetch / XHR
├── realworld/      # manual smoke against real sites
└── reftest-baseline/  # reference renderings (docs/PLAN.md "Snapshot tests")
```

Fixtures land phase by phase; each phase gate requires its fixture set to
pass (docs/PLAN.md gate table). `fixtures/` is empty at Phase 0.
