# External WPT profiles

Use this directory for small, committed JSON profiles that point at files in an
ignored upstream WPT checkout, usually `.tmp/wpt/`. This avoids vendoring broad
WPT HTML fixture sets into Vixen while keeping the selected paths, provenance,
and expected checks reviewable.

Workflow:

1. Check out upstream WPT under `.tmp/wpt/` at the profile's pinned revision.
2. Add/update a profile JSON in this directory.
3. Run:

   ```sh
   just wpt-profile fixtures/wpt-profiles/<profile>.json .tmp/wpt
   ```

Minimal profile shape:

```json
{
  "name": "layout-block-inline-position",
  "description": "Curated upstream WPT layout smoke profile.",
  "upstream": {
    "repo": "https://github.com/web-platform-tests/wpt.git",
    "revision": "<commit-sha>",
    "sparse_paths": ["css/css-display", "css/CSS2"]
  },
  "fixtures": [
    {
      "path": "css/css-display/display-flow-root-001.html",
      "category": "layout block/inline/position",
      "checks": [
        { "type": "title", "expected": "display: flow-root" },
        { "type": "no-critical-diagnostics" }
      ]
    }
  ]
}
```

Profile `path` values are always relative to the external WPT root; absolute
paths and `..` are rejected by the harness.
