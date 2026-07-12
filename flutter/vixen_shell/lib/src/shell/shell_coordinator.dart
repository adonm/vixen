import 'dart:async';
import 'dart:collection';

import 'package:flutter/foundation.dart';

import '../bridge/browser_controller.dart';
import '../bridge/browser_models.dart';
import 'address.dart';

final class ShellCoordinator extends ChangeNotifier {
  static const int maxPendingInputEvents = 64;

  ShellCoordinator(this.controller);

  final BrowserController controller;
  final List<BrowsingContextState> _contexts = [];
  final Set<int> _closedContextIds = {};
  final Map<int, int> _pendingNavigations = {};
  final Map<int, String> _statusByContext = {};

  StreamSubscription<SequencedBrowserEvent>? _eventSubscription;
  Future<void>? _eventWork;
  Future<void>? _startFuture;
  Future<void>? _closeFuture;
  Future<void> _inputTail = Future<void>.value();
  _FrameCaptureRequest? _replacementCapture;
  _FrameCaptureKey? _lastCaptureKey;
  _AccessibilityCaptureRequest? _replacementAccessibilityCapture;
  _FrameCaptureKey? _lastAccessibilityKey;
  int? _activeContextId;
  int? _lastEventSequence;
  int _viewportWidth = 0;
  int _viewportHeight = 0;
  int _captureGeneration = 0;
  int _accessibilityGeneration = 0;
  int _projectionGeneration = 0;
  int? _frameProjectionGeneration;
  int _inputGeneration = 0;
  int _pendingInputEvents = 0;
  String? _errorMessage;
  BrowserFrame? _frame;
  BrowserAccessibilitySnapshot? _accessibility;
  _PendingAccessibility? _pendingAccessibility;
  InputDispatchedResponse? _lastInputResult;
  bool _captureInFlight = false;
  bool _accessibilityCaptureInFlight = false;
  bool _isStarting = false;
  bool _isReady = false;
  bool _disposed = false;

  UnmodifiableListView<BrowsingContextState> get contexts =>
      UnmodifiableListView(_contexts);
  int? get activeContextId => _activeContextId;
  BrowsingContextState? get selectedContext {
    final active = _activeContextId;
    if (active == null) return null;
    for (final context in _contexts) {
      if (context.contextId == active) return context;
    }
    return null;
  }

  bool get isStarting => _isStarting;
  bool get isReady => _isReady;
  String? get errorMessage => _errorMessage;
  BrowserFrame? get frame => _frame;
  BrowserAccessibilitySnapshot? get accessibility => _accessibility;
  InputDispatchedResponse? get lastInputResult => _lastInputResult;
  int? get lastEventSequence => _lastEventSequence;
  String get selectedStatus {
    final selected = selectedContext;
    if (selected == null) return _isStarting ? 'Starting Vixen...' : 'No tab';
    return _statusByContext[selected.contextId] ??
        (selected.isLoading ? 'Loading...' : selected.url);
  }

  Future<void> start() => _startFuture ??= _start();

  Future<void> _start() async {
    if (_closeFuture != null) return;
    _isStarting = true;
    _notify();
    try {
      await controller.start();
      _eventSubscription = controller.events.listen(
        _queueEvent,
        onError: _queueStreamError,
      );

      final session = await controller.loadProfileSession();
      var snapshot = await controller.browserSnapshot();
      _replaceFromSnapshot(snapshot);
      if (snapshot.contexts.isEmpty) {
        final urls = session.tabs.isEmpty
            ? const [vixenStartUrl]
            : session.tabs;
        final created = <int>[];
        for (final url in urls) {
          final contextId = await controller.createContext();
          created.add(contextId);
          await controller.navigate(contextId, url);
        }
        final activeIndex = session.activeIndex.clamp(0, created.length - 1);
        await controller.activateContext(created[activeIndex]);
        snapshot = await controller.browserSnapshot();
        _replaceFromSnapshot(snapshot);
      }
      _isReady = true;
      _scheduleFrameCapture();
    } catch (error) {
      _showError('Unable to start browser', error);
    } finally {
      _isStarting = false;
      _notify();
    }
  }

  Future<void> newTab({String url = vixenStartUrl}) => _runAction(() async {
    final contextId = await controller.createContext();
    final navigationId = await controller.navigate(contextId, url);
    await controller.activateContext(contextId);
    _pendingNavigations[contextId] = navigationId;
    _statusByContext[contextId] = 'Loading...';
  });

  Future<void> closeTab(int contextId) => _runAction(() async {
    final soleTab =
        _contexts.length == 1 && _contexts.first.contextId == contextId;
    if (soleTab) {
      final navigationId = await controller.navigate(contextId, vixenStartUrl);
      _pendingNavigations[contextId] = navigationId;
      _statusByContext[contextId] = 'Loading...';
    } else {
      await controller.closeContext(contextId);
    }
  });

  Future<void> activateTab(int contextId) =>
      _runAction(() => controller.activateContext(contextId));

  Future<void> navigate(String address) => _withSelected((context) async {
    final url = normalizeAddress(address);
    final navigationId = await controller.navigate(context.contextId, url);
    _pendingNavigations[context.contextId] = navigationId;
    _statusByContext[context.contextId] = 'Loading $url';
    _clearFrame();
  });

  Future<void> reload() => _withSelected((context) async {
    final navigationId = await controller.reload(context.contextId);
    _pendingNavigations[context.contextId] = navigationId;
    _statusByContext[context.contextId] = 'Reloading...';
    _clearFrame();
  });

  Future<void> stop() => _withSelected((context) async {
    await controller.stop(context.contextId);
    _pendingNavigations.remove(context.contextId);
    _statusByContext[context.contextId] = 'Stopped';
  });

  Future<void> goBack() => _traverse(-1);
  Future<void> goForward() => _traverse(1);

  Future<void> _traverse(int delta) => _withSelected((context) async {
    final navigationId = await controller.traverseHistory(
      context.contextId,
      delta,
    );
    if (navigationId != null) {
      _pendingNavigations[context.contextId] = navigationId;
      _statusByContext[context.contextId] = 'Loading history...';
      _clearFrame();
    }
  });

  void updatePhysicalViewport(int width, int height) {
    final bounded = boundFrameViewport(width, height);
    if (_viewportWidth == bounded.width && _viewportHeight == bounded.height) {
      return;
    }
    _viewportWidth = bounded.width;
    _viewportHeight = bounded.height;
    _scheduleFrameCapture();
  }

  Future<void> dispatchMouseEvent(String eventType, BrowserMouseEvent event) =>
      _enqueueInput((generation) async {
        _lastInputResult = await controller.dispatchMouseEvent(
          contextId: generation.contextId,
          documentId: generation.documentId,
          runtimeContextId: generation.runtimeContextId,
          viewportWidth: generation.viewportWidth,
          viewportHeight: generation.viewportHeight,
          eventType: eventType,
          event: event,
        );
      });

  Future<void> dispatchKeyEvent(String eventType, BrowserKeyEvent event) =>
      _enqueueInput((generation) async {
        _lastInputResult = await controller.dispatchKeyEvent(
          contextId: generation.contextId,
          documentId: generation.documentId,
          runtimeContextId: generation.runtimeContextId,
          viewportWidth: generation.viewportWidth,
          viewportHeight: generation.viewportHeight,
          eventType: eventType,
          event: event,
        );
      });

  Future<void> dispatchSemanticTap(
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node,
  ) async {
    final bounds = node.bounds;
    if (!_isCurrentSemanticAction(snapshot, node, 'tap') ||
        bounds == null ||
        node.disabled) {
      return;
    }
    final x = bounds.x + bounds.width / 2;
    final y = bounds.y + bounds.height / 2;
    await dispatchMouseEvent(
      'mousedown',
      BrowserMouseEvent(x: x, y: y, button: 0, buttons: 1, detail: 1),
    );
    await dispatchMouseEvent(
      'mouseup',
      BrowserMouseEvent(x: x, y: y, button: 0, buttons: 0, detail: 1),
    );
  }

  Future<void> dispatchSemanticFocus(
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node,
  ) {
    if (!_isCurrentSemanticAction(snapshot, node, 'focus') || node.disabled) {
      return Future<void>.value();
    }
    return _enqueueInput((inputGeneration) async {
      if (!_isCurrentSemanticAction(snapshot, node, 'focus')) return;
      _lastInputResult = await controller.dispatchAccessibilityFocus(
        contextId: inputGeneration.contextId,
        documentId: inputGeneration.documentId,
        runtimeContextId: inputGeneration.runtimeContextId,
        viewportWidth: inputGeneration.viewportWidth,
        viewportHeight: inputGeneration.viewportHeight,
        sourceGeneration: snapshot.sourceGeneration,
        generation: snapshot.generation,
        nodeId: node.id,
      );
    });
  }

  Future<void> dispatchSemanticSetValue(
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node,
    String value,
  ) {
    if (!_isCurrentSemanticAction(snapshot, node, 'set_value') ||
        node.disabled) {
      return Future<void>.value();
    }
    return _enqueueInput((inputGeneration) async {
      if (!_isCurrentSemanticAction(snapshot, node, 'set_value')) return;
      _lastInputResult = await controller.dispatchAccessibilitySetValue(
        contextId: inputGeneration.contextId,
        documentId: inputGeneration.documentId,
        runtimeContextId: inputGeneration.runtimeContextId,
        viewportWidth: inputGeneration.viewportWidth,
        viewportHeight: inputGeneration.viewportHeight,
        sourceGeneration: snapshot.sourceGeneration,
        generation: snapshot.generation,
        nodeId: node.id,
        value: value,
      );
    });
  }

  bool _isCurrentSemanticAction(
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node,
    String action,
  ) {
    final current = _accessibility;
    final selected = selectedContext;
    return current != null &&
        selected != null &&
        current.contextId == snapshot.contextId &&
        current.documentId == snapshot.documentId &&
        current.sourceGeneration == snapshot.sourceGeneration &&
        current.generation == snapshot.generation &&
        current.viewportWidth == snapshot.viewportWidth &&
        current.viewportHeight == snapshot.viewportHeight &&
        snapshot.contextId == selected.contextId &&
        snapshot.documentId == selected.documentId &&
        snapshot.viewportWidth == _viewportWidth &&
        snapshot.viewportHeight == _viewportHeight &&
        node.actions.contains(action) &&
        current.nodes.any(
          (candidate) =>
              candidate.id == node.id && candidate.actions.contains(action),
        );
  }

  Future<void> _enqueueInput(
    Future<void> Function(_InputGeneration generation) operation,
  ) {
    final selected = selectedContext;
    final runtimeContextId = selected?.runtimeContextId;
    if (_closeFuture != null ||
        !_isReady ||
        selected == null ||
        selected.isLoading ||
        runtimeContextId == null ||
        _viewportWidth <= 0 ||
        _viewportHeight <= 0) {
      return Future<void>.value();
    }
    if (_pendingInputEvents >= maxPendingInputEvents) {
      _showError(
        'Unable to dispatch browser input',
        'bounded input queue is full',
      );
      return Future<void>.value();
    }
    final generation = _InputGeneration(
      epoch: _inputGeneration,
      contextId: selected.contextId,
      documentId: selected.documentId,
      runtimeContextId: runtimeContextId,
      viewportWidth: _viewportWidth,
      viewportHeight: _viewportHeight,
    );
    final completed = Completer<void>();
    _pendingInputEvents++;
    _inputTail = _inputTail.then((_) async {
      try {
        if (_isCurrentInputGeneration(generation)) {
          await operation(generation);
          if (_isCurrentInputGeneration(generation)) {
            _scheduleFrameCapture(force: true);
            _notify();
          }
        }
      } catch (error) {
        if (_isCurrentInputGeneration(generation)) {
          _showError('Unable to dispatch browser input', error);
        }
      } finally {
        _pendingInputEvents--;
        if (!completed.isCompleted) completed.complete();
      }
    });
    return completed.future;
  }

  bool _isCurrentInputGeneration(_InputGeneration generation) {
    final selected = selectedContext;
    return _closeFuture == null &&
        generation.epoch == _inputGeneration &&
        selected != null &&
        !selected.isLoading &&
        selected.contextId == generation.contextId &&
        selected.documentId == generation.documentId &&
        selected.runtimeContextId == generation.runtimeContextId &&
        _viewportWidth == generation.viewportWidth &&
        _viewportHeight == generation.viewportHeight;
  }

  Future<void> _withSelected(
    Future<void> Function(BrowsingContextState context) action,
  ) {
    final context = selectedContext;
    if (context == null) return Future.value();
    return _runAction(() => action(context));
  }

  Future<void> _runAction(Future<void> Function() action) async {
    if (_closeFuture != null) return;
    try {
      await action();
    } catch (error) {
      _showError('Browser command failed', error);
    }
    _notify();
  }

  void clearError() {
    if (_errorMessage == null) return;
    _errorMessage = null;
    _notify();
  }

  void _queueEvent(SequencedBrowserEvent envelope) {
    _eventWork = (_eventWork ?? Future.value()).then(
      (_) => _consumeEvent(envelope),
    );
  }

  void _queueStreamError(Object error, StackTrace stackTrace) {
    _eventWork = (_eventWork ?? Future.value()).then((_) async {
      if (error is BrowserFailure && error.code == 'browser.event-lagged') {
        await _reconcile();
      } else {
        _showError('Browser event stream failed', error);
      }
    });
  }

  Future<void> _consumeEvent(SequencedBrowserEvent envelope) async {
    if (_closeFuture != null) return;
    final expected = (_lastEventSequence ?? 0) + 1;
    if (envelope.sequence != expected) {
      _lastEventSequence = envelope.sequence;
      await _reconcile();
      return;
    }
    _lastEventSequence = envelope.sequence;
    _reduce(envelope.event);
    _scheduleFrameCapture();
    _notify();
  }

  Future<void> _reconcile() async {
    try {
      _replaceFromSnapshot(await controller.browserSnapshot());
      _pendingNavigations.clear();
      _statusByContext.clear();
      _scheduleFrameCapture();
      _notify();
    } catch (error) {
      _showError('Unable to reconcile browser state', error);
    }
  }

  void _reduce(BrowserEvent event) {
    switch (event.type) {
      case 'browsing_context_created':
        final state = event.state!;
        _closedContextIds.remove(state.contextId);
        _upsertContext(state);
      case 'browsing_context_closed':
        final contextId = event.contextId!;
        _closedContextIds.add(contextId);
        _contexts.removeWhere((state) => state.contextId == contextId);
        _pendingNavigations.remove(contextId);
        _statusByContext.remove(contextId);
        if (_activeContextId == contextId) {
          _activeContextId = null;
          _clearFrame();
        }
      case 'active_browsing_context_changed':
        final contextId = event.contextId;
        if (contextId == null || _hasContext(contextId)) {
          if (_activeContextId != contextId) _clearFrame();
          _activeContextId = contextId;
        }
      case 'browsing_context_state_changed':
        final state = event.state!;
        if (!_closedContextIds.contains(state.contextId) &&
            _hasContext(state.contextId)) {
          final previous = _contextById(state.contextId);
          _upsertContext(state);
          if (state.contextId == _activeContextId &&
              (previous?.documentId != state.documentId ||
                  previous?.isLoading == false && state.isLoading)) {
            _clearFrame();
          }
          if (!state.isLoading) _pendingNavigations.remove(state.contextId);
        }
      case 'navigation_requested':
      case 'navigation_started':
        if (event.contextId == _activeContextId) _clearFrame();
        _setNavigationStatus(event, 'Loading...');
      case 'navigation_redirected':
        _setNavigationStatus(event, 'Redirecting...');
      case 'navigation_committed':
        _setNavigationStatus(event, 'Rendering...');
      case 'dom_content_loaded':
        _setNavigationStatus(event, 'Loading resources...');
      case 'document_load_completed':
        _setNavigationStatus(event, 'Finishing...');
      case 'navigation_phase_changed':
        final phase = event.phase!;
        if (_isTerminalPhase(phase)) {
          _finishNavigation(event);
        } else {
          _setNavigationStatus(event, _phaseLabel(phase));
        }
      case 'navigation_cancelled':
        _finishNavigation(event, status: 'Navigation cancelled');
      case 'navigation_failed':
        final contextId = event.contextId;
        if (contextId != null && _hasContext(contextId)) {
          _finishNavigation(event, status: 'Navigation failed');
          final error = event.error;
          _errorMessage = error == null
              ? 'Navigation failed'
              : '${error.code}: ${error.message}';
        }
      default:
        break;
    }
  }

  void _setNavigationStatus(BrowserEvent event, String status) {
    final contextId = event.contextId;
    if (contextId == null || !_hasContext(contextId)) return;
    final navigationId = event.navigationId;
    if (navigationId != null) _pendingNavigations[contextId] = navigationId;
    _statusByContext[contextId] = status;
  }

  void _finishNavigation(BrowserEvent event, {String status = 'Done'}) {
    final contextId = event.contextId;
    if (contextId == null || !_hasContext(contextId)) return;
    final pending = _pendingNavigations[contextId];
    if (pending != null &&
        event.navigationId != null &&
        pending != event.navigationId) {
      return;
    }
    _pendingNavigations.remove(contextId);
    _statusByContext[contextId] = status;
  }

  void _replaceFromSnapshot(BrowserSnapshot snapshot) {
    final previous = selectedContext;
    _contexts
      ..clear()
      ..addAll(snapshot.contexts);
    _activeContextId = snapshot.activeContextId;
    final selected = selectedContext;
    if (previous?.contextId != selected?.contextId ||
        previous?.documentId != selected?.documentId) {
      _clearFrame();
    }
    _closedContextIds.removeAll(
      snapshot.contexts.map((context) => context.contextId),
    );
  }

  void _upsertContext(BrowsingContextState state) {
    final index = _contexts.indexWhere(
      (context) => context.contextId == state.contextId,
    );
    if (index < 0) {
      _contexts.add(state);
      _contexts.sort((a, b) => a.contextId.compareTo(b.contextId));
    } else {
      _contexts[index] = state;
    }
  }

  bool _hasContext(int contextId) =>
      _contexts.any((context) => context.contextId == contextId);

  BrowsingContextState? _contextById(int contextId) {
    for (final context in _contexts) {
      if (context.contextId == contextId) return context;
    }
    return null;
  }

  void _scheduleFrameCapture({bool force = false}) {
    if (_closeFuture != null || !_isReady) return;
    final selected = selectedContext;
    if (selected == null ||
        selected.isLoading ||
        _pendingNavigations.containsKey(selected.contextId) ||
        _viewportWidth <= 0 ||
        _viewportHeight <= 0) {
      return;
    }
    final key = _FrameCaptureKey(
      contextId: selected.contextId,
      documentId: selected.documentId,
      width: _viewportWidth,
      height: _viewportHeight,
    );
    if (!force && key == _lastCaptureKey && key == _lastAccessibilityKey) {
      return;
    }
    final projectionGeneration = ++_projectionGeneration;
    _accessibility = null;
    _pendingAccessibility = null;
    _frameProjectionGeneration = null;
    _scheduleAccessibilityCapture(
      key,
      projectionGeneration: projectionGeneration,
    );
    _lastCaptureKey = key;
    final request = _FrameCaptureRequest(
      ++_captureGeneration,
      projectionGeneration,
      key,
    );
    if (_captureInFlight) {
      _replacementCapture = request;
      return;
    }
    unawaited(_captureFrame(request));
  }

  Future<void> _captureFrame(_FrameCaptureRequest request) async {
    _captureInFlight = true;
    try {
      final key = request.key;
      final captured = await controller.captureFrame(
        contextId: key.contextId,
        documentId: key.documentId,
        width: key.width,
        height: key.height,
      );
      if (captured != null &&
          request.generation == _captureGeneration &&
          _isCurrentCapture(key) &&
          captured.contextId == key.contextId &&
          captured.documentId == key.documentId &&
          captured.width == key.width &&
          captured.height == key.height &&
          (_frame == null ||
              _frame!.contextId != captured.contextId ||
              _frame!.documentId != captured.documentId ||
              captured.frameId > _frame!.frameId)) {
        _frame = captured;
        _frameProjectionGeneration = request.projectionGeneration;
        _publishAccessibilityIfPaired();
        _notify();
      }
    } catch (error) {
      if (request.generation == _captureGeneration &&
          _isCurrentCapture(request.key)) {
        _showError('Unable to capture browser frame', error);
      }
    } finally {
      _captureInFlight = false;
      final replacement = _replacementCapture;
      _replacementCapture = null;
      if (replacement != null &&
          _closeFuture == null &&
          replacement.generation == _captureGeneration &&
          _isCurrentCapture(replacement.key)) {
        unawaited(_captureFrame(replacement));
      }
    }
  }

  void _scheduleAccessibilityCapture(
    _FrameCaptureKey key, {
    required int projectionGeneration,
  }) {
    _lastAccessibilityKey = key;
    final request = _AccessibilityCaptureRequest(
      ++_accessibilityGeneration,
      projectionGeneration,
      key,
    );
    if (_accessibilityCaptureInFlight) {
      _replacementAccessibilityCapture = request;
      return;
    }
    unawaited(_captureAccessibility(request));
  }

  Future<void> _captureAccessibility(
    _AccessibilityCaptureRequest request,
  ) async {
    _accessibilityCaptureInFlight = true;
    try {
      final key = request.key;
      final snapshot = await controller.accessibilitySnapshot(
        contextId: key.contextId,
        documentId: key.documentId,
        viewportWidth: key.width,
        viewportHeight: key.height,
      );
      if (request.generation == _accessibilityGeneration &&
          _isCurrentCapture(key) &&
          snapshot.contextId == key.contextId &&
          snapshot.documentId == key.documentId &&
          snapshot.viewportWidth == key.width &&
          snapshot.viewportHeight == key.height) {
        _pendingAccessibility = _PendingAccessibility(
          request.projectionGeneration,
          snapshot,
        );
        _publishAccessibilityIfPaired();
      }
    } catch (error) {
      if (request.generation == _accessibilityGeneration &&
          _isCurrentCapture(request.key)) {
        _showError('Unable to capture browser accessibility', error);
      }
    } finally {
      _accessibilityCaptureInFlight = false;
      final replacement = _replacementAccessibilityCapture;
      _replacementAccessibilityCapture = null;
      if (replacement != null &&
          _closeFuture == null &&
          replacement.generation == _accessibilityGeneration &&
          _isCurrentCapture(replacement.key)) {
        unawaited(_captureAccessibility(replacement));
      }
    }
  }

  void _publishAccessibilityIfPaired() {
    final pending = _pendingAccessibility;
    final frame = _frame;
    if (pending == null ||
        frame == null ||
        pending.projectionGeneration != _frameProjectionGeneration ||
        frame.contextId != pending.snapshot.contextId ||
        frame.documentId != pending.snapshot.documentId ||
        frame.width != pending.snapshot.viewportWidth ||
        frame.height != pending.snapshot.viewportHeight) {
      return;
    }
    _accessibility = pending.snapshot;
    _pendingAccessibility = null;
    _notify();
  }

  bool _isCurrentCapture(_FrameCaptureKey key) {
    final selected = selectedContext;
    return selected != null &&
        !selected.isLoading &&
        selected.contextId == key.contextId &&
        selected.documentId == key.documentId &&
        _viewportWidth == key.width &&
        _viewportHeight == key.height;
  }

  void _clearFrame() {
    _frame = null;
    _lastCaptureKey = null;
    _replacementCapture = null;
    _captureGeneration++;
    _frameProjectionGeneration = null;
    _projectionGeneration++;
    _accessibility = null;
    _pendingAccessibility = null;
    _lastAccessibilityKey = null;
    _replacementAccessibilityCapture = null;
    _accessibilityGeneration++;
    _inputGeneration++;
    _lastInputResult = null;
  }

  void _showError(String prefix, Object error) {
    _errorMessage = '$prefix: $error';
    _notify();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  Future<void> close() => _closeFuture ??= _close();

  Future<void> _close() async {
    _clearFrame();
    _notify();
    await _eventSubscription?.cancel();
    try {
      if (_eventWork case final eventWork?) await eventWork;
      if (_startFuture != null) await _startFuture;
      await _inputTail;
      await controller.saveCurrentProfileSession();
    } catch (_) {
      // Shutdown still has to release the sole native browser owner.
    } finally {
      await controller.shutdown();
    }
  }

  @override
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    unawaited(close());
    super.dispose();
  }
}

({int width, int height}) boundFrameViewport(int width, int height) {
  if (width <= 0 || height <= 0) return (width: 0, height: 0);
  final boundedWidth = width.clamp(1, browserMaxFrameDimension);
  final boundedHeight = height.clamp(1, browserMaxFrameDimension);
  if (boundedWidth * boundedHeight * 4 > browserMaxFrameBytes) {
    return (width: 0, height: 0);
  }
  return (width: boundedWidth, height: boundedHeight);
}

final class _FrameCaptureKey {
  const _FrameCaptureKey({
    required this.contextId,
    required this.documentId,
    required this.width,
    required this.height,
  });

  final int contextId;
  final int documentId;
  final int width;
  final int height;

  @override
  bool operator ==(Object other) =>
      other is _FrameCaptureKey &&
      contextId == other.contextId &&
      documentId == other.documentId &&
      width == other.width &&
      height == other.height;

  @override
  int get hashCode => Object.hash(contextId, documentId, width, height);
}

final class _FrameCaptureRequest {
  const _FrameCaptureRequest(
    this.generation,
    this.projectionGeneration,
    this.key,
  );

  final int generation;
  final int projectionGeneration;
  final _FrameCaptureKey key;
}

final class _AccessibilityCaptureRequest {
  const _AccessibilityCaptureRequest(
    this.generation,
    this.projectionGeneration,
    this.key,
  );

  final int generation;
  final int projectionGeneration;
  final _FrameCaptureKey key;
}

final class _PendingAccessibility {
  const _PendingAccessibility(this.projectionGeneration, this.snapshot);

  final int projectionGeneration;
  final BrowserAccessibilitySnapshot snapshot;
}

final class _InputGeneration {
  const _InputGeneration({
    required this.epoch,
    required this.contextId,
    required this.documentId,
    required this.runtimeContextId,
    required this.viewportWidth,
    required this.viewportHeight,
  });

  final int epoch;
  final int contextId;
  final int documentId;
  final int runtimeContextId;
  final int viewportWidth;
  final int viewportHeight;
}

bool _isTerminalPhase(String phase) =>
    phase == 'settled' || phase == 'failed' || phase == 'cancelled';

String _phaseLabel(String phase) => switch (phase) {
  'intent' || 'policy' => 'Preparing...',
  'request' => 'Requesting...',
  'response' => 'Receiving...',
  'commit' || 'parse' => 'Rendering...',
  'scripts_and_subresources' => 'Loading resources...',
  'dom_content_loaded' || 'load' => 'Finishing...',
  _ => 'Loading...',
};
