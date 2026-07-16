import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import test from 'node:test';

import {
  captureDurationFromTrace,
  FlutterDiagnosticTracker,
  parseFlutterLinuxBaselineArgs,
  validateFlutterCapture,
} from './flutter-linux-baseline.mjs';

test('Flutter Linux baseline arguments are bounded', () => {
  assert.deepEqual(parseFlutterLinuxBaselineArgs([]), {
    app: 'flutter/vixen_shell/build/linux/x64/release/bundle/vixen_shell',
    library: 'flutter/vixen_shell/build/linux/x64/release/bundle/lib/libvixen_ffi.so',
    runs: 5,
    warmups: 1,
    port: 9324,
    timeoutMs: 60_000,
    json: false,
  });
  assert.deepEqual(
    parseFlutterLinuxBaselineArgs([
      '--app', 'app',
      '--library', 'library',
      '--runs', '9',
      '--warmups', '2',
      '--port', '12345',
      '--timeout-ms', '90000',
      '--json',
    ]),
    {
      app: 'app',
      library: 'library',
      runs: 9,
      warmups: 2,
      port: 12345,
      timeoutMs: 90_000,
      json: true,
    },
  );
  assert.throws(() => parseFlutterLinuxBaselineArgs(['--runs', '0']), /\[1, 20\]/);
  assert.throws(() => parseFlutterLinuxBaselineArgs(['--port', '65536']), /\[1, 65535\]/);
  assert.throws(() => parseFlutterLinuxBaselineArgs(['--wat']), /unknown argument/);
});

test('Flutter diagnostics survive arbitrary chunk boundaries', () => {
  const tracker = new FlutterDiagnosticTracker(1_000_000_000n);
  tracker.append('Using the Impeller rendering back', 1_100_000_000n);
  tracker.append('end (OpenGLES).\nVixen renderer presented context=4 doc', 1_200_000_000n);
  tracker.append('ument=5 commit=6 scroll_y=0\nVixen automation CDP listen', 1_300_000_000n);
  tracker.append('ing on ws://127.0.0.1:9324\n', 1_400_000_000n);

  assert.equal(tracker.controlReady, true);
  assert.deepEqual(tracker.presented, {
    context_id: 4,
    document_id: 5,
    commit_id: 6,
    elapsed_ms: 300,
  });
  assert.deepEqual(tracker.cdp, { port: 9324, elapsed_ms: 400 });
});

test('capture validation requires exact PNG dimensions and hash', () => {
  const png = Buffer.alloc(24);
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).copy(png);
  png.writeUInt32BE(320, 16);
  png.writeUInt32BE(240, 20);
  const sha256 = createHash('sha256').update(png).digest('hex');

  assert.deepEqual(validateFlutterCapture(png, { width: 320, height: 240, sha256 }), {
    width: 320,
    height: 240,
    logical_bytes: 24,
    sha256,
  });
  assert.throws(
    () => validateFlutterCapture(png, { width: 480, height: 240, sha256 }),
    /expected 480x240/,
  );
  assert.throws(
    () => validateFlutterCapture(png, { width: 320, height: 240, sha256: '0'.repeat(64) }),
    /SHA-256/,
  );
});

test('capture trace parser rejects loss, duplicates, and failed events', () => {
  const event = {
    name: 'Page.captureScreenshot',
    cat: 'vixen.cdp',
    ph: 'X',
    dur: 1234,
    args: { ok: true },
  };
  assert.equal(captureDurationFromTrace({ traceEvents: [event] }), 1234);
  assert.throws(() => captureDurationFromTrace({ traceEvents: [event] }, true), /data loss/);
  assert.throws(() => captureDurationFromTrace({ traceEvents: [event, event] }), /got 2/);
  assert.throws(
    () => captureDurationFromTrace({ traceEvents: [{ ...event, args: { ok: false } }] }),
    /failed or malformed/,
  );
  assert.throws(
    () => captureDurationFromTrace({ traceEvents: [{ ...event, dur: 1.5 }] }),
    /failed or malformed/,
  );
});
