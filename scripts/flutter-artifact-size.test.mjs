import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import {
  analyzeFlutterBundles,
  parseFlutterArtifactArgs,
} from './flutter-artifact-size.mjs';

async function writeBundle(root, { vixen = false, engine = 'engine', forbidden = null } = {}) {
  await mkdir(join(root, 'lib'), { recursive: true });
  await mkdir(join(root, 'data', 'flutter_assets'), { recursive: true });
  await writeFile(join(root, vixen ? 'vixen_shell' : 'vixen_hello'), 'runner');
  await writeFile(join(root, 'lib', 'libflutter_linux_gtk.so'), engine);
  await writeFile(join(root, 'lib', 'libapp.so'), vixen ? 'vixen-aot' : 'hello-aot');
  await writeFile(join(root, 'data', 'icudtl.dat'), 'icu');
  await writeFile(join(root, 'data', 'flutter_assets', 'AssetManifest.bin'), 'assets');
  if (vixen) await writeFile(join(root, 'lib', 'libvixen_ffi.so'), 'native-browser');
  if (forbidden) await writeFile(join(root, forbidden), 'debug');
}

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'vixen-flutter-size-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const hello = join(root, 'hello');
  const vixen = join(root, 'vixen');
  await writeBundle(hello);
  await writeBundle(vixen, { vixen: true });
  return { hello, vixen };
}

const metadata = { test: true };

test('release bundle comparison attributes a deterministic hello delta', async (t) => {
  const { hello, vixen } = await fixture(t);
  const report = await analyzeFlutterBundles({
    helloBundle: hello,
    vixenBundle: vixen,
    metadata,
  });

  assert.equal(report.schema, 'vixen.flutter-linux-artifact-size-report');
  assert.equal(report.version, 1);
  assert.equal(report.measurement_only, true);
  assert.equal(report.flatpak_evidence, false);
  assert.equal(report.metadata, metadata);
  assert.equal(report.artifacts.hello.categories.vixen_native, undefined);
  assert.equal(report.artifacts.vixen.categories.vixen_native.file_count, 1);
  assert.equal(report.delta_from_hello.logical_bytes, 14);
  assert.equal(report.category_delta_from_hello.flutter_engine.logical_bytes, 0);
  assert.equal(report.category_delta_from_hello.flutter_icu.logical_bytes, 0);
  assert.deepEqual(
    report.artifacts.vixen.files.map((file) => file.path),
    [...report.artifacts.vixen.files.map((file) => file.path)].sort(),
  );
});

test('comparison rejects mismatched shared Flutter artifacts', async (t) => {
  const { hello, vixen } = await fixture(t);
  await writeFile(join(vixen, 'lib', 'libflutter_linux_gtk.so'), 'different');
  await assert.rejects(
    analyzeFlutterBundles({ helloBundle: hello, vixenBundle: vixen, metadata }),
    /shared Flutter artifact differs/,
  );
});

test('comparison rejects debug artifacts and misplaced Vixen native code', async (t) => {
  const { hello, vixen } = await fixture(t);
  await writeFile(join(hello, 'kernel_blob.bin'), 'debug');
  await assert.rejects(
    analyzeFlutterBundles({ helloBundle: hello, vixenBundle: vixen, metadata }),
    /forbidden build artifact/,
  );
  await rm(join(hello, 'kernel_blob.bin'));
  await writeFile(join(hello, 'lib', 'libvixen_ffi.so'), 'wrong');
  await assert.rejects(
    analyzeFlutterBundles({ helloBundle: hello, vixenBundle: vixen, metadata }),
    /hello bundle contains/,
  );
});

test('argument parser requires both unlike bundle paths', () => {
  assert.deepEqual(
    parseFlutterArtifactArgs([
      '--hello-bundle',
      'hello',
      '--vixen-bundle',
      'vixen',
      '--json',
    ]),
    { helloBundle: 'hello', vixenBundle: 'vixen', json: true },
  );
  assert.throws(() => parseFlutterArtifactArgs(['--hello-bundle', 'hello']), /required/);
  assert.throws(() => parseFlutterArtifactArgs(['--wat']), /unknown argument/);
});
