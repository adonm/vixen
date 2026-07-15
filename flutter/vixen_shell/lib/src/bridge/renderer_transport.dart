import 'dart:typed_data';

import 'native/native_renderer_protocol.dart';

/// Dedicated renderer endpoint, intentionally separate from serialized browser
/// commands and BrowserCore ownership.
abstract interface class RendererTransport {
  bool get rendererUpdatesEnabled;
  NativeRendererMessage? pollRenderer({int timeoutMilliseconds = 0});
  void respondRenderer(Map<String, Object?> response);
  void respondRendererCapture(int requestId, Uint8List png);
  void submitRenderer(Map<String, Object?> submission);
}
