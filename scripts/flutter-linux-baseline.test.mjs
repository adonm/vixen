import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import test from 'node:test';

import {
  captureDurationFromTrace,
  FlutterDiagnosticTracker,
  measuredOperation,
  parseWaylandEglInfo,
  parseFlutterLinuxBaselineArgs,
  validateMeasurementRecord,
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
    renderer: 'software',
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
      '--renderer', 'hardware',
      '--json',
    ]),
    {
      app: 'app',
      library: 'library',
      runs: 9,
      warmups: 2,
      port: 12345,
      timeoutMs: 90_000,
      renderer: 'hardware',
      json: true,
    },
  );
  assert.throws(() => parseFlutterLinuxBaselineArgs(['--runs', '0']), /\[1, 20\]/);
  assert.throws(() => parseFlutterLinuxBaselineArgs(['--port', '65536']), /\[1, 65535\]/);
  assert.throws(() => parseFlutterLinuxBaselineArgs(['--wat']), /unknown argument/);
  assert.throws(() => parseFlutterLinuxBaselineArgs(['--renderer', 'maybe']), /software or hardware/);
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

test('structured frame diagnostics join across arbitrary ordering and chunks', () => {
  const tracker = new FlutterDiagnosticTracker(1n);
  const presented = {
    v: 1,
    type: 'presented_commit',
    sequence: 4,
    context_id: 1,
    document_id: 2,
    commit_id: 3,
    revision: { source_generation: 2 },
    frame_number: 9,
    coordinator_return_wall_us: 2_000_000,
  };
  const frame = {
    v: 1,
    type: 'presented_commit_frame_timing',
    sequence: 4,
    context_id: 1,
    document_id: 2,
    commit_id: 3,
    frame_number: 9,
    refresh_rate_hz: 60,
    raster_finish_wall_us: 1_999_000,
    durations_us: { vsync_overhead: 100, build: 200, raster: 300, total_span: 600 },
  };
  const encoded = `Vixen measurement ${JSON.stringify(frame)}\nVixen measurement ${JSON.stringify(presented)}\n`;
  tracker.append(encoded.slice(0, 17), 2n);
  tracker.append(encoded.slice(17, 91), 3n);
  tracker.append(encoded.slice(91), 4n);

  assert.equal(tracker.measurementError, null);
  assert.deepEqual(tracker.completeMeasurementAfter(3), { presented, frame });
  assert.equal(tracker.latestMeasurementSequence, 4);
});

test('structured frame diagnostics reject malformed, duplicate, and limit records', () => {
  assert.throws(() => validateMeasurementRecord({ v: 2 }), /malformed/);
  assert.throws(
    () => validateMeasurementRecord({
      v: 1,
      type: 'presented_commit_frame_timing',
      sequence: 1,
      context_id: 1,
      document_id: 1,
      commit_id: 1,
      frame_number: 1,
      raster_finish_wall_us: 1,
      durations_us: { vsync_overhead: 0, build: -1, raster: 0, total_span: 0 },
    }),
    /duration build/,
  );
  const tracker = new FlutterDiagnosticTracker(1n);
  tracker.append('Vixen measurement {"v":1,"type":"frame_timing_limit_reached","limit":32}\n', 2n);
  assert.match(tracker.measurementError.message, /limit was reached/);
});

test('operation correlation uses the later exact presentation endpoint', () => {
  const operation = measuredOperation(
    'direct_mutation',
    { name: 'DOM.setAttributeValue', ts: 1_900_000, dur: 500 },
    {
      presented: {
        sequence: 1,
        context_id: 1,
        document_id: 2,
        commit_id: 3,
        revision: { source_generation: 2 },
        frame_number: 9,
        coordinator_return_wall_us: 2_000_000,
      },
      frame: {
        sequence: 1,
        context_id: 1,
        document_id: 2,
        commit_id: 3,
        frame_number: 9,
        refresh_rate_hz: 60,
        raster_finish_wall_us: 2_010_000,
        durations_us: { vsync_overhead: 100, build: 200, raster: 300, total_span: 600 },
      },
    },
  );
  assert.equal(operation.to_presented_commit_ms, 110);
  assert.equal(operation.frame.raster_us, 300);
  assert.throws(
    () => measuredOperation(
      'bad',
      { name: 'DOM.setAttributeValue', ts: 3_000_000, dur: 1 },
      {
        presented: { coordinator_return_wall_us: 2_000_000 },
        frame: { raster_finish_wall_us: 2_000_000 },
      },
    ),
    /preceded/,
  );
});

test('Wayland EGL fingerprint distinguishes hardware and software renderers', () => {
  assert.deepEqual(
    parseWaylandEglInfo(`GBM platform:\nignored\n\nWayland platform:\nEGL vendor string: Mesa Project\nEGL version string: 1.5\nOpenGL ES profile vendor: AMD\nOpenGL ES profile renderer: AMD Radeon RX 6600 (radeonsi, navi23, DRM 3.64)\nOpenGL ES profile version: OpenGL ES 3.2 Mesa 26.0.4\n\nX11 platform:\nignored\n`),
    {
      probe: 'eglinfo -B / Wayland platform OpenGL ES profile',
      egl_vendor: 'Mesa Project',
      egl_version: '1.5',
      opengl_es_vendor: 'AMD',
      opengl_es_renderer: 'AMD Radeon RX 6600 (radeonsi, navi23, DRM 3.64)',
      opengl_es_version: 'OpenGL ES 3.2 Mesa 26.0.4',
      hardware_accelerated: true,
    },
  );
  assert.equal(
    parseWaylandEglInfo(`Wayland platform:\nOpenGL ES profile renderer: llvmpipe (LLVM 21.1.8, 256 bits)\n\nX11 platform:\n`).hardware_accelerated,
    false,
  );
  assert.throws(() => parseWaylandEglInfo('GBM platform:\n'), /Wayland/);
});
