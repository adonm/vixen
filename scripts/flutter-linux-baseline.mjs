#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, rm, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

import {
  collectMetadata,
  resolveWorkspacePath,
  sampleProcStatus,
  sha256File,
  summarize,
  workspaceRoot,
} from './baseline-common.mjs';

const DEFAULT_APP = 'flutter/vixen_shell/build/linux/x64/release/bundle/vixen_shell';
const DEFAULT_LIBRARY = 'flutter/vixen_shell/build/linux/x64/release/bundle/lib/libvixen_ffi.so';
const FIXTURE = 'fixtures/dom/basic.html';
const VIEWPORT = { width: 320, height: 240 };
const EXPECTED_CAPTURE_SHA256 = '34ff6e88553c9396587d64131f48fa8e7d4579eccdc252398aa20d54472a42fb';
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
    json: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '-h' || argument === '--help') return { help: true };
    if (argument === '--json') {
      options.json = true;
      continue;
    }
    if (['--app', '--library', '--runs', '--warmups', '--port', '--timeout-ms'].includes(argument)) {
      const value = argv[++index];
      if (!value) throw new Error(`${argument} requires a value`);
      if (argument === '--app') options.app = value;
      else if (argument === '--library') options.library = value;
      else if (argument === '--runs') options.runs = boundedInteger(argument, value, 1, 20);
      else if (argument === '--warmups') options.warmups = boundedInteger(argument, value, 0, 10);
      else if (argument === '--port') options.port = boundedInteger(argument, value, 1, 65_535);
      else options.timeoutMs = boundedInteger(argument, value, 1_000, 120_000);
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
  }

  append(chunk, observedNs) {
    this.text = `${this.text}${chunk.toString('utf8')}`.slice(-65_536);
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
}

function elapsedMs(startedNs, endedNs) {
  return Number((Number(endedNs - startedNs) / 1_000_000).toFixed(3));
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
    child = spawn(paths.app, [
      '--vixen-cdp-automation',
      `--vixen-url=${paths.fixtureUrl}`,
      `--vixen-viewport=${VIEWPORT.width}x${VIEWPORT.height}`,
      `--vixen-cdp-port=${options.port}`,
    ], {
      cwd: workspaceRoot,
      env: {
        ...process.env,
        GDK_BACKEND: 'wayland',
        LIBGL_ALWAYS_SOFTWARE: '1',
        VIXEN_FFI_LIBRARY: paths.library,
        VIXEN_PROFILE_PATH: join(profileRoot, 'profile.redb'),
      },
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

    await waitUntil(() => tracker.controlReady || spawnError !== null, child, options.timeoutMs, label);
    if (spawnError) throw spawnError;
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
    const pngInfo = validateFlutterCapture(Buffer.from(capture.data, 'base64'));
    await waitUntil(() => tracker.presented !== null, child, options.timeoutMs, `${label} presentation`);

    const completed = waitForEvent(session, 'Tracing.tracingComplete', options.timeoutMs);
    await session.send('Tracing.end');
    const completion = await completed;
    const trace = await readTrace(session, completion);
    const captureDispatchUs = captureDurationFromTrace(trace, completion.dataLossOccurred);
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
  return {
    startup_cdp_ready_ms: summarize(samples.map((sample) => sample.startup_cdp_ready_ms)),
    startup_first_presented_ms: summarize(samples.map((sample) => sample.startup_first_presented_ms)),
    capture_dispatch_us: summarize(samples.map((sample) => sample.capture_dispatch_us), 0),
    capture_round_trip_ms: summarize(samples.map((sample) => sample.capture_round_trip_ms)),
    peak_memory_bytes: memorySummary(samples),
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
    tempRoot: resolveWorkspacePath('.tmp'),
  };
  paths.fixtureUrl = pathToFileURL(paths.fixture).href;
  for (const [name, path] of Object.entries({ app: paths.app, library: paths.library, fixture: paths.fixture })) {
    const value = await stat(path);
    if (!value.isFile()) throw new Error(`${name} is not a file: ${path}`);
  }
  await mkdir(paths.tempRoot, { recursive: true });
  const { chromium } = await import('playwright-core');

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
  const report = {
    schema: 'vixen.flutter-linux-renderer-baseline-report',
    version: 1,
    measurement_only: true,
    measured_at: new Date().toISOString(),
    runs: options.runs,
    warmups: options.warmups,
    workload: {
      fixture: paths.fixture,
      fixture_url: paths.fixtureUrl,
      viewport: VIEWPORT,
      renderer: 'Flutter Canvas/Paragraph/Scene through release/AOT CDP host',
      expected_capture_sha256: EXPECTED_CAPTURE_SHA256,
    },
    metric_definitions: {
      startup_cdp_ready_ms: 'release app spawn to the loopback CDP-listener diagnostic',
      startup_first_presented_ms: 'release app spawn to the first exact Flutter commit presentation induced by the controlled capture',
      capture_dispatch_us: 'Rust monotonic CDP Page.captureScreenshot dispatch, exact presentation through PNG/base64 result',
      capture_round_trip_ms: 'Node monotonic Page.captureScreenshot request/response round trip',
      peak_memory_bytes: 'Linux /proc values for the vixen_shell process; BrowserCore/V8 and Flutter are included',
    },
    summary: metricSummary(samples),
    samples,
    artifacts: {
      app: { path: paths.app, logical_bytes: appStats.size, sha256: await sha256File(paths.app) },
      library: { path: paths.library, logical_bytes: libraryStats.size, sha256: await sha256File(paths.library) },
      fixture: { path: paths.fixture, sha256: await sha256File(paths.fixture) },
      cargo_lock: { path: cargoLock, sha256: await sha256File(cargoLock) },
      flutter_pubspec_lock: { path: pubLock, sha256: await sha256File(pubLock) },
    },
    metadata: await collectMetadata(),
    limitations: [
      'Measurement only; no numerical startup, memory, or capture budget is accepted.',
      'vixen_shell process memory includes BrowserCore, V8, the Flutter engine, and Dart AOT; it excludes Cage and the Node client.',
      'The CDP dispatch duration covers exact-scene capture and encoding, not isolated GPU raster time or frame stability.',
      'Mesa software rendering under one headless Wayland compositor is not a physical GPU/driver matrix.',
    ],
  };

  if (options.json) {
    console.log(JSON.stringify(report, null, 2));
    return;
  }
  console.log(`Flutter Linux renderer baseline (measurement only) revision=${report.metadata.git.revision ?? 'unavailable'} dirty=${report.metadata.git.dirty ?? 'unavailable'}`);
  console.log(`runs=${report.runs} warmups=${report.warmups} viewport=${VIEWPORT.width}x${VIEWPORT.height} capture=${EXPECTED_CAPTURE_SHA256}`);
  for (const [name, summary] of Object.entries(report.summary)) {
    if (name === 'peak_memory_bytes') continue;
    console.log(`${name}: median=${summary.median} p95=${summary.p95} min=${summary.min} max=${summary.max}`);
  }
  const hwm = report.summary.peak_memory_bytes.vmhwm_bytes;
  console.log(`peak_vmhwm_bytes: median=${hwm?.median ?? 'unavailable'} p95=${hwm?.p95 ?? 'unavailable'}`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(`flutter-linux-baseline: ${error.message}`);
    process.exitCode = 1;
  });
}
