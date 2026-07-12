import 'dart:collection';
import 'dart:isolate';
import 'dart:typed_data';

const int browserAbiVersion = 1;
const String vixenStartUrl = 'about:vixen';
const int browserMaxFrameDimension = 4096;
const int browserMaxFrameBytes = 64 * 1024 * 1024;
const int browserMaxAccessibilityNodes = 256;

enum BrowserHostLifecycle {
  resumed('resumed'),
  inactive('inactive'),
  hidden('hidden'),
  paused('paused'),
  detached('detached');

  const BrowserHostLifecycle(this.wireName);
  final String wireName;
}

final class BrowserFrame {
  factory BrowserFrame({
    required Uint8List rgba,
    required int width,
    required int height,
    required int frameId,
    required int contextId,
    required int documentId,
  }) {
    validateBrowserFrameMetadata(
      byteLength: rgba.length,
      width: width,
      height: height,
      frameId: frameId,
      contextId: contextId,
      documentId: documentId,
    );
    return BrowserFrame._(
      rgba: Uint8List.fromList(rgba).asUnmodifiableView(),
      width: width,
      height: height,
      frameId: frameId,
      contextId: contextId,
      documentId: documentId,
    );
  }

  factory BrowserFrame.fromTransfer({
    required TransferableTypedData rgba,
    required int width,
    required int height,
    required int frameId,
    required int contextId,
    required int documentId,
  }) {
    final pixels = rgba.materialize().asUint8List();
    validateBrowserFrameMetadata(
      byteLength: pixels.length,
      width: width,
      height: height,
      frameId: frameId,
      contextId: contextId,
      documentId: documentId,
    );
    return BrowserFrame._(
      rgba: pixels.asUnmodifiableView(),
      width: width,
      height: height,
      frameId: frameId,
      contextId: contextId,
      documentId: documentId,
    );
  }

  const BrowserFrame._({
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

  int get rowStride => width * 4;
}

void validateBrowserFrameMetadata({
  required int byteLength,
  required int width,
  required int height,
  required int frameId,
  required int contextId,
  required int documentId,
}) {
  if (width <= 0 ||
      height <= 0 ||
      width > browserMaxFrameDimension ||
      height > browserMaxFrameDimension) {
    throw const FormatException('frame dimensions are outside ABI bounds');
  }
  final expectedLength = width * height * 4;
  if (expectedLength > browserMaxFrameBytes || byteLength != expectedLength) {
    throw const FormatException('frame byte length is not packed RGBA8');
  }
  if (frameId <= 0 || contextId <= 0 || documentId <= 0) {
    throw const FormatException('frame generation metadata must be nonzero');
  }
}

final class BrowserFailure implements Exception {
  const BrowserFailure(this.code, this.message);

  factory BrowserFailure.fromWire(Map<String, Object?> wire) {
    return BrowserFailure(_string(wire, 'code'), _string(wire, 'message'));
  }

  final String code;
  final String message;

  Map<String, Object?> toWire() => {'code': code, 'message': message};

  @override
  String toString() => '$code: $message';
}

final class ProfileSessionState {
  const ProfileSessionState({this.tabs = const [], this.activeIndex = 0});

  factory ProfileSessionState.fromWire(Map<String, Object?> wire) {
    return ProfileSessionState(
      tabs: _list(wire, 'tabs').map((value) => value as String).toList(),
      activeIndex: _int(wire, 'active_index'),
    );
  }

  final List<String> tabs;
  final int activeIndex;

  Map<String, Object?> toWire() => {'tabs': tabs, 'active_index': activeIndex};
}

final class BrowsingContextState {
  const BrowsingContextState({
    required this.contextId,
    required this.mainFrameId,
    required this.documentId,
    this.runtimeContextId,
    this.activeNavigationId,
    required this.url,
    this.title,
    required this.historyLength,
    required this.historyIndex,
    required this.canGoBack,
    required this.canGoForward,
    required this.isLoading,
    required this.loadProgress,
  });

  factory BrowsingContextState.initial(int contextId) {
    return BrowsingContextState(
      contextId: contextId,
      mainFrameId: contextId,
      documentId: contextId,
      url: 'about:blank',
      historyLength: 1,
      historyIndex: 0,
      canGoBack: false,
      canGoForward: false,
      isLoading: false,
      loadProgress: 0,
    );
  }

  factory BrowsingContextState.fromWire(Map<String, Object?> wire) {
    return BrowsingContextState(
      contextId: _positiveInt(wire, 'context_id'),
      mainFrameId: _positiveInt(wire, 'main_frame_id'),
      documentId: _positiveInt(wire, 'document_id'),
      runtimeContextId: _optionalPositiveInt(wire, 'runtime_context_id'),
      activeNavigationId: _optionalPositiveInt(wire, 'active_navigation_id'),
      url: _string(wire, 'url'),
      title: _optionalString(wire, 'title'),
      historyLength: _int(wire, 'history_length'),
      historyIndex: _int(wire, 'history_index'),
      canGoBack: _bool(wire, 'can_go_back'),
      canGoForward: _bool(wire, 'can_go_forward'),
      isLoading: _bool(wire, 'is_loading'),
      loadProgress: _number(wire, 'load_progress').toDouble(),
    );
  }

  final int contextId;
  final int mainFrameId;
  final int documentId;
  final int? runtimeContextId;
  final int? activeNavigationId;
  final String url;
  final String? title;
  final int historyLength;
  final int historyIndex;
  final bool canGoBack;
  final bool canGoForward;
  final bool isLoading;
  final double loadProgress;

  String get displayTitle {
    final candidate = title?.trim();
    return candidate == null || candidate.isEmpty ? url : candidate;
  }

  BrowsingContextState copyWith({
    int? mainFrameId,
    int? documentId,
    int? runtimeContextId,
    int? activeNavigationId,
    bool clearActiveNavigation = false,
    String? url,
    String? title,
    int? historyLength,
    int? historyIndex,
    bool? canGoBack,
    bool? canGoForward,
    bool? isLoading,
    double? loadProgress,
  }) {
    return BrowsingContextState(
      contextId: contextId,
      mainFrameId: mainFrameId ?? this.mainFrameId,
      documentId: documentId ?? this.documentId,
      runtimeContextId: runtimeContextId ?? this.runtimeContextId,
      activeNavigationId: clearActiveNavigation
          ? null
          : activeNavigationId ?? this.activeNavigationId,
      url: url ?? this.url,
      title: title ?? this.title,
      historyLength: historyLength ?? this.historyLength,
      historyIndex: historyIndex ?? this.historyIndex,
      canGoBack: canGoBack ?? this.canGoBack,
      canGoForward: canGoForward ?? this.canGoForward,
      isLoading: isLoading ?? this.isLoading,
      loadProgress: loadProgress ?? this.loadProgress,
    );
  }

  Map<String, Object?> toWire() => {
    'context_id': contextId,
    'main_frame_id': mainFrameId,
    'document_id': documentId,
    'runtime_context_id': runtimeContextId,
    'active_navigation_id': activeNavigationId,
    'url': url,
    'title': title,
    'history_length': historyLength,
    'history_index': historyIndex,
    'can_go_back': canGoBack,
    'can_go_forward': canGoForward,
    'is_loading': isLoading,
    'load_progress': loadProgress,
  };
}

final class BrowserSnapshot {
  const BrowserSnapshot({this.activeContextId, this.contexts = const []});

  factory BrowserSnapshot.fromWire(Map<String, Object?> wire) {
    return BrowserSnapshot(
      activeContextId: _optionalPositiveInt(wire, 'active_context_id'),
      contexts: _list(
        wire,
        'contexts',
      ).map((value) => BrowsingContextState.fromWire(_map(value))).toList(),
    );
  }

  final int? activeContextId;
  final List<BrowsingContextState> contexts;

  Map<String, Object?> toWire() => {
    'active_context_id': activeContextId,
    'contexts': contexts.map((context) => context.toWire()).toList(),
  };
}

final class BrowserAccessibilityRect {
  const BrowserAccessibilityRect({
    required this.x,
    required this.y,
    required this.width,
    required this.height,
  });

  factory BrowserAccessibilityRect.fromWire(Map<String, Object?> wire) =>
      BrowserAccessibilityRect(
        x: _number(wire, 'x').toDouble(),
        y: _number(wire, 'y').toDouble(),
        width: _number(wire, 'width').toDouble(),
        height: _number(wire, 'height').toDouble(),
      );

  final double x;
  final double y;
  final double width;
  final double height;

  Map<String, Object> toWire() => {
    'x': x,
    'y': y,
    'width': width,
    'height': height,
  };
}

final class BrowserAccessibilityNode {
  BrowserAccessibilityNode({
    required this.id,
    this.parentId,
    List<int> controlsIds = const [],
    required this.role,
    required this.label,
    this.value,
    this.range,
    this.bounds,
    required this.focused,
    required this.disabled,
    this.checked,
    required this.selected,
    this.expanded,
    required this.hidden,
    required this.focusable,
    required List<String> actions,
  }) : controlsIds = List.unmodifiable(controlsIds),
       actions = List.unmodifiable(actions);

  factory BrowserAccessibilityNode.fromWire(Map<String, Object?> wire) {
    final actions = _list(wire, 'actions');
    if (actions.any((action) => action is! String)) {
      throw const FormatException('accessibility actions must be strings');
    }
    final controlsIds = _list(wire, 'controls_ids');
    if (controlsIds.any((id) => id is! int || id <= 0)) {
      throw const FormatException(
        'accessibility controls ids must be positive integers',
      );
    }
    return BrowserAccessibilityNode(
      id: _positiveInt(wire, 'id'),
      parentId: _optionalPositiveInt(wire, 'parent_id'),
      controlsIds: controlsIds.cast<int>(),
      role: _string(wire, 'role'),
      label: _string(wire, 'label'),
      value: _optionalString(wire, 'value'),
      range: wire['range'] == null
          ? null
          : BrowserAccessibilityRange.fromWire(_map(wire['range'])),
      bounds: wire['bbox'] == null
          ? null
          : BrowserAccessibilityRect.fromWire(_map(wire['bbox'])),
      focused: _bool(wire, 'focused'),
      disabled: _bool(wire, 'disabled'),
      checked: _optionalBool(wire, 'checked'),
      selected: _bool(wire, 'selected'),
      expanded: _optionalBool(wire, 'expanded'),
      hidden: _bool(wire, 'hidden'),
      focusable: _bool(wire, 'focusable'),
      actions: actions.cast<String>(),
    );
  }

  final int id;
  final int? parentId;
  final List<int> controlsIds;
  final String role;
  final String label;
  final String? value;
  final BrowserAccessibilityRange? range;
  final BrowserAccessibilityRect? bounds;
  final bool focused;
  final bool disabled;
  final bool? checked;
  final bool selected;
  final bool? expanded;
  final bool hidden;
  final bool focusable;
  final List<String> actions;

  Map<String, Object?> toWire() => {
    'id': id,
    'parent_id': parentId,
    'controls_ids': controlsIds,
    'role': role,
    'label': label,
    'value': value,
    'range': range?.toWire(),
    'bbox': bounds?.toWire(),
    'focused': focused,
    'disabled': disabled,
    'checked': checked,
    'selected': selected,
    'expanded': expanded,
    'hidden': hidden,
    'focusable': focusable,
    'actions': actions,
  };
}

final class BrowserAccessibilityRange {
  const BrowserAccessibilityRange({
    required this.current,
    required this.minimum,
    required this.maximum,
    required this.step,
  });

  factory BrowserAccessibilityRange.fromWire(Map<String, Object?> wire) {
    final range = BrowserAccessibilityRange(
      current: _number(wire, 'current').toDouble(),
      minimum: _number(wire, 'minimum').toDouble(),
      maximum: _number(wire, 'maximum').toDouble(),
      step: _number(wire, 'step').toDouble(),
    );
    if (!range.current.isFinite ||
        !range.minimum.isFinite ||
        !range.maximum.isFinite ||
        !range.step.isFinite ||
        range.maximum < range.minimum ||
        range.current < range.minimum ||
        range.current > range.maximum ||
        range.step <= 0) {
      throw const FormatException('invalid accessibility range');
    }
    return range;
  }

  final double current;
  final double minimum;
  final double maximum;
  final double step;

  Map<String, Object> toWire() => {
    'current': current,
    'minimum': minimum,
    'maximum': maximum,
    'step': step,
  };
}

final class BrowserAccessibilitySnapshot {
  BrowserAccessibilitySnapshot({
    required this.sourceGeneration,
    required this.generation,
    required this.contextId,
    required this.documentId,
    required this.viewportWidth,
    required this.viewportHeight,
    required List<BrowserAccessibilityNode> nodes,
    required this.truncated,
  }) : nodes = List.unmodifiable(nodes) {
    if (nodes.length > browserMaxAccessibilityNodes) {
      throw const FormatException('accessibility snapshot exceeds ABI bound');
    }
    final seen = <int>{};
    for (final node in nodes) {
      if (!seen.add(node.id)) {
        throw const FormatException('accessibility node ids must be unique');
      }
      final parentId = node.parentId;
      if (parentId != null && !seen.contains(parentId)) {
        throw const FormatException(
          'accessibility parents must precede their children',
        );
      }
    }
    for (final node in nodes) {
      if (node.controlsIds.toSet().length != node.controlsIds.length ||
          node.controlsIds.any((id) => id == node.id || !seen.contains(id))) {
        throw const FormatException(
          'accessibility controls ids must be unique emitted nodes',
        );
      }
    }
  }

  factory BrowserAccessibilitySnapshot.fromWire(Map<String, Object?> wire) {
    final viewport = _map(wire['viewport']);
    return BrowserAccessibilitySnapshot(
      sourceGeneration: _positiveInt(wire, 'source_generation'),
      generation: _positiveInt(wire, 'generation'),
      contextId: _positiveInt(wire, 'context_id'),
      documentId: _positiveInt(wire, 'document_id'),
      viewportWidth: _positiveInt(viewport, 'width'),
      viewportHeight: _positiveInt(viewport, 'height'),
      nodes: _list(wire, 'nodes')
          .map((node) => BrowserAccessibilityNode.fromWire(_map(node)))
          .toList(growable: false),
      truncated: _bool(wire, 'truncated'),
    );
  }

  final int sourceGeneration;
  final int generation;
  final int contextId;
  final int documentId;
  final int viewportWidth;
  final int viewportHeight;
  final List<BrowserAccessibilityNode> nodes;
  final bool truncated;

  Map<String, Object?> toWire() => {
    'type': 'accessibility_snapshot',
    'source_generation': sourceGeneration,
    'generation': generation,
    'context_id': contextId,
    'document_id': documentId,
    'viewport': {'width': viewportWidth, 'height': viewportHeight},
    'nodes': nodes.map((node) => node.toWire()).toList(growable: false),
    'truncated': truncated,
  };
}

final class BrowserMouseEvent {
  const BrowserMouseEvent({
    required this.x,
    required this.y,
    required this.button,
    required this.buttons,
    this.detail = 0,
    this.bubbles = true,
    this.ctrlKey = false,
    this.shiftKey = false,
    this.altKey = false,
    this.metaKey = false,
    this.deltaX = 0,
    this.deltaY = 0,
  });

  final double x;
  final double y;
  final int button;
  final int buttons;
  final int detail;
  final bool bubbles;
  final bool ctrlKey;
  final bool shiftKey;
  final bool altKey;
  final bool metaKey;
  final double deltaX;
  final double deltaY;

  Map<String, Object> toWire() => {
    'x': x,
    'y': y,
    'button': button,
    'buttons': buttons,
    'detail': detail,
    'bubbles': bubbles,
    'ctrl_key': ctrlKey,
    'shift_key': shiftKey,
    'alt_key': altKey,
    'meta_key': metaKey,
    'delta_x': deltaX,
    'delta_y': deltaY,
  };
}

final class BrowserKeyEvent {
  const BrowserKeyEvent({
    required this.key,
    required this.code,
    this.text = '',
    this.applyText = false,
    this.ctrlKey = false,
    this.shiftKey = false,
    this.altKey = false,
    this.metaKey = false,
    this.repeat = false,
    this.location = 0,
  });

  final String key;
  final String code;
  final String text;
  final bool applyText;
  final bool ctrlKey;
  final bool shiftKey;
  final bool altKey;
  final bool metaKey;
  final bool repeat;
  final int location;

  Map<String, Object> toWire() => {
    'key': key,
    'code': code,
    'text': text,
    'apply_text': applyText,
    'ctrl_key': ctrlKey,
    'shift_key': shiftKey,
    'alt_key': altKey,
    'meta_key': metaKey,
    'repeat': repeat,
    'location': location,
  };
}

final class BrowserCommand {
  BrowserCommand._(this.type, Map<String, Object?> fields)
    : _fields = Map.unmodifiable(fields);

  factory BrowserCommand.loadProfileSession() =>
      BrowserCommand._('load_profile_session', const {});
  factory BrowserCommand.saveCurrentProfileSession() =>
      BrowserCommand._('save_current_profile_session', const {});
  factory BrowserCommand.browserSnapshot() =>
      BrowserCommand._('browser_snapshot', const {});
  factory BrowserCommand.createContext() =>
      BrowserCommand._('create_context', const {});
  factory BrowserCommand.closeContext(int contextId) =>
      BrowserCommand._('close_context', {'context_id': contextId});
  factory BrowserCommand.activateContext(int contextId) =>
      BrowserCommand._('activate_context', {'context_id': contextId});
  factory BrowserCommand.navigate(int contextId, String url) =>
      BrowserCommand._('navigate', {'context_id': contextId, 'url': url});
  factory BrowserCommand.reload(int contextId) =>
      BrowserCommand._('reload', {'context_id': contextId});
  factory BrowserCommand.stop(int contextId) =>
      BrowserCommand._('stop', {'context_id': contextId});
  factory BrowserCommand.traverseHistory(int contextId, int delta) =>
      BrowserCommand._('traverse_history', {
        'context_id': contextId,
        'delta': delta,
      });
  factory BrowserCommand.contextState(int contextId) =>
      BrowserCommand._('context_state', {'context_id': contextId});
  factory BrowserCommand.updateHostViewState({
    required int contextId,
    required int generation,
    required int viewportWidth,
    required int viewportHeight,
    required double scaleFactor,
    required bool focused,
    required bool visible,
    required BrowserHostLifecycle lifecycle,
  }) => BrowserCommand._('update_host_view_state', {
    'context_id': contextId,
    'generation': generation,
    'viewport': {'width': viewportWidth, 'height': viewportHeight},
    'scale_factor': scaleFactor,
    'focused': focused,
    'visible': visible,
    'lifecycle': lifecycle.wireName,
  });
  factory BrowserCommand.accessibilitySnapshot({
    required int contextId,
    required int documentId,
    required int viewportWidth,
    required int viewportHeight,
  }) => BrowserCommand._('accessibility_snapshot', {
    'context_id': contextId,
    'document_id': documentId,
    'viewport': {'width': viewportWidth, 'height': viewportHeight},
  });
  factory BrowserCommand.dispatchAccessibilityFocus({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required int sourceGeneration,
    required int generation,
    required int nodeId,
  }) => BrowserCommand._('dispatch_accessibility_action', {
    'context_id': contextId,
    'document_id': documentId,
    'runtime_context_id': runtimeContextId,
    'viewport': {'width': viewportWidth, 'height': viewportHeight},
    'source_generation': sourceGeneration,
    'generation': generation,
    'node_id': nodeId,
    'action': 'focus',
  });
  factory BrowserCommand.dispatchAccessibilitySetValue({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required int sourceGeneration,
    required int generation,
    required int nodeId,
    required String value,
  }) => BrowserCommand._('dispatch_accessibility_action', {
    'context_id': contextId,
    'document_id': documentId,
    'runtime_context_id': runtimeContextId,
    'viewport': {'width': viewportWidth, 'height': viewportHeight},
    'source_generation': sourceGeneration,
    'generation': generation,
    'node_id': nodeId,
    'action': 'set_value',
    'value': value,
  });
  factory BrowserCommand.dispatchAccessibilityAdjustment({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required int sourceGeneration,
    required int generation,
    required int nodeId,
    required bool increase,
  }) => BrowserCommand._('dispatch_accessibility_action', {
    'context_id': contextId,
    'document_id': documentId,
    'runtime_context_id': runtimeContextId,
    'viewport': {'width': viewportWidth, 'height': viewportHeight},
    'source_generation': sourceGeneration,
    'generation': generation,
    'node_id': nodeId,
    'action': increase ? 'increase' : 'decrease',
  });
  factory BrowserCommand.dispatchMouseEvent({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required String eventType,
    required BrowserMouseEvent event,
  }) => BrowserCommand._('dispatch_mouse_event', {
    'context_id': contextId,
    'document_id': documentId,
    'runtime_context_id': runtimeContextId,
    'viewport': {'width': viewportWidth, 'height': viewportHeight},
    'event_type': eventType,
    'event': event.toWire(),
  });
  factory BrowserCommand.dispatchKeyEvent({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required String eventType,
    required BrowserKeyEvent event,
  }) => BrowserCommand._('dispatch_key_event', {
    'context_id': contextId,
    'document_id': documentId,
    'runtime_context_id': runtimeContextId,
    'viewport': {'width': viewportWidth, 'height': viewportHeight},
    'event_type': eventType,
    'event': event.toWire(),
  });

  final String type;
  final Map<String, Object?> _fields;

  int? get contextId => _fields['context_id'] as int?;
  int? get delta => _fields['delta'] as int?;
  String? get url => _fields['url'] as String?;

  Map<String, Object?> toWire() => {
    'v': browserAbiVersion,
    'type': type,
    ..._fields,
  };

  @override
  String toString() => toWire().toString();
}

sealed class BrowserResponse {
  const BrowserResponse();

  factory BrowserResponse.fromWire(Map<String, Object?> wire) {
    return switch (_string(wire, 'type')) {
      'accepted' => const AcceptedResponse(),
      'profile_session' => ProfileSessionResponse(
        ProfileSessionState.fromWire(wire),
      ),
      'browser_snapshot' => BrowserSnapshotResponse(
        BrowserSnapshot.fromWire(wire),
      ),
      'context_created' => ContextCreatedResponse(
        _positiveInt(wire, 'context_id'),
      ),
      'navigation_accepted' => NavigationAcceptedResponse(
        _positiveInt(wire, 'navigation_id'),
      ),
      'context_state' => ContextStateResponse(
        BrowsingContextState.fromWire(_map(wire['state'])),
      ),
      'accessibility_snapshot' => AccessibilitySnapshotResponse(
        BrowserAccessibilitySnapshot.fromWire(wire),
      ),
      'input_dispatched' => InputDispatchedResponse(
        effects: _map(wire['effects']),
        navigationActions: _list(
          wire,
          'navigation_actions',
        ).map(_map).toList(growable: false),
      ),
      final type => throw FormatException('Unknown browser response: $type'),
    };
  }

  Map<String, Object?> toWire();
}

final class AcceptedResponse extends BrowserResponse {
  const AcceptedResponse();

  @override
  Map<String, Object?> toWire() => {'type': 'accepted'};
}

final class ProfileSessionResponse extends BrowserResponse {
  const ProfileSessionResponse(this.session);
  final ProfileSessionState session;

  @override
  Map<String, Object?> toWire() => {
    'type': 'profile_session',
    ...session.toWire(),
  };
}

final class BrowserSnapshotResponse extends BrowserResponse {
  const BrowserSnapshotResponse(this.snapshot);
  final BrowserSnapshot snapshot;

  @override
  Map<String, Object?> toWire() => {
    'type': 'browser_snapshot',
    ...snapshot.toWire(),
  };
}

final class ContextCreatedResponse extends BrowserResponse {
  const ContextCreatedResponse(this.contextId);
  final int contextId;

  @override
  Map<String, Object?> toWire() => {
    'type': 'context_created',
    'context_id': contextId,
  };
}

final class NavigationAcceptedResponse extends BrowserResponse {
  const NavigationAcceptedResponse(this.navigationId);
  final int navigationId;

  @override
  Map<String, Object?> toWire() => {
    'type': 'navigation_accepted',
    'navigation_id': navigationId,
  };
}

final class ContextStateResponse extends BrowserResponse {
  const ContextStateResponse(this.state);
  final BrowsingContextState state;

  @override
  Map<String, Object?> toWire() => {
    'type': 'context_state',
    'state': state.toWire(),
  };
}

final class AccessibilitySnapshotResponse extends BrowserResponse {
  const AccessibilitySnapshotResponse(this.snapshot);

  final BrowserAccessibilitySnapshot snapshot;

  @override
  Map<String, Object?> toWire() => snapshot.toWire();
}

final class InputDispatchedResponse extends BrowserResponse {
  InputDispatchedResponse({
    required Map<String, Object?> effects,
    required List<Map<String, Object?>> navigationActions,
  }) : effects = UnmodifiableMapView(Map.of(effects)),
       navigationActions = List.unmodifiable(
         navigationActions.map((action) => UnmodifiableMapView(Map.of(action))),
       );

  factory InputDispatchedResponse.empty() => InputDispatchedResponse(
    effects: const {
      'console': <Object?>[],
      'dialogs': <Object?>[],
      'bindings': <Object?>[],
      'network': <Object?>[],
      'exceptions': <Object?>[],
    },
    navigationActions: const [],
  );

  final Map<String, Object?> effects;
  final List<Map<String, Object?>> navigationActions;

  @override
  Map<String, Object?> toWire() => {
    'type': 'input_dispatched',
    'effects': effects,
    'navigation_actions': navigationActions,
  };
}

final class BrowserEvent {
  BrowserEvent._(Map<String, Object?> wire)
    : wire = UnmodifiableMapView(Map.of(wire));

  factory BrowserEvent.fromWire(Map<String, Object?> wire) {
    _string(wire, 'type');
    return BrowserEvent._(wire);
  }

  factory BrowserEvent.contextCreated(BrowsingContextState state) =>
      BrowserEvent._({
        'type': 'browsing_context_created',
        'state': state.toWire(),
      });
  factory BrowserEvent.contextClosed(int contextId) => BrowserEvent._({
    'type': 'browsing_context_closed',
    'context_id': contextId,
  });
  factory BrowserEvent.activeContextChanged(int? contextId) => BrowserEvent._({
    'type': 'active_browsing_context_changed',
    'context_id': contextId,
  });
  factory BrowserEvent.contextStateChanged(BrowsingContextState state) =>
      BrowserEvent._({
        'type': 'browsing_context_state_changed',
        'state': state.toWire(),
      });
  factory BrowserEvent.navigationPhaseChanged({
    required int contextId,
    required int frameId,
    required int navigationId,
    required String phase,
  }) => BrowserEvent._({
    'type': 'navigation_phase_changed',
    'context_id': contextId,
    'frame_id': frameId,
    'navigation_id': navigationId,
    'phase': phase,
  });
  factory BrowserEvent.navigationFailed({
    required int contextId,
    required int frameId,
    required int navigationId,
    int? requestId,
    required BrowserFailure error,
  }) => BrowserEvent._({
    'type': 'navigation_failed',
    'context_id': contextId,
    'frame_id': frameId,
    'navigation_id': navigationId,
    'request_id': requestId,
    'error': error.toWire(),
  });

  final Map<String, Object?> wire;

  String get type => _string(wire, 'type');
  int? get contextId => switch (wire['context_id']) {
    final int value => value,
    _ => state?.contextId,
  };
  int? get navigationId => wire['navigation_id'] as int?;
  String? get phase => wire['phase'] as String?;
  String? get url => wire['url'] as String?;
  BrowsingContextState? get state => wire['state'] == null
      ? null
      : BrowsingContextState.fromWire(_map(wire['state']));
  BrowserFailure? get error => wire['error'] == null
      ? null
      : BrowserFailure.fromWire(_map(wire['error']));

  Map<String, Object?> toWire() => Map.of(wire);
}

final class SequencedBrowserEvent {
  const SequencedBrowserEvent({required this.sequence, required this.event});

  factory SequencedBrowserEvent.fromWire(Map<String, Object?> wire) {
    if (_int(wire, 'v') != browserAbiVersion ||
        _string(wire, 'type') != 'event') {
      throw const FormatException('Not a Vixen ABI v1 event envelope');
    }
    return SequencedBrowserEvent(
      sequence: _positiveInt(wire, 'sequence'),
      event: BrowserEvent.fromWire(_map(wire['event'])),
    );
  }

  final int sequence;
  final BrowserEvent event;

  Map<String, Object?> toWire() => {
    'v': browserAbiVersion,
    'type': 'event',
    'sequence': sequence,
    'event': event.toWire(),
  };
}

Map<String, Object?> _map(Object? value) {
  if (value is Map<String, Object?>) return value;
  if (value is Map) return value.cast<String, Object?>();
  throw const FormatException('Expected a JSON object');
}

List<Object?> _list(Map<String, Object?> wire, String key) {
  final value = wire[key];
  if (value is List<Object?>) return value;
  if (value is List) return value.cast<Object?>();
  throw FormatException('$key must be an array');
}

String _string(Map<String, Object?> wire, String key) {
  final value = wire[key];
  if (value is String) return value;
  throw FormatException('$key must be a string');
}

String? _optionalString(Map<String, Object?> wire, String key) {
  final value = wire[key];
  if (value == null || value is String) return value as String?;
  throw FormatException('$key must be a string or null');
}

int _int(Map<String, Object?> wire, String key) {
  final value = wire[key];
  if (value is int) return value;
  throw FormatException('$key must be an integer');
}

int _positiveInt(Map<String, Object?> wire, String key) {
  final value = _int(wire, key);
  if (value > 0) return value;
  throw FormatException('$key must be nonzero');
}

int? _optionalPositiveInt(Map<String, Object?> wire, String key) {
  if (wire[key] == null) return null;
  return _positiveInt(wire, key);
}

bool _bool(Map<String, Object?> wire, String key) {
  final value = wire[key];
  if (value is bool) return value;
  throw FormatException('$key must be a boolean');
}

bool? _optionalBool(Map<String, Object?> wire, String key) {
  final value = wire[key];
  if (value == null || value is bool) return value as bool?;
  throw FormatException('$key must be a boolean or null');
}

num _number(Map<String, Object?> wire, String key) {
  final value = wire[key];
  if (value is num) return value;
  throw FormatException('$key must be a number');
}
