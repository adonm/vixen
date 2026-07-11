import 'dart:convert';
import 'dart:typed_data';

const int vixenAbiVersion = 1;
const int vixenMaxProfilePathBytes = 4096;
const int vixenMaxMessageBytes = 65536;
const int vixenMaxOutputBytes = 1048576;
const int vixenMaxWaitMilliseconds = 60000;
const int vixenMaxEventsPerDrain = 64;
const int vixenMaxFrameDimension = 4096;
const int vixenMaxFrameBytes = 64 * 1024 * 1024;

enum NativeStatus {
  ok(0, 'ffi.ok'),
  noEvent(1, 'ffi.no-event'),
  invalidArgument(2, 'ffi.invalid-argument'),
  invalidUtf8(3, 'ffi.invalid-utf8'),
  inputTooLarge(4, 'ffi.input-too-large'),
  invalidCommand(5, 'ffi.invalid-command'),
  unknownHandle(6, 'ffi.unknown-handle'),
  browserError(7, 'browser.error'),
  unknownBuffer(8, 'ffi.unknown-buffer'),
  panic(9, 'ffi.panic'),
  internalError(10, 'ffi.internal'),
  outputTooLarge(11, 'ffi.output-too-large'),
  bufferLimit(12, 'ffi.buffer-limit'),
  frameLimit(13, 'ffi.frame-limit');

  const NativeStatus(this.value, this.defaultCode);

  final int value;
  final String defaultCode;

  static NativeStatus fromValue(int value) {
    for (final status in values) {
      if (status.value == value) {
        return status;
      }
    }
    throw NativeProtocolException('unknown native status $value');
  }
}

class NativeBridgeException implements Exception {
  const NativeBridgeException(
    this.message, {
    this.code = 'ffi.internal',
    this.status,
  });

  factory NativeBridgeException.fromMessage(Map<Object?, Object?> message) {
    final statusValue = message['status'];
    NativeStatus? status;
    if (statusValue is int) {
      status = NativeStatus.fromValue(statusValue);
    }
    return NativeBridgeException(
      message['message'] is String
          ? message['message']! as String
          : 'native worker failed',
      code: message['code'] is String
          ? message['code']! as String
          : 'ffi.internal',
      status: status,
    );
  }

  final String message;
  final String code;
  final NativeStatus? status;

  Map<String, Object?> toMessage() => <String, Object?>{
    'code': code,
    'message': message,
    if (status != null) 'status': status!.value,
  };

  @override
  String toString() {
    final statusText = status == null ? '' : ' (${status!.name})';
    return 'NativeBridgeException[$code]$statusText: $message';
  }
}

class NativeProtocolException extends NativeBridgeException {
  const NativeProtocolException(super.message) : super(code: 'ffi.protocol');
}

Map<String, Object?> decodeNativeJson(Uint8List bytes) {
  if (bytes.isEmpty) {
    throw const NativeProtocolException('native JSON output is empty');
  }
  if (bytes.length > vixenMaxOutputBytes) {
    throw NativeProtocolException(
      'native JSON output exceeds $vixenMaxOutputBytes bytes',
    );
  }

  late final String text;
  try {
    text = utf8.decode(bytes, allowMalformed: false);
  } on FormatException {
    throw const NativeProtocolException('native output is not valid UTF-8');
  }

  late final Object? decoded;
  try {
    decoded = jsonDecode(text);
  } on FormatException catch (error) {
    throw NativeProtocolException('native output is not valid JSON: $error');
  }
  if (decoded is! Map<String, Object?>) {
    throw const NativeProtocolException('native output must be a JSON object');
  }
  _validateEnvelope(decoded);
  return decoded;
}

Uint8List encodeNativeCommand(Map<Object?, Object?> command) {
  final normalized = normalizeNativeCommand(command);
  final bytes = Uint8List.fromList(utf8.encode(jsonEncode(normalized)));
  if (bytes.length > vixenMaxMessageBytes) {
    throw NativeBridgeException(
      'command exceeds $vixenMaxMessageBytes bytes',
      code: NativeStatus.inputTooLarge.defaultCode,
      status: NativeStatus.inputTooLarge,
    );
  }
  return bytes;
}

Map<String, Object?> normalizeNativeCommand(Map<Object?, Object?> command) {
  final normalized = <String, Object?>{};
  for (final entry in command.entries) {
    if (entry.key is! String) {
      throw const NativeBridgeException(
        'command keys must be strings',
        code: 'ffi.invalid-command',
        status: NativeStatus.invalidCommand,
      );
    }
    normalized[entry.key! as String] = entry.value;
  }

  if (normalized['v'] != vixenAbiVersion) {
    throw const NativeBridgeException(
      'command version must be 1',
      code: 'ffi.invalid-command',
      status: NativeStatus.invalidCommand,
    );
  }
  final type = normalized['type'];
  if (type is! String) {
    throw const NativeBridgeException(
      'command type must be a string',
      code: 'ffi.invalid-command',
      status: NativeStatus.invalidCommand,
    );
  }

  switch (type) {
    case 'load_profile_session':
    case 'save_current_profile_session':
    case 'browser_snapshot':
    case 'create_context':
      _expectKeys(normalized, const <String>{'v', 'type'});
      break;
    case 'close_context':
    case 'activate_context':
    case 'reload':
    case 'stop':
    case 'context_state':
      _expectKeys(normalized, const <String>{'v', 'type', 'context_id'});
      _validateContextId(normalized['context_id']);
      break;
    case 'navigate':
      _expectKeys(normalized, const <String>{'v', 'type', 'context_id', 'url'});
      _validateContextId(normalized['context_id']);
      if (normalized['url'] is! String) {
        _invalidCommand('url must be a string');
      }
      break;
    case 'traverse_history':
      _expectKeys(normalized, const <String>{
        'v',
        'type',
        'context_id',
        'delta',
      });
      _validateContextId(normalized['context_id']);
      final delta = normalized['delta'];
      if (delta is! int || delta < -2147483648 || delta > 2147483647) {
        _invalidCommand('delta must fit signed 32 bits');
      }
      break;
    default:
      _invalidCommand('unknown command type');
  }
  return normalized;
}

void _validateEnvelope(Map<String, Object?> envelope) {
  if (envelope['v'] != vixenAbiVersion) {
    throw const NativeProtocolException('native JSON version must be 1');
  }
  switch (envelope['type']) {
    case 'opened':
      _expectEnvelopeKeys(envelope, const <String>{'v', 'type'});
      return;
    case 'response':
      _expectEnvelopeKeys(envelope, const <String>{'v', 'type', 'response'});
      _requireTaggedObject(envelope['response'], 'response');
      return;
    case 'event':
      _expectEnvelopeKeys(envelope, const <String>{
        'v',
        'type',
        'sequence',
        'event',
      });
      final sequence = envelope['sequence'];
      if (sequence is! int || sequence <= 0) {
        throw const NativeProtocolException(
          'event sequence must be a positive integer',
        );
      }
      _requireTaggedObject(envelope['event'], 'event');
      return;
    case 'error':
      _expectEnvelopeKeys(envelope, const <String>{'v', 'type', 'error'});
      final error = envelope['error'];
      if (error is! Map<String, Object?> ||
          error['code'] is! String ||
          error['message'] is! String) {
        throw const NativeProtocolException(
          'native error must contain string code and message fields',
        );
      }
      return;
    default:
      throw NativeProtocolException(
        'unknown native JSON envelope type ${envelope['type']}',
      );
  }
}

void _requireTaggedObject(Object? value, String name) {
  if (value is! Map<String, Object?> || value['type'] is! String) {
    throw NativeProtocolException('$name must be a tagged JSON object');
  }
}

void _expectKeys(Map<String, Object?> value, Set<String> expected) {
  if (value.length != expected.length ||
      !value.keys.toSet().containsAll(expected)) {
    _invalidCommand('command has missing or unknown fields');
  }
}

void _expectEnvelopeKeys(Map<String, Object?> value, Set<String> expected) {
  if (value.length != expected.length ||
      !value.keys.toSet().containsAll(expected)) {
    throw const NativeProtocolException(
      'native JSON envelope has missing or unknown fields',
    );
  }
}

void _validateContextId(Object? value) {
  if (value is! int || value <= 0 || value > 0x7fffffffffffffff) {
    _invalidCommand('context_id must be a positive signed 64-bit integer');
  }
}

Never _invalidCommand(String message) {
  throw NativeBridgeException(
    message,
    code: NativeStatus.invalidCommand.defaultCode,
    status: NativeStatus.invalidCommand,
  );
}

Map<String, Object?> nativeCommand(
  String type, [
  Map<String, Object?> fields = const <String, Object?>{},
]) => <String, Object?>{'v': vixenAbiVersion, 'type': type, ...fields};
