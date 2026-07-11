# External WPT profiles

Use this directory for small, committed JSON profiles that point at files in an
ignored upstream WPT checkout, usually `.tmp/wpt/`. This avoids vendoring broad
WPT HTML fixture sets into Vixen while keeping the selected paths, provenance,
and expected checks reviewable.

Profiles are fail-closed inputs. The loader requires:

- `upstream.repo` to be exactly
  `https://github.com/web-platform-tests/wpt.git`.
- `upstream.revision` to be exactly 40 lowercase hexadecimal characters. Uppercase
  hashes are rejected rather than normalized.
- Fixture and sparse paths to be canonical, slash-separated relative paths: no
  absolute paths, backslashes, empty components, `.`, or `..` components.
- Every fixture path to equal a declared `upstream.sparse_paths` entry or be a
  descendant of one on a path-component boundary. For example, `css/css-display`
  covers `css/css-display/test.html`, but not `css/css-display-other/test.html`.

Before a configured external profile runs, the harness also requires the supplied
root to have its own `.git` directory or linked-worktree `.git` file, to be the
worktree top level, to have `HEAD` exactly equal to the profile revision, and to
have empty `git status --porcelain=v1 --untracked-files=all` output. A missing or
nested worktree, Git command failure, revision mismatch, tracked modification, or
untracked file aborts before any fixture runs. Ignored files do not make Git report
the checkout as dirty.

To create the checkout for the committed layout profile from scratch:

```sh
mkdir -p .tmp
git clone --filter=blob:none --no-checkout --sparse \
  https://github.com/web-platform-tests/wpt.git .tmp/wpt
git -C .tmp/wpt sparse-checkout set css/css-display
git -C .tmp/wpt fetch --filter=blob:none origin \
  9089531cfc7a76fe192e640f0cd60141b1f21b3f
git -C .tmp/wpt checkout --detach \
  9089531cfc7a76fe192e640f0cd60141b1f21b3f
test "$(git -C .tmp/wpt rev-parse HEAD)" = \
  9089531cfc7a76fe192e640f0cd60141b1f21b3f
test -z "$(git -C .tmp/wpt status --porcelain=v1 --untracked-files=all)"
```

Then run only the optional profile test:

```sh
VIXEN_WPT_PROFILE=fixtures/wpt-profiles/layout-block-inline-position.json \
VIXEN_WPT_ROOT=.tmp/wpt \
cargo test -p vixen-headless --test wpt_profile_runner \
  external_wpt_profile_passes_when_configured -- --exact --nocapture
```

These commands describe how to reproduce and evaluate the profile. They do not
assert that `.tmp/wpt` is currently present or that Vixen passes the profile's
checks.

Minimal profile shape:

```json
{
  "name": "layout-block-inline-position",
  "description": "Curated upstream WPT layout smoke profile.",
  "upstream": {
    "repo": "https://github.com/web-platform-tests/wpt.git",
    "revision": "<40-lowercase-hex-commit>",
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
