import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/automation/automation_capture.dart';
import 'package:vixen_shell/src/automation/automation_config.dart';

void main() {
  test('parses one bounded explicit automation invocation', () {
    final config = AutomationConfig.parse(const [
      '--vixen-automation',
      '--vixen-url=file:///fixture.html',
      '--vixen-viewport=640x480',
      '--vixen-output=/tmp/capture.png',
    ]);

    expect(config.url, 'file:///fixture.html');
    expect(config.width, 640);
    expect(config.height, 480);
    expect(config.outputPath, '/tmp/capture.png');
    expect(isAutomationInvocation(const ['--vixen-automation']), isTrue);
    expect(isAutomationInvocation(const ['--other']), isFalse);

    expect(
      AutomationConfig.parse(const [
        '--vixen-automation',
        '--vixen-url=https://example.test/page#target',
        '--vixen-viewport=320x240',
        '--vixen-output=/tmp/fragment.png',
      ]).url,
      'https://example.test/page#target',
    );
  });

  test('rejects missing, duplicate, unknown, and malformed arguments', () {
    const valid = [
      '--vixen-automation',
      '--vixen-url=https://example.test/',
      '--vixen-viewport=320x240',
      '--vixen-output=/tmp/capture.png',
    ];
    for (final arguments in <List<String>>[
      valid.sublist(1),
      [...valid, '--vixen-automation'],
      [...valid, '--unknown'],
      [...valid, '--vixen-url=file:///other.html'],
      [
        for (final value in valid)
          if (!value.startsWith('--vixen-url=')) value,
      ],
      [
        for (final value in valid)
          value.startsWith('--vixen-url=') ? '--vixen-url=about:blank' : value,
      ],
      [
        for (final value in valid)
          value.startsWith('--vixen-url=') ? '--vixen-url=http:' : value,
      ],
      [
        for (final value in valid)
          value.startsWith('--vixen-url=')
              ? '--vixen-url=https://example.test:99999/'
              : value,
      ],
      [
        for (final value in valid)
          value.startsWith('--vixen-url=')
              ? '--vixen-url=file:relative.html'
              : value,
      ],
      [
        for (final value in valid)
          value.startsWith('--vixen-viewport=')
              ? '--vixen-viewport=0x240'
              : value,
      ],
      [
        for (final value in valid)
          value.startsWith('--vixen-viewport=')
              ? '--vixen-viewport=4097x4096'
              : value,
      ],
      [
        for (final value in valid)
          value.startsWith('--vixen-output=')
              ? '--vixen-output=relative.png'
              : value,
      ],
      [
        for (final value in valid)
          value.startsWith('--vixen-output=')
              ? '--vixen-output=/tmp/capture.jpg'
              : value,
      ],
      [
        for (final value in valid)
          value.startsWith('--vixen-url=')
              ? '--vixen-url=https://${List.filled(vixenAutomationMaxUrlBytes, 'a').join()}'
              : value,
      ],
      [
        for (final value in valid)
          value.startsWith('--vixen-output=')
              ? '--vixen-output=/${List.filled(vixenAutomationMaxOutputPathBytes, 'a').join()}.png'
              : value,
      ],
    ]) {
      expect(
        () => AutomationConfig.parse(arguments),
        throwsA(isA<FormatException>()),
        reason: '$arguments',
      );
    }
  });

  test('writes bounded PNG output only into an existing directory', () async {
    final directory = await Directory.systemTemp.createTemp(
      'vixen-automation-writer-',
    );
    addTearDown(() => directory.delete(recursive: true));
    final output = File('${directory.path}/capture.png');
    const writer = AutomationCaptureWriter();

    final png = _minimalPng();
    await writer.write(output.path, png);
    expect(await output.readAsBytes(), png);
    await expectLater(
      writer.write(output.path, png, canPublish: () => false),
      throwsA(isA<StateError>()),
    );
    expect(await output.readAsBytes(), png);
    final replacement = Uint8List.fromList(png)..[23] = 1;
    await writer.write(output.path, replacement);
    expect(await output.readAsBytes(), replacement);
    expect(
      await directory
          .list()
          .where((entry) => entry.path.endsWith('.tmp'))
          .toList(),
      isEmpty,
    );
    await expectLater(
      writer.write('${directory.path}/missing/capture.png', png),
      throwsA(isA<FileSystemException>()),
    );
    await expectLater(
      writer.write(output.path, Uint8List(0)),
      throwsA(isA<StateError>()),
    );
  });
}

Uint8List _minimalPng() {
  final bytes = Uint8List(24);
  bytes.setAll(0, [137, 80, 78, 71, 13, 10, 26, 10]);
  return bytes;
}
