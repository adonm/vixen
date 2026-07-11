# CDP / Playwright smoke

Run the committed smoke with:

```sh
mise install
just cdp-playwright-smoke
```

The script starts `vixen-headless --cdp`, connects with `playwright-core`, and
exercises:

1. Playwright's `chromium.connectOverCDP(...)` handshake.
2. `Runtime.enable`, `Page.enable`, `Network.enable`, `Target.getTargets`,
   `Page.getFrameTree`.
3. `Page.navigate` to a local fixture with a button click listener, including
   Playwright `page.addInitScript()` execution before page scripts.
4. `Runtime.evaluate` / `Runtime.awaitPromise` for DOM reads and promise handles.
5. CDP DOM query plumbing: `DOM.getDocument`, `DOM.querySelector`,
   `DOM.querySelectorAll`, `DOM.describeNode`, and `DOM.resolveNode`.
6. Top-level navigation network notifications: `Network.requestWillBeSent`,
   `Network.responseReceived`, and `Network.loadingFinished`; lifecycle opt-in
   observes `init` / `commit` / `DOMContentLoaded` / `load`.
7. `Input.dispatchMouseEvent` with `mousePressed` then `mouseReleased` over the button.
8. The click handler mutates `textContent`, attributes/classes, inline style,
   and a small `createElement`/`appendChild`/`removeChild`/`replaceChildren`
   structural subtree; later `Runtime.evaluate` calls read those mutations back.
9. Observe `Runtime.consoleAPICalled`, then call `Page.captureScreenshot` (`png`)
   and Playwright's high-level `page.screenshot()` path.
10. Playwright `page.setViewportSize()` plus CDP `Page.getLayoutMetrics` viewport
   reporting, page-level viewport globals, and `page.emulateMedia()` updates to
   `matchMedia()` for media type/color scheme.
11. Exercise high-level locator geometry/click/fill APIs over Vixen's minimal
    `DOM.describeNode` / `DOM.resolveNode` / `DOM.getContentQuads` backing.
    The smoke also covers `locator.hover()` mouse lifecycle events,
    `locator.dblclick()` click/detail ordering, right-click `contextmenu`,
    `page.mouse.wheel()` wheel-event deltas,
    `getByRole()` button lookup by accessible text, `getByLabel()`
    lookup/check/select/fill through DOM label/control associations, high-level
    Playwright keyboard input against a clicked form control, and
    `locator.setInputFiles()` against a file input.
12. Submit a form through Playwright's high-level locator click and wait for the
    resulting URL/title navigation.
13. Traverse session history with Playwright `page.goBack()` / `page.goForward()`
    and refresh with `page.reload()`.
14. Replace document content with Playwright `page.setContent()`.
15. Execute/apply dynamic inline scripts and styles inserted by Playwright
    `page.addScriptTag()` / `page.addStyleTag()`.
16. Deliver exposed function calls through Playwright `page.exposeFunction()`.
17. Create and close additional pages with Playwright `context.newPage()` /
    `page.close()`.
18. Read object properties through Playwright `JSHandle.getProperty()`.
19. Surface modal dialogs through Playwright's `dialog` event.
20. Replace and clear browser-context permission grants through Playwright
    `context.grantPermissions()` / `context.clearPermissions()`, with runtime
    `PermissionStatus` reads observing the override without rewriting profile
    decisions.
21. Start/stop Chromium tracing through Playwright `browser.startTracing()` /
    `browser.stopTracing()`, read the bounded JSON trace through `IO.read`, and
    verify stable `cdp.method-not-found` errors for unsupported methods.

Rust CDP tests additionally cover browser-shaped automation methods used by
Playwright/DevTools probes: idle `Page.stopLoading`,
`Page.resetNavigationHistory`,
`Page.getResourceTree`, `Page.getResourceContent`, `Page.setBypassCSP` as a
CDP-scoped script-CSP override for later navigations,
`Network.setCacheDisabled` bypassing runtime `fetch()` cache reads/writes,
`Network.setBypassServiceWorker`,
`Network.setExtraHTTPHeaders` propagation into runtime `fetch()`,
`Performance.getMetrics`, `Security.getSecurityState`, and DOM
attribute/`outerHTML` read-write methods, exact/wildcard permission override
scopes, detached-session rejection, stable protocol error data, and the bounded
4,096-event tracing buffer.

Current limits are intentional: one main frame per independently scripted target,
PNG screenshots only, Chromium JSON tracing only (not Playwright context trace
archives), and full-viewport mouse hit testing. The WebSocket path has one
BrowserCore event pump and keeps reading while navigation-producing requests are
pending. Gated real-socket tests prove same-connection `Page.stopLoading`
cancellation for page, history, and multi-action runtime navigations, clean later
work after cancellation, and unrelated command handling during target creation.
Configured initial-URL loading intentionally settles before socket acceptance.
Add methods only when this smoke shows a real automation gap.
