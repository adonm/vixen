import 'native/native_renderer_protocol.dart';

/// Dedicated renderer endpoint, intentionally separate from serialized browser
/// commands and BrowserCore ownership.
abstract interface class RendererTransport {
  NativeRendererMessage? pollRenderer({int timeoutMilliseconds = 0});
  void respondRenderer(Map<String, Object?> response);
  void submitRenderer(Map<String, Object?> submission);
}
