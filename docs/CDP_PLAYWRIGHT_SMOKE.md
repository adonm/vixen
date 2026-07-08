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
3. `Page.navigate` to a local fixture with a button click listener.
4. `Runtime.evaluate` for DOM reads.
5. `Input.dispatchMouseEvent` with `mousePressed` then `mouseReleased` over the button.
6. Observe `Runtime.consoleAPICalled`, then call `Page.captureScreenshot` (`png`).

Current limits are intentional: one target, one main frame, PNG screenshots only,
and full-viewport mouse hit testing. Add methods only when this smoke shows a real
automation gap.
