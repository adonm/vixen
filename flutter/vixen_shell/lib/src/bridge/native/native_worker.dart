import 'dart:async';
import 'dart:isolate';
import 'dart:typed_data';

import '../browser_models.dart';
import 'native_bindings.dart';
import 'native_paths.dart';
import 'native_protocol.dart';
import 'native_renderer_protocol.dart';

const Duration _eventPollInterval = Duration(milliseconds: 8);
const Duration _startupTimeout = Duration(seconds: 30);
const Duration _shutdownTimeout = Duration(seconds: 10);

class NativeWorkerClient {
  NativeWorkerClient._(this._isolate, this._messages);

  static Future<NativeWorkerClient> start({
    String? libraryPath,
    String? profilePath,
  }) async {
    final messages = ReceivePort();
    final isolate = await Isolate.spawn<Map<String, Object?>>(
      _nativeWorkerMain,
      <String, Object?>{
        'reply_to': messages.sendPort,
        'library_path': ?libraryPath,
        'profile_path': ?profilePath,
      },
      debugName: 'vixen-native-worker',
      errorsAreFatal: true,
      onError: messages.sendPort,
      onExit: messages.sendPort,
    );
    final client = NativeWorkerClient._(isolate, messages);
    client._subscription = messages.listen(client._handleMessage);
    try {
      await client._ready.future.timeout(_startupTimeout);
      return client;
    } catch (_) {
      await client._finish();
      rethrow;
    }
  }

  final Isolate _isolate;
  final ReceivePort _messages;
  final Completer<void> _ready = Completer<void>();
  final Map<int, Completer<Map<String, Object?>>> _pending =
      <int, Completer<Map<String, Object?>>>{};
  final StreamController<Map<String, Object?>> _events =
      StreamController<Map<String, Object?>>();

  late final StreamSubscription<Object?> _subscription;
  SendPort? _commands;
  VixenNativeApi? _rendererApi;
  int? _rendererHandle;
  int _nextRequestId = 1;
  bool _closing = false;
  bool _finished = false;
  Future<void>? _closeOperation;

  Stream<Map<String, Object?>> get events => _events.stream;

  NativeRendererMessage? pollRenderer({int timeoutMilliseconds = 0}) {
    if (_closing ||
        _finished ||
        _rendererApi == null ||
        _rendererHandle == null) {
      throw const NativeBridgeException(
        'native renderer broker is closed',
        code: 'render.closed',
      );
    }
    return _rendererApi!.pollRenderer(
      _rendererHandle!,
      timeoutMilliseconds: timeoutMilliseconds,
    );
  }

  void respondRenderer(Map<String, Object?> response) {
    if (_closing ||
        _finished ||
        _rendererApi == null ||
        _rendererHandle == null) {
      throw const NativeBridgeException(
        'native renderer broker is closed',
        code: 'render.closed',
      );
    }
    _rendererApi!.respondRenderer(_rendererHandle!, response);
  }

  void respondRendererCapture(int requestId, Uint8List png) {
    if (_closing ||
        _finished ||
        _rendererApi == null ||
        _rendererHandle == null) {
      throw const NativeBridgeException(
        'native renderer broker is closed',
        code: 'render.closed',
      );
    }
    _rendererApi!.respondRendererCapture(_rendererHandle!, requestId, png);
  }

  void submitRenderer(Map<String, Object?> submission) {
    if (_closing ||
        _finished ||
        _rendererApi == null ||
        _rendererHandle == null) {
      throw const NativeBridgeException(
        'native renderer broker is closed',
        code: 'render.closed',
      );
    }
    _rendererApi!.submitRenderer(_rendererHandle!, submission);
  }

  Future<Map<String, Object?>> command(Map<Object?, Object?> command) {
    if (_closing || _finished) {
      return Future<Map<String, Object?>>.error(
        const NativeBridgeException(
          'native worker is closed',
          code: 'ffi.worker-closed',
        ),
      );
    }
    final normalized = normalizeNativeCommand(command);
    return _request('command', command: normalized);
  }

  Future<Map<String, Object?>> captureFrame({
    required int contextId,
    required int documentId,
    required int width,
    required int height,
  }) {
    if (_closing || _finished) {
      return Future<Map<String, Object?>>.error(
        const NativeBridgeException(
          'native worker is closed',
          code: 'ffi.worker-closed',
        ),
      );
    }
    return _request(
      'capture_frame',
      fields: <String, Object?>{
        'context_id': contextId,
        'document_id': documentId,
        'width': width,
        'height': height,
      },
    );
  }

  Future<void> close() => _closeOperation ??= _close();

  Future<void> _close() async {
    if (_finished) {
      return;
    }
    final rendererApi = _rendererApi;
    final rendererHandle = _rendererHandle;
    _closing = true;
    Object? failure;
    StackTrace? failureTrace;
    try {
      if (rendererApi != null && rendererHandle != null) {
        try {
          rendererApi.shutdownRenderer(rendererHandle);
        } catch (error, stackTrace) {
          failure = error;
          failureTrace = stackTrace;
        }
      }
      try {
        await _request('shutdown').timeout(_shutdownTimeout);
      } catch (error, stackTrace) {
        failure ??= error;
        failureTrace ??= stackTrace;
      }
    } finally {
      await _finish();
    }
    if (failure != null) {
      Error.throwWithStackTrace(failure, failureTrace!);
    }
  }

  Future<Map<String, Object?>> _request(
    String kind, {
    Map<String, Object?>? command,
    Map<String, Object?> fields = const <String, Object?>{},
  }) {
    final port = _commands;
    if (port == null) {
      return Future<Map<String, Object?>>.error(
        const NativeBridgeException(
          'native worker is not ready',
          code: 'ffi.worker-not-ready',
        ),
      );
    }
    final id = _nextRequestId++;
    final completer = Completer<Map<String, Object?>>();
    _pending[id] = completer;
    port.send(<String, Object?>{
      'kind': kind,
      'id': id,
      'command': ?command,
      ...fields,
    });
    return completer.future;
  }

  void _handleMessage(Object? message) {
    if (message == null) {
      _failAll(
        const NativeBridgeException(
          'native worker exited',
          code: 'ffi.worker-exited',
        ),
      );
      unawaited(_finish());
      return;
    }
    if (message is List<Object?>) {
      final detail = message.isEmpty
          ? 'unknown isolate error'
          : '${message[0]}';
      _failAll(NativeBridgeException(detail, code: 'ffi.worker-isolate-error'));
      unawaited(_finish());
      return;
    }
    if (message is! Map<Object?, Object?> || message['kind'] is! String) {
      _failAll(
        const NativeProtocolException('native worker sent an invalid message'),
      );
      unawaited(_finish());
      return;
    }

    switch (message['kind']) {
      case 'ready':
        final port = message['port'];
        final libraryPath = message['library_path'];
        final rendererHandle = message['renderer_handle'];
        if (port is! SendPort ||
            libraryPath is! String ||
            rendererHandle is! int ||
            rendererHandle <= 0 ||
            _commands != null) {
          _failAll(
            const NativeProtocolException(
              'native worker sent invalid readiness',
            ),
          );
          unawaited(_finish());
          return;
        }
        _commands = port;
        _rendererApi = VixenNativeApi.open(libraryPath);
        _rendererHandle = rendererHandle;
        if (!_ready.isCompleted) {
          _ready.complete();
        }
        return;
      case 'response':
        final id = message['id'];
        final result = message['result'];
        if (id is! int || result is! Map<Object?, Object?>) {
          _failAll(
            const NativeProtocolException('native worker response is invalid'),
          );
          return;
        }
        final completer = _pending.remove(id);
        if (completer == null) {
          return;
        }
        completer.complete(_stringMap(result, 'worker response'));
        return;
      case 'error':
        final errorValue = message['error'];
        final error = errorValue is Map<Object?, Object?>
            ? NativeBridgeException.fromMessage(errorValue)
            : const NativeBridgeException(
                'native worker failed',
                code: 'ffi.worker-error',
              );
        final id = message['id'];
        if (id is int) {
          _pending.remove(id)?.completeError(error);
        } else {
          _failAll(error);
          _events.addError(error);
        }
        if (!_ready.isCompleted) {
          _ready.completeError(error);
        }
        return;
      case 'event':
        final event = message['event'];
        if (event is! Map<Object?, Object?>) {
          _events.addError(
            const NativeProtocolException('native worker event is invalid'),
          );
          return;
        }
        _events.add(_stringMap(event, 'worker event'));
        return;
      default:
        final error = NativeProtocolException(
          'unknown native worker message ${message['kind']}',
        );
        _failAll(error);
        _events.addError(error);
    }
  }

  void _failAll(Object error) {
    if (!_ready.isCompleted) {
      _ready.completeError(error);
    }
    final pending = _pending.values.toList(growable: false);
    _pending.clear();
    for (final completer in pending) {
      if (!completer.isCompleted) {
        completer.completeError(error);
      }
    }
  }

  Future<void> _finish() async {
    if (_finished) {
      return;
    }
    _finished = true;
    _rendererApi = null;
    _rendererHandle = null;
    _isolate.kill(priority: Isolate.immediate);
    await _subscription.cancel();
    _messages.close();
    _failAll(
      const NativeBridgeException(
        'native worker is closed',
        code: 'ffi.worker-closed',
      ),
    );
    await _events.close();
  }
}

Map<String, Object?> _stringMap(
  Map<Object?, Object?> value,
  String description,
) {
  final result = <String, Object?>{};
  for (final entry in value.entries) {
    if (entry.key is! String) {
      throw NativeProtocolException('$description has a non-string key');
    }
    result[entry.key! as String] = entry.value;
  }
  return result;
}

void _nativeWorkerMain(Map<String, Object?> bootstrap) {
  final replyTo = bootstrap['reply_to']! as SendPort;
  try {
    final libraryPath = bootstrap['library_path'] is String
        ? validateNativeLibraryPath(bootstrap['library_path']! as String)
        : resolveNativeLibraryPath();
    final profilePath = bootstrap['profile_path'] is String
        ? bootstrap['profile_path']! as String
        : resolveProfilePath();
    ensureProfileParentExists(profilePath);
    final api = VixenNativeApi.open(libraryPath);
    final handle = api.openProfile(profilePath);
    _NativeWorkerRuntime(replyTo, api, handle, libraryPath).start();
  } catch (error) {
    final bridgeError = error is NativeBridgeException
        ? error
        : NativeBridgeException('$error', code: 'ffi.worker-startup');
    Isolate.exit(replyTo, <String, Object?>{
      'kind': 'error',
      'error': bridgeError.toMessage(),
    });
  }
}

class _NativeWorkerRuntime {
  _NativeWorkerRuntime(
    this._replyTo,
    this._api,
    this._handle,
    this._libraryPath,
  );

  final SendPort _replyTo;
  final VixenNativeApi _api;
  final int _handle;
  final String _libraryPath;
  final ReceivePort _requests = ReceivePort();

  Timer? _pollTimer;
  bool _draining = false;
  bool _destroyed = false;

  void start() {
    _requests.listen(_handleRequest);
    _pollTimer = Timer.periodic(_eventPollInterval, (_) => _drainEvents());
    _replyTo.send(<String, Object?>{
      'kind': 'ready',
      'port': _requests.sendPort,
      'library_path': _libraryPath,
      'renderer_handle': _handle,
    });
    _drainEvents();
  }

  void _handleRequest(Object? message) {
    if (message is! Map<Object?, Object?> ||
        message['kind'] is! String ||
        message['id'] is! int) {
      _sendError(
        null,
        const NativeProtocolException('worker request is invalid'),
      );
      return;
    }
    final id = message['id']! as int;
    switch (message['kind']) {
      case 'command':
        final command = message['command'];
        if (command is! Map<Object?, Object?>) {
          _sendError(
            id,
            const NativeProtocolException('worker command is invalid'),
          );
          return;
        }
        try {
          final response = _api.command(
            _handle,
            normalizeNativeCommand(command),
          );
          _replyTo.send(<String, Object?>{
            'kind': 'response',
            'id': id,
            'result': response,
          });
          _drainEvents();
        } catch (error) {
          _sendError(id, _asBridgeError(error));
        }
        return;
      case 'capture_frame':
        try {
          final frame = _api.captureFrame(
            handle: _handle,
            contextId: _requestInt(message, 'context_id'),
            documentId: _requestInt(message, 'document_id'),
            width: _requestInt(message, 'width'),
            height: _requestInt(message, 'height'),
          );
          _replyTo.send(<String, Object?>{
            'kind': 'response',
            'id': id,
            'result': encodeWorkerFrameTransfer(frame),
          });
          _drainEvents();
        } catch (error) {
          _sendError(id, _asBridgeError(error));
        }
        return;
      case 'shutdown':
        _shutdown(id);
        return;
      default:
        _sendError(
          id,
          NativeProtocolException('unknown worker request ${message['kind']}'),
        );
    }
  }

  void _drainEvents() {
    if (_draining || _destroyed) {
      return;
    }
    _draining = true;
    try {
      for (var count = 0; count < vixenMaxEventsPerDrain; count++) {
        final event = _api.pollEvent(_handle);
        if (event == null) {
          break;
        }
        _replyTo.send(<String, Object?>{'kind': 'event', 'event': event});
      }
    } catch (error) {
      final bridgeError = _asBridgeError(error);
      _terminate(bridgeError);
    } finally {
      _draining = false;
    }
  }

  void _shutdown(int id) {
    NativeBridgeException? failure;
    try {
      _destroyOnce();
    } catch (error) {
      failure = _asBridgeError(error);
    }
    _pollTimer?.cancel();
    _requests.close();
    if (failure != null) {
      Isolate.exit(_replyTo, <String, Object?>{
        'kind': 'error',
        'id': id,
        'error': failure.toMessage(),
      });
    }
    Isolate.exit(_replyTo, <String, Object?>{
      'kind': 'response',
      'id': id,
      'result': <String, Object?>{'v': vixenAbiVersion, 'type': 'closed'},
    });
  }

  void _terminate(NativeBridgeException cause) {
    try {
      _destroyOnce();
    } catch (_) {
      // The original polling error is the actionable failure.
    }
    _pollTimer?.cancel();
    _requests.close();
    Isolate.exit(_replyTo, <String, Object?>{
      'kind': 'error',
      'error': cause.toMessage(),
    });
  }

  void _destroyOnce() {
    if (_destroyed) {
      return;
    }
    _destroyed = true;
    _api.destroy(_handle);
  }

  void _sendError(int? id, NativeBridgeException error) {
    _replyTo.send(<String, Object?>{
      'kind': 'error',
      'id': ?id,
      'error': error.toMessage(),
    });
  }
}

int _requestInt(Map<Object?, Object?> request, String key) {
  final value = request[key];
  if (value is! int) {
    throw NativeProtocolException('worker frame request $key is invalid');
  }
  return value;
}

Map<String, Object?> encodeWorkerFrameTransfer(NativeCapturedFrame frame) =>
    <String, Object?>{
      'type': 'frame',
      'width': frame.width,
      'height': frame.height,
      'frame_id': frame.frameId,
      'context_id': frame.contextId,
      'document_id': frame.documentId,
      'rgba': TransferableTypedData.fromList(<Uint8List>[frame.rgba]),
    };

BrowserFrame decodeWorkerFrameTransfer(
  Map<Object?, Object?> wire, {
  required int expectedContextId,
  required int expectedDocumentId,
  required int expectedWidth,
  required int expectedHeight,
}) {
  const expectedKeys = <String>{
    'type',
    'width',
    'height',
    'frame_id',
    'context_id',
    'document_id',
    'rgba',
  };
  if (wire.length != expectedKeys.length ||
      !wire.keys.every(expectedKeys.contains) ||
      wire['type'] != 'frame' ||
      wire['width'] is! int ||
      wire['height'] is! int ||
      wire['frame_id'] is! int ||
      wire['context_id'] is! int ||
      wire['document_id'] is! int ||
      wire['rgba'] is! TransferableTypedData) {
    throw const NativeProtocolException('worker frame response is invalid');
  }
  final width = wire['width']! as int;
  final height = wire['height']! as int;
  final contextId = wire['context_id']! as int;
  final documentId = wire['document_id']! as int;
  if (width != expectedWidth ||
      height != expectedHeight ||
      contextId != expectedContextId ||
      documentId != expectedDocumentId) {
    throw const NativeProtocolException(
      'worker frame response does not match its request',
    );
  }
  return BrowserFrame.fromTransfer(
    rgba: wire['rgba']! as TransferableTypedData,
    width: width,
    height: height,
    frameId: wire['frame_id']! as int,
    contextId: contextId,
    documentId: documentId,
  );
}

NativeBridgeException _asBridgeError(Object error) =>
    error is NativeBridgeException
    ? error
    : NativeBridgeException('$error', code: 'ffi.worker-internal');
