import 'dart:async';

import '../browser_controller.dart';
import '../browser_models.dart';
import 'native_protocol.dart';
import 'native_renderer_protocol.dart';
import 'native_worker.dart';

/// Production adapter from the shell's typed controller seam to the isolated
/// C ABI transport.
final class NativeBrowserController extends BrowserController {
  NativeBrowserController({this.libraryPath, this.profilePath});

  static Future<NativeBrowserController> open({
    String? libraryPath,
    String? profilePath,
  }) async {
    final controller = NativeBrowserController(
      libraryPath: libraryPath,
      profilePath: profilePath,
    );
    await controller.start();
    return controller;
  }

  final String? libraryPath;
  final String? profilePath;
  final StreamController<SequencedBrowserEvent> _events =
      StreamController<SequencedBrowserEvent>();

  NativeWorkerClient? _worker;
  StreamSubscription<Map<String, Object?>>? _eventSubscription;
  Future<void>? _startOperation;
  bool _shutdown = false;
  bool _eventsClosed = false;

  @override
  Stream<SequencedBrowserEvent> get events => _events.stream;

  @override
  Future<void> start() {
    if (_shutdown) {
      return Future<void>.error(
        const BrowserFailure('browser.closed', 'Browser is shut down'),
      );
    }
    return _startOperation ??= _start();
  }

  Future<void> _start() async {
    try {
      final worker = await NativeWorkerClient.start(
        libraryPath: libraryPath,
        profilePath: profilePath,
      );
      if (_shutdown) {
        await worker.close();
        throw const BrowserFailure('browser.closed', 'Browser is shut down');
      }
      _worker = worker;
      _eventSubscription = worker.events.listen(
        (wire) {
          try {
            _events.add(SequencedBrowserEvent.fromWire(wire));
          } on FormatException catch (error) {
            _events.addError(BrowserFailure('ffi.protocol', '$error'));
          }
        },
        onError: (Object error, StackTrace stackTrace) {
          _events.addError(_browserFailure(error), stackTrace);
        },
        onDone: _closeEvents,
      );
    } catch (error) {
      throw _browserFailure(error);
    }
  }

  @override
  Future<BrowserResponse> dispatch(BrowserCommand command) async {
    final worker = _worker;
    if (worker == null || _shutdown) {
      throw const BrowserFailure(
        'browser.closed',
        'Browser controller is not running',
      );
    }
    try {
      final envelope = await worker.command(command.toWire());
      if (envelope['type'] != 'response') {
        throw const NativeProtocolException(
          'native command returned a non-response envelope',
        );
      }
      final response = envelope['response'];
      if (response is! Map) {
        throw const NativeProtocolException(
          'native response payload is not an object',
        );
      }
      return BrowserResponse.fromWire(response.cast<String, Object?>());
    } catch (error) {
      throw _browserFailure(error);
    }
  }

  NativeRendererRequest? pollRenderer({int timeoutMilliseconds = 0}) {
    final worker = _worker;
    if (worker == null || _shutdown) {
      throw const BrowserFailure('render.closed', 'Renderer broker is closed');
    }
    return worker.pollRenderer(timeoutMilliseconds: timeoutMilliseconds);
  }

  void respondRenderer(Map<String, Object?> response) {
    final worker = _worker;
    if (worker == null || _shutdown) {
      throw const BrowserFailure('render.closed', 'Renderer broker is closed');
    }
    worker.respondRenderer(response);
  }

  @override
  Future<BrowserFrame?> captureFrame({
    required int contextId,
    required int documentId,
    required int width,
    required int height,
  }) async {
    final worker = _worker;
    if (worker == null || _shutdown) {
      throw const BrowserFailure(
        'browser.closed',
        'Browser controller is not running',
      );
    }
    try {
      final wire = await worker.captureFrame(
        contextId: contextId,
        documentId: documentId,
        width: width,
        height: height,
      );
      return decodeWorkerFrameTransfer(
        wire,
        expectedContextId: contextId,
        expectedDocumentId: documentId,
        expectedWidth: width,
        expectedHeight: height,
      );
    } catch (error) {
      throw _browserFailure(error);
    }
  }

  @override
  Future<void> shutdown() async {
    if (_shutdown) {
      return;
    }
    _shutdown = true;
    try {
      final startOperation = _startOperation;
      if (startOperation != null) {
        try {
          await startOperation;
        } catch (_) {
          // Startup already reports its own stable failure.
        }
      }
      await _worker?.close();
    } catch (error) {
      throw _browserFailure(error);
    } finally {
      await _eventSubscription?.cancel();
      await _closeEvents();
    }
  }

  Future<void> _closeEvents() async {
    if (_eventsClosed) {
      return;
    }
    _eventsClosed = true;
    await _events.close();
  }
}

BrowserFailure _browserFailure(Object error) {
  if (error is BrowserFailure) {
    return error;
  }
  if (error is NativeBridgeException) {
    return BrowserFailure(error.code, error.message);
  }
  if (error is FormatException) {
    return BrowserFailure('ffi.protocol', '$error');
  }
  return BrowserFailure('ffi.internal', '$error');
}
