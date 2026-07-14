# Vixen specification

Vixen's contract. What this document captures:

- **Vixen-specific surfaces** (CLI, error codes, WPT check types,
  diagnostics shape).
- **Vixen-specific configuration** of upstream behaviour (URL policy
  blocklist, cookie defaults, CSP enforcement points).
- **Behavioural invariants** that must be reproduced exactly because
  they're easy to get subtly wrong (event dispatch order, paint rules,
  form-validation edge cases).

What this document deliberately does **not** capture:

- Restatement of web-platform specs. Vixen delegates spec-heavy behavior where
  that improves correctness and size: Stylo/`selectors` for CSS,
  `html5ever` for HTML, `deno_core`/V8 for JS execution and host packaging, and
  Flutter Paragraph/Canvas/scene/Semantics for cross-platform render primitives
  (see [`DECISIONS.md`](DECISIONS.md) ADR-001 / ADR-011 / ADR-014 / ADR-022).
  Vixen implements CSS formatting semantics in the Flutter-hosted renderer.
  Behavioural parity is measured by the WPT profile
  documented in `docs/COMPAT.md`; if a behaviour isn't called out below, follow
  the latest stable spec and document deviations in `docs/COMPAT.md`.

---

## Headless CLI surface

The `vixen-headless` binary exposes this flag set. Flags and stable
error codes are a public contract — automation depends on them.

```
vixen-headless --url <URL> [options]

  --url <URL>                 Load a URL (required).
  --screenshot <file.png>     Save a PNG screenshot.
  --viewport <WxH>            Viewport size (default 800x600).
  --profile-dir <DIR>         Persist profile state under DIR.
  --extract-text              Print visible text content.
  --extract-selector <css>    Print JSON snapshots for matching elements.
  --eval <js>                 Execute JS, print result.
  --dump-dom                  Dump the DOM tree.
  --click-at <X,Y>            Dispatch a MouseEvent at coordinates.
  --focus <id>                Focus an element by id.
  --submit-form <id>          Submit a form by id.
  --incremental               Capture before/after frames (requires --screenshot + --eval).
  --cdp                       Start CDP WebSocket server on 127.0.0.1.
  --cdp-port <N>              CDP port (default 9222, with --cdp).
  --memory-stats              Print memory statistics.
```

The currently implemented `--dump-layout-tree`, `--dump-display-list`,
`--dump-lines`, `--paint-stats`, and `--list-fonts` flags expose transitional
Rust renderer internals and are removed at ADR-022 cutover rather than recreated
as compatibility APIs. Screenshot, viewport, visible extraction, coordinate
input, layout CDP, and visual fixture operations launch the chrome-less Flutter
host. DOM/runtime/network-only operations may remain native fast paths when they
do not invent geometry or pixels.

Without `--profile-dir`, each invocation owns and removes an isolated temporary
profile. With it, BrowserCore stores profile data in `<DIR>/profile.redb`; this
also applies to `--cdp`.

`--incremental` loads the URL once in one BrowserCore browsing context, captures
the loaded document, evaluates `--eval` in that document's current runtime, then
captures the resulting current document after any script-created navigation has
settled. Given `--screenshot <name.ext>`, the frames are written as
`<name>-frame-1.ext` and `<name>-frame-2.ext`; without an extension they are
`<name>-frame-1` and `<name>-frame-2`. The requested `--screenshot` path itself
is not written. On success stdout contains only the `--eval` result followed by
a newline, as in a normal `--eval` run; frame paths are deterministic and are
not printed. Other output, interaction, CDP, font-list, and memory-stat actions
are incompatible with `--incremental` and produce a command-line usage error
rather than being silently ignored.

Any invocation that requests renderer-derived geometry, coordinate input,
Semantics, or pixels runs its entire logical session—including evaluation and
other DOM/runtime operations—against the single BrowserCore owned by the
chrome-less Flutter host. `vixen-headless` is a launcher/client in that mode and
does not create a second native core. A wholly text-only invocation may use a
native BrowserCore fast path.

`--gpu` remains removed: Flutter is the sole renderer and Vixen explicitly
enables Impeller; callers cannot select another graphics backend. A Skia-backed
launch does not satisfy Vixen renderer support. On Linux, rendered automation
runs in Cage/headless Wayland. If the Flutter host cannot create/present/capture
an exact commit, screenshots fail closed with `unsupported.screenshot`.

**Stable error codes** (returned exactly as written):

| Code                       | When                                                       |
|----------------------------|------------------------------------------------------------|
| `unsupported.screenshot`   | Screenshot requested without an exact rendered host available |
| `invalid-selector`         | Malformed `--extract-selector` input                       |

**CDP methods required** at v1.0:

- `Browser.getVersion`
- `Target.createTarget`, `Target.attachToTarget`, `Target.getTargets`
- `Page.enable`, `Page.navigate`, `Page.reload`, `Page.stopLoading`,
  `Page.loadEventFired`, `Page.getFrameTree`, `Page.getResourceTree`,
  `Page.getResourceContent`, `Page.getLayoutMetrics`,
  `Page.getNavigationHistory`, `Page.navigateToHistoryEntry`,
  `Page.resetNavigationHistory`, `Page.setBypassCSP`, and
  `Page.captureScreenshot` (PNG)
- `Runtime.enable`, `Runtime.evaluate`, `Runtime.awaitPromise`,
  `Runtime.getProperties`, `Runtime.consoleAPICalled`, and
  `Runtime.exceptionThrown`
- `Network.enable`, top-level `Network.*` navigation notifications, and the
  Playwright network-toggle methods (`setCacheDisabled`,
  `setBypassServiceWorker`, `setExtraHTTPHeaders`; extra headers apply to
  runtime `fetch()` requests, cache-disabled bypasses runtime `fetch()` cache
  reads/writes)
- `DOM.getDocument`, `DOM.querySelector`, `DOM.querySelectorAll`,
  `DOM.describeNode`, `DOM.resolveNode`, `DOM.getContentQuads`,
  `DOM.getBoxModel`, `DOM.getAttributes`, `DOM.getOuterHTML`,
  `DOM.setAttributeValue`, and `DOM.removeAttribute`
- `Performance.getMetrics`, `Security.getSecurityState`
- `Input.dispatchMouseEvent` (mouse move/press/release over the current full
  viewport), `Input.dispatchKeyEvent`, and `Input.insertText`

## Flutter GUI shell contract

Flutter is the sole web renderer and native GUI shell target on Linux, macOS,
Windows, Android, and the Apple Silicon iOS Simulator. The Linux alpha baseline
implements chrome and BrowserCore FFI over transitional RGBA presentation; the
mutation/commit renderer and every other platform stay evidence-gated.
The Linux GUI requires a native Wayland display and rejects X11/XWayland;
rendered headless/CDP uses the chrome-less Flutter host under Cage after cutover.

Platform validation follows a rolling contemporary baseline: the latest stable
major release of Linux's reference distribution, macOS, Windows client, Android,
and iOS Simulator at each release cutoff. Exact versions and toolchains are
recorded in release evidence. Older majors are best-effort unless explicitly
promoted to an additional tested tier.

- BrowserCore owns browser/profile/context/document/runtime/computed-style/
  resource-policy/accessibility meaning. Dart owns bounded CSS formatting,
  Paragraph/Canvas scenes, renderer commits/queries, chrome, and host-service UI.
- The Dart FFI bridge carries bounded typed commands/events and opaque handles
  with explicit lifetime, allocation, version, sequence, and generation rules.
- BrowserCore sends exact bounded mutation/full-resync revisions; Flutter returns
  one atomic scene/basic-geometry/text/scroll/semantic-bound commit with an opaque
  Flutter-side hit-test handle and a separate presented acknowledgement.
- Flutter hit-tests the displayed commit and owns mechanical scroll geometry.
  BrowserCore validates targets and owns event cancellation/defaults, script
  scroll intent, history/persistence, selection meaning, and navigation effects.
- BrowserCore semantic meaning plus Flutter commit bounds publish one native
  Semantics generation; actions name the exact displayed commit.
- Rendered CLI/CDP/WPT use a chrome-less Flutter host. Text-only utilities may
  remain native and GUI bundles need not ship developer automation entrypoints.

Platform acceptance, Android V8/GLES/split-ABI gates, the iOS Simulator track,
Linux release/FlatPark packaging, and artifact policy are specified in
[`FLUTTER_SHELL.md`](FLUTTER_SHELL.md). JavaScript and WebAssembly use the same
`deno_core`/V8 runtime path on every declared target.

---

## WPT harness — check types

The WPT harness asserts document state against fixture manifests. The committed
`fixtures/manifest.json` remains the hermetic release-blocking smoke suite.
Larger upstream slices may instead be described by small JSON WPT profiles and
run against an ignored checkout such as `.tmp/wpt/` via `just wpt-profile
fixtures/wpt-profiles/<profile>.json .tmp/wpt`. The check types below are the
public contract for fixture/profile authors except where explicitly marked
transitional.

| Check type              | Asserts                                                  |
|-------------------------|----------------------------------------------------------|
| `title`                 | Document `<title>` text                                  |
| `selector-count`        | Number of elements matching a selector                   |
| `selectors-exact`       | Exact set of element ids matching a selector             |
| `body-contains`         | Body text contains a substring                           |
| `js-eval`               | Evaluate JS, compare result to expected                  |
| `min-nodes`             | DOM has at least N elements                              |
| `no-critical-diagnostics` | No critical `EngineDiagnostic` recorded                |
| `visual-hash`           | Perceptual hash of rendered screenshot matches expected  |
| `selector-match`        | Per-element selector match details                       |
| `computed-style`        | Per-element computed style value matches expected        |
| `element-attribute`     | Element attribute value matches expected                 |
| `layout-box`            | Element border-box `(x, y, w, h)` matches expected       |
| `display-list-contains` | **Transitional:** old Rust display-list substring; removed during R5 manifest migration |
| `dom-nodes-range`       | DOM node count is within [min, max]                      |
| `ref-equivalent`        | Rendered page matches a reference HTML fixture           |

WPT target profile lives in [`COMPAT.md`](COMPAT.md). End-to-end CSS/DOM/layout
behavior should move into fixtures when practical. Target Rust tests cover pure
logic such as URL/cookie/CSP parsing and redb round trips; a CSS algorithm remains
in Rust only through ADR-022's explicit stable formatter contract and
cross-language tests. R5 migrates the three current `display-list-contains`
assertions in two fixtures to commit-bound layout/pixel checks before proving the
full manifest.

---

## Diagnostics shape

```rust
pub struct EngineDiagnostic {
    pub category: EngineDiagnosticCategory,
    pub code: &'static str,        // e.g. "parse-dom.budget"
    pub message: String,
}

pub enum EngineDiagnosticCategory {
    Network,
    ParseDom,
    ScriptRuntime,
    LayoutRender,
    StorageCache,
}
```

The GUI shell surfaces diagnostics in chrome; the WPT
`no-critical-diagnostics` check consumes them. Codes are stable contract.

---

## URL policy

Every network fetch passes through `validate_http_url`. The blocklist is
Vixen's configuration of what counts as a "public" HTTP target.

```rust
use std::net::{Ipv4Addr, Ipv6Addr};
use url::{Host, Url};

#[derive(Debug, Clone)]
pub enum UrlPolicyError {
    UnsupportedScheme(String),
    BlockedHost { host: String },
}

pub fn validate_http_url(url: &Url) -> Result<(), UrlPolicyError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlPolicyError::UnsupportedScheme(url.scheme().to_owned()));
    }
    if let Some(host) = url.host()
        && is_private_host(&host)
    {
        return Err(UrlPolicyError::BlockedHost { host: host.to_string() });
    }
    Ok(())
}

pub fn is_private_host(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(ip) => is_private_ipv4(*ip),
        Host::Ipv6(ip) => is_private_ipv6(*ip),
        Host::Domain(domain) => {
            let lower = domain.to_lowercase();
            lower == "localhost"
                || lower == "localhost.localdomain"
                || lower.ends_with(".local")
                || lower.ends_with(".internal")
                || lower.ends_with(".onion")
                || lower.ends_with(".arpa")
                || lower.ends_with(".test")
                || lower.ends_with(".example")
                || lower.ends_with(".invalid")
        }
    }
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()              // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local()           // 169.254/16
        || ip.is_unspecified()          // 0.0.0.0 (unspecified only)
        || ip.is_broadcast()            // 255.255.255.255
        || ip.is_documentation()        // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || is_cgnat(ip)                 // 100.64.0.0/10
}

fn is_cgnat(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (o[1] & 0xc0) == 0x40   // 100.64.0.0/10 precisely
}

fn is_private_ipv6(ip: Ipv6Addr) -> bool {
    ip.is_loopback()                    // ::1
        || ip.is_unspecified()          // ::
        || ip.is_unique_local()         // fc00::/7
        || (ip.segments()[0] & 0xffc0) == 0xfe80   // link-local fe80::/10
        || ip.to_ipv4_mapped().is_some_and(is_private_ipv4)
}
```

---

## Cookie defaults

Cookies follow RFC 6265 with these Vixen-specific defaults:

- **Default `SameSite` is `Lax`** (matches modern browsers, not strict
  RFC 6265 which has no default).
- **Storage cap: 512 entries per jar.** Eviction is FIFO by insertion
  order (not the RFC's full eviction algorithm). This is a deliberate
  simplification.
- **`HttpOnly` rejected from `document.cookie`** but accepted from
  `Set-Cookie` HTTP response. This is RFC-correct but called out
  because it's a frequent bug source.
- **Outgoing `Cookie` header**: `SameSite=Lax` cookies are sent
  cross-site only for safe methods (GET/HEAD/OPTIONS). `SameSite=Strict`
  cookies are sent only to same-host requests. `HttpOnly` cookies never
  appear in `document.cookie` reads.
- **Domain policy uses the static Mozilla Public Suffix List**, including its
  private section. Parent public-suffix attributes are rejected; an exact-host
  public suffix is converted to host-only as required by RFC 6265bis.

Everything else (domain matching, path matching, secure-gating,
expiry handling, `Max-Age` semantics) follows RFC 6265 exactly.

---

## CSP enforcement points

CSP is parsed from `Content-Security-Policy` headers and
`<meta http-equiv="Content-Security-Policy">`. Enforcement happens at
three boundaries:

1. **Script execution** — `script-src` (or `default-src` fallback).
   Inline scripts blocked unless `'unsafe-inline'` or a matching
   hash/nonce is present.
2. **Fetch** — `connect-src`, `img-src`, `style-src`, `font-src`,
   `media-src`, `object-src`, etc. URLs matched against source-list.
3. **Plugin content** — `<embed>`, `<object>` allowed only if
   `object-src` permits.

Source-list grammar follows the CSP spec exactly (`'self'`, `'none'`,
`'unsafe-inline'`, `'unsafe-eval'`, host/scheme sources, nonces,
hashes).

---

## Form validation edge cases

These are pinned down because they're easy to get subtly wrong.

**Email format** (`typeMismatch` for `type="email"`):

- Exactly one `@`.
- Non-empty local-part.
- Domain contains at least one `.`.

**URL format** (`typeMismatch` for `type="url"`):

- Valid scheme (letters followed by `:`).
- `://` separator after the scheme.
- Non-empty host.

**Step arithmetic** (`stepMismatch`):

- Step base = `min` if present, else the type-specific default base.
- Default step per type: number/range = 1; date = 1 day; time = 60 s;
  week = 1 week; month = 1 month; datetime-local = 60 s.
- Valid when `(value - step_base)` is within float tolerance of an
  integer multiple of `step`.
- Date/time values use integer arithmetic on canonical units: `date` →
  days since epoch, `time` → seconds since midnight, `week` → weeks
  since epoch, `month` → months since year 0, `datetime-local` → epoch
  seconds.

Everything else in constraint validation (`valueMissing`,
`rangeUnderflow`/`rangeOverflow`, `tooLong`/`tooShort`, `badInput`,
`customError`, `willValidate`) follows the HTML5 spec exactly.

---

## Composed event dispatch invariants

Specific ordering invariants that must be reproduced exactly.

**Focus transitions** (when `document._setActiveElement` runs):

```
focusout → focusin → blur → focus
```

- `focusout` and `focusin` bubble.
- `blur` and `focus` do not bubble.

**`composedPath()`** walks target → parentNode chain, returning a flat
JS array. Respects shadow DOM boundaries when `composed: true` on the
event.

---

## Renderer commit and paint invariants

These rules apply to the Flutter-hosted formatter and every GUI/automation
surface:

1. **Exact revisions** — a mutation batch applies only to its named base and
   target `RenderRevision`; gaps request full resync.
2. **Atomic commit** — scene-ready layout, basic geometry, an opaque Flutter-side
   hit-test handle, text query state, scroll snapshot, semantic bounds, and
   truncation share one `commit_id` and revision.
3. **Presented identity** — input and native accessibility identify the displayed
   commit, not merely the newest completed layout.
4. **Stable paint order** — stacking contexts, z-index, positioned content, and
   document-order ties follow CSS; viewport background remains first.
5. **Clip and transform identity** — paint, hit testing, text/caret queries, and
   semantic bounds consume the same clip/transform chain.
6. **Opacity and visibility** — group opacity composes through ancestors;
   `opacity: 0` and hidden/collapsed paint are omitted while required layout state
   remains queryable.
7. **Scroll identity** — renderer offsets/extents/clips and the scene share one
   commit; BrowserCore scroll events/history accept only that result.
8. **Finite bounded geometry** — non-finite, oversized, unknown-node/resource,
   over-depth, or required-but-truncated geometry fails closed.
9. **No stale fallback** — stale commits cannot target input, answer required
   geometry, publish Semantics, or become visible after replacement.
10. **One renderer** — after cutover no WebRender/EGL/RGBA or second screenshot
    path remains. Text-only tools cannot fabricate geometry.
