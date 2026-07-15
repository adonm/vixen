import 'dart:io';
import 'dart:typed_data';

import '../bridge/browser_models.dart';

const int vixenAutomationMaxPngBytes = browserMaxViewportBytes + 1024 * 1024;
const List<int> _pngSignature = [137, 80, 78, 71, 13, 10, 26, 10];

final class AutomationCaptureWriter {
  const AutomationCaptureWriter();

  Future<void> write(
    String outputPath,
    Uint8List png, {
    bool Function()? canPublish,
  }) async {
    if (png.length < 24 ||
        png.length > vixenAutomationMaxPngBytes ||
        !_hasPngSignature(png)) {
      throw StateError(
        'automation output must be a PNG no larger than '
        '$vixenAutomationMaxPngBytes bytes',
      );
    }
    final output = File(outputPath);
    if (!await output.parent.exists()) {
      throw FileSystemException(
        'automation output directory does not exist',
        output.parent.path,
      );
    }
    final temporary = File(
      '${output.parent.path}/.vixen-capture-$pid-'
      '${DateTime.now().microsecondsSinceEpoch}.tmp',
    );
    try {
      await temporary.writeAsBytes(png, mode: FileMode.writeOnly, flush: true);
      if (canPublish != null && !canPublish()) {
        throw StateError(
          'automation commit changed before PNG output publication',
        );
      }
      await temporary.rename(outputPath);
    } finally {
      if (await temporary.exists()) await temporary.delete();
    }
  }

  bool _hasPngSignature(Uint8List bytes) {
    for (var index = 0; index < _pngSignature.length; index++) {
      if (bytes[index] != _pngSignature[index]) return false;
    }
    return true;
  }
}
