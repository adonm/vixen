#!/usr/bin/env node
import { mkdir, mkdtemp, rm, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

import {
  collectMetadata,
  measureCommand,
  measurePathSize,
  resolveWorkspacePath,
  sha256File,
  summarize,
  workspaceRoot,
} from './baseline-common.mjs';

const DEFAULT_BINARY = 'target/release/vixen-headless';
const DEFAULT_RUNS = 5;
const STORAGE_BYTES = 65_536;

function usage() {
  return [
    'usage: node scripts/profile-growth-baseline.mjs [options]',
    '',
    'Options:',
    '  --binary PATH   Release vixen-headless binary',
    '  --runs N        Repeated visits and unique data URLs, 1-50 (default 5)',
    '  --json          Print the structured report',
    '  -h, --help      Print this help',
    '',
    'Uses a temporary explicit --profile-dir and sizes it only after each process exits.',
    'Measurement only: no profile growth budget is enforced.',
  ].join('\n');
}

function parseArgs(argv) {
  const options = { binary: DEFAULT_BINARY, runs: DEFAULT_RUNS, json: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help' || arg === '-h') {
      console.log(usage());
      process.exit(0);
    }
    if (arg === '--json') {
      options.json = true;
      continue;
    }
    if (arg === '--binary' || arg === '--runs') {
      const value = argv[++index];
      if (!value) throw new Error(`${arg} requires a value`);
      if (arg === '--binary') options.binary = value;
      if (arg === '--runs') {
        options.runs = Number(value);
        if (!Number.isSafeInteger(options.runs) || options.runs < 1 || options.runs > 50) {
          throw new Error('--runs must be an integer in [1, 50]');
        }
      }
      continue;
    }
    throw new Error(`unknown argument: ${arg}`);
  }
  return options;
}

function processSample(label, result) {
  return {
    label,
    wall_ms: result.wall_ms,
    peak_memory_bytes: result.peak_memory,
    exit_status: result.status,
    stdout_bytes: result.stdout_bytes,
    stderr_bytes: result.stderr_bytes,
  };
}

async function runAction(binary, profile, label, url, expression, expected) {
  const result = await measureCommand(binary, [
    '--profile-dir',
    profile,
    '--url',
    url,
    '--eval',
    expression,
  ], {
    cwd: workspaceRoot,
    timeoutMs: 120_000,
    maxOutputBytes: 262_144,
    sampleIntervalMs: 5,
  });
  if (result.error || result.status !== 0 || result.timed_out) {
    throw new Error(`${label} failed: exit=${result.status} signal=${result.signal} timed_out=${result.timed_out} stderr=${JSON.stringify(result.stderr.trim().slice(0, 2_000))}`);
  }
  if (result.stdout.trim() !== expected) {
    throw new Error(`${label} returned ${JSON.stringify(result.stdout.trim())}, expected ${JSON.stringify(expected)}`);
  }
  return processSample(label, result);
}

async function checkpoint(name, profile, previous) {
  const size = await measurePathSize(profile);
  return {
    name,
    ...size,
    logical_growth_bytes: previous ? size.logical_bytes - previous.logical_bytes : size.logical_bytes,
    allocated_growth_bytes: previous ? size.allocated_bytes - previous.allocated_bytes : size.allocated_bytes,
  };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const binary = resolveWorkspacePath(options.binary);
  const binaryStats = await stat(binary);
  if (!binaryStats.isFile()) throw new Error(`binary is not a file: ${binary}`);
  const fixture = resolveWorkspacePath('fixtures/realworld/static-document.html');
  const fixtureUrl = pathToFileURL(fixture).href;
  const tempRoot = resolveWorkspacePath('.tmp');
  await mkdir(tempRoot, { recursive: true });
  const root = await mkdtemp(join(tempRoot, 'vixen-profile-growth-'));
  const profile = join(root, 'profile');
  const checkpoints = [];
  const processes = [];

  try {
    processes.push(await runAction(binary, profile, 'init', fixtureUrl, "document.title", 'Vixen static document control'));
    checkpoints.push(await checkpoint('init', profile, null));

    for (let run = 0; run < options.runs; run += 1) {
      processes.push(await runAction(
        binary,
        profile,
        `repeated-local-${run + 1}`,
        fixtureUrl,
        "document.querySelectorAll('article').length",
        '1',
      ));
    }
    checkpoints.push(await checkpoint('repeated_visits', profile, checkpoints.at(-1)));

    for (let run = 0; run < options.runs; run += 1) {
      const title = `Unique control ${String(run + 1).padStart(3, '0')}`;
      const dataUrl = `data:text/html;charset=utf-8,${encodeURIComponent(`<!doctype html><title>${title}</title><p>deterministic unique visit</p>`)}`;
      processes.push(await runAction(binary, profile, `unique-data-${run + 1}`, dataUrl, 'document.title', title));
    }
    checkpoints.push(await checkpoint('unique_visits', profile, checkpoints.at(-1)));

    const storageExpression = `(() => { localStorage.setItem('baseline-payload', 'x'.repeat(${STORAGE_BYTES})); localStorage.setItem('baseline-marker', 'persisted'); return localStorage.getItem('baseline-payload').length; })()`;
    processes.push(await runAction(binary, profile, 'storage-write', fixtureUrl, storageExpression, String(STORAGE_BYTES)));
    processes.push(await runAction(
      binary,
      profile,
      'storage-reopen-check',
      fixtureUrl,
      "localStorage.getItem('baseline-marker') + ':' + localStorage.getItem('baseline-payload').length",
      `persisted:${STORAGE_BYTES}`,
    ));
    checkpoints.push(await checkpoint('storage_payload', profile, checkpoints.at(-1)));

    const report = {
      schema: 'vixen.profile-growth-baseline-report',
      version: 1,
      measurement_only: true,
      measured_at: new Date().toISOString(),
      profile: {
        temporary: true,
        explicit_profile_dir: profile,
        storage_payload_bytes: STORAGE_BYTES,
      },
      workload: {
        repeated_local_visits: options.runs,
        unique_data_url_visits: options.runs,
        persistence_reopen_verified: true,
        fixture,
      },
      artifacts: {
        binary: { path: binary, logical_bytes: binaryStats.size, sha256: await sha256File(binary) },
        cargo_lock: {
          path: resolveWorkspacePath('Cargo.lock'),
          sha256: await sha256File(resolveWorkspacePath('Cargo.lock')),
        },
      },
      metadata: await collectMetadata(),
      checkpoints,
      process_wall_ms: summarize(processes.map((sample) => sample.wall_ms)),
      processes,
    };

    if (options.json) {
      console.log(JSON.stringify(report, null, 2));
      return;
    }
    console.log(`profile growth baseline (measurement only) runs=${options.runs} storage_payload_bytes=${STORAGE_BYTES}`);
    for (const point of checkpoints) {
      console.log(`${point.name}: logical_bytes=${point.logical_bytes} allocated_bytes=${point.allocated_bytes} logical_growth_bytes=${point.logical_growth_bytes} allocated_growth_bytes=${point.allocated_growth_bytes} files=${point.file_count}`);
    }
    console.log('localStorage persistence reopen check: passed');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(`profile-growth-baseline: ${error.message}`);
  process.exitCode = 1;
});
