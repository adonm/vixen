import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/native/native_bindings.dart';
import 'package:vixen_shell/src/bridge/native/native_protocol.dart';
import 'package:vixen_shell/src/bridge/native/native_worker.dart';

void main() {
  test('worker frame codec transfers pixels and exact metadata', () {
    final wire = encodeWorkerFrameTransfer(
      NativeCapturedFrame(
        rgba: Uint8List.fromList([1, 2, 3, 4, 5, 6, 7, 8]),
        width: 2,
        height: 1,
        frameId: 9,
        contextId: 10,
        documentId: 11,
      ),
    );

    final frame = decodeWorkerFrameTransfer(
      wire,
      expectedContextId: 10,
      expectedDocumentId: 11,
      expectedWidth: 2,
      expectedHeight: 1,
    );

    expect(frame.rgba, [1, 2, 3, 4, 5, 6, 7, 8]);
    expect(frame.frameId, 9);
    expect(frame.contextId, 10);
    expect(frame.documentId, 11);
  });

  test('worker frame codec rejects mismatched request metadata', () {
    final wire = encodeWorkerFrameTransfer(
      NativeCapturedFrame(
        rgba: Uint8List(4),
        width: 1,
        height: 1,
        frameId: 1,
        contextId: 2,
        documentId: 3,
      ),
    );

    expect(
      () => decodeWorkerFrameTransfer(
        wire,
        expectedContextId: 2,
        expectedDocumentId: 4,
        expectedWidth: 1,
        expectedHeight: 1,
      ),
      throwsA(isA<NativeProtocolException>()),
    );
  });
}
