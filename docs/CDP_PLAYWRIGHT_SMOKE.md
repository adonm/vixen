# CDP / Playwright smoke

Run the committed smoke with:

```sh
mise install
just cdp-playwright-smoke
```

The script starts `vixen-headless --cdp`, connects with `playwright-core`, and
exercises:

1. Playwright's `chromium.connectOverCDP(...)` handshake.
2. `Runtime.enable`, `Page.enable`, `Target.getTargets`, `Page.getFrameTree`.
3. `Page.navigate` to a local fixture with a button click listener, including
   Playwright `page.addInitScript()` execution before page scripts.
4. `Runtime.evaluate` for DOM reads.
5. `Input.dispatchMouseEvent` with `mousePressed` then `mouseReleased` over the button.
6. The click handler mutates `textContent`, attributes/classes, inline style,
   and a small `createElement`/`appendChild`/`removeChild`/`replaceChildren`
   structural subtree; later `Runtime.evaluate` calls read those mutations back.
7. Observe `Runtime.consoleAPICalled`, then call `Page.captureScreenshot` (`png`)
   and Playwright's high-level `page.screenshot()` path.
8. Playwright `page.setViewportSize()` plus CDP `Page.getLayoutMetrics` viewport
   reporting, page-level viewport globals, and `page.emulateMedia()` updates to
   `matchMedia()` for media type/color scheme.
9. Exercise high-level locator geometry/click/fill APIs over Vixen's minimal
   `DOM.describeNode` / `DOM.resolveNode` / `DOM.getContentQuads` backing.
   The smoke also covers `locator.hover()` mouse lifecycle events,
   `locator.dblclick()` click/detail ordering, right-click `contextmenu`,
   `page.mouse.wheel()` wheel-event deltas,
   `getByRole()` button lookup by accessible text, `getByLabel()`
   lookup/check/select/fill through DOM label/control associations, high-level
   Playwright keyboard input against a clicked form control, and
   `locator.setInputFiles()` against a file input.
10. Submit a form through Playwright's high-level locator click and wait for the
    resulting URL/title navigation.
11. Traverse session history with Playwright `page.goBack()` / `page.goForward()`
    and refresh with `page.reload()`.
12. Replace document content with Playwright `page.setContent()`.
13. Execute/apply dynamic inline scripts and styles inserted by Playwright
    `page.addScriptTag()` / `page.addStyleTag()`.
14. Deliver exposed function calls through Playwright `page.exposeFunction()`.
15. Create and close additional pages with Playwright `context.newPage()` /
    `page.close()`.
16. Read object properties through Playwright `JSHandle.getProperty()`.
17. Surface modal dialogs through Playwright's `dialog` event.

Current limits are intentional: one active scripted page, one main frame, PNG screenshots only,
and full-viewport mouse hit testing. Add methods only when this smoke shows a real
automation gap.
