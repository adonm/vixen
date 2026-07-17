import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { inflateSync } from 'node:zlib';
import { chromium } from 'playwright-core';
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const fixture = path.join(root, 'fixtures', 'cdp', 'playwright-smoke.html');
const fixtureUrl = pathToFileURL(fixture).href;
const app = process.env.VIXEN_CDP_APP;
const port = Number(process.env.VIXEN_CDP_PORT || 9323);
const endpoint = `ws://127.0.0.1:${port}`;
const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const expectedDatasetBaseline = '08ad4b805cd6d2be56fae95dc70660206cdac94b1c0776d52b1d0eb03bd32dce';
const expectedDatasetMutation = '4b8654191f7e9f4eb95486eb34bbb689d2153f4d2484cfaa617d2fb7075b1a24';
const expectedClassListMutation = '5633ca7a032c8c6a1582f5389b6b4a594b91d99e89784683fbf3679f18639f95';
const expectedRelListMutation = '7ae6e6d8f650d733922b1af018dfdcac310bdcbb4f14537cdb20500c44da3c04';
const expectedSandboxMutation = '57b9814c22902e40fc38180d79a1a78068f1b15154f4149bef8fbea5b6cf05cb';
const expectedInlineStyleMutation = 'b4fe0e2cdba9f98193e8dfc7aadb7fa892e508e269a4a94beb9c2970d8ce5096';

function fail(message) {
  throw new Error(`playwright-flutter-cdp-smoke: ${message}`);
}

function pngInfo(png) {
  if (png.length < 24 || !png.subarray(0, 8).equals(pngSignature)) {
    fail('capture is not a PNG');
  }
  const width = png.readUInt32BE(16);
  const height = png.readUInt32BE(20);
  let offset = 8;
  const compressed = [];
  while (offset + 12 <= png.length) {
    const length = png.readUInt32BE(offset);
    const type = png.subarray(offset + 4, offset + 8).toString('ascii');
    const end = offset + 12 + length;
    if (end > png.length) fail('PNG chunk exceeds output');
    if (type === 'IDAT') compressed.push(png.subarray(offset + 8, offset + 8 + length));
    offset = end;
    if (type === 'IEND') break;
  }
  const filtered = inflateSync(Buffer.concat(compressed));
  const stride = width * 4;
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
        const choices = [left, above, upperLeft];
        row[x] = (row[x] + choices.reduce((best, value) =>
          Math.abs(estimate - value) < Math.abs(estimate - best) ? value : best)) & 255;
      } else if (filter !== 0) fail(`unsupported PNG filter ${filter}`);
    }
    row.copy(rgba, y * stride);
  }
  return {
    width,
    height,
    firstPixel: [...rgba.subarray(0, 4)],
    hash: createHash('sha256').update(png).digest('hex'),
  };
}

function assertCapture(png, width, height, firstPixel, label, expectedHash = null) {
  const info = pngInfo(png);
  if (info.width !== width || info.height !== height) {
    fail(`${label} dimensions were ${info.width}x${info.height}, expected ${width}x${height}`);
  }
  if (JSON.stringify(info.firstPixel) !== JSON.stringify(firstPixel)) {
    fail(`${label} first pixel was ${JSON.stringify(info.firstPixel)}, expected ${JSON.stringify(firstPixel)}`);
  }
  if (expectedHash !== null && info.hash !== expectedHash) {
    fail(`${label} SHA-256 was ${info.hash}, expected ${expectedHash}`);
  }
  return info;
}

function waitForServer(child) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('timed out waiting for Flutter CDP host')), 30000);
    const onData = (chunk) => {
      const text = chunk.toString('utf8');
      process.stderr.write(text);
      if (text.includes(`CDP listening on ws://127.0.0.1:${port}`)) {
        clearTimeout(timer);
        child.stderr.off('data', onData);
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

async function main() {
  if (!app) fail('VIXEN_CDP_APP is required');
  const child = spawn(app, [
    '--vixen-cdp-automation',
    `--vixen-url=${fixtureUrl}`,
    '--vixen-viewport=800x600',
    `--vixen-cdp-port=${port}`,
  ], { cwd: root, stdio: ['ignore', 'pipe', 'pipe'], env: process.env });
  child.stdout.on('data', (chunk) => process.stdout.write(chunk));

  let browser;
  try {
    await waitForServer(child);
    browser = await chromium.connectOverCDP(endpoint, { timeout: 15000 });
    const context = browser.contexts()[0] || fail('missing browser context');
    const page = context.pages()[0] || fail('missing initial target');
    const firstSession = await context.newCDPSession(page);

    await page.setViewportSize({ width: 320, height: 240 });
    const baseline = await page.screenshot({ timeout: 20000 });
    const baselineInfo = assertCapture(
      baseline,
      320,
      240,
      [34, 187, 102, 255],
      'baseline',
      expectedDatasetBaseline,
    );

    const datasetEvidence = await page.evaluate(() => {
      const target = document.querySelector('#dataset-target');
      const dataset = target.dataset;
      globalThis.__datasetObject = dataset;
      dataset.layoutMode = 'wide';
      const rect = target.getBoundingClientRect();
      return {
        stable: dataset === target.dataset,
        reflectedAttribute: target.getAttribute('data-layout-mode'),
        reflectedProperty: target.dataset.layoutMode,
        authorName: dataset.authorName,
        keys: Object.keys(dataset),
        synchronousRect: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
        },
      };
    });
    if (!datasetEvidence.stable
        || datasetEvidence.reflectedAttribute !== 'wide'
        || datasetEvidence.reflectedProperty !== 'wide'
        || datasetEvidence.authorName !== 'ada'
        || JSON.stringify(datasetEvidence.keys) !== JSON.stringify(['authorName', 'layoutMode'])
        || datasetEvidence.synchronousRect.width !== 140
        || datasetEvidence.synchronousRect.height !== 32) {
      fail(`live dataset evidence was ${JSON.stringify(datasetEvidence)}`);
    }
    const document = await firstSession.send('DOM.getDocument');
    const datasetNode = await firstSession.send('DOM.querySelector', {
      nodeId: document.root.nodeId,
      selector: '#dataset-target',
    });
    const datasetAttributes = await firstSession.send('DOM.getAttributes', {
      nodeId: datasetNode.nodeId,
    });
    const attributePairs = Object.fromEntries(Array.from(
      { length: datasetAttributes.attributes.length / 2 },
      (_, index) => datasetAttributes.attributes.slice(index * 2, index * 2 + 2),
    ));
    if (attributePairs['data-layout-mode'] !== 'wide') {
      fail(`CDP DOM did not inspect the dataset mutation: ${JSON.stringify(attributePairs)}`);
    }
    const datasetModel = await firstSession.send('DOM.getBoxModel', {
      nodeId: datasetNode.nodeId,
    });
    const datasetContent = datasetModel.model.content;
    if (datasetContent[2] - datasetContent[0] !== 140
        || datasetContent[5] - datasetContent[1] !== 32
        || await page.evaluate(() => globalThis.__datasetObject
          !== document.querySelector('#dataset-target').dataset)) {
      fail(`CDP/page dataset identity or geometry diverged: ${JSON.stringify(datasetModel.model)}`);
    }
    const afterDataset = await page.screenshot({ timeout: 20000 });
    const afterDatasetInfo = assertCapture(
      afterDataset,
      320,
      240,
      [34, 187, 102, 255],
      'post-dataset',
      expectedDatasetMutation,
    );
    if (afterDatasetInfo.hash === baselineInfo.hash) {
      fail('dataset mutation did not change exact Flutter pixels');
    }

    const classListBeforeClick = await page.evaluate(() => {
      const status = document.querySelector('#status');
      globalThis.__classListObject = status.classList;
      return status.classList === globalThis.__classListObject;
    });
    if (!classListBeforeClick) fail('classList identity was not stable before input');
    const hitBox = await page.locator('#hit').boundingBox({ timeout: 20000 });
    if (!hitBox || hitBox.x !== 0 || hitBox.y !== 0 || hitBox.width !== 120 || hitBox.height < 40) {
      fail(`Flutter commit geometry for #hit was ${JSON.stringify(hitBox)}`);
    }
    await page.mouse.click(hitBox.x + 10, hitBox.y + 10);
    if (await page.locator('#status').textContent() !== 'clicked:1') {
      fail('Flutter-routed click did not mutate the initial target');
    }
    const classListEvidence = await page.evaluate(() => {
      const status = document.querySelector('#status');
      const rect = status.getBoundingClientRect();
      return {
        stable: globalThis.__classListObject === status.classList,
        reflectedAttribute: status.getAttribute('class'),
        reflectedToken: globalThis.__classListObject.contains('clicked'),
        tokens: Array.from(status.classList),
        synchronousWidth: rect.width,
      };
    });
    if (!classListEvidence.stable
        || classListEvidence.reflectedAttribute !== 'clicked'
        || !classListEvidence.reflectedToken
        || JSON.stringify(classListEvidence.tokens) !== JSON.stringify(['clicked'])
        || classListEvidence.synchronousWidth !== 140) {
      fail(`live classList evidence was ${JSON.stringify(classListEvidence)}`);
    }
    const statusNode = await firstSession.send('DOM.querySelector', {
      nodeId: document.root.nodeId,
      selector: '#status',
    });
    const statusAttributes = await firstSession.send('DOM.getAttributes', {
      nodeId: statusNode.nodeId,
    });
    const statusAttributePairs = Object.fromEntries(Array.from(
      { length: statusAttributes.attributes.length / 2 },
      (_, index) => statusAttributes.attributes.slice(index * 2, index * 2 + 2),
    ));
    const statusModel = await firstSession.send('DOM.getBoxModel', {
      nodeId: statusNode.nodeId,
    });
    if (statusAttributePairs.class !== 'clicked'
        || statusModel.model.content[2] - statusModel.model.content[0] !== 140) {
      fail(`CDP DOM did not agree with classList: ${JSON.stringify({ statusAttributePairs, model: statusModel.model })}`);
    }
    const afterClick = await page.screenshot({ timeout: 20000 });
    const afterClickInfo = assertCapture(
      afterClick,
      320,
      240,
      [34, 187, 102, 255],
      'post-click',
      expectedClassListMutation,
    );
    if (afterClickInfo.hash === afterDatasetInfo.hash) {
      fail('click mutation did not change the post-dataset exact scene');
    }

    const relListEvidence = await page.evaluate(() => {
      const target = document.querySelector('#rel-target');
      const relList = target.relList;
      globalThis.__relListObject = relList;
      target.setAttribute('rel', 'wide');
      relList.add('tall');
      const rect = target.getBoundingClientRect();
      return {
        stable: relList === target.relList,
        reflectedAttribute: target.getAttribute('rel'),
        reflectedTokens: Array.from(relList),
        synchronousRect: {
          width: rect.width,
          height: rect.height,
        },
      };
    });
    if (!relListEvidence.stable
        || relListEvidence.reflectedAttribute !== 'wide tall'
        || JSON.stringify(relListEvidence.reflectedTokens) !== JSON.stringify(['wide', 'tall'])
        || relListEvidence.synchronousRect.width !== 120
        || relListEvidence.synchronousRect.height !== 32) {
      fail(`live relList evidence was ${JSON.stringify(relListEvidence)}`);
    }
    const relTargetNode = await firstSession.send('DOM.querySelector', {
      nodeId: document.root.nodeId,
      selector: '#rel-target',
    });
    const relTargetAttributes = await firstSession.send('DOM.getAttributes', {
      nodeId: relTargetNode.nodeId,
    });
    const relTargetAttributePairs = Object.fromEntries(Array.from(
      { length: relTargetAttributes.attributes.length / 2 },
      (_, index) => relTargetAttributes.attributes.slice(index * 2, index * 2 + 2),
    ));
    const relTargetModel = await firstSession.send('DOM.getBoxModel', {
      nodeId: relTargetNode.nodeId,
    });
    if (relTargetAttributePairs.rel !== 'wide tall'
        || relTargetModel.model.content[2] - relTargetModel.model.content[0] !== 120
        || relTargetModel.model.content[5] - relTargetModel.model.content[1] !== 32) {
      fail(`CDP DOM did not agree with relList: ${JSON.stringify({ relTargetAttributePairs, model: relTargetModel.model })}`);
    }
    const afterRelList = await page.screenshot({ timeout: 20000 });
    const afterRelListInfo = assertCapture(
      afterRelList,
      320,
      240,
      [34, 187, 102, 255],
      'post-relList',
      expectedRelListMutation,
    );
    if (afterRelListInfo.hash === afterClickInfo.hash) {
      fail('relList mutation did not change the post-classList exact scene');
    }

    const sandboxEvidence = await page.evaluate(() => {
      const target = document.querySelector('#sandbox-target');
      const sandbox = target.sandbox;
      globalThis.__sandboxObject = sandbox;
      target.setAttribute('sandbox', 'allow-same-origin');
      sandbox.add('allow-forms');
      const rect = target.getBoundingClientRect();
      return {
        stable: sandbox === target.sandbox,
        reflectedAttribute: target.getAttribute('sandbox'),
        reflectedTokens: Array.from(sandbox),
        synchronousRect: {
          width: rect.width,
          height: rect.height,
        },
      };
    });
    if (!sandboxEvidence.stable
        || sandboxEvidence.reflectedAttribute !== 'allow-same-origin allow-forms'
        || JSON.stringify(sandboxEvidence.reflectedTokens) !== JSON.stringify(['allow-same-origin', 'allow-forms'])
        || sandboxEvidence.synchronousRect.width !== 120
        || sandboxEvidence.synchronousRect.height !== 32) {
      fail(`live sandbox evidence was ${JSON.stringify(sandboxEvidence)}`);
    }
    const sandboxTargetNode = await firstSession.send('DOM.querySelector', {
      nodeId: document.root.nodeId,
      selector: '#sandbox-target',
    });
    const sandboxTargetAttributes = await firstSession.send('DOM.getAttributes', {
      nodeId: sandboxTargetNode.nodeId,
    });
    const sandboxTargetAttributePairs = Object.fromEntries(Array.from(
      { length: sandboxTargetAttributes.attributes.length / 2 },
      (_, index) => sandboxTargetAttributes.attributes.slice(index * 2, index * 2 + 2),
    ));
    const sandboxTargetModel = await firstSession.send('DOM.getBoxModel', {
      nodeId: sandboxTargetNode.nodeId,
    });
    if (sandboxTargetAttributePairs.sandbox !== 'allow-same-origin allow-forms'
        || sandboxTargetModel.model.content[2] - sandboxTargetModel.model.content[0] !== 120
        || sandboxTargetModel.model.content[5] - sandboxTargetModel.model.content[1] !== 32) {
      fail(`CDP DOM did not agree with sandbox: ${JSON.stringify({ sandboxTargetAttributePairs, model: sandboxTargetModel.model })}`);
    }
    const afterSandbox = await page.screenshot({ timeout: 20000 });
    const afterSandboxInfo = assertCapture(
      afterSandbox,
      320,
      240,
      [34, 187, 102, 255],
      'post-sandbox',
      expectedSandboxMutation,
    );
    if (afterSandboxInfo.hash === afterRelListInfo.hash) {
      fail('sandbox mutation did not change the post-relList exact scene');
    }

    const inlineStyleEvidence = await page.evaluate(() => {
      const target = document.querySelector('#style-target');
      const style = target.style;
      globalThis.__inlineStyleObject = style;
      target.setAttribute(
        'style',
        'display: block; width: 120px; height: 24px; background-color: #b8512c; color: white',
      );
      style.setProperty('height', '32px');
      const rect = target.getBoundingClientRect();
      return {
        stable: style === target.style,
        reflectedAttribute: target.getAttribute('style'),
        reflectedWidth: style.getPropertyValue('width'),
        reflectedHeight: target.style.height,
        synchronousRect: {
          width: rect.width,
          height: rect.height,
        },
      };
    });
    if (!inlineStyleEvidence.stable
        || inlineStyleEvidence.reflectedWidth !== '120px'
        || inlineStyleEvidence.reflectedHeight !== '32px'
        || inlineStyleEvidence.synchronousRect.width !== 120
        || inlineStyleEvidence.synchronousRect.height !== 32) {
      fail(`live inline style evidence was ${JSON.stringify(inlineStyleEvidence)}`);
    }
    const styleTargetNode = await firstSession.send('DOM.querySelector', {
      nodeId: document.root.nodeId,
      selector: '#style-target',
    });
    const styleTargetAttributes = await firstSession.send('DOM.getAttributes', {
      nodeId: styleTargetNode.nodeId,
    });
    const styleTargetAttributePairs = Object.fromEntries(Array.from(
      { length: styleTargetAttributes.attributes.length / 2 },
      (_, index) => styleTargetAttributes.attributes.slice(index * 2, index * 2 + 2),
    ));
    const styleTargetModel = await firstSession.send('DOM.getBoxModel', {
      nodeId: styleTargetNode.nodeId,
    });
    if (styleTargetAttributePairs.style !== inlineStyleEvidence.reflectedAttribute
        || styleTargetModel.model.content[2] - styleTargetModel.model.content[0] !== 120
        || styleTargetModel.model.content[5] - styleTargetModel.model.content[1] !== 32) {
      fail(`CDP DOM did not agree with inline style: ${JSON.stringify({ styleTargetAttributePairs, model: styleTargetModel.model })}`);
    }
    const afterInlineStyle = await page.screenshot({ timeout: 20000 });
    const afterInlineStyleInfo = assertCapture(
      afterInlineStyle,
      320,
      240,
      [34, 187, 102, 255],
      'post-inline-style',
      expectedInlineStyleMutation,
    );
    if (afterInlineStyleInfo.hash === afterSandboxInfo.hash) {
      fail('inline style mutation did not change the post-sandbox exact scene');
    }

    const second = await context.newPage();
    await second.setViewportSize({ width: 480, height: 300 });
    await second.setContent(`<!doctype html><style>
      body { margin: 0; background: #13579b; }
      #second { display: block; width: 90px; height: 45px; background: #ca2468; }
    </style><button id="second">Second</button>`);
    await second.evaluate(() => {
      document.querySelector('#second').addEventListener('click', () => {
        globalThis.__secondClicks = (globalThis.__secondClicks || 0) + 1;
      });
    });
    const secondPng = await second.screenshot({ timeout: 20000 });
    const secondInfo = assertCapture(secondPng, 480, 300, [202, 36, 104, 255], 'second target');
    if (secondInfo.hash === afterInlineStyleInfo.hash) fail('independent target captures were identical');
    const secondBox = await second.locator('#second').boundingBox({ timeout: 20000 });
    if (!secondBox) fail('second target has no Flutter commit geometry');
    await second.mouse.click(secondBox.x + 5, secondBox.y + 5);
    if (await second.evaluate(() => globalThis.__secondClicks) !== 1) {
      fail('Flutter-routed click did not reach the second target');
    }
    if (await page.evaluate(() => globalThis.__smokeClicks) !== 1) {
      fail('second-target input changed the first target');
    }

    const firstAgain = await page.screenshot({ timeout: 20000 });
    const firstAgainInfo = assertCapture(firstAgain, 320, 240, [34, 187, 102, 255], 'first target restored');
    if (firstAgainInfo.hash !== afterInlineStyleInfo.hash) {
      fail('switching targets changed the first target exact scene');
    }

    await firstSession.send('Vixen.resetRenderer');
    const recovered = await page.screenshot({ timeout: 20000 });
    const recoveredInfo = assertCapture(recovered, 320, 240, [34, 187, 102, 255], 'renderer-loss recovery');
    if (recoveredInfo.hash !== afterInlineStyleInfo.hash) {
      fail('renderer-loss full resync did not recover the exact scene');
    }

    console.log(`playwright-flutter-cdp-smoke ok baseline=${baselineInfo.hash} dataset=${afterDatasetInfo.hash} classList=${afterClickInfo.hash} relList=${afterRelListInfo.hash} sandbox=${afterSandboxInfo.hash} style=${afterInlineStyleInfo.hash} second=${secondInfo.hash}`);
  } finally {
    if (browser) await browser.close().catch(() => {});
    await stopServer(child);
  }
}

main().catch((error) => {
  console.error(error?.stack || error);
  process.exit(1);
});
