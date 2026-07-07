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

- Restatement of web-platform specs. CSS cascade/selectors, HTML parsing,
  ES execution, and paint are delegated to Firefox-family crates/components
  where possible (Stylo, `selectors`, SpiderMonkey, `html5ever`, WebRender —
  see [`DECISIONS.md`](DECISIONS.md) ADR-001 / ADR-011). Layout is
  Vixen-owned Rust code per ADR-013, with Ladybird used as the architecture
  reference. Behavioural parity is measured by the WPT profile documented in
  `docs/COMPAT.md`; if a behaviour isn't called out below, follow the latest
  stable spec and document deviations in `docs/COMPAT.md`.

---

## Headless CLI surface

The `vixen-headless` binary exposes this flag set. Flags and stable
error codes are a public contract — automation depends on them.

```
vixen-headless --url <URL> [options]

  --url <URL>                 Load a URL (required).
  --screenshot <file.png>     Save a PNG screenshot.
  --viewport <WxH>            Viewport size (default 800x600).
  --extract-text              Print visible text content.
  --extract-selector <css>    Print JSON snapshots for matching elements.
  --eval <js>                 Execute JS, print result.
  --dump-dom                  Dump the DOM tree.
  --dump-layout-tree          Dump the Vixen layout tree.
  --dump-display-list         Dump paint commands.
  --dump-lines                Dump inline layout lines.
  --click-at <X,Y>            Dispatch a MouseEvent at coordinates.
  --focus <id>                Focus an element by id.
  --submit-form <id>          Submit a form by id.
  --paint-stats               Print paint statistics.
  --incremental               Two-frame incremental repaint demo (with --screenshot + --eval).
  --cdp                       Start CDP WebSocket server on 127.0.0.1.
  --cdp-port <N>              CDP port (default 9222, with --cdp).
  --list-fonts                List system fonts and exit.
  --memory-stats              Print memory statistics.
```

`--gpu` is removed: every render path uses WebRender against a GPU
context (GLArea for GUI, EGL surfaceless for headless). Headless
without a GPU device fails closed with `unsupported.screenshot`.

**Stable error codes** (returned exactly as written):

| Code                       | When                                                       |
|----------------------------|------------------------------------------------------------|
| `unsupported.screenshot`   | Screenshot requested without offscreen renderer available   |
| `invalid-selector`         | Malformed `--extract-selector` input                       |

**CDP methods required** at v1.0:

- `Browser.getVersion`
- `Target.createTarget`, `Target.attachToTarget`
- `Page.navigate`, `Page.loadEventFired`
- `Runtime.evaluate`

---

## WPT harness — check types

The WPT harness asserts document state against fixture manifests. The
check types below are the public contract for fixture authors.

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
| `display-list-contains` | Stable display-list dump contains a substring            |
| `dom-nodes-range`       | DOM node count is within [min, max]                      |
| `ref-equivalent`        | Rendered page matches a reference HTML fixture           |

WPT target profile lives in [`COMPAT.md`](COMPAT.md). End-to-end CSS/DOM/layout
behavior should move into fixtures when practical; Rust tests cover pure logic
(URL parsing, cookie validation, CSP parsing, layout arithmetic, redb
round-trip) and low-level invariants.

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

The shell surfaces diagnostics in the status row; the WPT
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

## Display-list invariants

These are Vixen's paint rules, enforced by the display-list builder
before WebRender sees the commands. The same rules apply to every
surface (GUI and headless) because there is exactly one paint path.

1. **z-index stacking** — display list sorted negative → zero →
   positive z-index; viewport background always first; stable sort
   preserves document order for equal z-index.
2. **Clip stacking** — `overflow: hidden` clips content but not borders
   (CSS 2.1 § 11.1.1). `PushClip`/`PopClip` bracket content, not
   decorations.
3. **Opacity groups** — stack-based multiplication. Parent 0.5 × child
   0.5 = 0.25 effective. `opacity == 0` early-exit (no draw).
4. **Visibility** — `visibility: hidden` and `visibility: collapse`
   skip paint but keep layout space.
5. **Background clip** — `border-box` (no extra clip); `padding-box`
   and `content-box` emit `PushClip`/`PopClip` around background paint;
   `text` is post-v1.0.
6. **Background attachment** — `fixed` uses viewport-relative
   positioning; `scroll` and `local` use element-relative.
7. **Background origin** — positions the background image rect relative
   to border-box / padding-box / content-box per the property value.
8. **Empty clip skip** — any draw command with an empty
   pre-intersected clip is dropped before reaching WebRender.
