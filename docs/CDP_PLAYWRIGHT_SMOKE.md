# CDP / Playwright smoke

Run the rendered product smoke with:

```sh
mise install
just flutter-cdp-playwright-smoke
```

Cage launches the release Flutter executable in chrome-less CDP mode. That host
owns the sole BrowserCore, one `vixen-cdp` subscriber, and the same formatter and
commit painter used by the GUI. There is no native rendered Playwright smoke.

The smoke proves:

1. `playwright-core` connects through `chromium.connectOverCDP(...)`.
2. Target/page/runtime/network/DOM enable and navigation methods route to the
   selected BrowserCore context.
3. Playwright obtains layout from an exact Flutter commit.
4. One stable live `DOMStringMap` write reflects to `data-layout-mode`, advances
the normal mutation/cascade path, returns 140×32 synchronous page geometry,
then matches CDP DOM attributes/geometry and a pinned distinct Flutter PNG.
5. One retained live `classList` survives the pointer-driven `class` mutation,
   reflects `clicked`, agrees with 140px synchronous/CDP geometry, and produces
   a second pinned exact Flutter PNG.
6. One retained live `relList` survives external and list-driven `rel` writes on
   a real anchor, reflects ordered tokens, agrees with 120×32 synchronous/CDP
   geometry, and produces a third pinned exact Flutter PNG without changing the
   earlier hashes.
7. One retained iframe `sandbox` list survives external and list-driven writes,
   reflects valid ordered tokens, agrees with 120×32 synchronous/CDP geometry,
   and produces a fourth pinned exact Flutter PNG without changing earlier
   hashes.
8. One retained inline `CSSStyleDeclaration` survives external and declaration
   API writes, reflects current serialized declarations, agrees with 120×32
   synchronous/CDP geometry, and produces a fifth pinned exact Flutter PNG.
9. One retained `NamedNodeMap` and attached `Attr` survive external and
   `Attr.value` writes, preserve indexed/named identity, agree with 120×32
   synchronous/CDP state, and produce a sixth pinned exact Flutter PNG.
10. Retained structural collections start empty, reflect the pointer-created
    rendered `#dynamic.badge` through indexed/named access, and agree with an
    authoritative CDP node while a preexisting selector-all list remains static.
11. Pointer input uses the displayed commit's Flutter hit-test handle and target;
   the C ABI has no raw coordinate command.
12. Later DOM/style mutation produces a new source revision and distinct exact scene.
13. `Page.captureScreenshot` and high-level screenshot return direct Flutter scene
   PNGs without browser/compositor chrome.
14. Simultaneous 320×240 and 480×300 targets keep source, viewport, input, and
   scene state independent.
15. Switching targets does not lose presentation state.
16. Forced renderer reset requests a full snapshot and recovers a byte-identical
   scene.
17. Runtime, network, permissions, tracing, history, dialog, form/text input, and
    stable protocol-error slices remain available through the shared CDP core.

Native `vixen-headless --cdp` is text/runtime-only. Screenshot, layout geometry,
and pointer hit testing fail closed there. Rust CDP tests cover dispatcher,
session, lifecycle, cancellation, network, profile, and runtime behavior without
inventing renderer output.

Add methods only when this smoke or the Flutter fixture manifest demonstrates a
real product gap.
