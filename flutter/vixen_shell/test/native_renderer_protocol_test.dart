import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/native/native_protocol.dart';
import 'package:vixen_shell/src/bridge/native/native_renderer_protocol.dart';
import 'package:vixen_shell/src/bridge/render_models.dart';

void main() {
  test('Rust ensure-layout golden decodes to exact immutable revision', () {
    final envelope = decodeNativeJson(
      Uint8List.fromList(
        utf8.encode(
          '{"v":1,"type":"renderer_request","request_id":7,'
          '"request":{"type":"ensure_layout","required_revision":{'
          '"context_id":1,"document_id":2,"source_generation":3,'
          '"style_generation":4,"viewport_generation":5,'
          '"resource_generation":6}}}',
        ),
      ),
    );
    final request = decodeRendererRequest(envelope);
    expect(request, isA<NativeEnsureLayoutRequest>());
    final ensure = request as NativeEnsureLayoutRequest;
    expect(ensure.requestId, 7);
    expect(
      ensure.requiredRevision,
      const RenderRevision(
        contextId: 1,
        documentId: 2,
        sourceGeneration: 3,
        styleGeneration: 4,
        viewportGeneration: 5,
        resourceGeneration: 6,
      ),
    );
  });

  test('Dart cancellation golden matches strict Rust response shape', () {
    final response = rendererCancelledResponse(7, 'stop');
    expect(jsonDecode(utf8.decode(encodeRendererResponse(response))), {
      'v': 1,
      'type': 'renderer_response',
      'request_id': 7,
      'response': {'type': 'cancelled', 'reason': 'stop'},
    });
    expect(
      () => rendererCancelledResponse(7, 'unknown'),
      throwsA(isA<RenderProtocolException>()),
    );
  });
}
