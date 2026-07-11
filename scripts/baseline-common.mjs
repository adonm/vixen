import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { lstat, opendir, readFile, readlink } from 'node:fs/promises';
import { arch, platform, release, totalmem } from 'node:os';
import { dirname, isAbsolute, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const workspaceRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');

export function resolveWorkspacePath(path) {
  return isAbsolute(path) ? resolve(path) : resolve(workspaceRoot, path);
}

export function percentile(samples, fraction) {
  if (!Array.isArray(samples) || samples.length === 0) return null;
  if (!Number.isFinite(fraction) || fraction < 0 || fraction > 1) {
    throw new RangeError('percentile fraction must be in [0, 1]');
  }
  const sorted = [...samples].sort((a, b) => a - b);
  const rank = (sorted.length - 1) * fraction;
  const low = Math.floor(rank);
  const high = Math.ceil(rank);
  return low === high
    ? sorted[low]
    : sorted[low] + (sorted[high] - sorted[low]) * (rank - low);
}

export function summarize(samples, digits = 3) {
  if (!Array.isArray(samples) || samples.length === 0) return null;
  if (samples.some((sample) => !Number.isFinite(sample))) {
    throw new TypeError('summary samples must be finite numbers');
  }
  const round = (value) => Number(value.toFixed(digits));
  const sorted = [...samples].sort((a, b) => a - b);
  return {
    count: samples.length,
    min: round(sorted[0]),
    median: round(percentile(sorted, 0.5)),
    p95: round(percentile(sorted, 0.95)),
    max: round(sorted.at(-1)),
    mean: round(samples.reduce((sum, value) => sum + value, 0) / samples.length),
  };
}

export function parseProcStatus(text) {
  const values = {};
  for (const key of ['VmHWM', 'VmRSS', 'VmSize']) {
    const match = text.match(new RegExp(`^${key}:\\s+(\\d+)\\s+kB$`, 'm'));
    values[`${key.toLowerCase()}_bytes`] = match ? Number(match[1]) * 1024 : null;
  }
  return values;
}

export async function sampleProcStatus(pid) {
  if (platform() !== 'linux' || !Number.isSafeInteger(pid) || pid < 1) return null;
  try {
    return parseProcStatus(await readFile(`/proc/${pid}/status`, 'utf8'));
  } catch {
    return null;
  }
}

function appendBounded(chunks, chunk, state, limit) {
  const bytes = Buffer.byteLength(chunk);
  state.total += bytes;
  if (state.captured >= limit) return;
  const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
  const keep = Math.min(buffer.length, limit - state.captured);
  if (keep > 0) {
    chunks.push(buffer.subarray(0, keep));
    state.captured += keep;
  }
}

export function measureCommand(command, args = [], options = {}) {
  if (typeof command !== 'string' || !Array.isArray(args) || args.some((arg) => typeof arg !== 'string')) {
    throw new TypeError('measureCommand requires a command and string argument array');
  }
  const timeoutMs = options.timeoutMs ?? 60_000;
  const maxOutputBytes = options.maxOutputBytes ?? 1_048_576;
  const sampleIntervalMs = options.sampleIntervalMs ?? 10;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 600_000) {
    throw new RangeError('timeoutMs must be an integer in [1, 600000]');
  }
  if (!Number.isSafeInteger(maxOutputBytes) || maxOutputBytes < 0 || maxOutputBytes > 16_777_216) {
    throw new RangeError('maxOutputBytes must be an integer in [0, 16777216]');
  }
  if (!Number.isSafeInteger(sampleIntervalMs) || sampleIntervalMs < 1 || sampleIntervalMs > 1_000) {
    throw new RangeError('sampleIntervalMs must be an integer in [1, 1000]');
  }

  return new Promise((resolveResult) => {
    const started = process.hrtime.bigint();
    const stdoutChunks = [];
    const stderrChunks = [];
    const stdoutState = { total: 0, captured: 0 };
    const stderrState = { total: 0, captured: 0 };
    const peaks = { vmhwm_bytes: null, vmrss_bytes: null, vmsize_bytes: null };
    let timedOut = false;
    let spawnError = null;
    let settled = false;
    let exitStatus = null;
    let exitSignal = null;
    let pollTimer;
    let timeoutTimer;
    let forceTimer;

    const child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env,
      detached: platform() === 'linux',
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    child.stdout.on('data', (chunk) => appendBounded(stdoutChunks, chunk, stdoutState, maxOutputBytes));
    child.stderr.on('data', (chunk) => appendBounded(stderrChunks, chunk, stderrState, maxOutputBytes));
    child.on('error', (error) => {
      spawnError = error;
    });

    const poll = async () => {
      if (settled || !child.pid) return;
      const status = await sampleProcStatus(child.pid);
      if (status) {
        for (const key of Object.keys(peaks)) {
          if (status[key] !== null) peaks[key] = Math.max(peaks[key] ?? 0, status[key]);
        }
      }
      if (!settled) pollTimer = setTimeout(poll, sampleIntervalMs);
    };
    void poll();

    timeoutTimer = setTimeout(() => {
      timedOut = true;
      try {
        if (platform() === 'linux' && child.pid) process.kill(-child.pid, 'SIGKILL');
        else child.kill('SIGKILL');
      } catch {
        child.kill('SIGKILL');
      }
      forceTimer = setTimeout(() => {
        child.stdout.destroy();
        child.stderr.destroy();
        finish(exitStatus, exitSignal ?? 'SIGKILL');
      }, 1_000);
    }, timeoutMs);

    child.on('exit', (status, signal) => {
      exitStatus = status;
      exitSignal = signal;
    });

    const finish = (status, signal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeoutTimer);
      clearTimeout(pollTimer);
      clearTimeout(forceTimer);
      const wallMs = Number(process.hrtime.bigint() - started) / 1_000_000;
      resolveResult({
        command,
        args: [...args],
        status,
        signal,
        timed_out: timedOut,
        error: spawnError?.message ?? null,
        wall_ms: Number(wallMs.toFixed(3)),
        stdout: Buffer.concat(stdoutChunks).toString('utf8'),
        stderr: Buffer.concat(stderrChunks).toString('utf8'),
        stdout_bytes: stdoutState.total,
        stderr_bytes: stderrState.total,
        stdout_truncated: stdoutState.total > stdoutState.captured,
        stderr_truncated: stderrState.total > stderrState.captured,
        peak_memory: peaks,
      });
    };

    child.on('close', finish);
  });
}

export function sha256File(path) {
  return new Promise((resolveHash, reject) => {
    const hash = createHash('sha256');
    const stream = createReadStream(path);
    stream.on('error', reject);
    stream.on('data', (chunk) => hash.update(chunk));
    stream.on('end', () => resolveHash(hash.digest('hex')));
  });
}

export async function measurePathSize(path, options = {}) {
  const deduplicateHardlinks = options.deduplicateHardlinks ?? true;
  const seen = new Set();
  const result = {
    logical_bytes: 0,
    allocated_bytes: 0,
    file_count: 0,
    directory_count: 0,
    hardlinks_deduplicated: 0,
  };

  async function visit(entryPath) {
    const stats = await lstat(entryPath, { bigint: true });
    if (stats.isDirectory()) {
      result.directory_count += 1;
      const directory = await opendir(entryPath);
      const entries = [];
      for await (const entry of directory) entries.push(entry.name);
      entries.sort();
      for (const name of entries) await visit(resolve(entryPath, name));
      return;
    }
    const key = `${stats.dev}:${stats.ino}`;
    if (deduplicateHardlinks && stats.nlink > 1n && seen.has(key)) {
      result.hardlinks_deduplicated += 1;
      return;
    }
    seen.add(key);
    result.file_count += 1;
    result.logical_bytes += Number(stats.size);
    result.allocated_bytes += Number(stats.blocks * 512n);
  }

  await visit(path);
  return result;
}

async function readOptional(path) {
  try {
    return await readFile(path, 'utf8');
  } catch {
    return null;
  }
}

function parseKeyValues(text) {
  const values = {};
  for (const line of text?.split('\n') ?? []) {
    const match = line.match(/^([A-Z0-9_]+)=(.*)$/);
    if (!match) continue;
    values[match[1]] = match[2].replace(/^"|"$/g, '').replace(/\\n/g, '\n');
  }
  return values;
}

async function commandLine(command, args, cwd = workspaceRoot) {
  const result = await measureCommand(command, args, {
    cwd,
    timeoutMs: 5_000,
    maxOutputBytes: 65_536,
    sampleIntervalMs: 50,
  });
  return result.status === 0 ? result.stdout.trim() : null;
}

export async function collectMetadata(root = workspaceRoot) {
  const [osRelease, cpuInfo, memInfo, revision, status, rustc, cargo, pageSize] = await Promise.all([
    readOptional('/etc/os-release'),
    readOptional('/proc/cpuinfo'),
    readOptional('/proc/meminfo'),
    commandLine('git', ['rev-parse', 'HEAD'], root),
    commandLine('git', ['status', '--porcelain', '--untracked-files=normal'], root),
    commandLine('rustc', ['--version'], root),
    commandLine('cargo', ['--version'], root),
    commandLine('getconf', ['PAGESIZE'], root),
  ]);
  const distro = parseKeyValues(osRelease);
  const cpuModel = cpuInfo?.match(/^model name\s*:\s*(.+)$/m)?.[1]
    ?? cpuInfo?.match(/^Hardware\s*:\s*(.+)$/m)?.[1]
    ?? null;
  const procMemory = Number(memInfo?.match(/^MemTotal:\s+(\d+)\s+kB$/m)?.[1]) * 1024;
  const rendererVariables = [
    'LIBGL_ALWAYS_SOFTWARE',
    'MESA_LOADER_DRIVER_OVERRIDE',
    'GALLIUM_DRIVER',
    'WGPU_BACKEND',
    'DISPLAY',
    'WAYLAND_DISPLAY',
    'XDG_SESSION_TYPE',
  ];
  return {
    git: {
      revision,
      dirty: status === null ? null : status.length > 0,
    },
    toolchain: {
      node: process.version,
      rustc,
      cargo,
    },
    host: {
      platform: platform(),
      architecture: arch(),
      kernel_release: release(),
      distro: distro.PRETTY_NAME ?? distro.NAME ?? null,
      cpu_model: cpuModel,
      logical_cpu_count: (cpuInfo?.match(/^processor\s*:/gm) ?? []).length || null,
      memory_total_bytes: Number.isFinite(procMemory) ? procMemory : totalmem(),
      page_size_bytes: pageSize && /^\d+$/.test(pageSize) ? Number(pageSize) : null,
      renderer_environment: Object.fromEntries(
        rendererVariables.map((name) => [name, process.env[name] ?? null]),
      ),
    },
  };
}

export async function sha256PathManifest(path) {
  const root = resolve(path);
  const rootStats = await lstat(root);
  if (!rootStats.isDirectory()) return sha256File(root);
  const hash = createHash('sha256');

  async function visit(entryPath) {
    const stats = await lstat(entryPath);
    const name = relative(root, entryPath) || '.';
    if (stats.isDirectory()) {
      hash.update(`directory\0${name}\0`);
      const directory = await opendir(entryPath);
      const entries = [];
      for await (const entry of directory) entries.push(entry.name);
      entries.sort();
      for (const entry of entries) await visit(resolve(entryPath, entry));
    } else if (stats.isSymbolicLink()) {
      hash.update(`symlink\0${name}\0${await readlink(entryPath)}\0`);
    } else {
      hash.update(`file\0${name}\0${stats.size}\0${await sha256File(entryPath)}\0`);
    }
  }

  await visit(root);
  return hash.digest('hex');
}
