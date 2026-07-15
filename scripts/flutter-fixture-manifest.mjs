import { spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { inflateSync } from 'node:zlib';
import { chromium } from 'playwright-core';
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const manifestPath = path.join(root, 'fixtures', 'manifest.json');
const app = process.env.VIXEN_CDP_APP;
const port = Number(process.env.VIXEN_CDP_PORT || 9324);
const endpoint = `ws://127.0.0.1:${port}`;
const viewport = { width: 800, height: 600 };
const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const renderedTypes = new Set(['layout-box', 'visual-hash', 'ref-equivalent']);

function fail(message) {
  throw new Error(`flutter-fixture-manifest: ${message}`);
}

function decodePng(png) {
  if (png.length < 24 || !png.subarray(0, 8).equals(pngSignature)) {
    fail('renderer capture is not a PNG');
  }
  const width = png.readUInt32BE(16);
  const height = png.readUInt32BE(20);
  if (png[24] !== 8 || png[25] !== 6 || png[26] !== 0 || png[27] !== 0 || png[28] !== 0) {
    fail('renderer capture is not non-interlaced RGBA8 PNG');
  }
  const compressed = [];
  let offset = 8;
  while (offset + 12 <= png.length) {
    const length = png.readUInt32BE(offset);
    const type = png.subarray(offset + 4, offset + 8).toString('ascii');
    const end = offset + 12 + length;
    if (end > png.length) fail('PNG chunk exceeds capture');
    if (type === 'IDAT') compressed.push(png.subarray(offset + 8, offset + 8 + length));
    offset = end;
    if (type === 'IEND') break;
  }
  const filtered = inflateSync(Buffer.concat(compressed));
  const stride = width * 4;
  if (filtered.length !== height * (stride + 1)) fail('PNG scanline size mismatch');
  const rgba = Buffer.alloc(width * height * 4);
  let source = 0;
  for (let y = 0; y < height; y++) {
    const filter = filtered[source++];
    const row = Buffer.from(filtered.subarray(source, source + stride));
    source += stride;
    for (let x = 0; x < stride; x++) {
      const left = x >= 4 ? row[x - 4] : 0;
      const above = y ? rgba[(y - 1) * stride + x] : 0;
      const upperLeft = y && x >= 4 ? rgba[(y - 1) * stride + x - 4] : 0;
      if (filter === 1) row[x] = (row[x] + left) & 255;
      else if (filter === 2) row[x] = (row[x] + above) & 255;
      else if (filter === 3) row[x] = (row[x] + Math.floor((left + above) / 2)) & 255;
      else if (filter === 4) {
        const estimate = left + above - upperLeft;
        const pa = Math.abs(estimate - left);
        const pb = Math.abs(estimate - above);
        const pc = Math.abs(estimate - upperLeft);
        const predictor = pa <= pb && pa <= pc ? left : pb <= pc ? above : upperLeft;
        row[x] = (row[x] + predictor) & 255;
      } else if (filter !== 0) fail(`unsupported PNG filter ${filter}`);
    }
    row.copy(rgba, y * stride);
  }
  return { width, height, rgba };
}

function visualHash({ width, height, rgba }) {
  if (!width || !height || rgba.length !== width * height * 4) fail('invalid RGBA capture');
  const means = [];
  for (let cellY = 0; cellY < 8; cellY++) {
    const y0 = Math.floor(cellY * height / 8);
    const y1 = Math.max(y0 + 1, Math.floor((cellY + 1) * height / 8));
    for (let cellX = 0; cellX < 8; cellX++) {
      const x0 = Math.floor(cellX * width / 8);
      const x1 = Math.max(x0 + 1, Math.floor((cellX + 1) * width / 8));
      let total = 0;
      let count = 0;
      for (let y = y0; y < y1; y++) {
        for (let x = x0; x < x1; x++) {
          const i = (y * width + x) * 4;
          const alpha = rgba[i + 3];
          const composite = (channel) => Math.floor((channel * alpha + 255 * (255 - alpha) + 127) / 255);
          const red = composite(rgba[i]);
          const green = composite(rgba[i + 1]);
          const blue = composite(rgba[i + 2]);
          total += Math.floor((77 * red + 150 * green + 29 * blue + 128) / 256);
          count++;
        }
      }
      means.push(Math.floor(total / count));
    }
  }
  const threshold = Math.floor(means.reduce((sum, value) => sum + value, 0) / 64);
  let bits = 0n;
  for (let i = 0; i < means.length; i++) {
    if (means[i] > threshold) bits |= 1n << BigInt(63 - i);
  }
  return bits;
}

function parseVisualHash(value) {
  const match = /^([0-9a-fA-F]{16})(?:@(\d+))?$/.exec(value);
  if (!match) fail(`invalid visual hash ${JSON.stringify(value)}`);
  const tolerance = match[2] === undefined ? 1 : Number(match[2]);
  if (tolerance < 0 || tolerance > 64) fail(`invalid visual hash tolerance ${tolerance}`);
  return { bits: BigInt(`0x${match[1]}`), tolerance };
}

function hamming(left, right) {
  let value = left ^ right;
  let count = 0;
  while (value) {
    count += Number(value & 1n);
    value >>= 1n;
  }
  return count;
}

function formatHash(bits, tolerance = 1) {
  return `${bits.toString(16).padStart(16, '0')}@${tolerance}`;
}

function waitForServer(child) {
  return new Promise((resolve, reject) => {
    let ready = false;
    const timer = setTimeout(() => reject(new Error('timed out waiting for Flutter CDP host')), 30000);
    const onData = (chunk) => {
      const text = chunk.toString('utf8');
      process.stderr.write(text);
      if (!ready && text.includes(`CDP listening on ws://127.0.0.1:${port}`)) {
        ready = true;
        clearTimeout(timer);
        resolve();
      }
    };
    child.stderr.on('data', onData);
    child.once('exit', (code, signal) => {
      clearTimeout(timer);
      reject(new Error(`Flutter CDP host exited before ready: code=${code} signal=${signal}`));
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

async function displayValue(page, expression) {
  return page.evaluate((source) => {
    const value = (0, eval)(source);
    if (typeof value === 'number' && Object.is(value, -0)) return '0';
    return String(value);
  }, expression);
}

async function queryElements(session, selector, includeLayout = false) {
  const result = await session.send('Vixen.querySelectorAll', {
    selector,
    includeLayout,
  });
  return result.elements;
}

async function pageSnapshot(session) {
  return session.send('Vixen.getSnapshot');
}

function fixtureUrl(relativePath) {
  return pathToFileURL(path.join(root, relativePath)).href;
}

function displayMatches(actual, expected) {
  return actual === expected ||
    (expected.startsWith('fixtures/') && actual === fixtureUrl(expected));
}

async function runCheck({ check, fixture, page, session, context, referencePage }) {
  switch (check.type) {
    case 'title': {
      const actual = (await pageSnapshot(session)).title;
      if (actual !== check.expected) throw new Error(`expected title ${JSON.stringify(check.expected)}, got ${JSON.stringify(actual)}`);
      return;
    }
    case 'selector-count': {
      const actual = (await queryElements(session, check.selector)).length;
      if (actual !== check.expected) throw new Error(`expected ${check.expected} matches, got ${actual}`);
      return;
    }
    case 'selectors-exact': {
      const actual = (await queryElements(session, check.selector))
        .map((element) => element.id).filter(Boolean).sort();
      const expected = [...check.expected].sort();
      if (JSON.stringify(actual) !== JSON.stringify(expected)) {
        throw new Error(`expected ids ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
      }
      return;
    }
    case 'body-contains': {
      const actual = (await pageSnapshot(session)).textContent;
      if (!actual?.includes(check.expected)) throw new Error(`body does not contain ${JSON.stringify(check.expected)}`);
      return;
    }
    case 'js-eval':
    case 'flutter-js-eval': {
      const actual = (await session.send('Vixen.evaluate', {
        expression: check.expr,
      })).value;
      if (!displayMatches(actual, check.expected)) throw new Error(`expected ${JSON.stringify(check.expected)}, got ${JSON.stringify(actual)}`);
      return;
    }
    case 'min-nodes': {
      const actual = (await pageSnapshot(session)).elementCount;
      if (actual < check.min) throw new Error(`expected at least ${check.min} nodes, got ${actual}`);
      return;
    }
    case 'dom-nodes-range': {
      const actual = (await pageSnapshot(session)).elementCount;
      if (actual < check.min || actual > check.max) throw new Error(`expected ${check.min}..=${check.max} nodes, got ${actual}`);
      return;
    }
    case 'no-critical-diagnostics': {
      const result = await session.send('Vixen.getDiagnostics');
      if (result.diagnostics.length) throw new Error(`expected no diagnostics, got ${JSON.stringify(result.diagnostics)}`);
      return;
    }
    case 'selector-match': {
      const actual = (await queryElements(session, check.selector))
        .map((element) => element.tag.toLowerCase());
      if (JSON.stringify(actual) !== JSON.stringify(check.expected)) {
        throw new Error(`expected tags ${JSON.stringify(check.expected)}, got ${JSON.stringify(actual)}`);
      }
      return;
    }
    case 'computed-style': {
      const element = (await queryElements(session, check.selector))[0];
      if (!element) throw new Error('selector matched nothing');
      const result = await session.send('Vixen.getComputedStyle', {
        nodeId: element.nodeId,
      });
      const actual = result.styles.find(([property]) => property === check.property)?.[1];
      if (actual !== check.expected) throw new Error(`expected ${check.property}=${JSON.stringify(check.expected)}, got ${JSON.stringify(actual)}`);
      return;
    }
    case 'element-attribute': {
      const element = (await queryElements(session, check.selector))[0];
      if (!element) throw new Error('selector matched nothing');
      const actual = element.attributes.find(([name]) => name === check.attribute)?.[1] ?? null;
      if (actual !== check.expected) throw new Error(`expected ${check.attribute}=${JSON.stringify(check.expected)}, got ${JSON.stringify(actual)}`);
      return;
    }
    case 'layout-box': {
      const element = (await queryElements(session, check.selector, true))[0];
      if (!element?.layout) throw new Error('element has no Flutter commit geometry');
      const values = element.layout;
      const tolerance = check.tolerance ?? 0.1;
      if (values.some((value, index) => Math.abs(value - check.expected[index]) > tolerance)) {
        throw new Error(`expected ${JSON.stringify(check.expected)} ±${tolerance}, got ${JSON.stringify(values)}`);
      }
      return;
    }
    case 'visual-hash': {
      const capture = decodePng(await page.screenshot({ timeout: 20000 }));
      if (capture.width !== viewport.width || capture.height !== viewport.height) {
        throw new Error(`expected ${viewport.width}x${viewport.height}, got ${capture.width}x${capture.height}`);
      }
      const actual = visualHash(capture);
      if (process.env.VIXEN_PRINT_VISUAL_HASHES === '1') {
        console.log(`VISUAL ${fixture.url} ${formatHash(actual)}`);
      }
      const expected = parseVisualHash(check.expected);
      const distance = hamming(actual, expected.bits);
      if (distance > expected.tolerance) {
        throw new Error(`expected ${check.expected}, got ${formatHash(actual)} (distance ${distance})`);
      }
      return;
    }
    case 'ref-equivalent': {
      const testCapture = decodePng(await page.screenshot({ timeout: 20000 }));
      const referenceUrl = path.posix.join(path.posix.dirname(fixture.url), check.reference);
      if (!referencePage) throw new Error('reference target is unavailable');
      await referencePage.goto(fixtureUrl(referenceUrl), { waitUntil: 'load', timeout: 35000 });
      const referenceCapture = decodePng(await referencePage.screenshot({ timeout: 20000 }));
      if (testCapture.width !== referenceCapture.width ||
          testCapture.height !== referenceCapture.height ||
          !testCapture.rgba.equals(referenceCapture.rgba)) {
        throw new Error(`Flutter scene differs from reference ${referenceUrl}`);
      }
      return;
    }
    default:
      fail(`unsupported manifest check type ${check.type}`);
  }
}

async function main() {
  if (!app) fail('VIXEN_CDP_APP is required');
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  if (!Array.isArray(manifest.fixtures) || !manifest.fixtures.length) fail('manifest has no fixtures');
  const initialUrl = pathToFileURL(path.join(root, manifest.fixtures[0].url)).href;
  const child = spawn(app, [
    '--vixen-cdp-automation',
    `--vixen-url=${initialUrl}`,
    `--vixen-viewport=${viewport.width}x${viewport.height}`,
    `--vixen-cdp-port=${port}`,
  ], { cwd: root, stdio: ['ignore', 'pipe', 'pipe'], env: process.env });
  child.stdout.on('data', (chunk) => process.stdout.write(chunk));

  let browser;
  let checks = 0;
  let renderedChecks = 0;
  const failures = [];
  try {
    await waitForServer(child);
    browser = await chromium.connectOverCDP(endpoint, { timeout: 15000 });
    const context = browser.contexts()[0] || fail('missing browser context');
    const initialPage = context.pages()[0] || fail('missing initial target');

    for (const fixture of manifest.fixtures) {
      const page = await context.newPage();
      let referencePage;
      try {
        await page.setViewportSize(viewport);
        await page.goto(fixtureUrl(fixture.url), { waitUntil: 'load', timeout: 35000 });
        const session = await context.newCDPSession(page);
        if (fixture.checks.some((check) => check.type === 'ref-equivalent')) {
          referencePage = await context.newPage();
          await referencePage.setViewportSize(viewport);
        }
        for (const check of fixture.checks) {
          checks++;
          if (renderedTypes.has(check.type)) renderedChecks++;
          try {
            await runCheck({ check, fixture, page, session, context, referencePage });
          } catch (error) {
            const failure = `${fixture.url} ${check.type}: ${error.message}`;
            failures.push(failure);
            console.error(`FAIL ${failure}`);
          }
        }
      } finally {
        if (referencePage) await referencePage.close().catch(() => {});
        await page.close().catch(() => {});
      }
    }
    await initialPage.bringToFront().catch(() => {});

    if (failures.length) {
      console.error(`flutter fixture manifest failed: fixtures=${manifest.fixtures.length} checks=${checks} rendered=${renderedChecks} failures=${failures.length}`);
      process.exitCode = 1;
    } else {
      console.log(`flutter fixture manifest ok: fixtures=${manifest.fixtures.length} checks=${checks} rendered=${renderedChecks}`);
    }
  } finally {
    if (browser) await browser.close().catch(() => {});
    await stopServer(child);
  }
}

main().catch((error) => {
  console.error(error?.stack || error);
  process.exit(1);
});
