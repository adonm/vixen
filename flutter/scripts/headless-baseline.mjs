#!/usr/bin/env node
import { readFile, rm, stat, mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { pathToFileURL } from 'node:url';

import {
  collectMetadata,
  measureCommand,
  resolveWorkspacePath,
  sha256File,
  summarize,
  workspaceRoot,
} from './baseline-common.mjs';

const DEFAULT_RUNS = 5;
const DEFAULT_WARMUPS = 1;
const DEFAULT_BINARY = 'target/release/vixen-headless';
const DEFAULT_FIXTURE = 'fixtures/dom/basic.html';
const MAX_RUNS = 100;
const MAX_SCENARIOS = 32;
const PROCESS_TIMEOUT_MS = 120_000;

function usage() {
  return [
    'usage: node scripts/headless-baseline.mjs [options]',
    '',
    'Options:',
    '  --binary PATH    Release vixen-headless binary',
    '  --suite JSON     Scenario suite (workspace-relative paths are supported)',
    '  --fixture PATH   Legacy single navigation+runtime fixture',
    '  --runs N         Measured runs per scenario, 1-100 (default 5)',
    '  --warmups N      Unreported warmup runs per scenario, 0-20 (default 1)',
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

function parseArgs(argv) {
  const options = {
    binary: DEFAULT_BINARY,
    fixture: DEFAULT_FIXTURE,
    suite: null,
    runs: DEFAULT_RUNS,
    warmups: DEFAULT_WARMUPS,
    json: false,
  };
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
    if (['--binary', '--fixture', '--suite', '--runs', '--warmups'].includes(arg)) {
      const value = argv[++index];
      if (!value) throw new Error(`${arg} requires a value`);
      if (arg === '--binary') options.binary = value;
      if (arg === '--fixture') options.fixture = value;
      if (arg === '--suite') options.suite = value;
      if (arg === '--runs') options.runs = boundedInteger(arg, value, 1, MAX_RUNS);
      if (arg === '--warmups') options.warmups = boundedInteger(arg, value, 0, 20);
      continue;
    }
    throw new Error(`unknown argument: ${arg}`);
  }
  return options;
}

function legacySuite(fixture) {
  return {
    schema: 'vixen.headless-scenario-suite',
    version: 1,
    description: 'Legacy single-fixture startup, navigation, and runtime measurement.',
    scenarios: [{
      id: 'startup-navigation-runtime',
      fixture,
      args: [
        '--url',
        '{fixtureUrl}',
        '--eval',
        "document.readyState + ':' + document.body.textContent.trim().length",
      ],
      validation: { stdoutIncludes: ['complete:'] },
    }],
  };
}

function validateSuite(suite) {
  if (!suite || suite.schema !== 'vixen.headless-scenario-suite' || suite.version !== 1) {
    throw new Error('suite must use schema vixen.headless-scenario-suite version 1');
  }
  if (!Array.isArray(suite.scenarios) || suite.scenarios.length < 1 || suite.scenarios.length > MAX_SCENARIOS) {
    throw new Error(`suite scenarios must contain 1-${MAX_SCENARIOS} entries`);
  }
  const ids = new Set();
  for (const scenario of suite.scenarios) {
    if (!scenario || typeof scenario.id !== 'string' || !/^[a-z0-9][a-z0-9-]{0,63}$/.test(scenario.id)) {
      throw new Error('scenario id must be a lowercase slug of at most 64 characters');
    }
    if (ids.has(scenario.id)) throw new Error(`duplicate scenario id: ${scenario.id}`);
    ids.add(scenario.id);
    if (!Array.isArray(scenario.args) || scenario.args.length > 64 || scenario.args.some((arg) => typeof arg !== 'string' || arg.length > 8_192)) {
      throw new Error(`scenario ${scenario.id} args must be at most 64 bounded strings`);
    }
    if (scenario.fixture !== undefined && (typeof scenario.fixture !== 'string' || scenario.fixture.length > 4_096)) {
      throw new Error(`scenario ${scenario.id} fixture must be a string`);
    }
    if (scenario.output !== undefined && (
      typeof scenario.output !== 'object'
      || typeof scenario.output.extension !== 'string'
      || !/^\.[a-z0-9]{1,10}$/.test(scenario.output.extension)
      || (scenario.output.minBytes !== undefined && (
        !Number.isSafeInteger(scenario.output.minBytes)
        || scenario.output.minBytes < 1
        || scenario.output.minBytes > 1_073_741_824
      ))
      || (scenario.output.magicHex !== undefined && (
        typeof scenario.output.magicHex !== 'string'
        || !/^(?:[0-9a-fA-F]{2}){1,64}$/.test(scenario.output.magicHex)
      ))
    )) {
      throw new Error(`scenario ${scenario.id} output configuration is invalid`);
    }
    const validation = scenario.validation ?? {};
    if (validation.stdoutEquals !== undefined && (
      typeof validation.stdoutEquals !== 'string' || validation.stdoutEquals.length > 8_192
    )) {
      throw new Error(`scenario ${scenario.id} stdoutEquals must be a bounded string`);
    }
    for (const key of ['stdoutIncludes', 'stderrIncludes']) {
      if (validation[key] !== undefined && (
        !Array.isArray(validation[key])
        || validation[key].length > 16
        || validation[key].some((value) => typeof value !== 'string' || value.length > 1_024)
      )) {
        throw new Error(`scenario ${scenario.id} ${key} must contain at most 16 bounded strings`);
      }
    }
  }
}

async function loadSuite(options) {
  if (!options.suite) return { suite: legacySuite(options.fixture), path: null };
  const path = resolveWorkspacePath(options.suite);
  const suiteStats = await stat(path);
  if (!suiteStats.isFile() || suiteStats.size > 262_144) {
    throw new Error('suite must be a JSON file no larger than 262144 bytes');
  }
  const suite = JSON.parse(await readFile(path, 'utf8'));
  return { suite, path };
}

function formatFailure(result) {
  const stderr = result.stderr.trim().slice(0, 2_000);
  return `exit=${result.status} signal=${result.signal} timed_out=${result.timed_out}${stderr ? ` stderr=${JSON.stringify(stderr)}` : ''}`;
}

async function validateResult(scenario, result, outputPath) {
  if (result.error || result.status !== 0 || result.timed_out) {
    throw new Error(`${scenario.id} failed: ${formatFailure(result)}`);
  }
  const validation = scenario.validation ?? {};
  const stdout = result.stdout.trim();
  if (typeof validation.stdoutEquals === 'string' && stdout !== validation.stdoutEquals) {
    throw new Error(`${scenario.id} stdout did not equal ${JSON.stringify(validation.stdoutEquals)}: ${JSON.stringify(stdout)}`);
  }
  for (const expected of validation.stdoutIncludes ?? []) {
    if (!stdout.includes(expected)) throw new Error(`${scenario.id} stdout missing ${JSON.stringify(expected)}`);
  }
  for (const expected of validation.stderrIncludes ?? []) {
    if (!result.stderr.includes(expected)) throw new Error(`${scenario.id} stderr missing ${JSON.stringify(expected)}`);
  }
  if (scenario.output) {
    let outputStats;
    try {
      outputStats = await stat(outputPath);
    } catch {
      throw new Error(`${scenario.id} did not create its temporary output`);
    }
    if (!outputStats.isFile() || outputStats.size < (scenario.output.minBytes ?? 1)) {
      throw new Error(`${scenario.id} output was smaller than ${scenario.output.minBytes ?? 1} bytes`);
    }
    if (scenario.output.magicHex) {
      const bytes = await readFile(outputPath);
      if (!bytes.subarray(0, scenario.output.magicHex.length / 2).equals(Buffer.from(scenario.output.magicHex, 'hex'))) {
        throw new Error(`${scenario.id} output did not have the expected file signature`);
      }
    }
  }
}

async function runScenario(binary, scenario, runLabel, tempRoot) {
  const fixture = scenario.fixture ? resolveWorkspacePath(scenario.fixture) : null;
  if (fixture) {
    const fixtureStats = await stat(fixture);
    if (!fixtureStats.isFile()) throw new Error(`${scenario.id} fixture is not a file: ${fixture}`);
  }
  const outputPath = scenario.output
    ? join(tempRoot, `${scenario.id}-${runLabel}${scenario.output.extension}`)
    : null;
  const substitutions = {
    '{fixture}': fixture,
    '{fixtureUrl}': fixture ? pathToFileURL(fixture).href : null,
    '{output}': outputPath,
  };
  const commandArgs = scenario.args.map((arg) => {
    let value = arg;
    for (const [placeholder, replacement] of Object.entries(substitutions)) {
      if (value.includes(placeholder)) {
        if (replacement === null) throw new Error(`${scenario.id} uses ${placeholder} without configuring it`);
        value = value.replaceAll(placeholder, replacement);
      }
    }
    return value;
  });
  const result = await measureCommand(binary, commandArgs, {
    cwd: workspaceRoot,
    timeoutMs: PROCESS_TIMEOUT_MS,
    maxOutputBytes: 1_048_576,
    sampleIntervalMs: 5,
    env: process.env,
  });
  await validateResult(scenario, result, outputPath);
  return result;
}

function memorySummary(results) {
  const summary = {};
  for (const key of ['vmhwm_bytes', 'vmrss_bytes', 'vmsize_bytes']) {
    summary[key] = summarize(results.map((result) => result.peak_memory[key]).filter((value) => value !== null), 0);
  }
  return summary;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const binary = resolveWorkspacePath(options.binary);
  const binaryStats = await stat(binary);
  if (!binaryStats.isFile()) throw new Error(`binary is not a file: ${binary}`);
  const { suite, path: suitePath } = await loadSuite(options);
  validateSuite(suite);
  const cargoLock = resolveWorkspacePath('Cargo.lock');
  const tempRoot = await mkdtemp(join(tmpdir(), 'vixen-headless-baseline-'));

  try {
    const scenarios = [];
    for (const scenario of suite.scenarios) {
      for (let warmup = 0; warmup < options.warmups; warmup += 1) {
        await runScenario(binary, scenario, `warmup-${warmup}`, tempRoot);
      }
      const results = [];
      for (let run = 0; run < options.runs; run += 1) {
        results.push(await runScenario(binary, scenario, `run-${run}`, tempRoot));
      }
      scenarios.push({
        id: scenario.id,
        description: scenario.description ?? null,
        fixture: scenario.fixture ? resolveWorkspacePath(scenario.fixture) : null,
        arguments: scenario.args,
        validation: scenario.validation ?? {},
        output: scenario.output ?? null,
        wall_ms: summarize(results.map((result) => result.wall_ms)),
        peak_memory_bytes: memorySummary(results),
        samples: results.map((result) => ({
          wall_ms: result.wall_ms,
          peak_memory_bytes: result.peak_memory,
          exit_status: result.status,
          signal: result.signal,
          stdout_bytes: result.stdout_bytes,
          stderr_bytes: result.stderr_bytes,
          stdout_truncated: result.stdout_truncated,
          stderr_truncated: result.stderr_truncated,
        })),
      });
    }

    const report = {
      schema: 'vixen.headless-baseline-report',
      version: 1,
      measurement_only: true,
      measured_at: new Date().toISOString(),
      runs: options.runs,
      warmups: options.warmups,
      suite: {
        schema: suite.schema,
        version: suite.version,
        path: suitePath,
        name: suite.name ?? basename(suitePath ?? 'legacy-single-fixture'),
        description: suite.description ?? null,
      },
      artifacts: {
        binary: { path: binary, logical_bytes: binaryStats.size, sha256: await sha256File(binary) },
        cargo_lock: { path: cargoLock, sha256: await sha256File(cargoLock) },
      },
      metadata: await collectMetadata(),
      scenarios,
    };

    if (options.json) {
      console.log(JSON.stringify(report, null, 2));
      return;
    }
    console.log(`headless scenario baseline (measurement only) revision=${report.metadata.git.revision ?? 'unavailable'} dirty=${report.metadata.git.dirty ?? 'unavailable'}`);
    console.log(`suite ${report.suite.name} schema=${report.suite.schema}@${report.suite.version} runs=${report.runs} warmups=${report.warmups}`);
    for (const scenario of scenarios) {
      const wall = scenario.wall_ms;
      const hwm = scenario.peak_memory_bytes.vmhwm_bytes;
      console.log(`${scenario.id}: wall_ms median=${wall.median} p95=${wall.p95} min=${wall.min} max=${wall.max}; peak_vmhwm_bytes median=${hwm?.median ?? 'unavailable'} p95=${hwm?.p95 ?? 'unavailable'}`);
    }
  } finally {
    await rm(tempRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(`headless-baseline: ${error.message}`);
  process.exitCode = 1;
});
