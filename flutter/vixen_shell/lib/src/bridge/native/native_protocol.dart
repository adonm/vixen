import 'dart:convert';
import 'dart:typed_data';

const int vixenAbiVersion = 1;
const int vixenMaxProfilePathBytes = 4096;
const int vixenMaxMessageBytes = 65536;
const int vixenMaxOutputBytes = 1048576;
const int vixenMaxWaitMilliseconds = 60000;
const int vixenMaxEventsPerDrain = 64;
const int vixenMaxViewportDimension = 4096;
const int vixenMaxViewportBytes = 64 * 1024 * 1024;
const int vixenMaxAccessibilityValueBytes = 16 * 1024;
const int vixenMaxTextInputBytes = 16 * 1024;

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
  bufferLimit(12, 'ffi.buffer-limit');

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
    case 'start_cdp':
      _expectKeys(normalized, const <String>{'v', 'type', 'port'});
      final port = normalized['port'];
      if (port is! int || port <= 0 || port > 65535) {
        _invalidCommand('CDP port must be in 1..65535');
      }
      break;
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
    case 'find_text':
      _expectKeys(normalized, const <String>{
        'v',
        'type',
        'context_id',
        'document_id',
        'query',
        'case_sensitive',
        'forward',
      });
      _validateContextId(normalized['context_id']);
      _validatePositiveId(normalized['document_id'], 'document_id');
      _validateBoundedString(normalized['query'], 'query', 4096);
      if (normalized['case_sensitive'] is! bool) {
        _invalidCommand('case_sensitive must be a boolean');
      }
      if (normalized['forward'] is! bool) {
        _invalidCommand('forward must be a boolean');
      }
      break;
    case 'set_page_zoom':
      _expectKeys(normalized, const <String>{
        'v',
        'type',
        'context_id',
        'zoom',
      });
      _validateContextId(normalized['context_id']);
      final zoom = normalized['zoom'];
      if (zoom is! num || !zoom.isFinite || zoom < 0.25 || zoom > 5) {
        _invalidCommand('zoom must be finite and in range');
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
    case 'publish_renderer_snapshot':
      _expectKeys(normalized, const <String>{
        'v',
        'type',
        'context_id',
        'document_id',
        'viewport',
        'viewport_generation',
        'page_zoom',
      });
      _validateContextId(normalized['context_id']);
      _validatePositiveId(normalized['document_id'], 'document_id');
      _validateViewport(normalized['viewport']);
      _validatePositiveId(
        normalized['viewport_generation'],
        'viewport_generation',
      );
      final pageZoom = normalized['page_zoom'];
      if (pageZoom is! num ||
          !pageZoom.isFinite ||
          pageZoom < 0.25 ||
          pageZoom > 5) {
        _invalidCommand('page_zoom must be finite and in range');
      }
      break;
    case 'flush_renderer_submissions':
      _expectKeys(normalized, const <String>{'v', 'type'});
      break;
    case 'update_host_view_state':
      _expectKeys(normalized, const <String>{
        'v',
        'type',
        'context_id',
        'generation',
        'viewport',
        'scale_factor',
        'focused',
        'visible',
        'lifecycle',
      });
      _validateContextId(normalized['context_id']);
      _validatePositiveId(normalized['generation'], 'generation');
      _validateViewport(normalized['viewport']);
      final scaleFactor = normalized['scale_factor'];
      if (scaleFactor is! num ||
          !scaleFactor.isFinite ||
          scaleFactor < 0.1 ||
          scaleFactor > 16) {
        _invalidCommand('scale_factor must be finite and in range');
      }
      if (normalized['focused'] is! bool || normalized['visible'] is! bool) {
        _invalidCommand('host focus and visibility must be booleans');
      }
      if (!const {
        'resumed',
        'inactive',
        'hidden',
        'paused',
        'detached',
      }.contains(normalized['lifecycle'])) {
        _invalidCommand('unsupported host lifecycle');
      }
      break;
    case 'dispatch_accessibility_action':
      final action = normalized['action'];
      if (action == 'focus' || action == 'increase' || action == 'decrease') {
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
    case 'dispatch_renderer_mouse_event':
      _expectKeys(normalized, const <String>{
        'v',
        'type',
        'context_id',
        'document_id',
        'runtime_context_id',
        'viewport',
        'event_type',
        'event',
        'query',
        'target',
      });
      _validateContextId(normalized['context_id']);
      _validatePositiveId(normalized['document_id'], 'document_id');
      _validatePositiveId(
        normalized['runtime_context_id'],
        'runtime_context_id',
      );
      _validateViewport(normalized['viewport']);
      final eventType = normalized['event_type'];
      if (eventType != 'mousemove' &&
          eventType != 'mousedown' &&
          eventType != 'mouseup' &&
          eventType != 'wheel' &&
          eventType != 'cancel') {
        _invalidCommand(
          'event_type must be mousemove, mousedown, mouseup, wheel, or cancel',
        );
      }
      _validateMouseEvent(normalized['event']);
      _validateRenderHitTestQuery(normalized['query']);
      final target = normalized['target'];
      if (target != null) _validateRenderInputTarget(target);
      break;
    case 'dispatch_key_event':
      _validateInputCommand(normalized);
      final eventType = normalized['event_type'];
      if (eventType != 'keydown' && eventType != 'keyup') {
        _invalidCommand('event_type must be keydown or keyup');
      }
      _validateKeyEvent(normalized['event']);
      break;
    case 'dispatch_text_input':
      _expectKeys(normalized, const <String>{
        'v',
        'type',
        'context_id',
        'document_id',
        'runtime_context_id',
        'viewport',
        'state',
      });
      _validateContextId(normalized['context_id']);
      _validatePositiveId(normalized['document_id'], 'document_id');
      _validatePositiveId(
        normalized['runtime_context_id'],
        'runtime_context_id',
      );
      _validateViewport(normalized['viewport']);
      _validateTextInputState(normalized['state']);
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
    case 'renderer_request':
      _expectEnvelopeKeys(envelope, const <String>{
        'v',
        'type',
        'request_id',
        'request',
      });
      if (envelope['request_id'] is! int ||
          (envelope['request_id']! as int) <= 0) {
        throw const NativeProtocolException(
          'renderer request id must be positive',
        );
      }
      _requireTaggedObject(envelope['request'], 'renderer request');
      return;
    case 'renderer_update':
      _expectEnvelopeKeys(envelope, const <String>{'v', 'type', 'update'});
      _requireTaggedObject(envelope['update'], 'renderer update');
      return;
    case 'renderer_accepted':
      _expectEnvelopeKeys(envelope, const <String>{'v', 'type'});
      return;
    case 'renderer_shutdown':
      _expectEnvelopeKeys(envelope, const <String>{'v', 'type'});
      return;
    case 'renderer_submitted':
      _expectEnvelopeKeys(envelope, const <String>{'v', 'type'});
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
      width > vixenMaxViewportDimension ||
      height > vixenMaxViewportDimension ||
      width * height * 4 > vixenMaxViewportBytes) {
    _invalidCommand('viewport must have positive bounded dimensions and area');
  }
}

void _validateRenderHitTestQuery(Object? value) {
  final query = _commandObject(value, 'query');
  _expectKeys(query, const <String>{
    'v',
    'query_id',
    'context_id',
    'document_id',
    'displayed_commit_id',
    'revision',
    'handle',
    'point',
  });
  if (query['v'] != vixenAbiVersion) {
    _invalidCommand('renderer query version must be 1');
  }
  for (final field in const <String>[
    'query_id',
    'context_id',
    'document_id',
    'displayed_commit_id',
    'handle',
  ]) {
    _validatePositiveId(query[field], field);
  }
  _validateRenderRevision(query['revision']);
  _validateRenderPoint(query['point'], 'point');
}

void _validateRenderInputTarget(Object? value) {
  final target = _commandObject(value, 'target');
  _expectKeys(target, const <String>{
    'v',
    'query_id',
    'context_id',
    'document_id',
    'displayed_commit_id',
    'revision',
    'handle',
    'node_id',
    'fragment_id',
    'viewport_point',
    'local_point',
  });
  if (target['v'] != vixenAbiVersion) {
    _invalidCommand('renderer target version must be 1');
  }
  for (final field in const <String>[
    'query_id',
    'context_id',
    'document_id',
    'displayed_commit_id',
    'handle',
    'node_id',
    'fragment_id',
  ]) {
    _validatePositiveId(target[field], field);
  }
  _validateRenderRevision(target['revision']);
  _validateRenderPoint(target['viewport_point'], 'viewport_point');
  _validateRenderPoint(target['local_point'], 'local_point');
}

void _validateRenderRevision(Object? value) {
  final revision = _commandObject(value, 'revision');
  _expectKeys(revision, const <String>{
    'context_id',
    'document_id',
    'source_generation',
    'style_generation',
    'viewport_generation',
    'resource_generation',
  });
  for (final field in revision.keys) {
    _validatePositiveId(revision[field], field);
  }
}

void _validateRenderPoint(Object? value, String name) {
  final point = _commandObject(value, name);
  _expectKeys(point, const <String>{'x', 'y'});
  for (final field in const <String>['x', 'y']) {
    final coordinate = point[field];
    if (coordinate is! num || !coordinate.isFinite) {
      _invalidCommand('$name.$field must be finite');
    }
  }
}

void _validateTextInputState(Object? value) {
  final state = _commandObject(value, 'state');
  _expectKeys(state, const <String>{'text', 'selection', 'composing'});
  _validateBoundedString(state['text'], 'text', vixenMaxTextInputBytes);
  final text = state['text']! as String;
  _validateTextRange(state['selection'], 'selection', text.codeUnits.length);
  final composing = state['composing'];
  if (composing != null) {
    final range = _validateTextRange(
      composing,
      'composing',
      text.codeUnits.length,
    );
    if (range.base > range.extent) {
      _invalidCommand('composing range must be ordered');
    }
  }
}

({int base, int extent}) _validateTextRange(
  Object? value,
  String name,
  int textLength,
) {
  final range = _commandObject(value, name);
  _expectKeys(range, const <String>{'base_offset', 'extent_offset'});
  final base = range['base_offset'];
  final extent = range['extent_offset'];
  if (base is! int ||
      extent is! int ||
      base < 0 ||
      extent < 0 ||
      base > textLength ||
      extent > textLength) {
    _invalidCommand('$name offsets must be within the UTF-16 text length');
  }
  return (base: base, extent: extent);
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
