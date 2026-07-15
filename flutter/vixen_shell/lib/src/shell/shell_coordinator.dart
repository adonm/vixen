import 'dart:async';
import 'dart:collection';

import 'package:flutter/foundation.dart';

import '../bridge/browser_controller.dart';
import '../bridge/browser_models.dart';
import '../bridge/native/native_renderer_protocol.dart';
import '../bridge/render_models.dart';
import '../bridge/renderer_transport.dart';
import '../renderer/formatter.dart';
import '../renderer/renderer_broker_service.dart';
import 'address.dart';

final class ShellCoordinator extends ChangeNotifier {
  static const int maxPendingInputEvents = 64;
  static const int maxCaptureRetries = 2;
  static const int maxRendererPresentationRetries = 2;

  ShellCoordinator(this.controller, {this.initialUrl = vixenStartUrl}) {
    if (controller case final RendererTransport transport
        when transport.rendererUpdatesEnabled) {
      _rendererService = RendererBrokerService(
        transport: transport,
        formatter: _formatter,
      );
    }
  }

  final BrowserController controller;
  final String initialUrl;
  final VixenFormatter _formatter = VixenFormatter();
  RendererBrokerService? _rendererService;
  final List<BrowsingContextState> _contexts = [];
  final Set<int> _closedContextIds = {};
  final Map<int, int> _pendingNavigations = {};
  final Map<int, String> _statusByContext = {};

  StreamSubscription<SequencedBrowserEvent>? _eventSubscription;
  Future<void>? _eventWork;
  Future<void>? _startFuture;
  Future<void>? _closeFuture;
  Future<void> _inputTail = Future<void>.value();
  Future<void> _hostViewTail = Future<void>.value();
  Future<void> _rendererTail = Future<void>.value();
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
  int _inputGeneration = 0;
  int _hostViewGeneration = 0;
  int _rendererViewportGeneration = 0;
  int _rendererLifecycleGeneration = 0;
  int _nextRendererQueryId = 1;
  int _rendererPresentationFailures = 0;
  int _pendingInputEvents = 0;
  int _findRequestGeneration = 0;
  int _frameCaptureFailures = 0;
  int _accessibilityCaptureFailures = 0;
  String _findQuery = '';
  bool _findCaseSensitive = false;
  int? _findMatches;
  int? _findActiveMatch;
  FormatterFindResult? _rendererFindResult;
  String? _errorMessage;
  BrowserFrame? _frame;
  BrowserAccessibilitySnapshot? _accessibility;
  FormatterCommitView? _rendererView;
  _PendingAccessibility? _pendingAccessibility;
  _PendingFrame? _pendingFrame;
  InputDispatchedResponse? _lastInputResult;
  bool _captureInFlight = false;
  bool _accessibilityCaptureInFlight = false;
  bool _contentFocused = false;
  double _scaleFactor = 1;
  BrowserHostLifecycle _hostLifecycle = BrowserHostLifecycle.resumed;
  _HostViewKey? _lastHostViewKey;
  _FrameCaptureKey? _frameCaptureFailureKey;
  _FrameCaptureKey? _accessibilityCaptureFailureKey;
  bool _isStarting = false;
  bool _isReady = false;
  bool _disposed = false;
  _FrameCaptureKey? _lastRendererKey;
  int? _pendingRendererPresentationId;
  _RendererSnapshotRequest? _pendingRendererSnapshot;
  bool _rendererSnapshotScheduled = false;

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
  FormatterCommitView? get rendererView => _rendererView;
  InputDispatchedResponse? get lastInputResult => _lastInputResult;
  int? get lastEventSequence => _lastEventSequence;
  String get findQuery => _findQuery;
  int? get findMatches => _findMatches;
  int? get findActiveMatch => _findActiveMatch;
  FormatterFindResult? get rendererFindResult => _rendererFindResult;
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
        final urls = session.tabs.isEmpty ? [initialUrl] : session.tabs;
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

  Future<void> zoomIn() => _changeZoom(1);
  Future<void> zoomOut() => _changeZoom(-1);
  Future<void> resetZoom() => setPageZoom(1);

  Future<void> _changeZoom(int direction) {
    const levels = <double>[
      0.25,
      0.5,
      0.67,
      0.8,
      0.9,
      1,
      1.1,
      1.25,
      1.5,
      1.75,
      2,
      2.5,
      3,
      4,
      5,
    ];
    final current = selectedContext?.pageZoom ?? 1;
    final next = direction > 0
        ? levels.firstWhere(
            (level) => level > current,
            orElse: () => levels.last,
          )
        : levels.lastWhere(
            (level) => level < current,
            orElse: () => levels.first,
          );
    return setPageZoom(next);
  }

  Future<void> setPageZoom(double zoom) => _withSelected((context) async {
    final state = await controller.setPageZoom(context.contextId, zoom);
    final selected = selectedContext;
    if (selected?.contextId != state.contextId ||
        selected?.documentId != state.documentId) {
      return;
    }
    _upsertContext(state);
    _statusByContext[state.contextId] =
        'Zoom ${(state.pageZoom * 100).round()}%';
    _scheduleFrameCapture(force: true);
  });

  Future<void> findText(
    String query, {
    bool caseSensitive = false,
    bool forward = true,
  }) async {
    final selected = selectedContext;
    final generation = ++_findRequestGeneration;
    _findQuery = query;
    _findCaseSensitive = caseSensitive;
    if (selected == null || query.isEmpty) {
      _findMatches = query.isEmpty ? 0 : null;
      _findActiveMatch = null;
      _rendererFindResult = null;
      _notify();
      return;
    }
    _findMatches = null;
    _findActiveMatch = null;
    _rendererFindResult = null;
    _notify();
    try {
      final result = await controller.findText(
        contextId: selected.contextId,
        documentId: selected.documentId,
        query: query,
        caseSensitive: caseSensitive,
        forward: forward,
      );
      final current = selectedContext;
      if (generation == _findRequestGeneration &&
          current?.contextId == selected.contextId &&
          current?.documentId == selected.documentId &&
          _findQuery == query) {
        _findMatches = result.matches;
        _findActiveMatch = result.activeMatch;
        _refreshRendererFindGeometry();
        if (_rendererFindResult case final rendererResult?) {
          _findMatches = rendererResult.matches.length;
          if (rendererResult.matches.isEmpty) _findActiveMatch = null;
        }
        if (result.activeMatch != null) {
          _scheduleFrameCapture(force: true);
        }
        _notify();
      }
    } catch (error) {
      if (generation == _findRequestGeneration && _findQuery == query) {
        _showError('Unable to find text', error);
      }
    }
  }

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

  void updatePhysicalViewport(int width, int height, [double scaleFactor = 1]) {
    final bounded = boundFrameViewport(width, height);
    if (_viewportWidth == bounded.width &&
        _viewportHeight == bounded.height &&
        _scaleFactor == scaleFactor) {
      return;
    }
    _viewportWidth = bounded.width;
    _viewportHeight = bounded.height;
    _scaleFactor = scaleFactor;
    _scheduleHostViewUpdate();
    _scheduleFrameCapture();
  }

  void updateContentFocus(bool focused) {
    if (_contentFocused == focused) return;
    _contentFocused = focused;
    _scheduleHostViewUpdate();
  }

  void updateApplicationLifecycle(BrowserHostLifecycle lifecycle) {
    if (_hostLifecycle == lifecycle) return;
    _hostLifecycle = lifecycle;
    _rendererLifecycleGeneration++;
    if (!_hostViewVisible) {
      _rendererView = null;
      _rendererFindResult = null;
      _pendingRendererSnapshot = null;
      _pendingRendererPresentationId = null;
      _notify();
    }
    _scheduleHostViewUpdate();
    if (_hostViewVisible) _scheduleFrameCapture(force: true);
  }

  bool get _hostViewVisible =>
      _hostLifecycle == BrowserHostLifecycle.resumed ||
      _hostLifecycle == BrowserHostLifecycle.inactive;

  bool get _hostAcceptsInput =>
      _contentFocused && _hostLifecycle == BrowserHostLifecycle.resumed;

  void _scheduleHostViewUpdate() {
    if (_closeFuture != null ||
        !_isReady ||
        _viewportWidth <= 0 ||
        _viewportHeight <= 0) {
      return;
    }
    final selected = selectedContext;
    if (selected == null) return;
    final key = _HostViewKey(
      contextId: selected.contextId,
      width: _viewportWidth,
      height: _viewportHeight,
      scaleFactor: _scaleFactor,
      focused: _hostAcceptsInput,
      visible: _hostViewVisible,
      lifecycle: _hostLifecycle,
    );
    if (key == _lastHostViewKey) return;
    _lastHostViewKey = key;
    final generation = ++_hostViewGeneration;
    _inputGeneration++;
    _hostViewTail = _hostViewTail.then((_) async {
      try {
        _lastInputResult = await controller.updateHostViewState(
          contextId: key.contextId,
          generation: generation,
          viewportWidth: key.width,
          viewportHeight: key.height,
          scaleFactor: key.scaleFactor,
          focused: key.focused,
          visible: key.visible,
          lifecycle: key.lifecycle,
        );
      } catch (error) {
        if (_lastHostViewKey == key) {
          _showError('Unable to update browser host view', error);
        }
      }
    });
  }

  Future<void> dispatchMouseEvent(String eventType, BrowserMouseEvent event) =>
      _enqueueInput((generation) async {
        final rendererView = _rendererView;
        if (rendererView != null) {
          if (_rendererSnapshotScheduled || _pendingRendererSnapshot != null) {
            return;
          }
          final displayed = _formatter.displayedView;
          if (!identical(rendererView, displayed) ||
              rendererView.isRetired ||
              rendererView.commit.viewport.width != generation.viewportWidth ||
              rendererView.commit.viewport.height !=
                  generation.viewportHeight) {
            return;
          }
          final query = RenderHitTestQuery(
            queryId: _takeRendererQueryId(),
            contextId: generation.contextId,
            documentId: generation.documentId,
            displayedCommitId: rendererView.commit.commitId,
            revision: rendererView.commit.revision,
            handle: rendererView.commit.hitTestHandle,
            point: RenderPoint(event.x, event.y),
          );
          final target = rendererView.answerHitTest(query);
          if (eventType != 'mousemove') {
            debugPrint(
              'Vixen renderer input event=$eventType '
              'commit=${rendererView.commit.commitId} '
              'x=${event.x.toStringAsFixed(3)} y=${event.y.toStringAsFixed(3)} '
              'target=${target?.nodeId ?? "none"}',
            );
          }
          _lastInputResult = await controller.dispatchRendererMouseEvent(
            contextId: generation.contextId,
            documentId: generation.documentId,
            runtimeContextId: generation.runtimeContextId,
            viewportWidth: generation.viewportWidth,
            viewportHeight: generation.viewportHeight,
            eventType: eventType,
            event: event,
            query: query,
            target: target,
          );
          return;
        }
        _lastInputResult = await controller.dispatchMouseEvent(
          contextId: generation.contextId,
          documentId: generation.documentId,
          runtimeContextId: generation.runtimeContextId,
          viewportWidth: generation.viewportWidth,
          viewportHeight: generation.viewportHeight,
          eventType: eventType,
          event: event,
        );
      }, scheduleCapture: eventType != 'mousedown');

  int _takeRendererQueryId() {
    if (_nextRendererQueryId > 0x7fffffffffffffff) {
      throw const RenderProtocolException(
        'render.id-exhausted',
        'renderer hit-test query id exhausted',
      );
    }
    return _nextRendererQueryId++;
  }

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

  Future<void> dispatchTextInput(BrowserTextInputState state) =>
      _enqueueInput((generation) async {
        _lastInputResult = await controller.dispatchTextInput(
          contextId: generation.contextId,
          documentId: generation.documentId,
          runtimeContextId: generation.runtimeContextId,
          viewportWidth: generation.viewportWidth,
          viewportHeight: generation.viewportHeight,
          state: state,
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

  Future<void> dispatchRendererSemanticAction(
    FormatterCommitView view,
    RenderSemanticDescriptor descriptor,
    RenderSemanticActionKind action,
    String? value,
  ) async {
    final snapshot = _accessibility;
    if (snapshot == null ||
        !identical(view, _rendererView) ||
        !identical(view, _formatter.displayedView) ||
        view.isRetired ||
        view.commit.revision.contextId != snapshot.contextId ||
        view.commit.revision.documentId != snapshot.documentId ||
        view.commit.revision.sourceGeneration != snapshot.sourceGeneration ||
        descriptor.actionGeneration != snapshot.generation ||
        !descriptor.actions.contains(action)) {
      return;
    }
    BrowserAccessibilityNode? node;
    for (final candidate in snapshot.nodes) {
      if (candidate.id == descriptor.id) {
        node = candidate;
        break;
      }
    }
    if (node == null || node.disabled) return;
    final semanticNode = node;
    final actionName = switch (action) {
      RenderSemanticActionKind.activate => 'tap',
      RenderSemanticActionKind.focus => 'focus',
      RenderSemanticActionKind.setValue => 'set_value',
      RenderSemanticActionKind.increase => 'increase',
      RenderSemanticActionKind.decrease => 'decrease',
      _ => null,
    };
    if (actionName == null ||
        action == RenderSemanticActionKind.setValue && value == null ||
        !_isCurrentSemanticAction(snapshot, semanticNode, actionName)) {
      return;
    }
    if (action == RenderSemanticActionKind.activate) {
      FormatterSemanticRegion? region;
      for (final candidate in view.semanticRegions) {
        if (candidate.descriptor.id == descriptor.id) {
          region = candidate;
          break;
        }
      }
      if (region == null) return;
      final point = region.rect.center;
      await dispatchMouseEvent(
        'mousedown',
        BrowserMouseEvent(
          x: point.dx,
          y: point.dy,
          button: 0,
          buttons: 1,
          detail: 1,
        ),
      );
      await dispatchMouseEvent(
        'mouseup',
        BrowserMouseEvent(
          x: point.dx,
          y: point.dy,
          button: 0,
          buttons: 0,
          detail: 1,
        ),
      );
      return;
    }
    await _enqueueInput((inputGeneration) async {
      if (!_isCurrentRendererSemanticAction(
        view,
        snapshot,
        semanticNode,
        actionName,
      )) {
        return;
      }
      _lastInputResult = switch (action) {
        RenderSemanticActionKind.focus =>
          await controller.dispatchAccessibilityFocus(
            contextId: inputGeneration.contextId,
            documentId: inputGeneration.documentId,
            runtimeContextId: inputGeneration.runtimeContextId,
            viewportWidth: inputGeneration.viewportWidth,
            viewportHeight: inputGeneration.viewportHeight,
            sourceGeneration: snapshot.sourceGeneration,
            generation: snapshot.generation,
            nodeId: semanticNode.id,
          ),
        RenderSemanticActionKind.setValue when value != null =>
          await controller.dispatchAccessibilitySetValue(
            contextId: inputGeneration.contextId,
            documentId: inputGeneration.documentId,
            runtimeContextId: inputGeneration.runtimeContextId,
            viewportWidth: inputGeneration.viewportWidth,
            viewportHeight: inputGeneration.viewportHeight,
            sourceGeneration: snapshot.sourceGeneration,
            generation: snapshot.generation,
            nodeId: semanticNode.id,
            value: value,
          ),
        RenderSemanticActionKind.increase ||
        RenderSemanticActionKind.decrease =>
          await controller.dispatchAccessibilityAdjustment(
            contextId: inputGeneration.contextId,
            documentId: inputGeneration.documentId,
            runtimeContextId: inputGeneration.runtimeContextId,
            viewportWidth: inputGeneration.viewportWidth,
            viewportHeight: inputGeneration.viewportHeight,
            sourceGeneration: snapshot.sourceGeneration,
            generation: snapshot.generation,
            nodeId: semanticNode.id,
            increase: action == RenderSemanticActionKind.increase,
          ),
        _ => _lastInputResult,
      };
    });
  }

  bool _isCurrentRendererSemanticAction(
    FormatterCommitView view,
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node,
    String action,
  ) =>
      identical(view, _rendererView) &&
      identical(view, _formatter.displayedView) &&
      !view.isRetired &&
      _isCurrentSemanticAction(snapshot, node, action);

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

  Future<void> dispatchSemanticAdjustment(
    BrowserAccessibilitySnapshot snapshot,
    BrowserAccessibilityNode node, {
    required bool increase,
  }) {
    final action = increase ? 'increase' : 'decrease';
    if (!_isCurrentSemanticAction(snapshot, node, action) || node.disabled) {
      return Future<void>.value();
    }
    return _enqueueInput((inputGeneration) async {
      if (!_isCurrentSemanticAction(snapshot, node, action)) return;
      _lastInputResult = await controller.dispatchAccessibilityAdjustment(
        contextId: inputGeneration.contextId,
        documentId: inputGeneration.documentId,
        runtimeContextId: inputGeneration.runtimeContextId,
        viewportWidth: inputGeneration.viewportWidth,
        viewportHeight: inputGeneration.viewportHeight,
        sourceGeneration: snapshot.sourceGeneration,
        generation: snapshot.generation,
        nodeId: node.id,
        increase: increase,
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
    Future<void> Function(_InputGeneration generation) operation, {
    bool scheduleCapture = true,
  }) {
    final selected = selectedContext;
    final runtimeContextId = selected?.runtimeContextId;
    if (_closeFuture != null ||
        !_isReady ||
        selected == null ||
        selected.isLoading ||
        runtimeContextId == null ||
        !_hostAcceptsInput ||
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
        await _hostViewTail;
        if (_isCurrentInputGeneration(generation)) {
          await operation(generation);
          if (_isCurrentInputGeneration(generation)) {
            if (scheduleCapture) _scheduleFrameCapture(force: true);
            _notify();
          }
        }
      } catch (error) {
        if (_isCurrentInputGeneration(generation)) {
          debugPrint('Vixen renderer input failed: $error');
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
    final refreshRuntimeProjection =
        envelope.event.type == 'runtime_effects' &&
        envelope.event.contextId == _activeContextId;
    _scheduleFrameCapture(force: refreshRuntimeProjection);
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
    _scheduleHostViewUpdate();
    if (!_hostViewVisible) return;
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
    _scheduleRendererSnapshot(key, force: force);
    if (!force && key == _lastCaptureKey && key == _lastAccessibilityKey) {
      return;
    }
    final projectionGeneration = ++_projectionGeneration;
    _pendingAccessibility = null;
    _pendingFrame = null;
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

  void _scheduleRendererSnapshot(_FrameCaptureKey key, {required bool force}) {
    if (_rendererService == null || controller is! RendererTransport) return;
    if (!force && key == _lastRendererKey) return;
    if (force || key != _lastRendererKey) {
      _rendererViewportGeneration++;
    }
    _lastRendererKey = key;
    _pendingRendererSnapshot = _RendererSnapshotRequest(
      key: key,
      viewportGeneration: _rendererViewportGeneration,
      pageZoom: selectedContext?.pageZoom ?? 1,
      lifecycleGeneration: _rendererLifecycleGeneration,
    );
    _scheduleRendererSnapshotDrain();
  }

  void _scheduleRendererSnapshotDrain() {
    if (_rendererSnapshotScheduled) return;
    _rendererSnapshotScheduled = true;
    final operation = _rendererTail.then((_) async {
      final request = _pendingRendererSnapshot;
      _pendingRendererSnapshot = null;
      if (request != null) await _publishRendererSnapshot(request);
    });
    _rendererTail = operation.whenComplete(() {
      _rendererSnapshotScheduled = false;
      if (_pendingRendererSnapshot != null) {
        _scheduleRendererSnapshotDrain();
      }
    });
  }

  Future<void> _publishRendererSnapshot(
    _RendererSnapshotRequest request,
  ) async {
    final service = _rendererService;
    final key = request.key;
    if (service == null || !_isCurrentRendererRequest(request)) {
      return;
    }
    try {
      await controller.publishRendererSnapshot(
        contextId: key.contextId,
        documentId: key.documentId,
        viewportWidth: key.width,
        viewportHeight: key.height,
        viewportGeneration: request.viewportGeneration,
        pageZoom: request.pageZoom,
      );
      if (!_isCurrentRendererRequest(request)) return;
      await _drainRenderer(service);
      if (!_isCurrentRendererRequest(request)) return;
      final view = _formatter.acceptedView;
      if (view == null ||
          view.commit.revision.contextId != key.contextId ||
          view.commit.revision.documentId != key.documentId ||
          view.commit.revision.viewportGeneration !=
              request.viewportGeneration ||
          view.commit.viewport.width != key.width ||
          view.commit.viewport.height != key.height ||
          view.commit.viewport.pageZoom != request.pageZoom) {
        return;
      }
      await controller.flushRendererSubmissions();
      await _drainRenderer(service);
      if (!_isCurrentRendererRequest(request)) return;
      _rendererView = view;
      _notify();
    } catch (error) {
      try {
        await _drainRenderer(service);
      } catch (_) {
        // Preserve the original commit failure as the user-visible error.
      }
      if (_isCurrentRendererRequest(request)) {
        _showError('Unable to present Flutter renderer commit', error);
      }
    }
  }

  bool _isCurrentRendererRequest(_RendererSnapshotRequest request) =>
      _closeFuture == null &&
      _hostViewVisible &&
      request.lifecycleGeneration == _rendererLifecycleGeneration &&
      _isCurrentCapture(request.key);

  void rendererCommitPresented(FormatterCommitView view) {
    final service = _rendererService;
    final transport = controller is RendererTransport
        ? controller as RendererTransport
        : null;
    if (service == null ||
        transport == null ||
        !_hostViewVisible ||
        !identical(view, _rendererView) ||
        view.isRetired ||
        _formatter.displayedView?.commit.commitId == view.commit.commitId ||
        _pendingRendererPresentationId == view.commit.commitId) {
      return;
    }
    _pendingRendererPresentationId = view.commit.commitId;
    _rendererTail = _rendererTail.then((_) async {
      if (_closeFuture != null ||
          !identical(view, _rendererView) ||
          view.isRetired) {
        if (_pendingRendererPresentationId == view.commit.commitId) {
          _pendingRendererPresentationId = null;
        }
        return;
      }
      final presented = RenderPresented(
        contextId: view.commit.revision.contextId,
        documentId: view.commit.revision.documentId,
        commitId: view.commit.commitId,
        revision: view.commit.revision,
      );
      try {
        transport.submitRenderer(rendererPresentedSubmission(presented));
        await controller.flushRendererSubmissions();
        await _drainRenderer(service);
        if (!identical(view, _rendererView) || view.isRetired) return;
        _formatter.present(presented);
        final rootScroll = view.commit.scroll.isEmpty
            ? null
            : view.commit.scroll.first;
        debugPrint(
          'Vixen renderer presented context=${presented.contextId} '
          'document=${presented.documentId} commit=${presented.commitId} '
          'scroll_y=${rootScroll?.offsetY.toStringAsFixed(3) ?? "none"}',
        );
        _refreshRendererFindGeometry();
        _rendererPresentationFailures = 0;
      } catch (error) {
        try {
          await _drainRenderer(service);
        } catch (_) {
          // Preserve the original presentation failure as the visible error.
        }
        if (identical(view, _rendererView)) {
          _rendererView = _formatter.displayedView;
          _rendererPresentationFailures++;
          _showError('Unable to acknowledge Flutter renderer commit', error);
          if (_rendererPresentationFailures <= maxRendererPresentationRetries) {
            _scheduleFrameCapture(force: true);
          }
        }
      } finally {
        if (_pendingRendererPresentationId == view.commit.commitId) {
          _pendingRendererPresentationId = null;
        }
      }
    });
  }

  Future<void> _drainRenderer(RendererBrokerService service) async {
    for (var count = 0; count < renderBrokerQueueCapacity * 2; count++) {
      if (!await service.serviceNext()) return;
    }
    throw const RenderProtocolException(
      'render.queue-full',
      'renderer message drain exceeded its bounded work budget',
    );
  }

  void _refreshRendererFindGeometry() {
    final view = _formatter.displayedView;
    if (_findQuery.isEmpty ||
        view == null ||
        !identical(view, _rendererView) ||
        view.isRetired) {
      _rendererFindResult = null;
      return;
    }
    _rendererFindResult = view.findText(
      _findQuery,
      caseSensitive: _findCaseSensitive,
    );
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
        _clearFrameCaptureFailures(key);
        _pendingFrame = _PendingFrame(request.projectionGeneration, captured);
        _publishAccessibilityIfPaired();
      }
    } catch (error) {
      if (request.generation == _captureGeneration &&
          _isCurrentCapture(request.key)) {
        if (_shouldRetryFrameCapture(request.key)) {
          _replacementCapture = _FrameCaptureRequest(
            ++_captureGeneration,
            request.projectionGeneration,
            request.key,
          );
        } else {
          _showError('Unable to capture browser frame after recovery', error);
        }
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
        _clearAccessibilityCaptureFailures(key);
        _pendingAccessibility = _PendingAccessibility(
          request.projectionGeneration,
          snapshot,
        );
        _publishAccessibilityIfPaired();
      }
    } catch (error) {
      if (request.generation == _accessibilityGeneration &&
          _isCurrentCapture(request.key)) {
        if (_shouldRetryAccessibilityCapture(request.key)) {
          _replacementAccessibilityCapture = _AccessibilityCaptureRequest(
            ++_accessibilityGeneration,
            request.projectionGeneration,
            request.key,
          );
        } else {
          _showError(
            'Unable to capture browser accessibility after recovery',
            error,
          );
        }
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
    final pendingFrame = _pendingFrame;
    final frame = pendingFrame?.frame;
    if (pending == null ||
        frame == null ||
        pending.projectionGeneration != pendingFrame!.projectionGeneration ||
        frame.contextId != pending.snapshot.contextId ||
        frame.documentId != pending.snapshot.documentId ||
        frame.width != pending.snapshot.viewportWidth ||
        frame.height != pending.snapshot.viewportHeight) {
      return;
    }
    _frame = frame;
    _accessibility = pending.snapshot;
    _pendingFrame = null;
    _pendingAccessibility = null;
    _notify();
  }

  bool _shouldRetryFrameCapture(_FrameCaptureKey key) {
    if (_frameCaptureFailureKey != key) {
      _frameCaptureFailureKey = key;
      _frameCaptureFailures = 0;
    }
    _frameCaptureFailures++;
    return _frameCaptureFailures <= maxCaptureRetries;
  }

  void _clearFrameCaptureFailures(_FrameCaptureKey key) {
    if (_frameCaptureFailureKey != key) return;
    _frameCaptureFailureKey = null;
    _frameCaptureFailures = 0;
  }

  bool _shouldRetryAccessibilityCapture(_FrameCaptureKey key) {
    if (_accessibilityCaptureFailureKey != key) {
      _accessibilityCaptureFailureKey = key;
      _accessibilityCaptureFailures = 0;
    }
    _accessibilityCaptureFailures++;
    return _accessibilityCaptureFailures <= maxCaptureRetries;
  }

  void _clearAccessibilityCaptureFailures(_FrameCaptureKey key) {
    if (_accessibilityCaptureFailureKey != key) return;
    _accessibilityCaptureFailureKey = null;
    _accessibilityCaptureFailures = 0;
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
    _rendererView = null;
    _lastRendererKey = null;
    _pendingRendererPresentationId = null;
    _pendingRendererSnapshot = null;
    _rendererPresentationFailures = 0;
    _rendererFindResult = null;
    final selected = selectedContext;
    _formatter.reset(
      contextId: selected?.contextId ?? 1,
      documentId: selected?.documentId ?? 1,
    );
    _frame = null;
    _lastCaptureKey = null;
    _replacementCapture = null;
    _captureGeneration++;
    _pendingFrame = null;
    _projectionGeneration++;
    _accessibility = null;
    _pendingAccessibility = null;
    _lastAccessibilityKey = null;
    _replacementAccessibilityCapture = null;
    _accessibilityGeneration++;
    _inputGeneration++;
    _lastInputResult = null;
    _frameCaptureFailureKey = null;
    _frameCaptureFailures = 0;
    _accessibilityCaptureFailureKey = null;
    _accessibilityCaptureFailures = 0;
    _findRequestGeneration++;
    _findQuery = '';
    _findMatches = null;
    _findActiveMatch = null;
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
      await _hostViewTail;
      await _rendererTail;
      await controller.saveCurrentProfileSession();
    } catch (_) {
      // Shutdown still has to release the sole native browser owner.
    } finally {
      _formatter.dispose();
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

final class _HostViewKey {
  const _HostViewKey({
    required this.contextId,
    required this.width,
    required this.height,
    required this.scaleFactor,
    required this.focused,
    required this.visible,
    required this.lifecycle,
  });

  final int contextId;
  final int width;
  final int height;
  final double scaleFactor;
  final bool focused;
  final bool visible;
  final BrowserHostLifecycle lifecycle;

  @override
  bool operator ==(Object other) =>
      other is _HostViewKey &&
      contextId == other.contextId &&
      width == other.width &&
      height == other.height &&
      scaleFactor == other.scaleFactor &&
      focused == other.focused &&
      visible == other.visible &&
      lifecycle == other.lifecycle;

  @override
  int get hashCode => Object.hash(
    contextId,
    width,
    height,
    scaleFactor,
    focused,
    visible,
    lifecycle,
  );
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

final class _RendererSnapshotRequest {
  const _RendererSnapshotRequest({
    required this.key,
    required this.viewportGeneration,
    required this.pageZoom,
    required this.lifecycleGeneration,
  });

  final _FrameCaptureKey key;
  final int viewportGeneration;
  final double pageZoom;
  final int lifecycleGeneration;
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

final class _PendingFrame {
  const _PendingFrame(this.projectionGeneration, this.frame);

  final int projectionGeneration;
  final BrowserFrame frame;
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
