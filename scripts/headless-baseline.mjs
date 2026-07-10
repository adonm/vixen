#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { existsSync, statSync } from 'node:fs';
import { arch, platform, release } from 'node:os';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const DEFAULT_RUNS = 5;
const DEFAULT_BINARY = 'target/release/vixen-headless';
const DEFAULT_FIXTURE = 'fixtures/dom/basic.html';
const EVAL_SOURCE = "document.readyState + ':' + document.title + ':' + document.body.textContent.trim().length";

function usage() {
  return [
    'usage: node scripts/headless-baseline.mjs [--binary PATH] [--fixture PATH] [--runs N] [--json]',
    '',
    'Measures the release headless CLI startup + first navigation + eval path.',
    'Build the binary first, e.g. `just build-release` or `just baseline-headless`.',
  ].join('\n');
}

function parseArgs(argv) {
  const args = {
    binary: DEFAULT_BINARY,
    fixture: DEFAULT_FIXTURE,
    runs: DEFAULT_RUNS,
    json: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help' || arg === '-h') {
      console.log(usage());
      process.exit(0);
    }
    if (arg === '--json') {
      args.json = true;
      continue;
    }
    if (arg === '--binary' || arg === '--fixture' || arg === '--runs') {
      const value = argv[index + 1];
      if (!value) throw new Error(`${arg} requires a value`);
      index += 1;
      if (arg === '--binary') args.binary = value;
      if (arg === '--fixture') args.fixture = value;
      if (arg === '--runs') {
        const runs = Number.parseInt(value, 10);
        if (!Number.isSafeInteger(runs) || runs < 1 || runs > 100) {
          throw new Error('--runs must be an integer in [1, 100]');
        }
        args.runs = runs;
      }
      continue;
    }
    throw new Error(`unknown argument: ${arg}`);
  }
  return args;
}

function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const rank = (sorted.length - 1) * p;
  const lo = Math.floor(rank);
  const hi = Math.ceil(rank);
  if (lo === hi) return sorted[lo];
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo);
}

function summarize(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  const sum = samples.reduce((acc, value) => acc + value, 0);
  return {
    min_ms: Number(sorted[0].toFixed(3)),
    median_ms: Number(percentile(sorted, 0.5).toFixed(3)),
    p95_ms: Number(percentile(sorted, 0.95).toFixed(3)),
    max_ms: Number(sorted[sorted.length - 1].toFixed(3)),
    mean_ms: Number((sum / samples.length).toFixed(3)),
  };
}

function runOnce(binary, fixtureUrl) {
  const started = process.hrtime.bigint();
  const child = spawnSync(binary, ['--url', fixtureUrl, '--eval', EVAL_SOURCE], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const elapsedMs = Number(process.hrtime.bigint() - started) / 1_000_000;
  if (child.error) throw child.error;
  if (child.status !== 0) {
    throw new Error(`run failed with exit ${child.status}: ${child.stderr.trim()}`);
  }
  const stdout = child.stdout.trim();
  if (!stdout.startsWith('complete:DOM basic')) {
    throw new Error(`unexpected eval output: ${JSON.stringify(stdout)}`);
  }
  return elapsedMs;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const binary = resolve(args.binary);
  const fixture = resolve(args.fixture);
  if (!existsSync(binary)) throw new Error(`missing binary: ${binary}`);
  if (!statSync(binary).isFile()) throw new Error(`not a file: ${binary}`);
  if (!existsSync(fixture)) throw new Error(`missing fixture: ${fixture}`);
  const fixtureUrl = pathToFileURL(fixture).href;

  const samples = [];
  for (let run = 0; run < args.runs; run += 1) {
    samples.push(runOnce(binary, fixtureUrl));
  }
  const report = {
    benchmark: 'headless_startup_navigation_eval',
    measured_at_unix_ms: Date.now(),
    runs: args.runs,
    binary,
    binary_bytes: statSync(binary).size,
    fixture,
    eval: EVAL_SOURCE,
    host: {
      platform: platform(),
      release: release(),
      arch: arch(),
      node: process.version,
    },
    summary: summarize(samples),
    samples_ms: samples.map((sample) => Number(sample.toFixed(3))),
  };

  if (args.json) {
    console.log(JSON.stringify(report, null, 2));
    return;
  }
  console.log(`benchmark ${report.benchmark}`);
  console.log(`binary ${report.binary} ${report.binary_bytes} bytes`);
  console.log(`fixture ${report.fixture}`);
  console.log(`runs ${report.runs}`);
  console.log(
    `ms min=${report.summary.min_ms} median=${report.summary.median_ms} p95=${report.summary.p95_ms} max=${report.summary.max_ms} mean=${report.summary.mean_ms}`,
  );
}

try {
  main();
} catch (error) {
  console.error(`headless-baseline: ${error.message}`);
  process.exit(1);
}
