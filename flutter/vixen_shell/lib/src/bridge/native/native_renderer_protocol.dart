import 'dart:convert';
import 'dart:typed_data';

import '../render_models.dart';
import 'native_protocol.dart';

sealed class NativeRendererRequest {
  const NativeRendererRequest(this.requestId);
  final int requestId;
}

final class NativeEnsureLayoutRequest extends NativeRendererRequest {
  const NativeEnsureLayoutRequest(super.requestId, this.requiredRevision);
  final RenderRevision requiredRevision;
}

NativeRendererRequest decodeRendererRequest(Map<String, Object?> envelope) {
  renderKeys(envelope, const {'v', 'type', 'request_id', 'request'});
  if (envelope['v'] != renderProtocolVersion ||
      envelope['type'] != 'renderer_request') {
    throw const RenderProtocolException(
      'render.invalid-wire',
      'unsupported renderer request envelope',
    );
  }
  final requestId = renderPositiveInt(envelope['request_id'], 'request_id');
  final request = renderObject(envelope['request'], 'request');
  switch (request['type']) {
    case 'ensure_layout':
      renderKeys(request, const {'type', 'required_revision'});
      return NativeEnsureLayoutRequest(
        requestId,
        RenderRevision.fromWire(request['required_revision']),
      );
    default:
      throw RenderProtocolException(
        'render.invalid-wire',
        'unsupported renderer request ${request['type']}',
      );
  }
}

Map<String, Object?> rendererCommitResponse(
  int requestId,
  RenderCommit commit,
) => {
  'v': renderProtocolVersion,
  'type': 'renderer_response',
  'request_id': requestId,
  'response': commit.toWire(),
};

Map<String, Object?> rendererCancelledResponse(int requestId, String reason) {
  if (!const {
    'navigation',
    'stop',
    'context_closed',
    'shutdown',
    'deadline',
  }.contains(reason)) {
    throw const RenderProtocolException(
      'render.invalid-wire',
      'unsupported renderer cancellation reason',
    );
  }
  return {
    'v': renderProtocolVersion,
    'type': 'renderer_response',
    'request_id': requestId,
    'response': {'type': 'cancelled', 'reason': reason},
  };
}

Uint8List encodeRendererResponse(Map<String, Object?> response) {
  final bytes = Uint8List.fromList(utf8.encode(jsonEncode(response)));
  if (bytes.length > vixenMaxMessageBytes) {
    throw NativeBridgeException(
      'renderer response exceeds $vixenMaxMessageBytes bytes',
      code: NativeStatus.inputTooLarge.defaultCode,
      status: NativeStatus.inputTooLarge,
    );
  }
  return bytes;
}
