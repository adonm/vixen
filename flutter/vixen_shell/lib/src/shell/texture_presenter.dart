import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';

import '../bridge/browser_models.dart';
import '../bridge/render_models.dart';
import '../renderer/formatter.dart';
import '../renderer/formatter_painter.dart';

const String vixenTextureChannelName = 'dev.adonm.vixen/texture';

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

final class BrowserViewportTransform {
  BrowserViewportTransform._({
    required this.logicalSize,
    required this.width,
    required this.height,
    required this.scaleFactor,
  });

  factory BrowserViewportTransform.fromLogical(Size logicalSize, double dpr) {
    if (!logicalSize.width.isFinite ||
        !logicalSize.height.isFinite ||
        !dpr.isFinite ||
        logicalSize.width <= 0 ||
        logicalSize.height <= 0 ||
        dpr <= 0) {
      return BrowserViewportTransform._(
        logicalSize: logicalSize,
        width: 0,
        height: 0,
        scaleFactor: 0,
      );
    }
    final rawWidth = logicalSize.width * dpr;
    final rawHeight = logicalSize.height * dpr;
    if (!rawWidth.isFinite || !rawHeight.isFinite) {
      return BrowserViewportTransform._(
        logicalSize: logicalSize,
        width: 0,
        height: 0,
        scaleFactor: 0,
      );
    }
    final byteScale = math.sqrt(
      browserMaxFrameBytes / (rawWidth * rawHeight * 4),
    );
    final boundedScale = math.min(
      1.0,
      math.min(
        browserMaxFrameDimension / rawWidth,
        math.min(browserMaxFrameDimension / rawHeight, byteScale),
      ),
    );
    return BrowserViewportTransform._(
      logicalSize: logicalSize,
      width: (rawWidth * boundedScale).floor().clamp(
        1,
        browserMaxFrameDimension,
      ),
      height: (rawHeight * boundedScale).floor().clamp(
        1,
        browserMaxFrameDimension,
      ),
      scaleFactor: dpr * boundedScale,
    );
  }

  final Size logicalSize;
  final int width;
  final int height;
  final double scaleFactor;

  bool get isValid =>
      width > 0 &&
      height > 0 &&
      logicalSize.width > 0 &&
      logicalSize.height > 0;

  double get logicalPixelsPerPhysicalPixel => isValid
      ? math.min(logicalSize.width / width, logicalSize.height / height)
      : 0;

  double get offsetX =>
      (logicalSize.width - width * logicalPixelsPerPhysicalPixel) / 2;

  double get offsetY =>
      (logicalSize.height - height * logicalPixelsPerPhysicalPixel) / 2;

  double get displayWidth => width * logicalPixelsPerPhysicalPixel;
  double get displayHeight => height * logicalPixelsPerPhysicalPixel;

  Offset localToPhysical(Offset position) {
    final displayScale = logicalPixelsPerPhysicalPixel;
    if (displayScale <= 0) return Offset.zero;
    return Offset(
      ((position.dx - offsetX) / displayScale).clamp(0, width.toDouble()),
      ((position.dy - offsetY) / displayScale).clamp(0, height.toDouble()),
    );
  }

  Offset logicalDeltaToPhysical(Offset delta) {
    final displayScale = logicalPixelsPerPhysicalPixel;
    return displayScale > 0 ? delta / displayScale : Offset.zero;
  }

  Rect physicalRectToLocal(Rect rect) {
    final displayScale = logicalPixelsPerPhysicalPixel;
    return Rect.fromLTWH(
      offsetX + rect.left * displayScale,
      offsetY + rect.top * displayScale,
      rect.width * displayScale,
      rect.height * displayScale,
    );
  }

  @override
  bool operator ==(Object other) =>
      other is BrowserViewportTransform &&
      other.logicalSize == logicalSize &&
      other.width == width &&
      other.height == height &&
      other.scaleFactor == scaleFactor;

  @override
  int get hashCode => Object.hash(logicalSize, width, height, scaleFactor);
}

({int width, int height}) physicalFrameViewport(Size logicalSize, double dpr) {
  final transform = BrowserViewportTransform.fromLogical(logicalSize, dpr);
  return (width: transform.width, height: transform.height);
}

final class BrowserContentSurface extends StatefulWidget {
  const BrowserContentSurface({
    required this.contextState,
    required this.frame,
    this.rendererView,
    this.rendererFindResult,
    this.onRendererPresented,
    this.onRendererSemanticAction,
    this.lifecycle = BrowserHostLifecycle.resumed,
    this.onPhysicalViewportChanged,
    this.onFocusChanged,
    this.onMouseEvent,
    this.onKeyEvent,
    this.onTextInput,
    this.accessibility,
    this.onSemanticTap,
    this.onSemanticFocus,
    this.onSemanticSetValue,
    this.onSemanticAdjustment,
    this.textureController,
    super.key,
  });

  final BrowsingContextState? contextState;
  final BrowserFrame? frame;
  final FormatterCommitView? rendererView;
  final FormatterFindResult? rendererFindResult;
  final ValueChanged<FormatterCommitView>? onRendererPresented;
  final void Function(
    FormatterCommitView view,
    RenderSemanticDescriptor descriptor,
    RenderSemanticActionKind action,
    String? value,
  )?
  onRendererSemanticAction;
  final BrowserHostLifecycle lifecycle;
  final void Function(int width, int height, double scaleFactor)?
  onPhysicalViewportChanged;
  final ValueChanged<bool>? onFocusChanged;
  final void Function(String eventType, BrowserMouseEvent event)? onMouseEvent;
  final void Function(String eventType, BrowserKeyEvent event)? onKeyEvent;
  final ValueChanged<BrowserTextInputState>? onTextInput;
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
  final void Function(
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node,
    String value,
  )?
  onSemanticSetValue;
  final void Function(
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node,
    bool increase,
  )?
  onSemanticAdjustment;
  final BrowserTextureController? textureController;

  @override
  State<BrowserContentSurface> createState() => _BrowserContentSurfaceState();
}

final class _BrowserContentSurfaceState extends State<BrowserContentSurface> {
  static const int _maxPresentationRetries = 2;

  late BrowserTextureController _controller;
  Future<int>? _createOperation;
  BrowserFrame? _pendingFrame;
  BrowserFrame? _publishedFrame;
  BrowserFrame? _displayedFrame;
  BrowserViewportTransform? _viewportTransform;
  final FocusNode _contentFocus = FocusNode(debugLabel: 'browser-content');
  final Map<int, int> _pressedButtons = {};
  final Set<PhysicalKeyboardKey> _suppressedShortcutKeys = {};
  int? _activeTouchPointer;
  Offset? _touchOrigin;
  Offset? _touchLastPosition;
  bool _touchScrolling = false;
  late final _BrowserTextInputClient _textInputClient;
  Size _logicalViewport = Size.zero;
  PointerEvent? _pendingMouseMove;
  bool _mouseMoveScheduled = false;
  int? _textureId;
  int _controllerEpoch = 0;
  int _presentationFailures = 0;
  bool _presenting = false;
  Completer<void>? _presentationIdle;
  Future<void> _surfaceRelease = Future<void>.value();
  bool _disposed = false;
  bool _contentFocused = false;
  bool _presentationRecoveryFailed = false;
  (int, int, int)? _presentationFailureKey;
  (int, int, int, BrowserTextInputType, BrowserTextInputAction)?
  _textInputTarget;
  (int, int, int)? _scheduledRendererPresentation;
  (int, int, int)? _reportedRendererPresentation;

  @override
  void initState() {
    super.initState();
    _controller = widget.textureController ?? LinuxTextureController();
    _textInputClient = _BrowserTextInputClient(
      _handleTextInput,
      _handleTextInputAction,
    );
    _queueFrame(widget.frame);
  }

  @override
  void didUpdateWidget(BrowserContentSurface oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.textureController != widget.textureController) {
      _controllerEpoch++;
      _queueControllerDispose(_controller);
      _controller = widget.textureController ?? LinuxTextureController();
      _createOperation = null;
      _textureId = null;
      _publishedFrame = null;
      _displayedFrame = null;
      _clearPresentationFailures();
    }
    final wasEnabled = _lifecycleAllowsPresentation(oldWidget.lifecycle);
    final isEnabled = _lifecycleAllowsPresentation(widget.lifecycle);
    if (wasEnabled && !isEnabled) {
      _suspendPresentation();
    }
    _queueFrame(widget.frame);
    _syncTextInput();
  }

  void _queueFrame(BrowserFrame? frame) {
    if (!_presentationEnabled) {
      _pendingFrame = null;
      return;
    }
    if (frame == null) {
      _pendingFrame = null;
      _displayedFrame = null;
      return;
    }
    final key = _frameKey(frame);
    if (_presentationFailureKey != key) {
      _clearPresentationFailures();
    } else if (_presentationFailures > _maxPresentationRetries) {
      return;
    }
    _pendingFrame = frame;
    if (!_presenting) unawaited(_presentFrames());
  }

  Future<void> _presentFrames() async {
    _presenting = true;
    _presentationIdle = Completer<void>();
    BrowserFrame? attemptedFrame;
    BrowserTextureController? attemptedController;
    int? attemptedControllerEpoch;
    try {
      while (!_disposed && _pendingFrame != null) {
        final frame = _pendingFrame!;
        attemptedFrame = frame;
        _pendingFrame = null;
        if (!_isNewer(frame, _publishedFrame)) continue;
        await _surfaceRelease;
        if (_disposed || !_presentationEnabled) return;
        final controller = _controller;
        final controllerEpoch = _controllerEpoch;
        attemptedController = controller;
        attemptedControllerEpoch = controllerEpoch;
        final textureId = await (_createOperation ??= controller.create());
        if (_disposed || !_presentationEnabled) return;
        if (controllerEpoch != _controllerEpoch) return;
        if (_pendingFrame case final newer? when _isNewer(newer, frame)) {
          continue;
        }
        await controller.publish(frame);
        if (_disposed || !_presentationEnabled) return;
        if (controllerEpoch != _controllerEpoch) return;
        _textureId = textureId;
        _publishedFrame = frame;
        _clearPresentationFailures();
        if (_sameFrame(widget.frame, frame)) {
          _displayedFrame = frame;
          if (mounted) setState(() {});
        }
      }
    } catch (_) {
      final attemptIsCurrent =
          attemptedControllerEpoch == _controllerEpoch && _presentationEnabled;
      _displayedFrame = null;
      _publishedFrame = null;
      _textureId = null;
      _createOperation = null;
      if (attemptIsCurrent) {
        _controllerEpoch++;
        try {
          await attemptedController?.dispose();
        } catch (_) {
          // The original create/publish failure remains the recovery signal.
        }
      }
      final newer = _pendingFrame;
      final retry = newer ?? attemptedFrame;
      if (!_disposed && attemptIsCurrent && retry != null) {
        if (newer != null && _frameKey(newer) != _frameKey(attemptedFrame!)) {
          _clearPresentationFailures();
          _pendingFrame = newer;
        } else if (_shouldRetryPresentation(retry)) {
          _pendingFrame = retry;
        } else {
          _presentationRecoveryFailed = true;
        }
      }
      if (mounted) setState(() {});
    } finally {
      _presenting = false;
      _presentationIdle?.complete();
      _presentationIdle = null;
      if (!_disposed && _presentationEnabled && _pendingFrame != null) {
        unawaited(_presentFrames());
      }
    }
  }

  bool get _presentationEnabled =>
      _lifecycleAllowsPresentation(widget.lifecycle);

  void _suspendPresentation() {
    _controllerEpoch++;
    _pendingFrame = null;
    _publishedFrame = null;
    _displayedFrame = null;
    _textureId = null;
    _createOperation = null;
    _clearPresentationFailures();
    _queueControllerDispose(_controller);
  }

  void _queueControllerDispose(BrowserTextureController controller) {
    final previousRelease = _surfaceRelease;
    final presentationIdle = _presentationIdle?.future;
    _surfaceRelease = _disposeAfterPresentation(
      previousRelease,
      presentationIdle,
      controller,
    );
    unawaited(_surfaceRelease);
  }

  Future<void> _disposeAfterPresentation(
    Future<void> previousRelease,
    Future<void>? presentationIdle,
    BrowserTextureController controller,
  ) async {
    try {
      await previousRelease;
    } catch (_) {
      // A later release must still run after an earlier disposal failure.
    }
    if (presentationIdle != null) await presentationIdle;
    try {
      await controller.dispose();
    } catch (_) {
      // Lifecycle recovery is driven by recreation, not disposal success.
    }
  }

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        _logicalViewport = Size(constraints.maxWidth, constraints.maxHeight);
        final transform = BrowserViewportTransform.fromLogical(
          _logicalViewport,
          MediaQuery.devicePixelRatioOf(context),
        );
        if (_viewportTransform != transform) {
          _viewportTransform = transform;
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (!_disposed && _viewportTransform == transform) {
              widget.onPhysicalViewportChanged?.call(
                transform.width,
                transform.height,
                transform.scaleFactor,
              );
            }
          });
        }
        return Focus(
          focusNode: _contentFocus,
          onFocusChange: _handleFocusChanged,
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

  void _handleFocusChanged(bool focused) {
    _contentFocused = focused;
    if (!focused) {
      _pressedButtons.clear();
      _clearTouchGesture();
    }
    widget.onFocusChanged?.call(focused);
    _syncTextInput();
  }

  void _handleTextInput(TextEditingValue value) {
    final callback = widget.onTextInput;
    if (callback == null || !_contentFocused || _textInputTarget == null) {
      return;
    }
    final composing = value.composing.isValid
        ? BrowserAccessibilityTextSelection(
            baseOffset: value.composing.start,
            extentOffset: value.composing.end,
          )
        : null;
    callback(
      BrowserTextInputState(
        text: value.text,
        selection: BrowserAccessibilityTextSelection(
          baseOffset: value.selection.baseOffset,
          extentOffset: value.selection.extentOffset,
        ),
        composing: composing,
      ),
    );
  }

  void _handleTextInputAction(TextInputAction action) {
    final callback = widget.onKeyEvent;
    if (callback == null ||
        !_contentFocused ||
        _textInputTarget == null ||
        action == TextInputAction.none ||
        action == TextInputAction.unspecified) {
      return;
    }
    const event = BrowserKeyEvent(key: 'Enter', code: 'Enter');
    callback('keydown', event);
    callback('keyup', event);
  }

  void _handleSemanticFocus(
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node,
  ) {
    _contentFocus.requestFocus();
    widget.onSemanticFocus?.call(snapshot, node);
  }

  void _syncTextInput() {
    final contextState = widget.contextState;
    final snapshot = widget.accessibility;
    BrowserAccessibilityNode? node;
    for (final candidate
        in snapshot?.nodes ?? const <BrowserAccessibilityNode>[]) {
      if (candidate.focused &&
          !candidate.disabled &&
          candidate.actions.contains('set_value') &&
          candidate.textSelection != null &&
          candidate.value != null &&
          candidate.textInputType != null &&
          candidate.textInputAction != null) {
        node = candidate;
        break;
      }
    }
    if (!_contentFocused ||
        contextState == null ||
        snapshot == null ||
        snapshot.contextId != contextState.contextId ||
        snapshot.documentId != contextState.documentId ||
        node == null) {
      _textInputTarget = null;
      _textInputClient.close();
      return;
    }
    final target = (
      snapshot.contextId,
      snapshot.documentId,
      node.id,
      node.textInputType!,
      node.textInputAction!,
    );
    final selection = node.textSelection!;
    final text = node.value!;
    final value = TextEditingValue(
      text: text,
      selection: TextSelection(
        baseOffset: selection.baseOffset.clamp(0, text.length),
        extentOffset: selection.extentOffset.clamp(0, text.length),
      ),
    );
    if (_textInputTarget != target) {
      _textInputClient.close();
      _textInputTarget = target;
      _textInputClient.open(
        value,
        inputType: _platformTextInputType(node.textInputType!),
        action: _platformTextInputAction(node.textInputAction!),
      );
    } else {
      _textInputClient.reconcile(value);
    }
  }

  void _handlePointerDown(PointerDownEvent event) {
    if (event.kind == PointerDeviceKind.touch) {
      if (_activeTouchPointer != null) return;
      _activeTouchPointer = event.pointer;
      _touchOrigin = event.localPosition;
      _touchLastPosition = event.localPosition;
      _touchScrolling = false;
    }
    _contentFocus.requestFocus();
    final button = _domButton(event.buttons);
    _pressedButtons[event.pointer] = button;
    _emitMouse('mousedown', event, button: button, detail: 1);
  }

  void _handlePointerMove(PointerMoveEvent event) {
    if (event.kind == PointerDeviceKind.touch) {
      _handleTouchMove(event);
      return;
    }
    _scheduleMouseMove(event);
  }

  void _handleTouchMove(PointerMoveEvent event) {
    if (_activeTouchPointer != event.pointer ||
        _touchOrigin == null ||
        _touchLastPosition == null) {
      return;
    }
    final origin = _touchOrigin!;
    final last = _touchLastPosition!;
    final current = event.localPosition;
    _touchLastPosition = current;
    var delta = last - current;
    if (!_touchScrolling) {
      if ((current - origin).distance < kTouchSlop) return;
      _touchScrolling = true;
      delta = origin - current;
      final button = _pressedButtons.remove(event.pointer) ?? 0;
      _emitMouse('cancel', event, button: button, buttons: 0);
    }
    if (delta == Offset.zero) return;
    _emitMouse(
      'wheel',
      event,
      button: 0,
      buttons: 0,
      deltaX: delta.dx,
      deltaY: delta.dy,
    );
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
    if (event.kind == PointerDeviceKind.touch) {
      if (_activeTouchPointer != event.pointer) return;
      final wasScrolling = _touchScrolling;
      _clearTouchGesture();
      if (wasScrolling) {
        _pressedButtons.remove(event.pointer);
        return;
      }
    }
    final button = _pressedButtons.remove(event.pointer) ?? 0;
    _emitMouse('mouseup', event, button: button, buttons: 0, detail: 1);
  }

  void _handlePointerCancel(PointerCancelEvent event) {
    if (event.kind == PointerDeviceKind.touch) {
      if (_activeTouchPointer != event.pointer) return;
      final wasScrolling = _touchScrolling;
      _clearTouchGesture();
      if (wasScrolling) {
        _pressedButtons.remove(event.pointer);
        return;
      }
    }
    final button = _pressedButtons.remove(event.pointer) ?? 0;
    _emitMouse('cancel', event, button: button, buttons: 0);
  }

  void _clearTouchGesture() {
    _activeTouchPointer = null;
    _touchOrigin = null;
    _touchLastPosition = null;
    _touchScrolling = false;
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
    final transform = _viewportTransform;
    if (callback == null || transform == null || !transform.isValid) {
      return;
    }
    final physicalPosition = transform.localToPhysical(event.localPosition);
    final physicalDelta = transform.logicalDeltaToPhysical(
      Offset(deltaX, deltaY),
    );
    final keyboard = HardwareKeyboard.instance;
    callback(
      eventType,
      BrowserMouseEvent(
        x: physicalPosition.dx,
        y: physicalPosition.dy,
        button: button,
        buttons: buttons ?? event.buttons,
        detail: detail,
        ctrlKey: keyboard.isControlPressed,
        shiftKey: keyboard.isShiftPressed,
        altKey: keyboard.isAltPressed,
        metaKey: keyboard.isMetaPressed,
        deltaX: physicalDelta.dx,
        deltaY: physicalDelta.dy,
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
        _textInputTarget == null &&
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
    return _textInputTarget == null
        ? KeyEventResult.handled
        : KeyEventResult.ignored;
  }

  bool _isShellShortcut(KeyEvent event) {
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) return false;
    final keyboard = HardwareKeyboard.instance;
    final key = event.logicalKey;
    return keyboard.isControlPressed &&
            (key == LogicalKeyboardKey.keyL ||
                key == LogicalKeyboardKey.keyT ||
                key == LogicalKeyboardKey.keyW ||
                key == LogicalKeyboardKey.keyR ||
                key == LogicalKeyboardKey.keyF ||
                key == LogicalKeyboardKey.equal ||
                key == LogicalKeyboardKey.minus ||
                key == LogicalKeyboardKey.digit0) ||
        keyboard.isAltPressed &&
            (key == LogicalKeyboardKey.arrowLeft ||
                key == LogicalKeyboardKey.arrowRight) ||
        key == LogicalKeyboardKey.escape;
  }

  Widget _buildContent(BuildContext context) {
    final rendererView = widget.rendererView;
    final contextState = widget.contextState;
    if (rendererView != null &&
        !rendererView.isRetired &&
        _presentationEnabled &&
        contextState != null &&
        rendererView.commit.revision.contextId == contextState.contextId &&
        rendererView.commit.revision.documentId == contextState.documentId) {
      _scheduleRendererPresentation(rendererView);
      return SizedBox.expand(
        child: FittedBox(
          key: const Key('flutter-renderer-view'),
          fit: BoxFit.fill,
          alignment: Alignment.topLeft,
          child: SizedBox(
            width: rendererView.viewport.width,
            height: rendererView.viewport.height,
            child: CustomPaint(
              painter: RenderCommitPainter(
                rendererView,
                findResult: widget.rendererFindResult,
                onSemanticAction: (descriptor, action, value) {
                  widget.onRendererSemanticAction?.call(
                    rendererView,
                    descriptor,
                    action,
                    value,
                  );
                },
              ),
            ),
          ),
        ),
      );
    }
    final frame = _displayedFrame;
    final textureId = _textureId;
    final transform = _viewportTransform;
    Widget visual;
    if (frame != null &&
        textureId != null &&
        transform != null &&
        transform.isValid &&
        _sameFrame(widget.frame, frame)) {
      visual = SizedBox.expand(
        child: Stack(
          children: [
            Positioned(
              left: transform.offsetX,
              top: transform.offsetY,
              width: transform.displayWidth,
              height: transform.displayHeight,
              child: Texture(
                key: const Key('browser-texture'),
                textureId: textureId,
              ),
            ),
          ],
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
              if (_presentationRecoveryFailed) ...[
                const SizedBox(height: 4),
                const Text(
                  'Surface recovery failed',
                  key: Key('surface-recovery-failed'),
                ),
              ],
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

  void _scheduleRendererPresentation(FormatterCommitView view) {
    final key = (
      view.commit.revision.contextId,
      view.commit.revision.documentId,
      view.commit.commitId,
    );
    if (_reportedRendererPresentation == key ||
        _scheduledRendererPresentation == key) {
      return;
    }
    _scheduledRendererPresentation = key;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final current = widget.rendererView;
      if (_disposed ||
          !_presentationEnabled ||
          current == null ||
          current.isRetired ||
          !identical(current, view)) {
        if (_scheduledRendererPresentation == key) {
          _scheduledRendererPresentation = null;
        }
        return;
      }
      _reportedRendererPresentation = key;
      _scheduledRendererPresentation = null;
      widget.onRendererPresented?.call(view);
    });
  }

  bool _shouldRetryPresentation(BrowserFrame frame) {
    final key = _frameKey(frame);
    if (_presentationFailureKey != key) {
      _presentationFailureKey = key;
      _presentationFailures = 0;
    }
    _presentationFailures++;
    return _presentationFailures <= _maxPresentationRetries;
  }

  void _clearPresentationFailures() {
    _presentationFailureKey = null;
    _presentationFailures = 0;
    _presentationRecoveryFailed = false;
  }

  Widget _withAccessibility(Widget visual) {
    final snapshot = widget.accessibility;
    final contextState = widget.contextState;
    final transform = _viewportTransform;
    if (snapshot == null ||
        contextState == null ||
        transform == null ||
        !transform.isValid ||
        snapshot.contextId != contextState.contextId ||
        snapshot.documentId != contextState.documentId ||
        snapshot.viewportWidth != transform.width ||
        snapshot.viewportHeight != transform.height ||
        widget.frame?.width != snapshot.viewportWidth ||
        widget.frame?.height != snapshot.viewportHeight) {
      return visual;
    }
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
        final localBounds = transform.physicalRectToLocal(
          Rect.fromLTWH(bounds.x, bounds.y, bounds.width, bounds.height),
        );
        final absoluteX = localBounds.left;
        final absoluteY = localBounds.top;
        final children = buildNodes(node.id, absoluteX, absoluteY);
        final range = node.range;
        final semanticIdentifier = _semanticIdentifier(snapshot, node.id);
        widgets.add(
          Positioned(
            left: absoluteX - originX,
            top: absoluteY - originY,
            width: localBounds.width,
            height: localBounds.height,
            child: Semantics(
              key: ValueKey((
                snapshot.contextId,
                snapshot.documentId,
                node.id,
                jsonEncode(node.toWire()),
              )),
              container: true,
              explicitChildNodes: children.isNotEmpty,
              identifier: semanticIdentifier,
              controlsNodes: node.controlsIds.isEmpty
                  ? null
                  : Set<String>.from(
                      node.controlsIds.map(
                        (id) => _semanticIdentifier(snapshot, id),
                      ),
                    ),
              label: node.label,
              hint: node.description.isEmpty ? null : node.description,
              value: node.value,
              slider: node.role == 'slider' && range != null,
              minValue: range == null
                  ? null
                  : _formatSemanticNumber(range.minimum),
              maxValue: range == null
                  ? null
                  : _formatSemanticNumber(range.maximum),
              increasedValue: range == null
                  ? null
                  : _formatSemanticNumber(
                      math.min(range.maximum, range.current + range.step),
                    ),
              decreasedValue: range == null
                  ? null
                  : _formatSemanticNumber(
                      math.max(range.minimum, range.current - range.step),
                    ),
              enabled: _roleHasEnabledState(node.role) ? !node.disabled : null,
              checked: node.checked,
              mixed: node.mixed,
              selected: node.role == 'option' || node.role == 'tab'
                  ? node.selected
                  : null,
              expanded: node.expanded,
              headingLevel: node.headingLevel,
              liveRegion: node.liveRegion,
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
                  ? () => _handleSemanticFocus(snapshot, node)
                  : null,
              onSetText: node.actions.contains('set_value') && !node.disabled
                  ? (value) =>
                        widget.onSemanticSetValue?.call(snapshot, node, value)
                  : null,
              onIncrease: node.actions.contains('increase') && !node.disabled
                  ? () =>
                        widget.onSemanticAdjustment?.call(snapshot, node, true)
                  : null,
              onDecrease: node.actions.contains('decrease') && !node.disabled
                  ? () =>
                        widget.onSemanticAdjustment?.call(snapshot, node, false)
                  : null,
              child: _TextSelectionSemantics(
                selection: node.textSelection == null
                    ? null
                    : TextSelection(
                        baseOffset: node.textSelection!.baseOffset,
                        extentOffset: node.textSelection!.extentOffset,
                      ),
                child: children.isEmpty
                    ? const SizedBox.expand()
                    : Stack(clipBehavior: Clip.none, children: children),
              ),
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
    _controllerEpoch++;
    _textInputClient.close();
    _contentFocus.dispose();
    _queueControllerDispose(_controller);
    super.dispose();
  }
}

final class _BrowserTextInputClient with TextInputClient {
  _BrowserTextInputClient(this._onChanged, this._onAction);

  final ValueChanged<TextEditingValue> _onChanged;
  final ValueChanged<TextInputAction> _onAction;
  TextInputConnection? _connection;
  TextEditingValue _value = TextEditingValue.empty;
  TextInputAction _action = TextInputAction.none;

  @override
  TextEditingValue get currentTextEditingValue => _value;

  @override
  AutofillScope? get currentAutofillScope => null;

  void open(
    TextEditingValue value, {
    required TextInputType inputType,
    required TextInputAction action,
  }) {
    _value = value;
    _action = action;
    _connection = TextInput.attach(
      this,
      TextInputConfiguration(
        inputType: inputType,
        inputAction: action,
        enableSuggestions: true,
        autocorrect: true,
      ),
    );
    _connection!.setEditingState(value);
    _connection!.show();
  }

  void reconcile(TextEditingValue value) {
    if (_value.composing.isValid) return;
    final next = _value.text == value.text
        ? value.copyWith(composing: _value.composing)
        : value;
    if (_value == next) return;
    _value = next;
    _connection?.setEditingState(next);
  }

  void close() {
    _connection?.close();
    _connection = null;
    _action = TextInputAction.none;
  }

  @override
  void updateEditingValue(TextEditingValue value) {
    if (utf8.encode(value.text).length > browserMaxTextInputBytes ||
        !value.selection.isValid ||
        value.selection.baseOffset > value.text.length ||
        value.selection.extentOffset > value.text.length ||
        (value.composing.isValid &&
            value.composing.start > value.composing.end) ||
        (value.composing.isValid && value.composing.end > value.text.length)) {
      _connection?.setEditingState(_value);
      return;
    }
    _value = value;
    _onChanged(value);
  }

  @override
  void connectionClosed() {
    _connection = null;
    _action = TextInputAction.none;
  }

  @override
  bool onFocusReceived() => _connection != null;

  @override
  void performAction(TextInputAction action) {
    if (_connection != null && action == _action) _onAction(action);
  }

  @override
  void performPrivateCommand(String action, Map<String, dynamic> data) {}

  @override
  void showAutocorrectionPromptRect(int start, int end) {}

  @override
  void updateFloatingCursor(RawFloatingCursorPoint point) {}
}

TextInputType _platformTextInputType(BrowserTextInputType type) =>
    switch (type) {
      BrowserTextInputType.none => TextInputType.none,
      BrowserTextInputType.text => TextInputType.text,
      BrowserTextInputType.multiline => TextInputType.multiline,
      BrowserTextInputType.number => TextInputType.number,
      BrowserTextInputType.decimal => const TextInputType.numberWithOptions(
        decimal: true,
      ),
      BrowserTextInputType.telephone => TextInputType.phone,
      BrowserTextInputType.email => TextInputType.emailAddress,
      BrowserTextInputType.url => TextInputType.url,
      BrowserTextInputType.search => TextInputType.webSearch,
    };

TextInputAction _platformTextInputAction(BrowserTextInputAction action) =>
    switch (action) {
      BrowserTextInputAction.newline => TextInputAction.newline,
      BrowserTextInputAction.done => TextInputAction.done,
      BrowserTextInputAction.go => TextInputAction.go,
      BrowserTextInputAction.next => TextInputAction.next,
      BrowserTextInputAction.previous => TextInputAction.previous,
      BrowserTextInputAction.search => TextInputAction.search,
      BrowserTextInputAction.send => TextInputAction.send,
    };

final class _TextSelectionSemantics extends SingleChildRenderObjectWidget {
  const _TextSelectionSemantics({
    required this.selection,
    required super.child,
  });

  final TextSelection? selection;

  @override
  RenderObject createRenderObject(BuildContext context) =>
      _RenderTextSelectionSemantics(selection);

  @override
  void updateRenderObject(
    BuildContext context,
    _RenderTextSelectionSemantics renderObject,
  ) {
    renderObject.selection = selection;
  }
}

final class _RenderTextSelectionSemantics extends RenderProxyBox {
  _RenderTextSelectionSemantics(this._selection);

  TextSelection? _selection;

  set selection(TextSelection? value) {
    if (_selection == value) return;
    _selection = value;
    markNeedsSemanticsUpdate();
  }

  @override
  void describeSemanticsConfiguration(SemanticsConfiguration config) {
    super.describeSemanticsConfiguration(config);
    final selection = _selection;
    if (selection != null) config.textSelection = selection;
  }
}

String _semanticIdentifier(BrowserAccessibilitySnapshot snapshot, int nodeId) =>
    'vixen-${snapshot.contextId}-${snapshot.documentId}-$nodeId';

String _formatSemanticNumber(double value) => value == value.truncateToDouble()
    ? value.toInt().toString()
    : value.toString();

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

(int, int, int) _frameKey(BrowserFrame frame) =>
    (frame.contextId, frame.documentId, frame.frameId);

bool _lifecycleAllowsPresentation(BrowserHostLifecycle lifecycle) =>
    lifecycle == BrowserHostLifecycle.resumed ||
    lifecycle == BrowserHostLifecycle.inactive;
