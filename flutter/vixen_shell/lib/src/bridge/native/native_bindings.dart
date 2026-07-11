import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import 'native_protocol.dart';

final class VixenBuffer extends Struct {
  @Uint64()
  external int token;

  external Pointer<Uint8> ptr;

  @Size()
  external int len;
}

final class VixenFrame extends Struct {
  @Uint64()
  external int token;

  external Pointer<Uint8> ptr;

  @Size()
  external int len;

  @Uint32()
  external int width;

  @Uint32()
  external int height;

  @Size()
  external int rowStride;

  @Uint64()
  external int frameId;

  @Uint64()
  external int contextId;

  @Uint64()
  external int documentId;
}

final class NativeCapturedFrame {
  const NativeCapturedFrame({
    required this.rgba,
    required this.width,
    required this.height,
    required this.frameId,
    required this.contextId,
    required this.documentId,
  });

  final Uint8List rgba;
  final int width;
  final int height;
  final int frameId;
  final int contextId;
  final int documentId;
}

typedef _AbiVersionNative = Uint32 Function();
typedef _AbiVersionDart = int Function();
typedef _OpenNative =
    Uint32 Function(
      Pointer<Uint8>,
      Size,
      Pointer<Uint64>,
      Pointer<VixenBuffer>,
    );
typedef _OpenDart =
    int Function(Pointer<Uint8>, int, Pointer<Uint64>, Pointer<VixenBuffer>);
typedef _DestroyNative = Uint32 Function(Uint64);
typedef _DestroyDart = int Function(int);
typedef _CommandNative =
    Uint32 Function(Uint64, Pointer<Uint8>, Size, Pointer<VixenBuffer>);
typedef _CommandDart =
    int Function(int, Pointer<Uint8>, int, Pointer<VixenBuffer>);
typedef _PollEventNative = Uint32 Function(Uint64, Pointer<VixenBuffer>);
typedef _PollEventDart = int Function(int, Pointer<VixenBuffer>);
typedef _WaitEventNative =
    Uint32 Function(Uint64, Uint64, Pointer<VixenBuffer>);
typedef _WaitEventDart = int Function(int, int, Pointer<VixenBuffer>);
typedef _BufferReleaseNative = Uint32 Function(Uint64);
typedef _BufferReleaseDart = int Function(int);
typedef _CaptureFrameNative =
    Uint32 Function(
      Uint64,
      Uint64,
      Uint64,
      Uint32,
      Uint32,
      Pointer<VixenFrame>,
      Pointer<VixenBuffer>,
    );
typedef _CaptureFrameDart =
    int Function(
      int,
      int,
      int,
      int,
      int,
      Pointer<VixenFrame>,
      Pointer<VixenBuffer>,
    );
typedef _FrameReleaseNative = Uint32 Function(Uint64);
typedef _FrameReleaseDart = int Function(int);

class _VixenNativeBindings {
  _VixenNativeBindings(DynamicLibrary library)
    : abiVersion = library.lookupFunction<_AbiVersionNative, _AbiVersionDart>(
        'vixen_abi_version',
      ),
      open = library.lookupFunction<_OpenNative, _OpenDart>('vixen_open'),
      destroy = library.lookupFunction<_DestroyNative, _DestroyDart>(
        'vixen_destroy',
      ),
      command = library.lookupFunction<_CommandNative, _CommandDart>(
        'vixen_command',
      ),
      pollEvent = library.lookupFunction<_PollEventNative, _PollEventDart>(
        'vixen_poll_event',
      ),
      waitEvent = library.lookupFunction<_WaitEventNative, _WaitEventDart>(
        'vixen_wait_event',
      ),
      bufferRelease = library
          .lookupFunction<_BufferReleaseNative, _BufferReleaseDart>(
            'vixen_buffer_release',
          ),
      captureFrame = library
          .lookupFunction<_CaptureFrameNative, _CaptureFrameDart>(
            'vixen_capture_frame',
          ),
      frameRelease = library
          .lookupFunction<_FrameReleaseNative, _FrameReleaseDart>(
            'vixen_frame_release',
          );

  final _AbiVersionDart abiVersion;
  final _OpenDart open;
  final _DestroyDart destroy;
  final _CommandDart command;
  final _PollEventDart pollEvent;
  final _WaitEventDart waitEvent;
  final _BufferReleaseDart bufferRelease;
  final _CaptureFrameDart captureFrame;
  final _FrameReleaseDart frameRelease;
}

class VixenNativeApi {
  VixenNativeApi.open(String libraryPath)
    : _bindings = _VixenNativeBindings(DynamicLibrary.open(libraryPath)) {
    final version = _bindings.abiVersion();
    if (version != vixenAbiVersion) {
      throw NativeProtocolException(
        'native ABI version is $version, expected $vixenAbiVersion',
      );
    }
  }

  final _VixenNativeBindings _bindings;

  int openProfile(String profilePath) {
    final bytes = Uint8List.fromList(utf8.encode(profilePath));
    if (bytes.isEmpty) {
      throw const NativeBridgeException(
        'profile path must not be empty',
        code: 'ffi.invalid-argument',
        status: NativeStatus.invalidArgument,
      );
    }
    if (bytes.length > vixenMaxProfilePathBytes) {
      throw NativeBridgeException(
        'profile path exceeds $vixenMaxProfilePathBytes bytes',
        code: 'ffi.input-too-large',
        status: NativeStatus.inputTooLarge,
      );
    }

    final input = _copyInput(bytes);
    final handle = calloc<Uint64>();
    final output = calloc<VixenBuffer>();
    var retainedHandle = false;
    try {
      final status = _bindings.open(input, bytes.length, handle, output);
      final payload = _consumeOutput(status, output, expectedType: 'opened');
      if (payload == null || handle.value <= 0) {
        throw const NativeProtocolException(
          'successful open did not return a nonzero handle',
        );
      }
      retainedHandle = true;
      return handle.value;
    } finally {
      if (!retainedHandle && handle.value != 0) {
        _bindings.destroy(handle.value);
      }
      calloc.free(output);
      calloc.free(handle);
      malloc.free(input);
    }
  }

  Map<String, Object?> command(int handle, Map<Object?, Object?> command) {
    final bytes = encodeNativeCommand(command);
    final input = _copyInput(bytes);
    final output = calloc<VixenBuffer>();
    try {
      final status = _bindings.command(handle, input, bytes.length, output);
      return _consumeOutput(status, output, expectedType: 'response')!;
    } finally {
      calloc.free(output);
      malloc.free(input);
    }
  }

  Map<String, Object?>? pollEvent(int handle) {
    final output = calloc<VixenBuffer>();
    try {
      final status = _bindings.pollEvent(handle, output);
      return _consumeOutput(
        status,
        output,
        expectedType: 'event',
        allowNoEvent: true,
      );
    } finally {
      calloc.free(output);
    }
  }

  Map<String, Object?>? waitEvent(int handle, int timeoutMilliseconds) {
    if (timeoutMilliseconds < 0 ||
        timeoutMilliseconds > vixenMaxWaitMilliseconds) {
      throw NativeBridgeException(
        'wait timeout must be between 0 and $vixenMaxWaitMilliseconds ms',
        code: 'ffi.invalid-argument',
        status: NativeStatus.invalidArgument,
      );
    }
    final output = calloc<VixenBuffer>();
    try {
      final status = _bindings.waitEvent(handle, timeoutMilliseconds, output);
      return _consumeOutput(
        status,
        output,
        expectedType: 'event',
        allowNoEvent: true,
      );
    } finally {
      calloc.free(output);
    }
  }

  NativeCapturedFrame captureFrame({
    required int handle,
    required int contextId,
    required int documentId,
    required int width,
    required int height,
  }) {
    validateFrameCaptureRequest(
      contextId: contextId,
      documentId: documentId,
      width: width,
      height: height,
    );
    final frame = calloc<VixenFrame>();
    final output = calloc<VixenBuffer>();
    NativeCapturedFrame? result;
    Object? failure;
    StackTrace? failureTrace;
    try {
      final status = _bindings.captureFrame(
        handle,
        contextId,
        documentId,
        width,
        height,
        frame,
        output,
      );
      _consumeFrameStatus(status, output);
      final descriptor = frame.ref;
      validateNativeFrameDescriptor(
        token: descriptor.token,
        pointerAddress: descriptor.ptr.address,
        length: descriptor.len,
        width: descriptor.width,
        height: descriptor.height,
        rowStride: descriptor.rowStride,
        frameId: descriptor.frameId,
        contextId: descriptor.contextId,
        documentId: descriptor.documentId,
        expectedWidth: width,
        expectedHeight: height,
        expectedContextId: contextId,
        expectedDocumentId: documentId,
      );
      result = NativeCapturedFrame(
        rgba: Uint8List.fromList(descriptor.ptr.asTypedList(descriptor.len)),
        width: descriptor.width,
        height: descriptor.height,
        frameId: descriptor.frameId,
        contextId: descriptor.contextId,
        documentId: descriptor.documentId,
      );
    } catch (error, stackTrace) {
      failure = error;
      failureTrace = stackTrace;
    } finally {
      final token = frame.ref.token;
      if (token != 0) {
        try {
          final releaseStatus = NativeStatus.fromValue(
            _bindings.frameRelease(token),
          );
          if (releaseStatus != NativeStatus.ok && failure == null) {
            failure = NativeBridgeException(
              'could not release native frame token',
              code: releaseStatus.defaultCode,
              status: releaseStatus,
            );
            failureTrace = StackTrace.current;
          }
        } catch (error, stackTrace) {
          failure ??= error;
          failureTrace ??= stackTrace;
        }
      }
      calloc.free(output);
      calloc.free(frame);
    }
    if (failure != null) {
      Error.throwWithStackTrace(failure, failureTrace!);
    }
    return result!;
  }

  void destroy(int handle) {
    final status = NativeStatus.fromValue(_bindings.destroy(handle));
    if (status != NativeStatus.ok) {
      throw NativeBridgeException(
        'could not destroy native browser handle',
        code: status.defaultCode,
        status: status,
      );
    }
  }

  Pointer<Uint8> _copyInput(Uint8List bytes) {
    final pointer = malloc<Uint8>(bytes.isEmpty ? 1 : bytes.length);
    if (bytes.isNotEmpty) {
      pointer.asTypedList(bytes.length).setAll(0, bytes);
    }
    return pointer;
  }

  Map<String, Object?>? _consumeOutput(
    int statusValue,
    Pointer<VixenBuffer> output, {
    required String expectedType,
    bool allowNoEvent = false,
  }) {
    final payload = _copyDecodeAndRelease(output.ref);
    final status = NativeStatus.fromValue(statusValue);

    if (status == NativeStatus.noEvent) {
      if (!allowNoEvent || payload != null) {
        throw const NativeProtocolException(
          'NO_EVENT returned an unexpected output buffer',
        );
      }
      return null;
    }
    if (status == NativeStatus.ok) {
      if (payload == null || payload['type'] != expectedType) {
        throw NativeProtocolException(
          'successful native call did not return a $expectedType envelope',
        );
      }
      return payload;
    }

    if (payload != null) {
      if (payload['type'] != 'error') {
        throw NativeProtocolException(
          '${status.name} returned a non-error JSON envelope',
        );
      }
      final error = payload['error']! as Map<String, Object?>;
      throw NativeBridgeException(
        error['message']! as String,
        code: error['code']! as String,
        status: status,
      );
    }
    throw NativeBridgeException(
      'native call failed with ${status.name}',
      code: status.defaultCode,
      status: status,
    );
  }

  void _consumeFrameStatus(int statusValue, Pointer<VixenBuffer> output) {
    final payload = _copyDecodeAndRelease(output.ref);
    final status = NativeStatus.fromValue(statusValue);
    if (status == NativeStatus.ok) {
      if (payload != null) {
        throw const NativeProtocolException(
          'successful frame capture returned error JSON',
        );
      }
      return;
    }
    if (payload != null) {
      if (payload['type'] != 'error') {
        throw NativeProtocolException(
          '${status.name} returned a non-error JSON envelope',
        );
      }
      final error = payload['error']! as Map<String, Object?>;
      throw NativeBridgeException(
        error['message']! as String,
        code: error['code']! as String,
        status: status,
      );
    }
    throw NativeBridgeException(
      'native frame capture failed with ${status.name}',
      code: status.defaultCode,
      status: status,
    );
  }

  Map<String, Object?>? _copyDecodeAndRelease(VixenBuffer buffer) {
    final token = buffer.token;
    final pointer = buffer.ptr;
    final length = buffer.len;
    if (token == 0) {
      if (pointer.address != 0 || length != 0) {
        throw const NativeProtocolException(
          'zero-token native buffer is not an empty descriptor',
        );
      }
      return null;
    }

    Object? failure;
    StackTrace? failureTrace;
    Map<String, Object?>? payload;
    try {
      if (pointer.address == 0) {
        throw const NativeProtocolException(
          'native buffer has a token but a null pointer',
        );
      }
      if (length <= 0 || length > vixenMaxOutputBytes) {
        throw NativeProtocolException(
          'native buffer length $length is outside 1..$vixenMaxOutputBytes',
        );
      }
      final copied = Uint8List.fromList(pointer.asTypedList(length));
      payload = decodeNativeJson(copied);
    } catch (error, stackTrace) {
      failure = error;
      failureTrace = stackTrace;
    } finally {
      try {
        final releaseStatus = NativeStatus.fromValue(
          _bindings.bufferRelease(token),
        );
        if (releaseStatus != NativeStatus.ok && failure == null) {
          failure = NativeBridgeException(
            'could not release native output buffer token',
            code: releaseStatus.defaultCode,
            status: releaseStatus,
          );
          failureTrace = StackTrace.current;
        }
      } catch (error, stackTrace) {
        failure ??= error;
        failureTrace ??= stackTrace;
      }
    }
    if (failure != null) {
      Error.throwWithStackTrace(failure, failureTrace!);
    }
    return payload;
  }
}

void validateFrameCaptureRequest({
  required int contextId,
  required int documentId,
  required int width,
  required int height,
}) {
  if (contextId <= 0 || documentId <= 0) {
    throw const NativeBridgeException(
      'frame context and document ids must be nonzero',
      code: 'ffi.invalid-argument',
      status: NativeStatus.invalidArgument,
    );
  }
  if (width <= 0 ||
      height <= 0 ||
      width > vixenMaxFrameDimension ||
      height > vixenMaxFrameDimension ||
      width * height * 4 > vixenMaxFrameBytes) {
    throw const NativeBridgeException(
      'frame dimensions exceed ABI bounds',
      code: 'ffi.invalid-argument',
      status: NativeStatus.invalidArgument,
    );
  }
}

void validateNativeFrameDescriptor({
  required int token,
  required int pointerAddress,
  required int length,
  required int width,
  required int height,
  required int rowStride,
  required int frameId,
  required int contextId,
  required int documentId,
  required int expectedWidth,
  required int expectedHeight,
  required int expectedContextId,
  required int expectedDocumentId,
}) {
  final expectedLength = expectedWidth * expectedHeight * 4;
  if (token == 0 ||
      pointerAddress == 0 ||
      width != expectedWidth ||
      height != expectedHeight ||
      rowStride != expectedWidth * 4 ||
      length != expectedLength ||
      length > vixenMaxFrameBytes ||
      frameId <= 0 ||
      contextId != expectedContextId ||
      documentId != expectedDocumentId) {
    throw const NativeProtocolException(
      'native frame descriptor violates the packed RGBA8 contract',
    );
  }
}
