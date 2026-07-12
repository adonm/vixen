import 'dart:async';

import '../browser_controller.dart';
import '../browser_models.dart';

typedef BrowserCommandHandler =
    FutureOr<BrowserResponse> Function(
      BrowserCommand command,
      ScriptedBrowserController controller,
    );
typedef BrowserFrameHandler =
    FutureOr<BrowserFrame?> Function(
      int contextId,
      int documentId,
      int width,
      int height,
    );
typedef BrowserAccessibilityHandler =
    BrowserAccessibilitySnapshot Function(
      int contextId,
      int documentId,
      int width,
      int height,
    );

/// An in-memory command/event transport for shell tests. It models BrowserCore's
/// authoritative registry but never renders or interprets page content.
final class ScriptedBrowserController extends BrowserController {
  ScriptedBrowserController({
    this.session = const ProfileSessionState(),
    BrowserSnapshot snapshot = const BrowserSnapshot(),
    this.onCommand,
    this.onCaptureFrame,
    this.onAccessibilitySnapshot,
  }) : _activeContextId = snapshot.activeContextId,
       _contexts = {
         for (final context in snapshot.contexts) context.contextId: context,
       },
       _nextContextId = snapshot.contexts.fold(
         1,
         (next, state) => state.contextId >= next ? state.contextId + 1 : next,
       );

  final BrowserCommandHandler? onCommand;
  final BrowserFrameHandler? onCaptureFrame;
  final BrowserAccessibilityHandler? onAccessibilitySnapshot;
  final StreamController<SequencedBrowserEvent> _events =
      StreamController<SequencedBrowserEvent>.broadcast();
  final List<BrowserCommand> commands = [];
  final Map<int, BrowsingContextState> _contexts;
  ProfileSessionState session;
  int? _activeContextId;
  int _nextContextId;
  int _nextNavigationId = 1;
  int _nextSequence = 1;
  bool _started = false;
  bool _shutdown = false;

  int startCount = 0;
  int shutdownCount = 0;
  int snapshotCount = 0;
  final List<({int contextId, int documentId, int width, int height})>
  frameRequests = [];

  @override
  Stream<SequencedBrowserEvent> get events => _events.stream;

  BrowserSnapshot get snapshot => BrowserSnapshot(
    activeContextId: _activeContextId,
    contexts: List.unmodifiable(_contexts.values),
  );

  @override
  Future<void> start() async {
    if (_shutdown) throw const BrowserFailure('browser.closed', 'closed');
    if (_started) return;
    _started = true;
    startCount++;
  }

  @override
  Future<BrowserResponse> dispatch(BrowserCommand command) async {
    if (!_started || _shutdown) {
      throw const BrowserFailure('browser.closed', 'Browser is not running');
    }
    commands.add(command);
    if (onCommand != null) return onCommand!(command, this);
    return _dispatchDefault(command);
  }

  BrowserResponse _dispatchDefault(BrowserCommand command) {
    switch (command.type) {
      case 'load_profile_session':
        return ProfileSessionResponse(session);
      case 'save_current_profile_session':
        final contexts = _contexts.values.toList();
        session = ProfileSessionState(
          tabs: contexts.map((state) => state.url).toList(),
          activeIndex: _activeContextId == null
              ? 0
              : contexts.indexWhere(
                  (state) => state.contextId == _activeContextId,
                ),
        );
        return const AcceptedResponse();
      case 'browser_snapshot':
        snapshotCount++;
        return BrowserSnapshotResponse(snapshot);
      case 'create_context':
        final context = BrowsingContextState.initial(_nextContextId++);
        _contexts[context.contextId] = context;
        _activeContextId = context.contextId;
        emitEvent(BrowserEvent.contextCreated(context));
        emitEvent(BrowserEvent.activeContextChanged(context.contextId));
        return ContextCreatedResponse(context.contextId);
      case 'close_context':
        final contextId = _knownContext(command.contextId);
        _contexts.remove(contextId);
        emitEvent(BrowserEvent.contextClosed(contextId));
        if (_activeContextId == contextId) {
          _activeContextId = _contexts.keys.lastOrNull;
          emitEvent(BrowserEvent.activeContextChanged(_activeContextId));
        }
        return const AcceptedResponse();
      case 'activate_context':
        final contextId = _knownContext(command.contextId);
        _activeContextId = contextId;
        emitEvent(BrowserEvent.activeContextChanged(contextId));
        return const AcceptedResponse();
      case 'navigate':
        return _beginNavigation(_knownContext(command.contextId), command.url!);
      case 'reload':
        final contextId = _knownContext(command.contextId);
        return _beginNavigation(contextId, _contexts[contextId]!.url);
      case 'stop':
        final contextId = _knownContext(command.contextId);
        final state = _contexts[contextId]!;
        replaceContext(
          state.copyWith(
            isLoading: false,
            clearActiveNavigation: true,
            loadProgress: 0,
          ),
        );
        return const AcceptedResponse();
      case 'traverse_history':
        _knownContext(command.contextId);
        return const AcceptedResponse();
      case 'context_state':
        return ContextStateResponse(
          _contexts[_knownContext(command.contextId)]!,
        );
      case 'accessibility_snapshot':
        final contextId = _knownContext(command.contextId);
        final wire = command.toWire();
        final viewport = (wire['viewport']! as Map).cast<String, Object?>();
        final documentId = wire['document_id']! as int;
        final width = viewport['width']! as int;
        final height = viewport['height']! as int;
        final custom = onAccessibilitySnapshot?.call(
          contextId,
          documentId,
          width,
          height,
        );
        if (custom != null) return AccessibilitySnapshotResponse(custom);
        return AccessibilitySnapshotResponse(
          BrowserAccessibilitySnapshot(
            sourceGeneration: 1,
            generation: 1,
            contextId: contextId,
            documentId: documentId,
            viewportWidth: width,
            viewportHeight: height,
            nodes: const [],
            truncated: false,
          ),
        );
      case 'dispatch_accessibility_action':
      case 'dispatch_mouse_event':
      case 'dispatch_key_event':
        _knownContext(command.contextId);
        return InputDispatchedResponse.empty();
      default:
        throw BrowserFailure(
          'browser.invalid-argument',
          'Unknown command ${command.type}',
        );
    }
  }

  @override
  Future<BrowserFrame?> captureFrame({
    required int contextId,
    required int documentId,
    required int width,
    required int height,
  }) async {
    frameRequests.add((
      contextId: contextId,
      documentId: documentId,
      width: width,
      height: height,
    ));
    return onCaptureFrame?.call(contextId, documentId, width, height);
  }

  NavigationAcceptedResponse _beginNavigation(int contextId, String url) {
    final navigationId = _nextNavigationId++;
    final state = _contexts[contextId]!.copyWith(
      activeNavigationId: navigationId,
      url: url,
      isLoading: true,
      loadProgress: 0,
    );
    replaceContext(state);
    return NavigationAcceptedResponse(navigationId);
  }

  int _knownContext(int? contextId) {
    if (contextId == null || !_contexts.containsKey(contextId)) {
      throw BrowserFailure(
        'browser.unknown-context',
        'Unknown browsing context $contextId',
      );
    }
    return contextId;
  }

  void emitEvent(BrowserEvent event, {int? sequence}) {
    if (_shutdown) return;
    final deliveredSequence = sequence ?? _nextSequence;
    _nextSequence = deliveredSequence + 1;
    _events.add(
      SequencedBrowserEvent(sequence: deliveredSequence, event: event),
    );
  }

  void replaceContext(BrowsingContextState state, {bool emit = true}) {
    _contexts[state.contextId] = state;
    if (emit) emitEvent(BrowserEvent.contextStateChanged(state));
  }

  void replaceSnapshot(BrowserSnapshot value) {
    _contexts
      ..clear()
      ..addEntries(
        value.contexts.map((state) => MapEntry(state.contextId, state)),
      );
    _activeContextId = value.activeContextId;
  }

  void settleNavigation(int contextId, {String? title}) {
    final state = _contexts[contextId]!;
    final navigationId = state.activeNavigationId;
    if (navigationId == null) return;
    emitEvent(
      BrowserEvent.navigationPhaseChanged(
        contextId: contextId,
        frameId: state.mainFrameId,
        navigationId: navigationId,
        phase: 'settled',
      ),
    );
    replaceContext(
      state.copyWith(
        title: title,
        isLoading: false,
        loadProgress: 1,
        clearActiveNavigation: true,
      ),
    );
  }

  void failNavigation(
    int contextId, {
    BrowserFailure error = const BrowserFailure(
      'navigation.load',
      'Navigation failed',
    ),
  }) {
    final state = _contexts[contextId]!;
    final navigationId = state.activeNavigationId;
    if (navigationId == null) return;
    emitEvent(
      BrowserEvent.navigationFailed(
        contextId: contextId,
        frameId: state.mainFrameId,
        navigationId: navigationId,
        error: error,
      ),
    );
    replaceContext(
      state.copyWith(isLoading: false, clearActiveNavigation: true),
    );
  }

  @override
  Future<void> shutdown() async {
    if (_shutdown) return;
    _shutdown = true;
    shutdownCount++;
    await _events.close();
  }
}

extension<T> on Iterable<T> {
  T? get lastOrNull {
    final iterator = this.iterator;
    if (!iterator.moveNext()) return null;
    var result = iterator.current;
    while (iterator.moveNext()) {
      result = iterator.current;
    }
    return result;
  }
}
