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
const int vixenMaxAccessibilityValueBytes = 16 * 1024;

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
    case 'accessibility_snapshot':
      _expectKeys(normalized, const <String>{
        'v',
        'type',
        'context_id',
        'document_id',
        'viewport',
      });
      _validateContextId(normalized['context_id']);
      _validatePositiveId(normalized['document_id'], 'document_id');
      _validateViewport(normalized['viewport']);
      break;
    case 'dispatch_accessibility_action':
      final action = normalized['action'];
      if (action == 'focus') {
        _expectKeys(normalized, const <String>{
          'v',
          'type',
          'context_id',
          'document_id',
          'runtime_context_id',
          'viewport',
          'source_generation',
          'generation',
          'node_id',
          'action',
        });
      } else if (action == 'set_value') {
        _expectKeys(normalized, const <String>{
          'v',
          'type',
          'context_id',
          'document_id',
          'runtime_context_id',
          'viewport',
          'source_generation',
          'generation',
          'node_id',
          'action',
          'value',
        });
        _validateBoundedString(
          normalized['value'],
          'value',
          vixenMaxAccessibilityValueBytes,
        );
      } else {
        _invalidCommand('unsupported accessibility action');
      }
      _validateContextId(normalized['context_id']);
      _validatePositiveId(normalized['document_id'], 'document_id');
      _validatePositiveId(
        normalized['runtime_context_id'],
        'runtime_context_id',
      );
      _validateViewport(normalized['viewport']);
      _validatePositiveId(normalized['source_generation'], 'source_generation');
      _validatePositiveId(normalized['generation'], 'generation');
      _validatePositiveId(normalized['node_id'], 'node_id');
      break;
    case 'dispatch_mouse_event':
      _validateInputCommand(normalized);
      final eventType = normalized['event_type'];
      if (eventType != 'mousemove' &&
          eventType != 'mousedown' &&
          eventType != 'mouseup' &&
          eventType != 'wheel') {
        _invalidCommand(
          'event_type must be mousemove, mousedown, mouseup, or wheel',
        );
      }
      _validateMouseEvent(normalized['event']);
      break;
    case 'dispatch_key_event':
      _validateInputCommand(normalized);
      final eventType = normalized['event_type'];
      if (eventType != 'keydown' && eventType != 'keyup') {
        _invalidCommand('event_type must be keydown or keyup');
      }
      _validateKeyEvent(normalized['event']);
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
  _validatePositiveId(value, 'context_id');
}

void _validatePositiveId(Object? value, String name) {
  if (value is! int || value <= 0 || value > 0x7fffffffffffffff) {
    _invalidCommand('$name must be a positive signed 64-bit integer');
  }
}

void _validateInputCommand(Map<String, Object?> command) {
  _expectKeys(command, const <String>{
    'v',
    'type',
    'context_id',
    'document_id',
    'runtime_context_id',
    'viewport',
    'event_type',
    'event',
  });
  _validateContextId(command['context_id']);
  _validatePositiveId(command['document_id'], 'document_id');
  _validatePositiveId(command['runtime_context_id'], 'runtime_context_id');
  _validateViewport(command['viewport']);
  if (command['event_type'] is! String) {
    _invalidCommand('event_type must be a string');
  }
}

void _validateViewport(Object? value) {
  final viewport = _commandObject(value, 'viewport');
  _expectKeys(viewport, const <String>{'width', 'height'});
  final width = viewport['width'];
  final height = viewport['height'];
  if (width is! int ||
      height is! int ||
      width <= 0 ||
      height <= 0 ||
      width > vixenMaxFrameDimension ||
      height > vixenMaxFrameDimension ||
      width * height * 4 > vixenMaxFrameBytes) {
    _invalidCommand(
      'viewport must have positive bounded dimensions and RGBA byte length',
    );
  }
}

void _validateMouseEvent(Object? value) {
  final event = _commandObject(value, 'event');
  _expectKeys(event, const <String>{
    'x',
    'y',
    'button',
    'buttons',
    'detail',
    'bubbles',
    'ctrl_key',
    'shift_key',
    'alt_key',
    'meta_key',
    'delta_x',
    'delta_y',
  });
  for (final field in const <String>['x', 'y', 'delta_x', 'delta_y']) {
    final number = event[field];
    if (number is! num || !number.isFinite) {
      _invalidCommand('$field must be a finite number');
    }
  }
  _validateSignedInteger(event['button'], 'button', bits: 32);
  _validateSignedInteger(event['buttons'], 'buttons');
  _validateSignedInteger(event['detail'], 'detail');
  for (final field in const <String>[
    'bubbles',
    'ctrl_key',
    'shift_key',
    'alt_key',
    'meta_key',
  ]) {
    if (event[field] is! bool) _invalidCommand('$field must be a boolean');
  }
}

void _validateKeyEvent(Object? value) {
  final event = _commandObject(value, 'event');
  _expectKeys(event, const <String>{
    'key',
    'code',
    'text',
    'apply_text',
    'ctrl_key',
    'shift_key',
    'alt_key',
    'meta_key',
    'repeat',
    'location',
  });
  _validateBoundedString(event['key'], 'key', 256);
  _validateBoundedString(event['code'], 'code', 256);
  _validateBoundedString(event['text'], 'text', 4096);
  for (final field in const <String>[
    'apply_text',
    'ctrl_key',
    'shift_key',
    'alt_key',
    'meta_key',
    'repeat',
  ]) {
    if (event[field] is! bool) _invalidCommand('$field must be a boolean');
  }
  _validateSignedInteger(event['location'], 'location');
}

Map<String, Object?> _commandObject(Object? value, String name) {
  if (value is! Map<Object?, Object?> ||
      value.keys.any((key) => key is! String)) {
    _invalidCommand('$name must be an object with string keys');
  }
  return value.cast<String, Object?>();
}

void _validateSignedInteger(Object? value, String name, {int bits = 64}) {
  if (value is! int ||
      bits == 32 && (value < -2147483648 || value > 2147483647)) {
    _invalidCommand('$name must fit signed $bits bits');
  }
}

void _validateBoundedString(Object? value, String name, int maximumBytes) {
  if (value is! String) _invalidCommand('$name must be a string');
  if (utf8.encode(value).length > maximumBytes) {
    _invalidCommand('$name exceeds $maximumBytes UTF-8 bytes');
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
