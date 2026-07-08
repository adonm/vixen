import { chromium } from 'playwright-core';
import { spawn } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const fixture = path.join(root, 'fixtures', 'cdp', 'playwright-smoke.html');
const fixtureUrl = pathToFileURL(fixture).href;
const port = Number(process.env.VIXEN_CDP_PORT || 9322);
const wsEndpoint = `ws://127.0.0.1:${port}`;

function fail(message) {
  throw new Error(`playwright-cdp-smoke: ${message}`);
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
    const targets = await session.send('Target.getTargets');
    if (!Array.isArray(targets.targetInfos) || targets.targetInfos.length < 1) {
      fail('Target.getTargets returned no page targets');
    }

    await session.send('Page.navigate', { url: fixtureUrl });
    const title = await session.send('Runtime.evaluate', { expression: 'document.title' });
    if (title.result?.value !== 'Vixen CDP Playwright Smoke') {
      fail(`unexpected document title: ${JSON.stringify(title)}`);
    }

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

    const clicked = await session.send('Runtime.evaluate', { expression: '__smokeClicks' });
    if (clicked.result?.value !== 1) {
      fail(`click did not update JS state: ${JSON.stringify(clicked)}`);
    }

    const screenshot = await session.send('Page.captureScreenshot', {
      format: 'png',
      clip: { x: 0, y: 0, width: 160, height: 100, scale: 1 },
    });
    const png = Buffer.from(screenshot.data || '', 'base64');
    const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    if (!png.subarray(0, 8).equals(signature)) {
      fail('Page.captureScreenshot did not return a PNG');
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
