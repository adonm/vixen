import { chromium } from 'playwright-core';
import { spawn } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const fixture = path.join(root, 'fixtures', 'cdp', 'playwright-smoke.html');
const fixtureUrl = pathToFileURL(fixture).href;
const port = Number(process.env.VIXEN_CDP_PORT || 9322);
const wsEndpoint = `ws://127.0.0.1:${port}`;
const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function fail(message) {
  throw new Error(`playwright-cdp-smoke: ${message}`);
}

function assertPng(png, width, height, label) {
  if (png.length < 24 || !png.subarray(0, 8).equals(pngSignature)) {
    fail(`${label} did not return a PNG`);
  }
  const actualWidth = png.readUInt32BE(16);
  const actualHeight = png.readUInt32BE(20);
  if (actualWidth !== width || actualHeight !== height) {
    fail(`${label} dimensions were ${actualWidth}x${actualHeight}, expected ${width}x${height}`);
  }
}

function waitForServer(child) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('timed out waiting for CDP listener')), 30000);
    const onData = (chunk) => {
      const text = chunk.toString('utf8');
      process.stderr.write(text);
      if (text.includes(`CDP listening on ws://127.0.0.1:${port}`)) {
        clearTimeout(timeout);
        child.stderr.off('data', onData);
        resolve();
      }
    };
    child.stderr.on('data', onData);
    child.once('exit', (code, signal) => {
      clearTimeout(timeout);
      reject(new Error(`vixen-headless exited before CDP was ready: code=${code} signal=${signal}`));
    });
  });
}

async function evaluateValue(session, expression) {
  const result = await session.send('Runtime.evaluate', { expression });
  if (result.exceptionDetails) {
    fail(`Runtime.evaluate raised: ${JSON.stringify(result.exceptionDetails)}`);
  }
  return result.result?.value;
}

async function waitForValue(session, expression, predicate, label) {
  const deadline = Date.now() + 5000;
  let value;
  while (Date.now() < deadline) {
    value = await evaluateValue(session, expression);
    if (predicate(value)) return value;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  fail(`${label} did not settle: ${JSON.stringify(value)}`);
}

async function waitForCondition(predicate, label) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  fail(`${label} did not settle`);
}

async function stopServer(child) {
  if (child.exitCode !== null) return;
  child.kill('SIGTERM');
  await new Promise((resolve) => {
    const timer = setTimeout(() => {
      if (child.exitCode === null) child.kill('SIGKILL');
      resolve();
    }, 5000);
    child.once('exit', () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

async function waitForClickConsole(session, action) {
  const events = [];
  session.on('Runtime.consoleAPICalled', (event) => events.push(event));
  await action();
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const found = events.find((event) => event.args?.[0]?.value === 'playwright-click');
    if (found) return found;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  fail('did not receive Runtime.consoleAPICalled for click');
}

function codeForTextChar(char) {
  if (char === ' ') return 'Space';
  const upper = char.toUpperCase();
  if (/^[A-Z]$/.test(upper)) return `Key${upper}`;
  if (/^[0-9]$/.test(char)) return `Digit${char}`;
  return '';
}

async function typeText(session, text) {
  for (const char of text) {
    await session.send('Input.dispatchKeyEvent', {
      type: 'keyDown',
      key: char,
      code: codeForTextChar(char),
      text: char,
    });
    await session.send('Input.dispatchKeyEvent', {
      type: 'keyUp',
      key: char,
      code: codeForTextChar(char),
    });
  }
}

async function main() {
  const child = spawn('cargo', [
    'run', '-q', '-p', 'vixen-headless', '--',
    '--url', fixtureUrl,
    '--cdp',
    '--cdp-port', String(port),
  ], {
    cwd: root,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.on('data', (chunk) => process.stdout.write(chunk));

  let browser;
  try {
    await waitForServer(child);
    browser = await chromium.connectOverCDP(wsEndpoint, { timeout: 15000 });

    const context = browser.contexts()[0] || fail('Playwright did not expose a browser context');
    const page = context.pages()[0] || fail('Playwright did not expose a page target');
    const session = await context.newCDPSession(page);

    await session.send('Runtime.enable');
    await session.send('Page.enable');
    const lifecycleEvents = [];
    session.on('Page.lifecycleEvent', (event) => lifecycleEvents.push(event));
    await session.send('Page.setLifecycleEventsEnabled', { enabled: true });
    const networkEvents = [];
    for (const method of ['Network.requestWillBeSent', 'Network.responseReceived', 'Network.loadingFinished']) {
      session.on(method, (event) => networkEvents.push({ method, event }));
    }
    await session.send('Network.enable');
    const targets = await session.send('Target.getTargets');
    if (!Array.isArray(targets.targetInfos) || targets.targetInfos.length < 1) {
      fail('Target.getTargets returned no page targets');
    }

    await page.addInitScript(() => {
      globalThis.__playwrightInitScriptValue = 'vixen-init-script';
    });

    await session.send('Page.navigate', { url: fixtureUrl });
    await waitForCondition(
      () => ['Network.requestWillBeSent', 'Network.responseReceived', 'Network.loadingFinished'].every((method) => networkEvents.some((entry) => entry.method === method)),
      'Network navigation events',
    );
    const expectedLifecycle = ['init', 'commit', 'DOMContentLoaded', 'load'];
    await waitForCondition(
      () => expectedLifecycle.every((name) => lifecycleEvents.some((event) => event.name === name)),
      'Page lifecycle events',
    );
    const lifecycleNames = lifecycleEvents.slice(0, expectedLifecycle.length).map((event) => event.name);
    if (JSON.stringify(lifecycleNames) !== JSON.stringify(expectedLifecycle)) {
      fail(`Page lifecycle events were out of order: ${JSON.stringify(lifecycleNames)}`);
    }
    const requestEvent = networkEvents.find((entry) => entry.method === 'Network.requestWillBeSent')?.event;
    const responseEvent = networkEvents.find((entry) => entry.method === 'Network.responseReceived')?.event;
    const finishedEvent = networkEvents.find((entry) => entry.method === 'Network.loadingFinished')?.event;
    if (!requestEvent?.requestId || requestEvent.request?.url !== fixtureUrl || requestEvent.request?.method !== 'GET') {
      fail(`Network.requestWillBeSent did not describe the navigation: ${JSON.stringify(requestEvent)}`);
    }
    if (responseEvent?.requestId !== requestEvent.requestId || responseEvent.response?.status !== 200 || responseEvent.type !== 'Document') {
      fail(`Network.responseReceived did not match the navigation: ${JSON.stringify(responseEvent)}`);
    }
    if (finishedEvent?.requestId !== requestEvent.requestId) {
      fail(`Network.loadingFinished did not match the navigation: ${JSON.stringify(finishedEvent)}`);
    }
    const title = await evaluateValue(session, 'document.title');
    if (title !== 'Vixen CDP Playwright Smoke') {
      fail(`unexpected document title: ${JSON.stringify(title)}`);
    }

    await context.grantPermissions(['notifications']);
    const grantedPermission = await page.evaluate(() => navigator.permissions.query({ name: 'notifications' }).then((status) => status.state));
    if (grantedPermission !== 'granted') {
      fail(`Playwright context.grantPermissions() did not reach the runtime: ${JSON.stringify(grantedPermission)}`);
    }
    await context.clearPermissions();
    const resetPermission = await page.evaluate(() => navigator.permissions.query({ name: 'notifications' }).then((status) => status.state));
    if (resetPermission !== 'prompt') {
      fail(`Playwright context.clearPermissions() did not reset the runtime: ${JSON.stringify(resetPermission)}`);
    }

    await browser.startTracing(page, { categories: ['devtools.timeline'] });
    await session.send('Page.stopLoading');
    await session.send('Page.getLayoutMetrics');
    const traceBuffer = await browser.stopTracing();
    const trace = JSON.parse(traceBuffer.toString('utf8'));
    const traceNames = trace.traceEvents?.map((event) => event.name) || [];
    if (!traceNames.includes('Page.stopLoading') || !traceNames.includes('Page.getLayoutMetrics')) {
      fail(`Playwright browser tracing missed protocol events: ${JSON.stringify(traceNames)}`);
    }

    let stableError = '';
    try {
      await session.send('Vixen.missing');
    } catch (error) {
      stableError = error?.message || '';
    }
    if (!stableError.includes('cdp.method-not-found')) {
      fail(`unsupported CDP method did not return a stable error: ${JSON.stringify(stableError)}`);
    }

    const domDocument = await session.send('DOM.getDocument', { depth: 1 });
    const rootNodeId = domDocument.root?.nodeId;
    if (!rootNodeId || domDocument.root?.nodeType !== 9) {
      fail(`DOM.getDocument did not return a document root: ${JSON.stringify(domDocument)}`);
    }
    const domHit = await session.send('DOM.querySelector', { nodeId: rootNodeId, selector: '#hit' });
    if (!domHit.nodeId) {
      fail(`DOM.querySelector did not find #hit: ${JSON.stringify(domHit)}`);
    }
    const domForms = await session.send('DOM.querySelectorAll', { nodeId: rootNodeId, selector: 'form' });
    if (!Array.isArray(domForms.nodeIds) || domForms.nodeIds.length !== 2) {
      fail(`DOM.querySelectorAll did not return both forms: ${JSON.stringify(domForms)}`);
    }
    const domDescription = await session.send('DOM.describeNode', { nodeId: domHit.nodeId });
    if (domDescription.node?.localName !== 'button') {
      fail(`DOM.describeNode did not describe #hit as a button: ${JSON.stringify(domDescription)}`);
    }
    const domResolved = await session.send('DOM.resolveNode', { nodeId: domHit.nodeId });
    if (domResolved.object?.subtype !== 'node') {
      fail(`DOM.resolveNode did not return a node remote object: ${JSON.stringify(domResolved)}`);
    }
    const initScriptValue = await evaluateValue(session, 'globalThis.__playwrightInitScriptValue');
    if (initScriptValue !== 'vixen-init-script') {
      fail(`Playwright page.addInitScript() did not run on navigation: ${JSON.stringify(initScriptValue)}`);
    }

    const baselineScreenshot = await session.send('Page.captureScreenshot', {
      format: 'png',
      clip: { x: 0, y: 0, width: 160, height: 100, scale: 1 },
    });
    const baselinePng = Buffer.from(baselineScreenshot.data || '', 'base64');
    assertPng(baselinePng, 160, 100, 'baseline Page.captureScreenshot');

    const consoleEvent = await waitForClickConsole(session, async () => {
      await session.send('Input.dispatchMouseEvent', {
        type: 'mousePressed',
        x: 10,
        y: 10,
        button: 'left',
        buttons: 1,
      });
      await session.send('Input.dispatchMouseEvent', {
        type: 'mouseReleased',
        x: 10,
        y: 10,
        button: 'left',
        buttons: 0,
      });
    });
    if (consoleEvent.args?.[1]?.value !== 1) {
      fail(`unexpected click console payload: ${JSON.stringify(consoleEvent)}`);
    }

    const clicked = await evaluateValue(session, '__smokeClicks');
    if (clicked !== 1) {
      fail(`click did not update JS state: ${JSON.stringify(clicked)}`);
    }

    const status = await evaluateValue(session, "document.querySelector('#status').textContent");
    if (status !== 'clicked:1') {
      fail(`click textContent mutation did not reach DOM: ${JSON.stringify(status)}`);
    }

    const statusAttrs = await evaluateValue(session, "(() => document.querySelector('#status').classList.contains('clicked') + ':' + document.querySelector('#status').getAttribute('data-clicked') + ':' + document.querySelector('#status').style.width)()");
    if (statusAttrs !== 'true:1:140px') {
      fail(`click attribute/class/style mutations did not reach DOM: ${JSON.stringify(statusAttrs)}`);
    }

    const structural = await evaluateValue(session, "(() => document.querySelector('#dynamic').textContent + ':' + document.querySelector('#dynamic').className + ':' + (document.querySelector('#gone') === null) + ':' + document.querySelector('#dynamic-root').textContent)()");
    if (structural !== 'dynamic:1:badge:true:dynamic:1 ready') {
      fail(`click structural mutations did not reach DOM: ${JSON.stringify(structural)}`);
    }

    const dynamicNode = await session.send('DOM.querySelector', { nodeId: rootNodeId, selector: '#dynamic' });
    const removedNode = await session.send('DOM.querySelector', { nodeId: rootNodeId, selector: '#gone' });
    if (!dynamicNode.nodeId || removedNode.nodeId) {
      fail(`live DOM mutation was not reflected through CDP DOM: ${JSON.stringify({ dynamicNode, removedNode })}`);
    }

    const eventOrder = await evaluateValue(session, '__eventOrder.join(\'>\')');
    if (eventOrder !== 'document-capture>body-capture>target>body-bubble') {
      fail(`click event propagation order was wrong: ${JSON.stringify(eventOrder)}`);
    }

    const defaults = await evaluateValue(session, "(() => document.querySelector('#default-check').checked + ':' + String(globalThis.__submitSeen) + ':' + String(globalThis.__vixenLastFormSubmit))()");
    if (defaults !== 'true:smoke-form:undefined') {
      fail(`click default actions did not run correctly: ${JSON.stringify(defaults)}`);
    }

    const observer = await waitForValue(session, 'globalThis.__smokeObserver || null', (value) => typeof value === 'string', 'MutationObserver');
    for (const expected of ['attributes:class:status', 'attributes:data-clicked:status', 'childList::dynamic-root', 'attributes:checked:default-check']) {
      if (!observer.includes(expected)) {
        fail(`MutationObserver missed ${expected}: ${JSON.stringify(observer)}`);
      }
    }

    const screenshot = await session.send('Page.captureScreenshot', {
      format: 'png',
      clip: { x: 0, y: 0, width: 160, height: 100, scale: 1 },
    });
    const png = Buffer.from(screenshot.data || '', 'base64');
    assertPng(png, 160, 100, 'post-mutation Page.captureScreenshot');
    if (png.equals(baselinePng)) {
      fail('post-mutation Page.captureScreenshot did not repaint changed DOM');
    }

    const playwrightPng = await page.screenshot({
      clip: { x: 0, y: 0, width: 160, height: 100 },
      timeout: 5000,
    });
    assertPng(playwrightPng, 160, 100, 'Playwright page.screenshot()');

    await page.setViewportSize({ width: 500, height: 320 });
    const viewportInfo = await page.evaluate(() => `${innerWidth}x${innerHeight}:${document.documentElement.clientWidth}x${document.documentElement.clientHeight}:${matchMedia('(max-width: 600px)').matches}`);
    if (viewportInfo !== '500x320:500x320:true') {
      fail(`Playwright page.setViewportSize() did not update viewport globals: ${JSON.stringify(viewportInfo)}`);
    }
    const viewportMetrics = await session.send('Page.getLayoutMetrics');
    if (viewportMetrics.cssLayoutViewport?.clientWidth !== 500 || viewportMetrics.cssLayoutViewport?.clientHeight !== 320) {
      fail(`CDP layout metrics did not reflect viewport override: ${JSON.stringify(viewportMetrics.cssLayoutViewport)}`);
    }
    await page.setViewportSize({ width: 800, height: 600 });

    await page.emulateMedia({ media: 'print', colorScheme: 'dark' });
    const mediaInfo = await page.evaluate(() => `${matchMedia('screen').matches}:${matchMedia('print').matches}:${matchMedia('(prefers-color-scheme: dark)').matches}:${matchMedia('(prefers-color-scheme: light)').matches}`);
    if (mediaInfo !== 'false:true:true:false') {
      fail(`Playwright page.emulateMedia() did not update matchMedia(): ${JSON.stringify(mediaInfo)}`);
    }
    await page.emulateMedia({ media: 'screen', colorScheme: 'light' });

    const hitBox = await page.locator('#hit').boundingBox({ timeout: 5000 });
    if (!hitBox || hitBox.width <= 0 || hitBox.height <= 0) {
      fail(`Playwright locator.boundingBox() returned no hit box: ${JSON.stringify(hitBox)}`);
    }
    await page.locator('#hit').click({ timeout: 5000 });
    const highLevelStatus = await page.locator('#status').textContent({ timeout: 5000 });
    if (highLevelStatus !== 'clicked:2') {
      fail(`Playwright locator.click() did not update DOM: ${JSON.stringify(highLevelStatus)}`);
    }
    await page.evaluate(() => {
      globalThis.__playwrightHoverEvents = [];
      const status = document.querySelector('#status');
      for (const type of ['mouseover', 'mouseenter', 'mousemove']) {
        status.addEventListener(type, () => globalThis.__playwrightHoverEvents.push(type));
      }
    });
    await page.locator('#status').hover({ timeout: 5000 });
    const hoverEvents = await page.evaluate(() => globalThis.__playwrightHoverEvents.join('>'));
    for (const expected of ['mouseover', 'mouseenter', 'mousemove']) {
      if (!hoverEvents.split('>').includes(expected)) {
        fail(`Playwright locator.hover() missed ${expected}: ${JSON.stringify(hoverEvents)}`);
      }
    }
    await page.evaluate(() => {
      globalThis.__playwrightDoubleClickEvents = [];
      const status = document.querySelector('#status');
      for (const type of ['click', 'dblclick']) {
        status.addEventListener(type, (event) => globalThis.__playwrightDoubleClickEvents.push(`${type}:${event.detail}`));
      }
    });
    await page.locator('#status').dblclick({ timeout: 5000 });
    const doubleClickEvents = await page.evaluate(() => globalThis.__playwrightDoubleClickEvents.join('>'));
    if (doubleClickEvents !== 'click:1>click:2>dblclick:2') {
      fail(`Playwright locator.dblclick() event order was wrong: ${JSON.stringify(doubleClickEvents)}`);
    }
    await page.evaluate(() => {
      globalThis.__playwrightContextMenuEvents = [];
      const status = document.querySelector('#status');
      for (const type of ['mousedown', 'mouseup', 'contextmenu']) {
        status.addEventListener(type, (event) => globalThis.__playwrightContextMenuEvents.push(`${type}:${event.button}`));
      }
    });
    await page.locator('#status').click({ button: 'right', timeout: 5000 });
    const contextMenuEvents = await page.evaluate(() => globalThis.__playwrightContextMenuEvents.join('>'));
    if (contextMenuEvents !== 'mousedown:2>mouseup:2>contextmenu:2') {
      fail(`Playwright right-click contextmenu events were wrong: ${JSON.stringify(contextMenuEvents)}`);
    }
    await page.evaluate(() => {
      globalThis.__playwrightWheelEvents = [];
      document.querySelector('#status').addEventListener('wheel', (event) => globalThis.__playwrightWheelEvents.push(`${event.deltaX}:${event.deltaY}:${event.deltaMode}`));
    });
    const statusBoxForWheel = await page.locator('#status').boundingBox({ timeout: 5000 });
    if (!statusBoxForWheel) {
      fail('Playwright locator.boundingBox() returned no status box for wheel');
    }
    await page.mouse.move(statusBoxForWheel.x + 5, statusBoxForWheel.y + 5);
    await page.mouse.wheel(4, 25);
    const wheelEvents = await page.evaluate(() => globalThis.__playwrightWheelEvents.join('>'));
    if (wheelEvents !== '4:25:0') {
      fail(`Playwright page.mouse.wheel() events were wrong: ${JSON.stringify(wheelEvents)}`);
    }
    await page.evaluate(() => scrollTo(0, 0));

    const nestedBox = await page.locator('#nested-scroll').boundingBox({ timeout: 5000 });
    if (!nestedBox) {
      fail('Playwright locator.boundingBox() returned no nested scrollport');
    }
    const clippedMarkerBox = await page.locator('#nested-marker').boundingBox({ timeout: 5000 });
    if (!clippedMarkerBox) {
      fail('Playwright locator.boundingBox() returned no clipped nested marker');
    }
    const clippedMarkerHit = await page.evaluate(({ x, y }) => {
      const marker = document.querySelector('#nested-marker');
      const scroller = document.querySelector('#nested-scroll');
      return {
        hit: document.elementFromPoint(x, y)?.id || '',
        marker: marker.getBoundingClientRect().toJSON(),
        scroller: scroller.getBoundingClientRect().toJSON(),
        scrollTop: scroller.scrollTop,
      };
    }, {
      x: clippedMarkerBox.x + clippedMarkerBox.width / 2,
      y: clippedMarkerBox.y + clippedMarkerBox.height / 2,
    });
    if (clippedMarkerHit.hit === 'nested-marker') {
      fail(`document.elementFromPoint() exposed a descendant outside its overflow clip: ${JSON.stringify(clippedMarkerHit)}`);
    }
    await page.mouse.click(
      clippedMarkerBox.x + clippedMarkerBox.width / 2,
      clippedMarkerBox.y + clippedMarkerBox.height / 2,
    );
    if (await page.evaluate(() => globalThis.__nestedMarkerClicks) !== 0) {
      fail('CDP mouse input clicked a descendant outside its overflow clip');
    }
    await page.mouse.move(nestedBox.x + 10, nestedBox.y + 10);
    await page.mouse.wheel(0, 35);
    const nestedScroll = await page.evaluate(() => {
      const inner = document.querySelector('#nested-scroll');
      return `${inner.scrollTop}:${scrollY}:${globalThis.__nestedScrollEvents.join('>')}`;
    });
    if (nestedScroll !== '35:0:nested-scroll:false:false') {
      fail(`wheel did not prefer the nested scrollport: ${JSON.stringify(nestedScroll)}`);
    }

    await page.evaluate(() => {
      globalThis.__cancelNestedWheel = true;
      globalThis.__nestedScrollEvents = [];
    });
    await page.mouse.wheel(0, 20);
    const canceledNestedScroll = await page.evaluate(() => {
      const inner = document.querySelector('#nested-scroll');
      return `${inner.scrollTop}:${scrollY}:${globalThis.__nestedScrollEvents.length}`;
    });
    if (canceledNestedScroll !== '35:0:0') {
      fail(`preventDefault did not block nested scrolling: ${JSON.stringify(canceledNestedScroll)}`);
    }

    await page.evaluate(() => {
      globalThis.__cancelNestedWheel = false;
      document.querySelector('#nested-scroll').scrollTo(0, 1e9);
    });
    await page.evaluate(() => { globalThis.__nestedScrollEvents = []; });
    await page.mouse.wheel(0, 25);
    const chainedScroll = await page.evaluate(() => `${scrollY}:${globalThis.__nestedScrollEvents.length}`);
    if (chainedScroll !== '25:0') {
      fail(`wheel did not chain from nested boundary to root: ${JSON.stringify(chainedScroll)}`);
    }
    await page.evaluate(() => {
      scrollTo(0, 0);
      document.querySelector('#nested-scroll').scrollTo(0, 0);
    });
    await page.locator('#nested-marker').click({ timeout: 5000 });
    const nestedAutoScroll = await page.evaluate(() => {
      const inner = document.querySelector('#nested-scroll');
      return `${inner.scrollTop > 0}:${scrollY}:${globalThis.__nestedMarkerClicks}`;
    });
    if (nestedAutoScroll !== 'true:0:1') {
      fail(`locator.click() did not scroll the nearest container and click its target: ${JSON.stringify(nestedAutoScroll)}`);
    }
    await page.evaluate(() => document.querySelector('#nested-scroll').scrollTo(0, 0));

    const roleText = await page.getByRole('button', { name: 'Hit me' }).textContent({ timeout: 5000 });
    if (roleText !== 'Hit me') {
      fail(`Playwright getByRole() did not resolve button name: ${JSON.stringify(roleText)}`);
    }

    await page.getByLabel('Extra check').check({ timeout: 5000 });
    const extraChecked = await page.locator('#playwright-check').isChecked({ timeout: 5000 });
    if (extraChecked !== true) {
      fail(`Playwright getByLabel().check() did not update checkbox state: ${JSON.stringify(extraChecked)}`);
    }

    await page.evaluate(() => {
      const host = document.createElement('label');
      host.id = 'playwright-plan-host';
      host.append('Plan ');
      const select = document.createElement('select');
      select.id = 'playwright-plan';
      for (const [value, text] of [['free', 'Free'], ['pro', 'Pro']]) {
        const option = document.createElement('option');
        option.value = value;
        option.textContent = text;
        select.appendChild(option);
      }
      host.appendChild(select);
      document.body.appendChild(host);
    });
    const selectedPlan = await page.getByLabel('Plan').selectOption('pro', { timeout: 5000 });
    const planValue = await page.locator('#playwright-plan').inputValue({ timeout: 5000 });
    if (selectedPlan[0] !== 'pro' || planValue !== 'pro') {
      fail(`Playwright getByLabel().selectOption() did not update select state: ${JSON.stringify({ selectedPlan, planValue })}`);
    }
    await page.evaluate(() => {
      const host = document.querySelector('#playwright-plan-host');
      if (host?.parentNode) host.parentNode.removeChild(host);
    });

    await page.getByLabel('Typed name').fill('Probe', { timeout: 5000 });
    const highLevelInput = await page.locator('#typed-name').inputValue({ timeout: 5000 });
    if (highLevelInput !== 'Probe') {
      fail(`Playwright getByLabel().fill() did not update input value: ${JSON.stringify(highLevelInput)}`);
    }
    await page.getByLabel('Typed body').fill('Body probe', { timeout: 5000 });
    const highLevelTextArea = await page.locator('#typed-body').inputValue({ timeout: 5000 });
    if (highLevelTextArea !== 'Body probe') {
      fail(`Playwright getByLabel().fill() did not update textarea value: ${JSON.stringify(highLevelTextArea)}`);
    }
    await page.locator('#typed-name').click({ timeout: 5000 });
    await page.keyboard.press('Control+A');
    await page.keyboard.type('Keyed');
    const keyboardInput = await page.locator('#typed-name').inputValue({ timeout: 5000 });
    if (keyboardInput !== 'Keyed') {
      fail(`Playwright high-level keyboard input did not update focused input: ${JSON.stringify(keyboardInput)}`);
    }
    await page.keyboard.press('Control+A');
    await page.keyboard.insertText('é🦊');
    const insertedUnicode = await page.locator('#typed-name').inputValue({ timeout: 5000 });
    const insertedSelection = await page.locator('#typed-name').evaluate((element) => `${element.selectionStart}:${element.selectionEnd}`);
    if (insertedUnicode !== 'é🦊' || insertedSelection !== '3:3') {
      fail(`Playwright keyboard.insertText() did not preserve UTF-16 text state: ${JSON.stringify({ insertedUnicode, insertedSelection })}`);
    }

    await page.evaluate(() => {
      globalThis.__playwrightUploadEvents = [];
      const label = document.createElement('label');
      label.id = 'playwright-upload-label';
      label.append('Upload file ');
      const input = document.createElement('input');
      input.id = 'playwright-upload';
      input.type = 'file';
      input.name = 'upload';
      input.addEventListener('input', () => globalThis.__playwrightUploadEvents.push('input'));
      input.addEventListener('change', () => globalThis.__playwrightUploadEvents.push('change'));
      label.appendChild(input);
      document.body.appendChild(label);
    });
    await page.getByLabel('Upload file').setInputFiles(fixture, { timeout: 5000 });
    const uploadInfo = JSON.parse(await page.evaluate(() => {
      const input = document.querySelector('#playwright-upload');
      const file = input.files && input.files[0];
      return JSON.stringify({
        length: input.files ? input.files.length : 0,
        name: file ? file.name : '',
        type: file ? file.type : '',
        size: file ? file.size : 0,
        value: input.value,
        events: globalThis.__playwrightUploadEvents.join('>'),
      });
    }));
    if (uploadInfo.length !== 1 || uploadInfo.name !== path.basename(fixture) || uploadInfo.type !== 'text/html' || uploadInfo.size <= 0 || !uploadInfo.value.endsWith(path.basename(fixture)) || uploadInfo.events !== 'input>change') {
      fail(`Playwright locator.setInputFiles() did not update file input: ${JSON.stringify(uploadInfo)}`);
    }

    await session.send('Runtime.evaluate', {
      expression: "const name = document.querySelector('#typed-name'); name.focus(); name.select(); 'name-ready'",
    });
    await typeText(session, 'Vixen');
    await session.send('Runtime.evaluate', {
      expression: "const body = document.querySelector('#typed-body'); body.focus(); body.select(); 'body-ready'",
    });
    await typeText(session, 'Hello CDP');
    const formData = await evaluateValue(session, "(() => { const form = document.querySelector('#nav-form'); return new FormData(form).get('typed') + ':' + new FormData(form).get('body'); })()");
    if (formData !== 'Vixen:Hello CDP') {
      fail(`typed form data was wrong before submit: ${JSON.stringify(formData)}`);
    }

    await Promise.all([
      page.waitForURL(/playwright-next\.html\?typed=Vixen&body=Hello\+CDP$/, { timeout: 5000 }),
      page.locator('#nav-submit').click({ timeout: 5000 }),
    ]);
    const navigatedTitle = await page.title();
    if (navigatedTitle !== 'Vixen CDP Navigation Smoke') {
      fail(`typed form did not navigate: ${JSON.stringify(navigatedTitle)}`);
    }
    const query = await evaluateValue(session, 'location.search');
    if (query !== '?typed=Vixen&body=Hello+CDP') {
      fail(`typed form query was wrong after submit: ${JSON.stringify(query)}`);
    }

    await page.goBack({ waitUntil: 'load', timeout: 5000 });
    const backTitle = await page.title();
    if (backTitle !== 'Vixen CDP Playwright Smoke') {
      fail(`Playwright page.goBack() landed on wrong title: ${JSON.stringify(backTitle)}`);
    }
    await page.goForward({ waitUntil: 'load', timeout: 5000 });
    const forwardTitle = await page.title();
    if (forwardTitle !== 'Vixen CDP Navigation Smoke') {
      fail(`Playwright page.goForward() landed on wrong title: ${JSON.stringify(forwardTitle)}`);
    }
    await page.reload({ waitUntil: 'load', timeout: 5000 });
    const reloadTitle = await page.title();
    if (reloadTitle !== 'Vixen CDP Navigation Smoke') {
      fail(`Playwright page.reload() landed on wrong title: ${JSON.stringify(reloadTitle)}`);
    }

    await page.setContent('<!doctype html><title>Vixen CDP Set Content</title><main id="set-content">Hello setContent</main>', { waitUntil: 'load', timeout: 5000 });
    const setContentTitle = await page.title();
    const setContentText = await page.locator('#set-content').textContent({ timeout: 5000 });
    if (setContentTitle !== 'Vixen CDP Set Content' || setContentText !== 'Hello setContent') {
      fail(`Playwright page.setContent() did not replace content: ${JSON.stringify({ setContentTitle, setContentText })}`);
    }

    await page.addScriptTag({ content: 'globalThis.__playwrightAddedScriptTag = 7;' });
    const addedScriptTagValue = await page.evaluate(() => globalThis.__playwrightAddedScriptTag ?? null);
    if (addedScriptTagValue !== 7) {
      fail(`Playwright page.addScriptTag({ content }) did not execute: ${JSON.stringify(addedScriptTagValue)}`);
    }

    await page.addStyleTag({ content: '#set-content { width: 177px; }' });
    const addedStyleWidth = await page.locator('#set-content').evaluate((element) => getComputedStyle(element).width, { timeout: 5000 });
    if (addedStyleWidth !== '177px') {
      fail(`Playwright page.addStyleTag({ content }) did not affect computed style: ${JSON.stringify(addedStyleWidth)}`);
    }

    let exposedPayload = null;
    await page.exposeFunction('fromHost', (value) => {
      exposedPayload = value;
      return `echo:${value}`;
    });
    await page.evaluate(() => {
      globalThis.fromHost('vixen-binding');
      globalThis.__playwrightExposedFunctionQueued = true;
    });
    const exposedQueued = await page.evaluate(() => globalThis.__playwrightExposedFunctionQueued === true);
    if (exposedQueued !== true) {
      fail('Playwright page.exposeFunction() did not install a callable function');
    }
    const exposedDeadline = Date.now() + 5000;
    while (exposedPayload !== 'vixen-binding' && Date.now() < exposedDeadline) {
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    if (exposedPayload !== 'vixen-binding') {
      fail(`Playwright page.exposeFunction() did not receive binding payload: ${JSON.stringify(exposedPayload)}`);
    }

    const extraPage = await context.newPage();
    if (extraPage.isClosed()) {
      fail('Playwright context.newPage() returned a closed page');
    }
    await extraPage.close();
    if (!extraPage.isClosed()) {
      fail('Playwright page.close() did not close the created page');
    }

    const objectHandle = await page.evaluateHandle(() => ({ answer: 42 }));
    const answerHandle = await objectHandle.getProperty('answer');
    const answerValue = await answerHandle.jsonValue();
    if (answerValue !== 42) {
      fail(`Playwright JSHandle.getProperty() did not read object property: ${JSON.stringify(answerValue)}`);
    }
    await answerHandle.dispose();
    await objectHandle.dispose();

    const dialogPromise = new Promise((resolve) => {
      page.once('dialog', async (dialog) => {
        const info = `${dialog.type()}:${dialog.message()}`;
        await dialog.accept();
        resolve(info);
      });
    });
    await page.evaluate(() => alert('hello dialog'));
    const dialogInfo = await dialogPromise;
    if (dialogInfo !== 'alert:hello dialog') {
      fail(`Playwright dialog event was wrong: ${JSON.stringify(dialogInfo)}`);
    }

    console.log('playwright-cdp-smoke ok');
  } finally {
    if (browser) await browser.close().catch(() => {});
    await stopServer(child);
  }
}

main().catch((err) => {
  console.error(err?.stack || err);
  process.exit(1);
});
