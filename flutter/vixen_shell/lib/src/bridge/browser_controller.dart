import 'browser_models.dart';
import 'render_models.dart';

/// The browser-scoped seam. Implementations own one BrowserCore handle and
/// expose its sole ordered event stream.
abstract class BrowserController {
  Stream<SequencedBrowserEvent> get events;

  Future<void> start();

  Future<BrowserResponse> dispatch(BrowserCommand command);

  Future<BrowserFrame?> captureFrame({
    required int contextId,
    required int documentId,
    required int width,
    required int height,
  }) async => null;

  Future<void> shutdown();

  Future<ProfileSessionState> loadProfileSession() async {
    final response = await dispatch(BrowserCommand.loadProfileSession());
    return _expect<ProfileSessionResponse>(response).session;
  }

  Future<void> saveCurrentProfileSession() async {
    _expect<AcceptedResponse>(
      await dispatch(BrowserCommand.saveCurrentProfileSession()),
    );
  }

  Future<void> startCdp(int port) async {
    _expect<AcceptedResponse>(await dispatch(BrowserCommand.startCdp(port)));
  }

  Future<BrowserSnapshot> browserSnapshot() async {
    final response = await dispatch(BrowserCommand.browserSnapshot());
    return _expect<BrowserSnapshotResponse>(response).snapshot;
  }

  Future<int> createContext() async {
    final response = await dispatch(BrowserCommand.createContext());
    return _expect<ContextCreatedResponse>(response).contextId;
  }

  Future<void> closeContext(int contextId) async {
    _expect<AcceptedResponse>(
      await dispatch(BrowserCommand.closeContext(contextId)),
    );
  }

  Future<void> activateContext(int contextId) async {
    _expect<AcceptedResponse>(
      await dispatch(BrowserCommand.activateContext(contextId)),
    );
  }

  Future<int> navigate(int contextId, String url) async {
    final response = await dispatch(BrowserCommand.navigate(contextId, url));
    return _expect<NavigationAcceptedResponse>(response).navigationId;
  }

  Future<int> reload(int contextId) async {
    final response = await dispatch(BrowserCommand.reload(contextId));
    return _expect<NavigationAcceptedResponse>(response).navigationId;
  }

  Future<void> stop(int contextId) async {
    _expect<AcceptedResponse>(await dispatch(BrowserCommand.stop(contextId)));
  }

  Future<int?> traverseHistory(int contextId, int delta) async {
    final response = await dispatch(
      BrowserCommand.traverseHistory(contextId, delta),
    );
    return switch (response) {
      NavigationAcceptedResponse(:final navigationId) => navigationId,
      AcceptedResponse() => null,
      _ => throw StateError(
        'Unexpected ${response.runtimeType} for traverse_history',
      ),
    };
  }

  Future<BrowsingContextState> contextState(int contextId) async {
    final response = await dispatch(BrowserCommand.contextState(contextId));
    return _expect<ContextStateResponse>(response).state;
  }

  Future<BrowsingContextState> setPageZoom(int contextId, double zoom) async {
    final response = await dispatch(
      BrowserCommand.setPageZoom(contextId, zoom),
    );
    return _expect<ContextStateResponse>(response).state;
  }

  Future<FindTextResponse> findText({
    required int contextId,
    required int documentId,
    required String query,
    bool caseSensitive = false,
    bool forward = true,
  }) async => _expect<FindTextResponse>(
    await dispatch(
      BrowserCommand.findText(
        contextId: contextId,
        documentId: documentId,
        query: query,
        caseSensitive: caseSensitive,
        forward: forward,
      ),
    ),
  );

  Future<InputDispatchedResponse> updateHostViewState({
    required int contextId,
    required int generation,
    required int viewportWidth,
    required int viewportHeight,
    required double scaleFactor,
    required bool focused,
    required bool visible,
    required BrowserHostLifecycle lifecycle,
  }) async => _expect<InputDispatchedResponse>(
    await dispatch(
      BrowserCommand.updateHostViewState(
        contextId: contextId,
        generation: generation,
        viewportWidth: viewportWidth,
        viewportHeight: viewportHeight,
        scaleFactor: scaleFactor,
        focused: focused,
        visible: visible,
        lifecycle: lifecycle,
      ),
    ),
  );

  Future<BrowserAccessibilitySnapshot> accessibilitySnapshot({
    required int contextId,
    required int documentId,
    required int viewportWidth,
    required int viewportHeight,
  }) async => _expect<AccessibilitySnapshotResponse>(
    await dispatch(
      BrowserCommand.accessibilitySnapshot(
        contextId: contextId,
        documentId: documentId,
        viewportWidth: viewportWidth,
        viewportHeight: viewportHeight,
      ),
    ),
  ).snapshot;

  Future<void> publishRendererSnapshot({
    required int contextId,
    required int documentId,
    required int viewportWidth,
    required int viewportHeight,
    required int viewportGeneration,
    required double pageZoom,
  }) async {
    _expect<AcceptedResponse>(
      await dispatch(
        BrowserCommand.publishRendererSnapshot(
          contextId: contextId,
          documentId: documentId,
          viewportWidth: viewportWidth,
          viewportHeight: viewportHeight,
          viewportGeneration: viewportGeneration,
          pageZoom: pageZoom,
        ),
      ),
    );
  }

  Future<void> flushRendererSubmissions() async {
    _expect<AcceptedResponse>(
      await dispatch(BrowserCommand.flushRendererSubmissions()),
    );
  }

  Future<InputDispatchedResponse> dispatchAccessibilityFocus({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required int sourceGeneration,
    required int generation,
    required int nodeId,
  }) async => _expect<InputDispatchedResponse>(
    await dispatch(
      BrowserCommand.dispatchAccessibilityFocus(
        contextId: contextId,
        documentId: documentId,
        runtimeContextId: runtimeContextId,
        viewportWidth: viewportWidth,
        viewportHeight: viewportHeight,
        sourceGeneration: sourceGeneration,
        generation: generation,
        nodeId: nodeId,
      ),
    ),
  );

  Future<InputDispatchedResponse> dispatchAccessibilitySetValue({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required int sourceGeneration,
    required int generation,
    required int nodeId,
    required String value,
  }) async => _expect<InputDispatchedResponse>(
    await dispatch(
      BrowserCommand.dispatchAccessibilitySetValue(
        contextId: contextId,
        documentId: documentId,
        runtimeContextId: runtimeContextId,
        viewportWidth: viewportWidth,
        viewportHeight: viewportHeight,
        sourceGeneration: sourceGeneration,
        generation: generation,
        nodeId: nodeId,
        value: value,
      ),
    ),
  );

  Future<InputDispatchedResponse> dispatchAccessibilityAdjustment({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required int sourceGeneration,
    required int generation,
    required int nodeId,
    required bool increase,
  }) async => _expect<InputDispatchedResponse>(
    await dispatch(
      BrowserCommand.dispatchAccessibilityAdjustment(
        contextId: contextId,
        documentId: documentId,
        runtimeContextId: runtimeContextId,
        viewportWidth: viewportWidth,
        viewportHeight: viewportHeight,
        sourceGeneration: sourceGeneration,
        generation: generation,
        nodeId: nodeId,
        increase: increase,
      ),
    ),
  );

  Future<InputDispatchedResponse> dispatchMouseEvent({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required String eventType,
    required BrowserMouseEvent event,
  }) async => _expect<InputDispatchedResponse>(
    await dispatch(
      BrowserCommand.dispatchMouseEvent(
        contextId: contextId,
        documentId: documentId,
        runtimeContextId: runtimeContextId,
        viewportWidth: viewportWidth,
        viewportHeight: viewportHeight,
        eventType: eventType,
        event: event,
      ),
    ),
  );

  Future<InputDispatchedResponse> dispatchRendererMouseEvent({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required String eventType,
    required BrowserMouseEvent event,
    required RenderHitTestQuery query,
    required RenderInputTarget? target,
  }) async => _expect<InputDispatchedResponse>(
    await dispatch(
      BrowserCommand.dispatchRendererMouseEvent(
        contextId: contextId,
        documentId: documentId,
        runtimeContextId: runtimeContextId,
        viewportWidth: viewportWidth,
        viewportHeight: viewportHeight,
        eventType: eventType,
        event: event,
        query: query,
        target: target,
      ),
    ),
  );

  Future<InputDispatchedResponse> dispatchKeyEvent({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required String eventType,
    required BrowserKeyEvent event,
  }) async => _expect<InputDispatchedResponse>(
    await dispatch(
      BrowserCommand.dispatchKeyEvent(
        contextId: contextId,
        documentId: documentId,
        runtimeContextId: runtimeContextId,
        viewportWidth: viewportWidth,
        viewportHeight: viewportHeight,
        eventType: eventType,
        event: event,
      ),
    ),
  );

  Future<InputDispatchedResponse> dispatchTextInput({
    required int contextId,
    required int documentId,
    required int runtimeContextId,
    required int viewportWidth,
    required int viewportHeight,
    required BrowserTextInputState state,
  }) async => _expect<InputDispatchedResponse>(
    await dispatch(
      BrowserCommand.dispatchTextInput(
        contextId: contextId,
        documentId: documentId,
        runtimeContextId: runtimeContextId,
        viewportWidth: viewportWidth,
        viewportHeight: viewportHeight,
        state: state,
      ),
    ),
  );
}

T _expect<T extends BrowserResponse>(BrowserResponse response) {
  if (response case final T typed) return typed;
  throw StateError('Expected $T, received ${response.runtimeType}');
}
