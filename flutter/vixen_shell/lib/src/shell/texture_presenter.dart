import 'dart:async';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../bridge/browser_models.dart';

const String vixenTextureChannelName = 'org.vixen.Vixen/texture';

abstract interface class BrowserTextureController {
  Future<int> create();

  Future<void> publish(BrowserFrame frame);

  Future<void> dispose();
}

final class LinuxTextureController implements BrowserTextureController {
  LinuxTextureController({MethodChannel? channel, bool? isLinux})
    : _channel = channel ?? const MethodChannel(vixenTextureChannelName),
      _isLinux = isLinux ?? Platform.isLinux;

  final MethodChannel _channel;
  final bool _isLinux;
  bool _created = false;

  void _requireLinux() {
    if (!_isLinux) {
      throw UnsupportedError('Vixen pixel-buffer textures require Linux');
    }
  }

  @override
  Future<int> create() async {
    _requireLinux();
    final textureId = await _channel.invokeMethod<int>('create');
    if (textureId == null || textureId < 0) {
      throw PlatformException(
        code: 'texture.invalid-id',
        message: 'Linux runner returned an invalid texture id',
      );
    }
    _created = true;
    return textureId;
  }

  @override
  Future<void> publish(BrowserFrame frame) async {
    _requireLinux();
    await _channel.invokeMethod<void>('publish', <String, Object>{
      'width': frame.width,
      'height': frame.height,
      'rgba': frame.rgba,
    });
  }

  @override
  Future<void> dispose() async {
    if (!_isLinux || !_created) return;
    _created = false;
    await _channel.invokeMethod<void>('dispose');
  }
}

({int width, int height}) physicalFrameViewport(Size logicalSize, double dpr) {
  if (!logicalSize.width.isFinite ||
      !logicalSize.height.isFinite ||
      !dpr.isFinite ||
      logicalSize.width <= 0 ||
      logicalSize.height <= 0 ||
      dpr <= 0) {
    return (width: 0, height: 0);
  }
  final rawWidth = logicalSize.width * dpr;
  final rawHeight = logicalSize.height * dpr;
  if (!rawWidth.isFinite || !rawHeight.isFinite) {
    return (width: 0, height: 0);
  }
  final byteScale = math.sqrt(
    browserMaxFrameBytes / (rawWidth * rawHeight * 4),
  );
  final scale = math.min(
    1.0,
    math.min(
      browserMaxFrameDimension / rawWidth,
      math.min(browserMaxFrameDimension / rawHeight, byteScale),
    ),
  );
  return (
    width: (rawWidth * scale).floor().clamp(1, browserMaxFrameDimension),
    height: (rawHeight * scale).floor().clamp(1, browserMaxFrameDimension),
  );
}

final class BrowserContentSurface extends StatefulWidget {
  const BrowserContentSurface({
    required this.contextState,
    required this.frame,
    this.onPhysicalViewportChanged,
    this.onMouseEvent,
    this.onKeyEvent,
    this.accessibility,
    this.onSemanticTap,
    this.onSemanticFocus,
    this.textureController,
    super.key,
  });

  final BrowsingContextState? contextState;
  final BrowserFrame? frame;
  final void Function(int width, int height)? onPhysicalViewportChanged;
  final void Function(String eventType, BrowserMouseEvent event)? onMouseEvent;
  final void Function(String eventType, BrowserKeyEvent event)? onKeyEvent;
  final BrowserAccessibilitySnapshot? accessibility;
  final void Function(
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node,
  )?
  onSemanticTap;
  final void Function(
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node,
  )?
  onSemanticFocus;
  final BrowserTextureController? textureController;

  @override
  State<BrowserContentSurface> createState() => _BrowserContentSurfaceState();
}

final class _BrowserContentSurfaceState extends State<BrowserContentSurface> {
  late BrowserTextureController _controller;
  Future<int>? _createOperation;
  BrowserFrame? _pendingFrame;
  BrowserFrame? _publishedFrame;
  BrowserFrame? _displayedFrame;
  ({int width, int height})? _reportedViewport;
  final FocusNode _contentFocus = FocusNode(debugLabel: 'browser-content');
  final Map<int, int> _pressedButtons = {};
  final Set<PhysicalKeyboardKey> _suppressedShortcutKeys = {};
  Size _logicalViewport = Size.zero;
  PointerEvent? _pendingMouseMove;
  bool _mouseMoveScheduled = false;
  int? _textureId;
  int _controllerEpoch = 0;
  bool _presenting = false;
  bool _disposed = false;

  @override
  void initState() {
    super.initState();
    _controller = widget.textureController ?? LinuxTextureController();
    _queueFrame(widget.frame);
  }

  @override
  void didUpdateWidget(BrowserContentSurface oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.textureController != widget.textureController) {
      _controllerEpoch++;
      unawaited(_controller.dispose());
      _controller = widget.textureController ?? LinuxTextureController();
      _createOperation = null;
      _textureId = null;
      _publishedFrame = null;
      _displayedFrame = null;
    }
    _queueFrame(widget.frame);
  }

  void _queueFrame(BrowserFrame? frame) {
    if (frame == null) {
      _pendingFrame = null;
      _displayedFrame = null;
      return;
    }
    _pendingFrame = frame;
    if (!_presenting) unawaited(_presentFrames());
  }

  Future<void> _presentFrames() async {
    _presenting = true;
    try {
      while (!_disposed && _pendingFrame != null) {
        final frame = _pendingFrame!;
        _pendingFrame = null;
        if (!_isNewer(frame, _publishedFrame)) continue;
        final controller = _controller;
        final controllerEpoch = _controllerEpoch;
        final textureId = await (_createOperation ??= controller.create());
        if (_disposed) return;
        if (controllerEpoch != _controllerEpoch) {
          await controller.dispose();
          continue;
        }
        if (_pendingFrame case final newer? when _isNewer(newer, frame)) {
          continue;
        }
        await controller.publish(frame);
        if (_disposed) return;
        if (controllerEpoch != _controllerEpoch) {
          await controller.dispose();
          continue;
        }
        _textureId = textureId;
        _publishedFrame = frame;
        if (_sameFrame(widget.frame, frame)) {
          _displayedFrame = frame;
          if (mounted) setState(() {});
        }
      }
    } catch (_) {
      _displayedFrame = null;
      if (mounted) setState(() {});
    } finally {
      _presenting = false;
      if (!_disposed && _pendingFrame != null) unawaited(_presentFrames());
    }
  }

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        _logicalViewport = Size(constraints.maxWidth, constraints.maxHeight);
        final viewport = physicalFrameViewport(
          _logicalViewport,
          MediaQuery.devicePixelRatioOf(context),
        );
        if (_reportedViewport != viewport) {
          _reportedViewport = viewport;
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (!_disposed && _reportedViewport == viewport) {
              widget.onPhysicalViewportChanged?.call(
                viewport.width,
                viewport.height,
              );
            }
          });
        }
        return Focus(
          focusNode: _contentFocus,
          onKeyEvent: _handleKeyEvent,
          child: Listener(
            behavior: HitTestBehavior.opaque,
            onPointerDown: _handlePointerDown,
            onPointerHover: _scheduleMouseMove,
            onPointerMove: _handlePointerMove,
            onPointerUp: _handlePointerUp,
            onPointerCancel: _handlePointerCancel,
            onPointerSignal: _handlePointerSignal,
            child: ColoredBox(
              key: const Key('content-surface'),
              color: Theme.of(context).colorScheme.surface,
              child: _buildContent(context),
            ),
          ),
        );
      },
    );
  }

  void _handlePointerDown(PointerDownEvent event) {
    _contentFocus.requestFocus();
    final button = _domButton(event.buttons);
    _pressedButtons[event.pointer] = button;
    _emitMouse('mousedown', event, button: button, detail: 1);
  }

  void _handlePointerMove(PointerMoveEvent event) {
    _scheduleMouseMove(event);
  }

  void _scheduleMouseMove(PointerEvent event) {
    _pendingMouseMove = event;
    if (_mouseMoveScheduled) return;
    _mouseMoveScheduled = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _mouseMoveScheduled = false;
      final pending = _pendingMouseMove;
      _pendingMouseMove = null;
      if (_disposed || pending == null) return;
      _emitMouse(
        'mousemove',
        pending,
        button: _pressedButtons[pending.pointer] ?? _domButton(pending.buttons),
      );
    });
  }

  void _handlePointerUp(PointerUpEvent event) {
    final button = _pressedButtons.remove(event.pointer) ?? 0;
    _emitMouse('mouseup', event, button: button, buttons: 0, detail: 1);
  }

  void _handlePointerCancel(PointerCancelEvent event) {
    _pressedButtons.remove(event.pointer);
  }

  void _handlePointerSignal(PointerSignalEvent event) {
    if (event is PointerScrollEvent) {
      _emitMouse(
        'wheel',
        event,
        button: 0,
        buttons: 0,
        deltaX: event.scrollDelta.dx,
        deltaY: event.scrollDelta.dy,
      );
    }
  }

  void _emitMouse(
    String eventType,
    PointerEvent event, {
    required int button,
    int? buttons,
    int detail = 0,
    double deltaX = 0,
    double deltaY = 0,
  }) {
    final callback = widget.onMouseEvent;
    final viewport = _reportedViewport;
    if (callback == null ||
        viewport == null ||
        viewport.width <= 0 ||
        viewport.height <= 0 ||
        _logicalViewport.width <= 0 ||
        _logicalViewport.height <= 0) {
      return;
    }
    final keyboard = HardwareKeyboard.instance;
    callback(
      eventType,
      BrowserMouseEvent(
        x: (event.localPosition.dx / _logicalViewport.width * viewport.width)
            .clamp(0, viewport.width.toDouble()),
        y: (event.localPosition.dy / _logicalViewport.height * viewport.height)
            .clamp(0, viewport.height.toDouble()),
        button: button,
        buttons: buttons ?? event.buttons,
        detail: detail,
        ctrlKey: keyboard.isControlPressed,
        shiftKey: keyboard.isShiftPressed,
        altKey: keyboard.isAltPressed,
        metaKey: keyboard.isMetaPressed,
        deltaX: deltaX,
        deltaY: deltaY,
      ),
    );
  }

  KeyEventResult _handleKeyEvent(FocusNode node, KeyEvent event) {
    final callback = widget.onKeyEvent;
    if (callback == null) {
      return KeyEventResult.ignored;
    }
    if (event is KeyUpEvent &&
        _suppressedShortcutKeys.remove(event.physicalKey)) {
      return KeyEventResult.ignored;
    }
    if (_isShellShortcut(event)) {
      _suppressedShortcutKeys.add(event.physicalKey);
      return KeyEventResult.ignored;
    }
    final eventType = switch (event) {
      KeyDownEvent() || KeyRepeatEvent() => 'keydown',
      KeyUpEvent() => 'keyup',
      _ => null,
    };
    if (eventType == null) return KeyEventResult.ignored;
    final keyboard = HardwareKeyboard.instance;
    final character = event.character ?? '';
    final applyText =
        eventType == 'keydown' &&
        character.isNotEmpty &&
        !keyboard.isControlPressed &&
        !keyboard.isAltPressed &&
        !keyboard.isMetaPressed;
    callback(
      eventType,
      BrowserKeyEvent(
        key: _domKey(event, keyboard),
        code: _domCode(event.physicalKey),
        text: applyText ? character : '',
        applyText: applyText,
        ctrlKey: keyboard.isControlPressed,
        shiftKey: keyboard.isShiftPressed,
        altKey: keyboard.isAltPressed,
        metaKey: keyboard.isMetaPressed,
        repeat: event is KeyRepeatEvent,
      ),
    );
    return KeyEventResult.handled;
  }

  bool _isShellShortcut(KeyEvent event) {
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) return false;
    final keyboard = HardwareKeyboard.instance;
    final key = event.logicalKey;
    return keyboard.isControlPressed &&
            (key == LogicalKeyboardKey.keyL ||
                key == LogicalKeyboardKey.keyT ||
                key == LogicalKeyboardKey.keyW ||
                key == LogicalKeyboardKey.keyR) ||
        keyboard.isAltPressed &&
            (key == LogicalKeyboardKey.arrowLeft ||
                key == LogicalKeyboardKey.arrowRight) ||
        key == LogicalKeyboardKey.escape;
  }

  Widget _buildContent(BuildContext context) {
    final frame = _displayedFrame;
    final textureId = _textureId;
    Widget visual;
    if (frame != null && textureId != null && _sameFrame(widget.frame, frame)) {
      visual = SizedBox.expand(
        child: FittedBox(
          fit: BoxFit.contain,
          child: SizedBox(
            width: frame.width.toDouble(),
            height: frame.height.toDouble(),
            child: Texture(
              key: const Key('browser-texture'),
              textureId: textureId,
            ),
          ),
        ),
      );
    } else {
      visual = Center(
        child: Semantics(
          liveRegion: true,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                Icons.web_asset_off,
                size: 42,
                color: Theme.of(context).colorScheme.outline,
              ),
              const SizedBox(height: 12),
              const Text(
                'Renderer frame unavailable',
                key: Key('renderer-unavailable'),
              ),
              const SizedBox(height: 4),
              Text(
                widget.contextState?.url ?? 'No browsing context',
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
          ),
        ),
      );
    }
    return _withAccessibility(visual);
  }

  Widget _withAccessibility(Widget visual) {
    final snapshot = widget.accessibility;
    final contextState = widget.contextState;
    if (snapshot == null ||
        contextState == null ||
        snapshot.contextId != contextState.contextId ||
        snapshot.documentId != contextState.documentId ||
        snapshot.viewportWidth != _reportedViewport?.width ||
        snapshot.viewportHeight != _reportedViewport?.height ||
        widget.frame?.width != snapshot.viewportWidth ||
        widget.frame?.height != snapshot.viewportHeight ||
        _logicalViewport.width <= 0 ||
        _logicalViewport.height <= 0) {
      return visual;
    }
    final scale = math.min(
      _logicalViewport.width / snapshot.viewportWidth,
      _logicalViewport.height / snapshot.viewportHeight,
    );
    final offsetX =
        (_logicalViewport.width - snapshot.viewportWidth * scale) / 2;
    final offsetY =
        (_logicalViewport.height - snapshot.viewportHeight * scale) / 2;
    final childrenByParent = <int?, List<BrowserAccessibilityNode>>{};
    for (final node in snapshot.nodes) {
      childrenByParent.putIfAbsent(node.parentId, () => []).add(node);
    }

    List<Widget> buildNodes(int? parentId, double originX, double originY) {
      final widgets = <Widget>[];
      for (final node in childrenByParent[parentId] ?? const []) {
        if (node.hidden) continue;
        final bounds = node.bounds;
        if (bounds == null || bounds.width <= 0 || bounds.height <= 0) {
          widgets.addAll(buildNodes(node.id, originX, originY));
          continue;
        }
        final absoluteX = offsetX + bounds.x * scale;
        final absoluteY = offsetY + bounds.y * scale;
        final children = buildNodes(node.id, absoluteX, absoluteY);
        widgets.add(
          Positioned(
            left: absoluteX - originX,
            top: absoluteY - originY,
            width: bounds.width * scale,
            height: bounds.height * scale,
            child: Semantics(
              key: ValueKey('semantic-${snapshot.generation}-${node.id}'),
              container: true,
              explicitChildNodes: children.isNotEmpty,
              label: node.label,
              value: node.value,
              enabled: _roleHasEnabledState(node.role) ? !node.disabled : null,
              checked: node.checked,
              selected: node.role == 'option' || node.role == 'tab'
                  ? node.selected
                  : null,
              expanded: node.expanded,
              focusable: node.focusable,
              focused: node.focused,
              button: node.role == 'button',
              link: node.role == 'link',
              header: node.role == 'heading',
              image: node.role == 'image',
              textField: node.role == 'textbox' || node.role == 'searchbox',
              onTap: node.actions.contains('tap') && !node.disabled
                  ? () => widget.onSemanticTap?.call(snapshot, node)
                  : null,
              onFocus: node.actions.contains('focus') && !node.disabled
                  ? () => widget.onSemanticFocus?.call(snapshot, node)
                  : null,
              child: children.isEmpty
                  ? const SizedBox.expand()
                  : Stack(clipBehavior: Clip.none, children: children),
            ),
          ),
        );
      }
      return widgets;
    }

    final nodes = buildNodes(null, 0, 0);
    if (nodes.isEmpty) return visual;
    return Semantics(
      container: true,
      explicitChildNodes: true,
      child: Stack(
        fit: StackFit.expand,
        children: [
          ExcludeSemantics(child: visual),
          Stack(clipBehavior: Clip.none, children: nodes),
        ],
      ),
    );
  }

  @override
  void dispose() {
    _disposed = true;
    _contentFocus.dispose();
    unawaited(_controller.dispose());
    super.dispose();
  }
}

int _domButton(int buttons) {
  if (buttons & kMiddleMouseButton != 0) return 1;
  if (buttons & kSecondaryMouseButton != 0) return 2;
  return 0;
}

bool _roleHasEnabledState(String role) => switch (role) {
  'button' ||
  'link' ||
  'checkbox' ||
  'radio' ||
  'textbox' ||
  'searchbox' ||
  'combobox' ||
  'slider' ||
  'spinbutton' => true,
  _ => false,
};

String _domKey(KeyEvent event, HardwareKeyboard keyboard) {
  final character = event.character;
  if (character != null && character.isNotEmpty) return character;
  final key = event.logicalKey;
  if (key == LogicalKeyboardKey.enter) return 'Enter';
  if (key == LogicalKeyboardKey.escape) return 'Escape';
  if (key == LogicalKeyboardKey.backspace) return 'Backspace';
  if (key == LogicalKeyboardKey.tab) return 'Tab';
  if (key == LogicalKeyboardKey.space) return ' ';
  if (key == LogicalKeyboardKey.arrowLeft) return 'ArrowLeft';
  if (key == LogicalKeyboardKey.arrowRight) return 'ArrowRight';
  if (key == LogicalKeyboardKey.arrowUp) return 'ArrowUp';
  if (key == LogicalKeyboardKey.arrowDown) return 'ArrowDown';
  if (key == LogicalKeyboardKey.delete) return 'Delete';
  if (key == LogicalKeyboardKey.home) return 'Home';
  if (key == LogicalKeyboardKey.end) return 'End';
  if (key == LogicalKeyboardKey.pageUp) return 'PageUp';
  if (key == LogicalKeyboardKey.pageDown) return 'PageDown';
  if (key == LogicalKeyboardKey.shiftLeft ||
      key == LogicalKeyboardKey.shiftRight) {
    return 'Shift';
  }
  if (key == LogicalKeyboardKey.controlLeft ||
      key == LogicalKeyboardKey.controlRight) {
    return 'Control';
  }
  if (key == LogicalKeyboardKey.altLeft || key == LogicalKeyboardKey.altRight) {
    return 'Alt';
  }
  if (key == LogicalKeyboardKey.metaLeft ||
      key == LogicalKeyboardKey.metaRight) {
    return 'Meta';
  }
  final label = key.keyLabel;
  if (label.length == 1) {
    return keyboard.isShiftPressed ? label.toUpperCase() : label.toLowerCase();
  }
  return 'Unidentified';
}

String _domCode(PhysicalKeyboardKey key) {
  final usage = key.usbHidUsage;
  if (usage >> 16 != 0x07) return 'Unidentified';
  final id = usage & 0xffff;
  if (id >= 0x04 && id <= 0x1d) {
    return 'Key${String.fromCharCode(65 + id - 0x04)}';
  }
  if (id >= 0x1e && id <= 0x27) {
    const digits = <String>[
      'Digit1',
      'Digit2',
      'Digit3',
      'Digit4',
      'Digit5',
      'Digit6',
      'Digit7',
      'Digit8',
      'Digit9',
      'Digit0',
    ];
    return digits[id - 0x1e];
  }
  if (id >= 0x3a && id <= 0x45) return 'F${id - 0x39}';
  if (id >= 0x59 && id <= 0x61) return 'Numpad${id - 0x58}';
  return const <int, String>{
        0x28: 'Enter',
        0x29: 'Escape',
        0x2a: 'Backspace',
        0x2b: 'Tab',
        0x2c: 'Space',
        0x2d: 'Minus',
        0x2e: 'Equal',
        0x2f: 'BracketLeft',
        0x30: 'BracketRight',
        0x31: 'Backslash',
        0x33: 'Semicolon',
        0x34: 'Quote',
        0x35: 'Backquote',
        0x36: 'Comma',
        0x37: 'Period',
        0x38: 'Slash',
        0x39: 'CapsLock',
        0x46: 'PrintScreen',
        0x47: 'ScrollLock',
        0x48: 'Pause',
        0x49: 'Insert',
        0x4a: 'Home',
        0x4b: 'PageUp',
        0x4c: 'Delete',
        0x4d: 'End',
        0x4e: 'PageDown',
        0x4f: 'ArrowRight',
        0x50: 'ArrowLeft',
        0x51: 'ArrowDown',
        0x52: 'ArrowUp',
        0x53: 'NumLock',
        0x54: 'NumpadDivide',
        0x55: 'NumpadMultiply',
        0x56: 'NumpadSubtract',
        0x57: 'NumpadAdd',
        0x58: 'NumpadEnter',
        0x62: 'Numpad0',
        0x63: 'NumpadDecimal',
        0xe0: 'ControlLeft',
        0xe1: 'ShiftLeft',
        0xe2: 'AltLeft',
        0xe3: 'MetaLeft',
        0xe4: 'ControlRight',
        0xe5: 'ShiftRight',
        0xe6: 'AltRight',
        0xe7: 'MetaRight',
      }[id] ??
      'Unidentified';
}

bool _isNewer(BrowserFrame candidate, BrowserFrame? previous) {
  if (previous == null ||
      candidate.contextId != previous.contextId ||
      candidate.documentId != previous.documentId) {
    return true;
  }
  return candidate.frameId > previous.frameId;
}

bool _sameFrame(BrowserFrame? first, BrowserFrame second) =>
    first != null &&
    first.contextId == second.contextId &&
    first.documentId == second.documentId &&
    first.frameId == second.frameId;
