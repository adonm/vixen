#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, rm, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

import {
  collectMetadata,
  measureCommand,
  resolveWorkspacePath,
  sampleProcStatus,
  sha256File,
  summarize,
  workspaceRoot,
} from './baseline-common.mjs';

const DEFAULT_APP = 'flutter/vixen_shell/build/linux/x64/release/bundle/vixen_shell';
const DEFAULT_LIBRARY = 'flutter/vixen_shell/build/linux/x64/release/bundle/lib/libvixen_ffi.so';
const FIXTURE = 'fixtures/dom/basic.html';
const INTERACTION_FIXTURE = 'fixtures/cdp/playwright-smoke.html';
const VIEWPORT = { width: 320, height: 240 };
const INTERACTION_MUTATIONS = 8;
const FRAME_TIMING_LIMIT = 32;
const MEASUREMENT_PREFIX = 'Vixen measurement ';
const EXPECTED_CAPTURE_SHA256 = '34ff6e88553c9396587d64131f48fa8e7d4579eccdc252398aa20d54472a42fb';
const EXPECTED_HARDWARE_CAPTURE_SHA256 = 'd29624bf78207e6e2056742b1d6c242515f8411dd5097af7aec4baaf6cb0b152';
const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const DEFAULT_RUNS = 5;
const DEFAULT_WARMUPS = 1;
const DEFAULT_PORT = 9324;
const DEFAULT_TIMEOUT_MS = 60_000;
const MAX_OUTPUT_BYTES = 1_048_576;

function usage() {
  return [
    'usage: node scripts/flutter-linux-baseline.mjs [options]',
    '',
    'Options:',
    '  --app PATH       Release/AOT vixen_shell executable',
    '  --library PATH   Release libvixen_ffi.so',
    '  --runs N         Measured runs, 1-20 (default 5)',
    '  --warmups N      Unreported warmup runs, 0-10 (default 1)',
    '  --port N         Loopback CDP port, 1-65535 (default 9324)',
    '  --timeout-ms N   Per-run timeout, 1000-120000 (default 60000)',
    '  --renderer MODE  software or hardware (default software)',
    '  --json           Print the structured report',
    '  -h, --help       Print this help',
    '',
    'Measurement only: this command does not enforce a performance budget.',
  ].join('\n');
}

function boundedInteger(flag, value, min, max) {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < min || number > max) {
    throw new Error(`${flag} must be an integer in [${min}, ${max}]`);
  }
  return number;
}

export function parseFlutterLinuxBaselineArgs(argv) {
  const options = {
    app: DEFAULT_APP,
    library: DEFAULT_LIBRARY,
    runs: DEFAULT_RUNS,
    warmups: DEFAULT_WARMUPS,
    port: DEFAULT_PORT,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    renderer: 'software',
    json: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '-h' || argument === '--help') return { help: true };
    if (argument === '--json') {
      options.json = true;
      continue;
    }
    if (['--app', '--library', '--runs', '--warmups', '--port', '--timeout-ms', '--renderer'].includes(argument)) {
      const value = argv[++index];
      if (!value) throw new Error(`${argument} requires a value`);
      if (argument === '--app') options.app = value;
      else if (argument === '--library') options.library = value;
      else if (argument === '--runs') options.runs = boundedInteger(argument, value, 1, 20);
      else if (argument === '--warmups') options.warmups = boundedInteger(argument, value, 0, 10);
      else if (argument === '--port') options.port = boundedInteger(argument, value, 1, 65_535);
      else if (argument === '--timeout-ms') options.timeoutMs = boundedInteger(argument, value, 1_000, 120_000);
      else {
        if (!['software', 'hardware'].includes(value)) {
          throw new Error('--renderer must be software or hardware');
        }
        options.renderer = value;
      }
      continue;
    }
    throw new Error(`unknown argument: ${argument}`);
  }
  return options;
}

const CDP_RE = /Vixen automation CDP listening on ws:\/\/127\.0\.0\.1:(\d+)/;
const PRESENTED_RE = /Vixen renderer presented context=(\d+) document=(\d+) commit=(\d+) scroll_y=([^\s]+)/;

export class FlutterDiagnosticTracker {
  constructor(startedNs) {
    this.startedNs = startedNs;
    this.text = '';
    this.impeller = false;
    this.cdp = null;
    this.presented = null;
    this.measurementLine = '';
    this.measurementError = null;
    this.presentedMeasurements = new Map();
    this.frameMeasurements = new Map();
  }

  append(chunk, observedNs) {
    const decoded = chunk.toString('utf8');
    this.text = `${this.text}${decoded}`.slice(-65_536);
    this.measurementLine = `${this.measurementLine}${decoded}`;
    if (this.measurementLine.length > 131_072) {
      this.measurementError ??= new Error('measurement diagnostic line exceeded 128 KiB');
      this.measurementLine = '';
    }
    for (;;) {
      const newline = this.measurementLine.indexOf('\n');
      if (newline < 0) break;
      const line = this.measurementLine.slice(0, newline).trimEnd();
      this.measurementLine = this.measurementLine.slice(newline + 1);
      if (line.startsWith(MEASUREMENT_PREFIX)) this.#recordMeasurement(line);
    }
    this.impeller ||= this.text.includes('Using the Impeller rendering backend');
    if (this.cdp === null) {
      const match = this.text.match(CDP_RE);
      if (match) {
        this.cdp = {
          port: Number(match[1]),
          elapsed_ms: elapsedMs(this.startedNs, observedNs),
        };
      }
    }
    if (this.presented === null) {
      const match = this.text.match(PRESENTED_RE);
      if (match) {
        this.presented = {
          context_id: Number(match[1]),
          document_id: Number(match[2]),
          commit_id: Number(match[3]),
          elapsed_ms: elapsedMs(this.startedNs, observedNs),
        };
      }
    }
  }

  get controlReady() {
    return this.impeller && this.cdp !== null;
  }

  get latestMeasurementSequence() {
    return Math.max(0, ...this.presentedMeasurements.keys());
  }

  completeMeasurementAfter(sequence) {
    for (const [candidate, presented] of this.presentedMeasurements) {
      if (candidate <= sequence) continue;
      const frame = this.frameMeasurements.get(candidate);
      if (frame) return { presented, frame };
    }
    return null;
  }

  #recordMeasurement(line) {
    try {
      const record = JSON.parse(line.slice(MEASUREMENT_PREFIX.length));
      validateMeasurementRecord(record);
      if (record.type === 'frame_timing_limit_reached') {
        throw new Error('Flutter frame timing diagnostic limit was reached');
      }
      const records = record.type === 'presented_commit'
        ? this.presentedMeasurements
        : this.frameMeasurements;
      if (records.has(record.sequence)) {
        throw new Error(`duplicate ${record.type} sequence ${record.sequence}`);
      }
      records.set(record.sequence, record);
      if (records.size > FRAME_TIMING_LIMIT) {
        throw new Error('Flutter frame timing diagnostics exceeded their bound');
      }
    } catch (error) {
      this.measurementError ??= error;
    }
  }
}

export function validateMeasurementRecord(record) {
  if (!record || typeof record !== 'object' || record.v !== 1 || typeof record.type !== 'string') {
    throw new Error('malformed Flutter measurement record');
  }
  if (record.type === 'frame_timing_limit_reached') {
    if (!Number.isSafeInteger(record.limit) || record.limit < 1) {
      throw new Error('malformed Flutter measurement limit record');
    }
    return record;
  }
  if (!['presented_commit', 'presented_commit_frame_timing'].includes(record.type)) {
    throw new Error(`unknown Flutter measurement record ${record.type}`);
  }
  for (const key of ['sequence', 'context_id', 'document_id', 'commit_id', 'frame_number']) {
    if (!Number.isSafeInteger(record[key]) || record[key] < 1) {
      throw new Error(`Flutter measurement ${key} must be a positive safe integer`);
    }
  }
  if (record.type === 'presented_commit') {
    if (!Number.isSafeInteger(record.coordinator_return_wall_us)
        || record.coordinator_return_wall_us < 1
        || !record.revision || typeof record.revision !== 'object') {
      throw new Error('malformed presented-commit measurement');
    }
    return record;
  }
  if (!Number.isSafeInteger(record.raster_finish_wall_us)
      || record.raster_finish_wall_us < 1
      || !record.durations_us || typeof record.durations_us !== 'object') {
    throw new Error('malformed frame-timing measurement');
  }
  for (const key of ['vsync_overhead', 'build', 'raster', 'total_span']) {
    if (!Number.isSafeInteger(record.durations_us[key]) || record.durations_us[key] < 0) {
      throw new Error(`Flutter frame duration ${key} is malformed`);
    }
  }
  return record;
}

function elapsedMs(startedNs, endedNs) {
  return Number((Number(endedNs - startedNs) / 1_000_000).toFixed(3));
}

function expectedCaptureSha256(renderer) {
  return renderer === 'hardware'
    ? EXPECTED_HARDWARE_CAPTURE_SHA256
    : EXPECTED_CAPTURE_SHA256;
}

export function validateFlutterCapture(png, expected = {}) {
  const width = expected.width ?? VIEWPORT.width;
  const height = expected.height ?? VIEWPORT.height;
  const expectedHash = expected.sha256 ?? EXPECTED_CAPTURE_SHA256;
  if (png.length < 24 || !png.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE)) {
    throw new Error('capture is not a PNG');
  }
  const actualWidth = png.readUInt32BE(16);
  const actualHeight = png.readUInt32BE(20);
  if (actualWidth !== width || actualHeight !== height) {
    throw new Error(`capture is ${actualWidth}x${actualHeight}, expected ${width}x${height}`);
  }
  const sha256 = createHash('sha256').update(png).digest('hex');
  if (sha256 !== expectedHash) {
    throw new Error(`capture SHA-256 is ${sha256}, expected ${expectedHash}`);
  }
  return { width, height, logical_bytes: png.length, sha256 };
}

export function captureDurationFromTrace(trace, dataLossOccurred = false) {
  if (dataLossOccurred) throw new Error('CDP trace reported data loss');
  const events = trace?.traceEvents?.filter((event) =>
    event?.name === 'Page.captureScreenshot'
    && event?.cat === 'vixen.cdp'
    && event?.ph === 'X') ?? [];
  if (events.length !== 1) {
    throw new Error(`expected one Page.captureScreenshot trace event, got ${events.length}`);
  }
  const event = events[0];
  if (event.args?.ok !== true || !Number.isSafeInteger(event.dur) || event.dur < 0) {
    throw new Error('Page.captureScreenshot trace event was failed or malformed');
  }
  return event.dur;
}

function appendBounded(state, chunk) {
  const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
  state.total += buffer.length;
  if (state.captured >= MAX_OUTPUT_BYTES) return;
  const keep = Math.min(buffer.length, MAX_OUTPUT_BYTES - state.captured);
  state.chunks.push(buffer.subarray(0, keep));
  state.captured += keep;
}

function outputResult(state) {
  return {
    text: Buffer.concat(state.chunks).toString('utf8'),
    bytes: state.total,
    truncated: state.total > state.captured,
  };
}

function updatePeaks(peaks, sample) {
  if (!sample) return;
  for (const key of Object.keys(peaks)) {
    if (sample[key] !== null) peaks[key] = Math.max(peaks[key] ?? 0, sample[key]);
  }
}

async function waitUntil(predicate, child, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(`${label}: app exited before readiness code=${child.exitCode} signal=${child.signalCode}`);
    }
    if (Date.now() >= deadline) throw new Error(`${label}: timed out after ${timeoutMs}ms`);
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 5));
  }
}

function waitForEvent(emitter, event, timeoutMs) {
  return new Promise((resolveEvent, rejectEvent) => {
    const timer = setTimeout(() => {
      emitter.off(event, onEvent);
      rejectEvent(new Error(`timed out waiting for ${event}`));
    }, timeoutMs);
    const onEvent = (value) => {
      clearTimeout(timer);
      resolveEvent(value);
    };
    emitter.once(event, onEvent);
  });
}

async function readTrace(session, completion) {
  if (completion.dataLossOccurred) throw new Error('CDP trace reported data loss');
  if (typeof completion.stream !== 'string' || completion.stream.length === 0) {
    throw new Error('CDP trace did not return a stream handle');
  }
  const chunks = [];
  let bytes = 0;
  for (;;) {
    const result = await session.send('IO.read', { handle: completion.stream });
    if (!result.base64Encoded || typeof result.data !== 'string') {
      throw new Error('CDP trace stream returned malformed data');
    }
    const chunk = Buffer.from(result.data, 'base64');
    bytes += chunk.length;
    if (bytes > 4_194_304) throw new Error('CDP trace exceeded 4 MiB');
    chunks.push(chunk);
    if (result.eof) break;
  }
  await session.send('IO.close', { handle: completion.stream });
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}

async function waitForMeasurement(tracker, child, afterSequence, timeoutMs, label) {
  await waitUntil(
    () => tracker.measurementError !== null
      || tracker.completeMeasurementAfter(afterSequence) !== null,
    child,
    timeoutMs,
    label,
  );
  if (tracker.measurementError) throw tracker.measurementError;
  const pair = tracker.completeMeasurementAfter(afterSequence);
  if (!pair) throw new Error(`${label}: missing complete Flutter frame measurement`);
  for (const key of ['sequence', 'context_id', 'document_id', 'commit_id', 'frame_number']) {
    if (pair.presented[key] !== pair.frame[key]) {
      throw new Error(`${label}: Flutter measurement ${key} identity mismatch`);
    }
  }
  return pair;
}

function traceEvents(trace, method, expectedCount) {
  const events = trace?.traceEvents?.filter((event) =>
    event?.name === method
    && event?.cat === 'vixen.cdp'
    && event?.ph === 'X') ?? [];
  if (events.length !== expectedCount) {
    throw new Error(`expected ${expectedCount} ${method} trace events, got ${events.length}`);
  }
  for (const event of events) {
    if (event.args?.ok !== true
        || !Number.isSafeInteger(event.ts) || event.ts < 1
        || !Number.isSafeInteger(event.dur) || event.dur < 0) {
      throw new Error(`${method} trace event was failed or malformed`);
    }
  }
  return events;
}

export function measuredOperation(trigger, trace, measurement) {
  const { presented, frame } = measurement;
  const endpointWallUs = Math.max(
    presented.coordinator_return_wall_us,
    frame.raster_finish_wall_us,
  );
  if (endpointWallUs < trace.ts) {
    throw new Error(`${trigger} Flutter frame endpoint preceded its CDP operation`);
  }
  return {
    trigger,
    trace: {
      method: trace.name,
      start_wall_us: trace.ts,
      dispatch_us: trace.dur,
      ok: true,
    },
    presented: {
      sequence: presented.sequence,
      context_id: presented.context_id,
      document_id: presented.document_id,
      commit_id: presented.commit_id,
      revision: presented.revision,
      coordinator_return_wall_us: presented.coordinator_return_wall_us,
    },
    frame: {
      frame_number: frame.frame_number,
      refresh_rate_hz: frame.refresh_rate_hz,
      build_us: frame.durations_us.build,
      raster_us: frame.durations_us.raster,
      vsync_overhead_us: frame.durations_us.vsync_overhead,
      total_span_us: frame.durations_us.total_span,
      raster_finish_wall_us: frame.raster_finish_wall_us,
    },
    to_presented_commit_ms: Number(((endpointWallUs - trace.ts) / 1_000).toFixed(3)),
  };
}

function frameStability(operations) {
  const frames = operations.map((operation) => operation.frame);
  const refreshRates = frames
    .map((frame) => frame.refresh_rate_hz)
    .filter((value) => Number.isFinite(value) && value > 0);
  const refreshRateHz = refreshRates.length > 0 ? refreshRates[0] : null;
  const refreshIntervalUs = refreshRateHz === null ? null : 1_000_000 / refreshRateHz;
  const summary = (key) => summarize(frames.map((frame) => frame[key]), 0);
  const over = (key) => refreshIntervalUs === null
    ? null
    : frames.filter((frame) => frame[key] > refreshIntervalUs).length;
  return {
    frame_count: frames.length,
    refresh_rate_hz: refreshRateHz,
    refresh_interval_us: refreshIntervalUs === null
      ? null
      : Number(refreshIntervalUs.toFixed(3)),
    build_us: summary('build_us'),
    raster_us: summary('raster_us'),
    vsync_overhead_us: summary('vsync_overhead_us'),
    total_span_us: summary('total_span_us'),
    over_refresh_interval_count: {
      build: over('build_us'),
      raster: over('raster_us'),
      total_span: over('total_span_us'),
    },
  };
}

export function parseWaylandEglInfo(text) {
  const marker = 'Wayland platform:\n';
  const start = text.indexOf(marker);
  if (start < 0) throw new Error('eglinfo did not report a Wayland platform');
  const rest = text.slice(start + marker.length);
  const nextPlatform = rest.search(/^\S[^\n]* platform:\n/m);
  const section = nextPlatform < 0 ? rest : rest.slice(0, nextPlatform);
  const field = (name) => section.match(new RegExp(`^${name}: (.+)$`, 'm'))?.[1] ?? null;
  const renderer = field('OpenGL ES profile renderer');
  if (!renderer) throw new Error('eglinfo omitted its Wayland OpenGL ES renderer');
  const software = /llvmpipe|softpipe|swrast|software rasterizer/i.test(renderer);
  return {
    probe: 'eglinfo -B / Wayland platform OpenGL ES profile',
    egl_vendor: field('EGL vendor string'),
    egl_version: field('EGL version string'),
    opengl_es_vendor: field('OpenGL ES profile vendor'),
    opengl_es_renderer: renderer,
    opengl_es_version: field('OpenGL ES profile version'),
    hardware_accelerated: !software,
  };
}

async function probeHardwareRenderer() {
  const result = await measureCommand('eglinfo', ['-B'], {
    cwd: workspaceRoot,
    env: process.env,
    timeoutMs: 15_000,
    maxOutputBytes: 262_144,
    sampleIntervalMs: 50,
  });
  if (result.status !== 0 || result.signal !== null || result.timed_out
      || result.error !== null || result.stdout_truncated) {
    throw new Error(
      `hardware renderer probe failed: status=${result.status} signal=${result.signal} `
      + `timeout=${result.timed_out} error=${result.error}`,
    );
  }
  const renderer = parseWaylandEglInfo(result.stdout);
  if (!renderer.hardware_accelerated) {
    throw new Error(
      `hardware mode resolved to software renderer ${renderer.opengl_es_renderer}`,
    );
  }
  return renderer;
}

async function runInteraction(options, paths, child, tracker, page, session) {
  const navigationFloor = tracker.latestMeasurementSequence;
  await page.goto(paths.interactionFixtureUrl, {
    waitUntil: 'load',
    timeout: Math.min(options.timeoutMs, 35_000),
  });
  await page.setViewportSize(VIEWPORT);
  await page.screenshot({ timeout: 20_000 });
  await waitForMeasurement(
    tracker,
    child,
    navigationFloor,
    options.timeoutMs,
    'interaction fixture presentation',
  );

  const document = await session.send('DOM.getDocument');
  const rootNodeId = document?.root?.nodeId;
  if (!Number.isSafeInteger(rootNodeId) || rootNodeId < 1) {
    throw new Error('DOM.getDocument omitted its root node');
  }
  const statusNode = await session.send('DOM.querySelector', {
    nodeId: rootNodeId,
    selector: '#status',
  });
  if (!Number.isSafeInteger(statusNode.nodeId) || statusNode.nodeId < 1) {
    throw new Error('interaction fixture status node is missing');
  }
  const hitBox = await page.locator('#hit').boundingBox({ timeout: 20_000 });
  if (!hitBox) throw new Error('interaction fixture hit target has no Flutter geometry');

  await session.send('Tracing.start', {
    transferMode: 'ReturnAsStream',
    streamFormat: 'json',
    streamCompression: 'none',
  });
  const mutationMeasurements = [];
  for (let index = 0; index < INTERACTION_MUTATIONS; index += 1) {
    const floor = tracker.latestMeasurementSequence;
    await session.send('DOM.setAttributeValue', {
      nodeId: statusNode.nodeId,
      name: 'class',
      value: index % 2 === 0 ? 'clicked' : '',
    });
    await session.send('DOM.getBoxModel', { nodeId: statusNode.nodeId });
    mutationMeasurements.push(await waitForMeasurement(
      tracker,
      child,
      floor,
      options.timeoutMs,
      `mutation frame ${index + 1}`,
    ));
  }

  const x = hitBox.x + Math.min(10, hitBox.width / 2);
  const y = hitBox.y + Math.min(10, hitBox.height / 2);
  await session.send('Input.dispatchMouseEvent', {
    type: 'mousePressed',
    x,
    y,
    button: 'left',
    buttons: 1,
    clickCount: 1,
  });
  await new Promise((resolvePromise) => setTimeout(resolvePromise, 100));
  const inputFloor = tracker.latestMeasurementSequence;
  await session.send('Input.dispatchMouseEvent', {
    type: 'mouseReleased',
    x,
    y,
    button: 'left',
    buttons: 0,
    clickCount: 1,
  });
  await session.send('DOM.getBoxModel', { nodeId: statusNode.nodeId });
  const inputMeasurement = await waitForMeasurement(
    tracker,
    child,
    inputFloor,
    options.timeoutMs,
    'mouse release mutation frame',
  );

  const completed = waitForEvent(session, 'Tracing.tracingComplete', options.timeoutMs);
  await session.send('Tracing.end');
  const completion = await completed;
  const trace = await readTrace(session, completion);
  if (completion.dataLossOccurred) throw new Error('interaction CDP trace reported data loss');
  const mutationEvents = traceEvents(trace, 'DOM.setAttributeValue', INTERACTION_MUTATIONS);
  const inputEvents = traceEvents(trace, 'Input.dispatchMouseEvent', 2);
  const mutations = mutationMeasurements.map((measurement, index) => ({
    index,
    ...measuredOperation('direct_mutation', mutationEvents[index], measurement),
  }));
  const input = measuredOperation('mouse_release', inputEvents[1], inputMeasurement);
  const operations = [...mutations, input];
  return {
    mutations,
    input,
    frame_stability: frameStability(operations),
  };
}

async function stopChild(child, closePromise) {
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
  const graceful = await Promise.race([
    closePromise.then((result) => ({ result })),
    new Promise((resolvePromise) => setTimeout(() => resolvePromise(null), 5_000)),
  ]);
  if (graceful) return graceful.result;
  child.kill('SIGKILL');
  return closePromise;
}

async function runSample(options, paths, label, chromium) {
  const profileRoot = await mkdtemp(join(paths.tempRoot, `${label}-`));
  const startedNs = process.hrtime.bigint();
  const tracker = new FlutterDiagnosticTracker(startedNs);
  const stdout = { chunks: [], total: 0, captured: 0 };
  const stderr = { chunks: [], total: 0, captured: 0 };
  const peaks = { vmhwm_bytes: null, vmrss_bytes: null, vmsize_bytes: null };
  let pollTimer;
  let spawnError = null;
  let browser;
  let child;
  let closePromise;

  try {
    const childEnvironment = {
      ...process.env,
      GDK_BACKEND: 'wayland',
      VIXEN_FFI_LIBRARY: paths.library,
      VIXEN_PROFILE_PATH: join(profileRoot, 'profile.redb'),
    };
    if (options.renderer === 'software') childEnvironment.LIBGL_ALWAYS_SOFTWARE = '1';
    else delete childEnvironment.LIBGL_ALWAYS_SOFTWARE;
    child = spawn(paths.app, [
      '--vixen-cdp-automation',
      `--vixen-url=${paths.fixtureUrl}`,
      `--vixen-viewport=${VIEWPORT.width}x${VIEWPORT.height}`,
      `--vixen-cdp-port=${options.port}`,
      `--vixen-frame-timing-limit=${FRAME_TIMING_LIMIT}`,
    ], {
      cwd: workspaceRoot,
      env: childEnvironment,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    child.on('error', (error) => {
      spawnError = error;
    });
    closePromise = new Promise((resolveClose) => {
      child.once('close', (status, signal) => resolveClose({ status, signal }));
    });
    const observe = (state) => (chunk) => {
      appendBounded(state, chunk);
      tracker.append(chunk, process.hrtime.bigint());
    };
    child.stdout.on('data', observe(stdout));
    child.stderr.on('data', observe(stderr));

    const poll = async () => {
      if (child.exitCode !== null || child.signalCode !== null) return;
      updatePeaks(peaks, await sampleProcStatus(child.pid));
      pollTimer = setTimeout(poll, 5);
    };
    void poll();

    await waitUntil(
      () => tracker.controlReady || tracker.measurementError !== null || spawnError !== null,
      child,
      options.timeoutMs,
      label,
    );
    if (spawnError) throw spawnError;
    if (tracker.measurementError) throw tracker.measurementError;
    if (tracker.cdp.port !== options.port) {
      throw new Error(`CDP diagnostic named port ${tracker.cdp.port}, expected ${options.port}`);
    }

    browser = await chromium.connectOverCDP(`ws://127.0.0.1:${options.port}`, {
      timeout: Math.min(options.timeoutMs, 30_000),
    });
    const context = browser.contexts()[0];
    const page = context?.pages()[0];
    if (!context || !page) throw new Error('Flutter CDP host did not expose its initial target');
    await page.setViewportSize(VIEWPORT);
    const session = await context.newCDPSession(page);
    await session.send('Tracing.start', {
      transferMode: 'ReturnAsStream',
      streamFormat: 'json',
      streamCompression: 'none',
    });
    const captureStartedNs = process.hrtime.bigint();
    const capture = await session.send('Page.captureScreenshot', { format: 'png' });
    const captureRoundTripMs = elapsedMs(captureStartedNs, process.hrtime.bigint());
    if (typeof capture.data !== 'string') throw new Error('Page.captureScreenshot omitted PNG data');
    const pngInfo = validateFlutterCapture(Buffer.from(capture.data, 'base64'), {
      sha256: expectedCaptureSha256(options.renderer),
    });
    await waitUntil(() => tracker.presented !== null, child, options.timeoutMs, `${label} presentation`);

    const completed = waitForEvent(session, 'Tracing.tracingComplete', options.timeoutMs);
    await session.send('Tracing.end');
    const completion = await completed;
    const trace = await readTrace(session, completion);
    const captureDispatchUs = captureDurationFromTrace(trace, completion.dataLossOccurred);
    const interaction = await runInteraction(
      options,
      paths,
      child,
      tracker,
      page,
      session,
    );
    updatePeaks(peaks, await sampleProcStatus(child.pid));

    await browser.close();
    browser = null;
    const exit = await stopChild(child, closePromise);
    if (exit.status !== 0 || exit.signal !== null) {
      throw new Error(`app did not shut down cleanly: code=${exit.status} signal=${exit.signal}`);
    }
    clearTimeout(pollTimer);
    const stdoutResult = outputResult(stdout);
    const stderrResult = outputResult(stderr);
    return {
      startup_cdp_ready_ms: tracker.cdp.elapsed_ms,
      startup_first_presented_ms: tracker.presented.elapsed_ms,
      capture_dispatch_us: captureDispatchUs,
      capture_round_trip_ms: captureRoundTripMs,
      peak_memory_bytes: peaks,
      presented: {
        context_id: tracker.presented.context_id,
        document_id: tracker.presented.document_id,
        commit_id: tracker.presented.commit_id,
      },
      capture: pngInfo,
      interaction,
      exit_status: exit.status,
      signal: exit.signal,
      stdout_bytes: stdoutResult.bytes,
      stderr_bytes: stderrResult.bytes,
      stdout_truncated: stdoutResult.truncated,
      stderr_truncated: stderrResult.truncated,
    };
  } catch (error) {
    const output = `${outputResult(stdout).text}\n${outputResult(stderr).text}`.trim().slice(-4_000);
    throw new Error(`${label}: ${error.message}${output ? `\n${output}` : ''}`);
  } finally {
    clearTimeout(pollTimer);
    if (browser) await browser.close().catch(() => {});
    if (child && closePromise && child.exitCode === null && child.signalCode === null) {
      await stopChild(child, closePromise).catch(() => {});
    }
    await rm(profileRoot, { recursive: true, force: true });
  }
}

function memorySummary(samples) {
  return Object.fromEntries(['vmhwm_bytes', 'vmrss_bytes', 'vmsize_bytes'].map((key) => [
    key,
    summarize(samples.map((sample) => sample.peak_memory_bytes[key]).filter((value) => value !== null), 0),
  ]));
}

function metricSummary(samples) {
  const mutations = samples.flatMap((sample) => sample.interaction.mutations);
  const inputs = samples.map((sample) => sample.interaction.input);
  const operations = [...mutations, ...inputs];
  const frames = operations.map((operation) => operation.frame);
  const frameSummary = (key) => summarize(frames.map((frame) => frame[key]), 0);
  const overRefreshIntervalCount = (key) => {
    const counts = samples.map(
      (sample) => sample.interaction.frame_stability.over_refresh_interval_count[key],
    );
    return counts.some((count) => count === null)
      ? null
      : counts.reduce((total, count) => total + count, 0);
  };
  return {
    startup_cdp_ready_ms: summarize(samples.map((sample) => sample.startup_cdp_ready_ms)),
    startup_first_presented_ms: summarize(samples.map((sample) => sample.startup_first_presented_ms)),
    capture_dispatch_us: summarize(samples.map((sample) => sample.capture_dispatch_us), 0),
    capture_round_trip_ms: summarize(samples.map((sample) => sample.capture_round_trip_ms)),
    peak_memory_bytes: memorySummary(samples),
    interaction: {
      mutation_to_presented_commit_ms: summarize(
        mutations.map((operation) => operation.to_presented_commit_ms),
      ),
      mutation_cdp_dispatch_us: summarize(
        mutations.map((operation) => operation.trace.dispatch_us),
        0,
      ),
      mouse_release_to_presented_commit_ms: summarize(
        inputs.map((operation) => operation.to_presented_commit_ms),
      ),
      mouse_release_cdp_dispatch_us: summarize(
        inputs.map((operation) => operation.trace.dispatch_us),
        0,
      ),
      frames: {
        count: frames.length,
        build_us: frameSummary('build_us'),
        raster_us: frameSummary('raster_us'),
        vsync_overhead_us: frameSummary('vsync_overhead_us'),
        total_span_us: frameSummary('total_span_us'),
        over_refresh_interval_count: {
          build: overRefreshIntervalCount('build'),
          raster: overRefreshIntervalCount('raster'),
          total_span: overRefreshIntervalCount('total_span'),
        },
      },
    },
  };
}

async function main() {
  const options = parseFlutterLinuxBaselineArgs(process.argv.slice(2));
  if (options.help) {
    console.log(usage());
    return;
  }
  const paths = {
    app: resolveWorkspacePath(options.app),
    library: resolveWorkspacePath(options.library),
    fixture: resolveWorkspacePath(FIXTURE),
    interactionFixture: resolveWorkspacePath(INTERACTION_FIXTURE),
    tempRoot: resolveWorkspacePath('.tmp'),
  };
  paths.fixtureUrl = pathToFileURL(paths.fixture).href;
  paths.interactionFixtureUrl = pathToFileURL(paths.interactionFixture).href;
  for (const [name, path] of Object.entries({
    app: paths.app,
    library: paths.library,
    fixture: paths.fixture,
    interactionFixture: paths.interactionFixture,
  })) {
    const value = await stat(path);
    if (!value.isFile()) throw new Error(`${name} is not a file: ${path}`);
  }
  await mkdir(paths.tempRoot, { recursive: true });
  const { chromium } = await import('playwright-core');
  const hardwareRenderer = options.renderer === 'hardware'
    ? await probeHardwareRenderer()
    : null;

  for (let warmup = 0; warmup < options.warmups; warmup += 1) {
    await runSample(options, paths, `warmup-${warmup}`, chromium);
  }
  const samples = [];
  for (let run = 0; run < options.runs; run += 1) {
    samples.push(await runSample(options, paths, `run-${run}`, chromium));
  }

  const [appStats, libraryStats] = await Promise.all([stat(paths.app), stat(paths.library)]);
  const cargoLock = resolveWorkspacePath('Cargo.lock');
  const pubLock = resolveWorkspacePath('flutter/vixen_shell/pubspec.lock');
  const metadata = await collectMetadata();
  if (hardwareRenderer !== null) metadata.host.hardware_renderer = hardwareRenderer;
  const report = {
    schema: 'vixen.flutter-linux-renderer-baseline-report',
    version: 2,
    measurement_only: true,
    measured_at: new Date().toISOString(),
    runs: options.runs,
    warmups: options.warmups,
    workload: {
      fixture: paths.fixture,
      fixture_url: paths.fixtureUrl,
      interaction_fixture: paths.interactionFixture,
      interaction_fixture_url: paths.interactionFixtureUrl,
      viewport: VIEWPORT,
      renderer: 'Flutter Canvas/Paragraph/Scene through release/AOT CDP host',
      renderer_mode: options.renderer,
      expected_capture_sha256: expectedCaptureSha256(options.renderer),
      interaction: {
        direct_mutation_method: 'DOM.setAttributeValue',
        direct_mutation_target: '#status class',
        direct_mutation_iterations: INTERACTION_MUTATIONS,
        input_method: 'Input.dispatchMouseEvent mouseReleased',
        input_target: '#hit',
        frame_timing_limit: FRAME_TIMING_LIMIT,
      },
    },
    metric_definitions: {
      startup_cdp_ready_ms: 'release app spawn to the loopback CDP-listener diagnostic',
      startup_first_presented_ms: 'release app spawn to the first exact Flutter commit presentation induced by the controlled capture',
      capture_dispatch_us: 'Rust monotonic CDP Page.captureScreenshot dispatch, exact presentation through PNG/base64 result',
      capture_round_trip_ms: 'Node monotonic Page.captureScreenshot request/response round trip',
      peak_memory_bytes: 'Linux /proc values for the vixen_shell process; BrowserCore/V8 and Flutter are included',
      mutation_to_presented_commit_ms: 'CDP DOM.setAttributeValue trace start to the later exact coordinator acknowledgement or matching Flutter raster finish',
      mouse_release_to_presented_commit_ms: 'CDP mouseReleased trace start to the later exact coordinator acknowledgement or matching Flutter raster finish',
      frame_durations_us: 'Flutter engine timings for exact commit frames; build/raster/total spans are not physical display presentation',
    },
    summary: metricSummary(samples),
    samples,
    artifacts: {
      app: { path: paths.app, logical_bytes: appStats.size, sha256: await sha256File(paths.app) },
      library: { path: paths.library, logical_bytes: libraryStats.size, sha256: await sha256File(paths.library) },
      fixture: { path: paths.fixture, sha256: await sha256File(paths.fixture) },
      interaction_fixture: {
        path: paths.interactionFixture,
        sha256: await sha256File(paths.interactionFixture),
      },
      cargo_lock: { path: cargoLock, sha256: await sha256File(cargoLock) },
      flutter_pubspec_lock: { path: pubLock, sha256: await sha256File(pubLock) },
    },
    metadata,
    limitations: [
      'Measurement only; no numerical startup, memory, or capture budget is accepted.',
      'vixen_shell process memory includes BrowserCore, V8, the Flutter engine, and Dart AOT; it excludes Cage and the Node client.',
      'The CDP dispatch duration covers exact-scene capture and encoding, not isolated GPU raster time or frame stability.',
      'Flutter raster finish is not compositor acceptance, scanout, or physical display presentation.',
      'Synthetic CDP mutation and mouse-release starts exclude physical device and Wayland input delivery latency.',
      'Nine serialized exact commit frames measure discrete controlled variability, not animation cadence, dropped-vsync count, or burst throughput.',
      'Frame timing callbacks and bounded diagnostics add opt-in measurement overhead.',
      'The version-2 interaction workload makes memory results unlike the shorter version-1 workload.',
      options.renderer === 'software'
        ? 'Mesa software rendering under one headless Wayland compositor is not a physical GPU/driver matrix.'
        : 'One hardware-rendered headless-Wayland run is not a GPU/driver matrix or physical scanout measurement.',
    ],
  };

  if (options.json) {
    console.log(JSON.stringify(report, null, 2));
    return;
  }
  console.log(`Flutter Linux renderer baseline (measurement only) revision=${report.metadata.git.revision ?? 'unavailable'} dirty=${report.metadata.git.dirty ?? 'unavailable'}`);
  console.log(`runs=${report.runs} warmups=${report.warmups} renderer=${options.renderer} viewport=${VIEWPORT.width}x${VIEWPORT.height} capture=${expectedCaptureSha256(options.renderer)}`);
  for (const [name, summary] of Object.entries(report.summary)) {
    if (name === 'peak_memory_bytes' || name === 'interaction') continue;
    console.log(`${name}: median=${summary.median} p95=${summary.p95} min=${summary.min} max=${summary.max}`);
  }
  const hwm = report.summary.peak_memory_bytes.vmhwm_bytes;
  console.log(`peak_vmhwm_bytes: median=${hwm?.median ?? 'unavailable'} p95=${hwm?.p95 ?? 'unavailable'}`);
  console.log(`mutation_to_presented_commit_ms: median=${report.summary.interaction.mutation_to_presented_commit_ms.median} p95=${report.summary.interaction.mutation_to_presented_commit_ms.p95}`);
  console.log(`mouse_release_to_presented_commit_ms: median=${report.summary.interaction.mouse_release_to_presented_commit_ms.median} p95=${report.summary.interaction.mouse_release_to_presented_commit_ms.p95}`);
  console.log(`frame_total_span_us: median=${report.summary.interaction.frames.total_span_us.median} p95=${report.summary.interaction.frames.total_span_us.p95}`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(`flutter-linux-baseline: ${error.message}`);
    process.exitCode = 1;
  });
}
