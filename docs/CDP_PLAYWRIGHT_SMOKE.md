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
5. Pointer input uses the displayed commit's Flutter hit-test handle and target;
   the C ABI has no raw coordinate command.
6. Later DOM/style mutation produces a new source revision and distinct exact scene.
7. `Page.captureScreenshot` and high-level screenshot return direct Flutter scene
   PNGs without browser/compositor chrome.
8. Simultaneous 320×240 and 480×300 targets keep source, viewport, input, and
   scene state independent.
9. Switching targets does not lose presentation state.
10. Forced renderer reset requests a full snapshot and recovers a byte-identical
   scene.
11. Runtime, network, permissions, tracing, history, dialog, form/text input, and
    stable protocol-error slices remain available through the shared CDP core.

Native `vixen-headless --cdp` is text/runtime-only. Screenshot, layout geometry,
and pointer hit testing fail closed there. Rust CDP tests cover dispatcher,
session, lifecycle, cancellation, network, profile, and runtime behavior without
inventing renderer output.

Add methods only when this smoke or the Flutter fixture manifest demonstrates a
real product gap.