import assert from 'node:assert/strict';
import { link, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import {
  measureCommand,
  measurePathSize,
  parseProcStatus,
  percentile,
  sha256File,
  summarize,
} from './baseline-common.mjs';

test('percentile interpolates and summary is deterministic', () => {
  assert.equal(percentile([40, 10, 30, 20], 0.5), 25);
  assert.equal(percentile([], 0.5), null);
  assert.deepEqual(summarize([1, 2, 3, 4]), {
    count: 4,
    min: 1,
    median: 2.5,
    p95: 3.85,
    max: 4,
    mean: 2.5,
  });
});

test('proc status parser tolerates missing fields', () => {
  assert.deepEqual(parseProcStatus('Name:\ttest\nVmSize:\t 100 kB\nVmRSS:\t25 kB\n'), {
    vmhwm_bytes: null,
    vmrss_bytes: 25 * 1024,
    vmsize_bytes: 100 * 1024,
  });
});

test('process measurement uses argument arrays and bounds captured output', async () => {
  const result = await measureCommand(
    process.execPath,
    ['-e', "process.stdout.write(process.argv[1]); process.stderr.write('z'.repeat(20))", 'a;$(false)'],
    { timeoutMs: 2_000, maxOutputBytes: 8, sampleIntervalMs: 5 },
  );
  assert.equal(result.status, 0);
  assert.equal(result.stdout, 'a;$(fals');
  assert.equal(result.stdout_bytes, 10);
  assert.equal(result.stderr, 'zzzzzzzz');
  assert.equal(result.stderr_bytes, 20);
  assert.equal(result.stdout_truncated, true);
  assert.equal(result.stderr_truncated, true);
  assert.equal(result.timed_out, false);
});

test('process measurement kills a timed out child', async () => {
  const result = await measureCommand(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {
    timeoutMs: 100,
    sampleIntervalMs: 5,
  });
  assert.equal(result.timed_out, true);
  assert.equal(result.status, null);
  assert.equal(result.signal, 'SIGKILL');
  assert.ok(result.wall_ms < 2_000);
});

test('hash and recursive size account for hardlinks once', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'vixen-baseline-test-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const first = join(root, 'first');
  await writeFile(first, 'abc');
  await link(first, join(root, 'second'));
  assert.equal(
    await sha256File(first),
    'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
  );
  const size = await measurePathSize(root);
  assert.equal(size.logical_bytes, 3);
  assert.equal(size.file_count, 1);
  assert.equal(size.directory_count, 1);
  assert.equal(size.hardlinks_deduplicated, 1);
  assert.ok(size.allocated_bytes >= 3);
});
