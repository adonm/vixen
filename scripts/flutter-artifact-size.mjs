#!/usr/bin/env node
import { lstat, opendir } from 'node:fs/promises';
import { basename, relative, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

import {
  collectMetadata,
  resolveWorkspacePath,
  sha256File,
  sha256PathManifest,
} from './baseline-common.mjs';

const requiredFiles = [
  'lib/libflutter_linux_gtk4.so',
  'lib/libapp.so',
  'data/icudtl.dat',
];
const forbiddenNames = new Set([
  'kernel_blob.bin',
  '.dart_tool',
  'CMakeCache.txt',
  'build.ninja',
]);

function usage() {
  return [
    'usage: node scripts/flutter-artifact-size.mjs --hello-bundle PATH --vixen-bundle PATH [options]',
    '',
    'Options:',
    '  --hello-bundle PATH  Required hello-Flutter Linux release bundle',
    '  --vixen-bundle PATH  Required Flutter+Vixen Linux release bundle',
    '  --json               Print the structured report',
    '  -h, --help           Print this help',
    '',
    'Measurement only: raw relocatable bundles are not Flatpak/download/install evidence.',
  ].join('\n');
}

export function parseFlutterArtifactArgs(argv) {
  const options = { helloBundle: null, vixenBundle: null, json: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '-h' || argument === '--help') return { help: true };
    if (argument === '--json') {
      options.json = true;
      continue;
    }
    if (argument === '--hello-bundle' || argument === '--vixen-bundle') {
      const value = argv[++index];
      if (!value) throw new Error(`${argument} requires a value`);
      if (argument === '--hello-bundle') options.helloBundle = value;
      else options.vixenBundle = value;
      continue;
    }
    throw new Error(`unknown argument: ${argument}`);
  }
  if (!options.helloBundle || !options.vixenBundle) {
    throw new Error('--hello-bundle and --vixen-bundle are required');
  }
  return options;
}

function categoryFor(path) {
  if (path === 'lib/libflutter_linux_gtk4.so') return 'flutter_engine';
  if (path === 'data/icudtl.dat') return 'flutter_icu';
  if (path === 'lib/libapp.so') return 'dart_aot';
  if (path === 'lib/libvixen_ffi.so') return 'vixen_native';
  if (path.startsWith('data/flutter_assets/')) return 'flutter_assets';
  if (!path.includes('/')) return 'native_runner';
  if (path.startsWith('lib/')) return 'plugins_and_native_assets';
  return 'other';
}

async function listBundleFiles(root) {
  const files = [];
  async function visit(path) {
    const stats = await lstat(path, { bigint: true });
    const name = relative(root, path).split('\\').join('/');
    if (stats.isSymbolicLink()) throw new Error(`bundle contains symlink: ${name}`);
    if (stats.isDirectory()) {
      if (name && forbiddenNames.has(basename(name))) {
        throw new Error(`bundle contains forbidden build artifact: ${name}`);
      }
      const directory = await opendir(path);
      const entries = [];
      for await (const entry of directory) entries.push(entry.name);
      entries.sort();
      for (const entry of entries) await visit(resolve(path, entry));
      return;
    }
    if (!stats.isFile()) throw new Error(`bundle contains unsupported entry: ${name}`);
    if (forbiddenNames.has(basename(name)) || /(^|\/)target(\/|$)/.test(name)) {
      throw new Error(`bundle contains forbidden build artifact: ${name}`);
    }
    files.push({
      path: name,
      category: categoryFor(name),
      logical_bytes: Number(stats.size),
      allocated_bytes: Number(stats.blocks * 512n),
      sha256: await sha256File(path),
    });
  }
  await visit(root);
  return files;
}

function sumFiles(files) {
  return files.reduce(
    (total, file) => ({
      logical_bytes: total.logical_bytes + file.logical_bytes,
      allocated_bytes: total.allocated_bytes + file.allocated_bytes,
      file_count: total.file_count + 1,
    }),
    { logical_bytes: 0, allocated_bytes: 0, file_count: 0 },
  );
}

function categoryTotals(files) {
  const totals = {};
  for (const file of files) {
    const total = totals[file.category] ?? {
      logical_bytes: 0,
      allocated_bytes: 0,
      file_count: 0,
    };
    total.logical_bytes += file.logical_bytes;
    total.allocated_bytes += file.allocated_bytes;
    total.file_count += 1;
    totals[file.category] = total;
  }
  return Object.fromEntries(Object.entries(totals).sort(([a], [b]) => a.localeCompare(b)));
}

function difference(vixen, hello) {
  return {
    logical_bytes: vixen.logical_bytes - hello.logical_bytes,
    allocated_bytes: vixen.allocated_bytes - hello.allocated_bytes,
    file_count: vixen.file_count - hello.file_count,
  };
}

async function inspectBundle(kind, configuredPath) {
  const root = resolveWorkspacePath(configuredPath);
  const stats = await lstat(root);
  if (!stats.isDirectory()) throw new Error(`${kind} bundle is not a directory: ${root}`);
  const files = await listBundleFiles(root);
  const paths = new Set(files.map((file) => file.path));
  for (const required of requiredFiles) {
    if (!paths.has(required)) throw new Error(`${kind} bundle is missing ${required}`);
  }
  const ffiCount = files.filter((file) => file.path === 'lib/libvixen_ffi.so').length;
  if (kind === 'hello' && ffiCount !== 0) throw new Error('hello bundle contains libvixen_ffi.so');
  if (kind === 'vixen' && ffiCount !== 1) {
    throw new Error('Vixen bundle must contain exactly one lib/libvixen_ffi.so');
  }
  return {
    root,
    sha256: await sha256PathManifest(root),
    totals: sumFiles(files),
    categories: categoryTotals(files),
    files,
  };
}

export async function analyzeFlutterBundles({ helloBundle, vixenBundle, metadata = null }) {
  const helloRoot = resolveWorkspacePath(helloBundle);
  const vixenRoot = resolveWorkspacePath(vixenBundle);
  if (helloRoot === vixenRoot) throw new Error('hello and Vixen bundle paths must differ');
  const [hello, vixen] = await Promise.all([
    inspectBundle('hello', helloRoot),
    inspectBundle('vixen', vixenRoot),
  ]);
  for (const shared of ['lib/libflutter_linux_gtk4.so', 'data/icudtl.dat']) {
    const helloFile = hello.files.find((file) => file.path === shared);
    const vixenFile = vixen.files.find((file) => file.path === shared);
    if (helloFile.sha256 !== vixenFile.sha256) {
      throw new Error(`shared Flutter artifact differs between bundles: ${shared}`);
    }
  }

  const categories = new Set([
    ...Object.keys(hello.categories),
    ...Object.keys(vixen.categories),
  ]);
  const categoryDelta = {};
  for (const category of [...categories].sort()) {
    categoryDelta[category] = difference(
      vixen.categories[category] ?? { logical_bytes: 0, allocated_bytes: 0, file_count: 0 },
      hello.categories[category] ?? { logical_bytes: 0, allocated_bytes: 0, file_count: 0 },
    );
  }

  return {
    schema: 'vixen.flutter-linux-artifact-size-report',
    version: 1,
    measurement_only: true,
    package_format: 'flutter-linux-relocatable-bundle',
    compressed_download_bytes: null,
    installed_bytes: null,
    flatpak_evidence: false,
    limitations: [
      'Raw release bundles are not distributable Flatpak, compressed-download, or installed-size evidence.',
      'libvixen_ffi.so aggregates BrowserCore/Rust and V8; this report does not invent static subcomponent sizes.',
      'No numerical artifact budget is accepted.',
    ],
    metadata: metadata ?? await collectMetadata(),
    artifacts: { hello, vixen },
    delta_from_hello: difference(vixen.totals, hello.totals),
    category_delta_from_hello: categoryDelta,
  };
}

function printText(report) {
  console.log('Flutter Linux release-bundle sizes (measurement only; not Flatpak evidence)');
  for (const [name, artifact] of Object.entries(report.artifacts)) {
    console.log(`${name}: logical_bytes=${artifact.totals.logical_bytes} allocated_bytes=${artifact.totals.allocated_bytes} files=${artifact.totals.file_count} sha256=${artifact.sha256} path=${artifact.root}`);
  }
  const delta = report.delta_from_hello;
  console.log(`vixen-minus-hello: logical_bytes=${delta.logical_bytes} allocated_bytes=${delta.allocated_bytes} files=${delta.file_count}`);
  for (const [name, value] of Object.entries(report.category_delta_from_hello)) {
    console.log(`  ${name}: logical_bytes=${value.logical_bytes} allocated_bytes=${value.allocated_bytes} files=${value.file_count}`);
  }
}

async function main() {
  const options = parseFlutterArtifactArgs(process.argv.slice(2));
  if (options.help) {
    console.log(usage());
    return;
  }
  const report = await analyzeFlutterBundles(options);
  if (options.json) console.log(JSON.stringify({ ...report, measured_at: new Date().toISOString() }, null, 2));
  else printText(report);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(`flutter-artifact-size: ${error.message}`);
    process.exitCode = 1;
  });
}
