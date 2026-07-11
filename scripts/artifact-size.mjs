#!/usr/bin/env node
import { stat } from 'node:fs/promises';

import {
  collectMetadata,
  measurePathSize,
  resolveWorkspacePath,
  sha256PathManifest,
} from './baseline-common.mjs';

function usage() {
  return [
    'usage: node scripts/artifact-size.mjs --headless PATH [options]',
    '',
    'Options:',
    '  --headless PATH         Required release vixen-headless binary',
    '  --flatpak-payload PATH  Optional exported /app payload directory',
    '  --flatpak-bundle PATH   Optional Flatpak bundle file',
    '  --json                  Print the structured report',
    '  -h, --help              Print this help',
    '',
    'Hardlinked files are counted once by device+inode. The GNOME runtime is excluded.',
    'Measurement only: this command does not enforce an artifact-size budget.',
  ].join('\n');
}

function parseArgs(argv) {
  const options = { headless: null, flatpakPayload: null, flatpakBundle: null, json: false };
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
    if (['--headless', '--flatpak-payload', '--flatpak-bundle'].includes(arg)) {
      const value = argv[++index];
      if (!value) throw new Error(`${arg} requires a value`);
      if (arg === '--headless') options.headless = value;
      if (arg === '--flatpak-payload') options.flatpakPayload = value;
      if (arg === '--flatpak-bundle') options.flatpakBundle = value;
      continue;
    }
    throw new Error(`unknown argument: ${arg}`);
  }
  if (!options.headless) throw new Error('--headless is required');
  return options;
}

async function artifact(kind, configuredPath, required) {
  if (!configuredPath) return { kind, configured: false, present: false, path: null };
  const path = resolveWorkspacePath(configuredPath);
  try {
    await stat(path);
  } catch (error) {
    if (error.code === 'ENOENT' && !required) return { kind, configured: true, present: false, path };
    throw new Error(`${kind} artifact unavailable at ${path}: ${error.message}`);
  }
  return {
    kind,
    configured: true,
    present: true,
    path,
    ...(await measurePathSize(path)),
    sha256: await sha256PathManifest(path),
  };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const artifacts = {
    headless: await artifact('headless-binary', options.headless, true),
    flatpak_payload: await artifact('flatpak-payload', options.flatpakPayload, false),
    flatpak_bundle: await artifact('flatpak-bundle', options.flatpakBundle, false),
  };
  const report = {
    schema: 'vixen.artifact-size-report',
    version: 1,
    measurement_only: true,
    measured_at: new Date().toISOString(),
    accounting: {
      logical: 'lstat size, with hardlinks deduplicated by device and inode',
      allocated: 'lstat blocks multiplied by 512, with hardlinks deduplicated by device and inode',
      gnome_runtime_included: false,
      note: 'Flatpak payload and bundle measurements exclude the separately supplied GNOME runtime.',
    },
    metadata: await collectMetadata(),
    artifacts,
  };

  if (options.json) {
    console.log(JSON.stringify(report, null, 2));
    return;
  }
  console.log('artifact sizes (measurement only; GNOME runtime excluded)');
  for (const value of Object.values(artifacts)) {
    if (!value.present) {
      console.log(`${value.kind}: absent${value.path ? ` (${value.path})` : ' (not configured)'}`);
      continue;
    }
    console.log(`${value.kind}: logical_bytes=${value.logical_bytes} allocated_bytes=${value.allocated_bytes} files=${value.file_count} sha256=${value.sha256} path=${value.path}`);
  }
}

main().catch((error) => {
  console.error(`artifact-size: ${error.message}`);
  process.exitCode = 1;
});
