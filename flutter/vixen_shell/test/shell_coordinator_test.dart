import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/browser_models.dart';
import 'package:vixen_shell/src/bridge/fake/scripted_browser_controller.dart';
import 'package:vixen_shell/src/bridge/native/native_renderer_protocol.dart';
import 'package:vixen_shell/src/bridge/render_models.dart';
import 'package:vixen_shell/src/shell/shell_coordinator.dart';

import 'browser_models_test.dart' show contextState;
import 'support/r3_fixture.dart';

void main() {
  test('internal broker pump services EnsureLayout independently', () async {
    final state = BrowsingContextState(
      contextId: 1,
      mainFrameId: 10,
      documentId: 2,
      runtimeContextId: 100,
      activeNavigationId: null,
      url: 'file:///basic.html',
      title: 'Basic',
      historyLength: 1,
      historyIndex: 0,
      canGoBack: false,
      canGoForward: false,
      isLoading: false,
      loadProgress: 1,
    );
    final controller = ScriptedBrowserController(
      rendererUpdatesEnabled: true,
      snapshot: BrowserSnapshot(activeContextId: 1, contexts: [state]),
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();
    final snapshot = r3Snapshot();
    controller.enqueueRendererRequest(NativeFullSnapshotUpdate(snapshot));
    controller.enqueueRendererRequest(
      NativeEnsureLayoutRequest(41, snapshot.revision),
    );

    for (
      var attempt = 0;
      attempt < 50 &&
          !controller.rendererResponses.any(
            (response) =>
                response['type'] == 'renderer_response' &&
                response['request_id'] == 41,
          );
      attempt++
    ) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }

    expect(
      controller.rendererResponses.any(
        (response) =>
            response['type'] == 'renderer_response' &&
            response['request_id'] == 41,
      ),
      isTrue,
    );
    expect(controller.rendererRequests, isEmpty);
    await coordinator.close();
  });

  test(
    'production renderer update becomes the displayed Flutter commit',
    () async {
      final state = BrowsingContextState(
        contextId: 1,
        mainFrameId: 10,
        documentId: 2,
        runtimeContextId: 100,
        activeNavigationId: null,
        url: 'file:///basic.html',
        title: 'Basic',
        historyLength: 1,
        historyIndex: 0,
        canGoBack: false,
        canGoForward: false,
        isLoading: false,
        loadProgress: 1,
      );
      final controller = ScriptedBrowserController(
        rendererUpdatesEnabled: true,
        snapshot: BrowserSnapshot(activeContextId: 1, contexts: [state]),
        onAccessibilitySnapshot: (contextId, documentId, width, height) =>
            BrowserAccessibilitySnapshot(
              contextId: contextId,
              documentId: documentId,
              sourceGeneration: 1,
              generation: 1,
              viewportWidth: width,
              viewportHeight: height,
              nodes: [
                BrowserAccessibilityNode(
                  id: 3,
                  role: 'link',
                  label: 'Read more',
                  bounds: const BrowserAccessibilityRect(
                    x: 10,
                    y: 10,
                    width: 80,
                    height: 20,
                  ),
                  focused: false,
                  disabled: false,
                  selected: false,
                  hidden: false,
                  focusable: true,
                  actions: const ['tap', 'focus'],
                ),
              ],
              truncated: false,
            ),
      );
      final coordinator = ShellCoordinator(controller);
      await coordinator.start();
      controller.enqueueRendererRequest(NativeFullSnapshotUpdate(r3Snapshot()));
      coordinator.updatePhysicalViewport(240, 160);
      for (
        var attempt = 0;
        attempt < 50 && coordinator.rendererView == null;
        attempt++
      ) {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }

      expect(coordinator.rendererView, isNotNull);
      expect(coordinator.rendererView?.commit.revision.documentId, 2);
      final view = coordinator.rendererView!;
      coordinator.rendererCommitPresented(view);
      for (
        var attempt = 0;
        attempt < 50 &&
            controller.commands
                    .where(
                      (command) => command.type == 'flush_renderer_submissions',
                    )
                    .length <
                2;
        attempt++
      ) {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      await coordinator.findText('Paragraph');
      expect(coordinator.rendererFindResult?.commitId, view.commit.commitId);
      expect(coordinator.rendererFindResult?.matches, hasLength(1));
      expect(coordinator.rendererFindResult?.boxes, isNotEmpty);
      coordinator.updateContentFocus(true);
      for (
        var attempt = 0;
        attempt < 50 && coordinator.accessibility == null;
        attempt++
      ) {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      final semantic = view.semanticRegions.singleWhere(
        (region) => region.descriptor.id == 3,
      );
      final snapshotsBeforeSemanticAction = controller.commands
          .where((command) => command.type == 'publish_renderer_snapshot')
          .length;
      await coordinator.dispatchRendererSemanticAction(
        view,
        semantic.descriptor,
        RenderSemanticActionKind.focus,
        null,
      );
      for (
        var attempt = 0;
        attempt < 50 &&
            controller.commands
                    .where(
                      (command) => command.type == 'publish_renderer_snapshot',
                    )
                    .length ==
                snapshotsBeforeSemanticAction;
        attempt++
      ) {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      final targetRect = view.commit.geometry
          .singleWhere((entry) => entry.nodeId == 9)
          .borderBox;
      final snapshotsBeforePress = controller.commands
          .where((command) => command.type == 'publish_renderer_snapshot')
          .length;
      await coordinator.dispatchMouseEvent(
        'mousedown',
        BrowserMouseEvent(
          x: targetRect.x + targetRect.width / 2,
          y: targetRect.y + targetRect.height / 2,
          button: 0,
          buttons: 1,
          detail: 1,
        ),
      );
      expect(
        controller.commands
            .where((command) => command.type == 'publish_renderer_snapshot')
            .length,
        snapshotsBeforePress,
      );
      await coordinator.dispatchMouseEvent(
        'mouseup',
        BrowserMouseEvent(
          x: targetRect.x + targetRect.width / 2,
          y: targetRect.y + targetRect.height / 2,
          button: 0,
          buttons: 0,
          detail: 1,
        ),
      );
      for (
        var attempt = 0;
        attempt < 50 &&
            controller.commands
                    .where(
                      (command) => command.type == 'publish_renderer_snapshot',
                    )
                    .length ==
                snapshotsBeforePress;
        attempt++
      ) {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      expect(
        controller.commands
            .where((command) => command.type == 'publish_renderer_snapshot')
            .length,
        greaterThan(snapshotsBeforePress),
      );
      expect(
        controller.commands.map((command) => command.type),
        containsAllInOrder([
          'publish_renderer_snapshot',
          'flush_renderer_submissions',
          'flush_renderer_submissions',
          'dispatch_accessibility_action',
          'dispatch_renderer_mouse_event',
        ]),
      );
      final rendererInput = controller.commands
          .lastWhere(
            (command) => command.type == 'dispatch_renderer_mouse_event',
          )
          .toWire();
      expect(rendererInput['query'], isA<Map<String, Object?>>());
      expect(rendererInput['target'], isA<Map<String, Object?>>());
      await coordinator.close();
    },
  );

  test('hidden renderer commits cannot reappear after resume', () async {
    final state = BrowsingContextState(
      contextId: 1,
      mainFrameId: 10,
      documentId: 2,
      runtimeContextId: 100,
      activeNavigationId: null,
      url: 'file:///lifecycle.html',
      title: 'Lifecycle',
      historyLength: 1,
      historyIndex: 0,
      canGoBack: false,
      canGoForward: false,
      isLoading: false,
      loadProgress: 1,
    );
    final controller = ScriptedBrowserController(
      rendererUpdatesEnabled: true,
      snapshot: BrowserSnapshot(activeContextId: 1, contexts: [state]),
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();
    controller.enqueueRendererRequest(NativeFullSnapshotUpdate(r3Snapshot()));
    coordinator.updatePhysicalViewport(240, 160);
    for (
      var attempt = 0;
      attempt < 50 && coordinator.rendererView == null;
      attempt++
    ) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }
    final first = coordinator.rendererView!;
    coordinator.rendererCommitPresented(first);
    for (
      var attempt = 0;
      attempt < 50 &&
          controller.commands
                  .where(
                    (command) => command.type == 'flush_renderer_submissions',
                  )
                  .length <
              2;
      attempt++
    ) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }

    coordinator.updateApplicationLifecycle(BrowserHostLifecycle.hidden);
    expect(coordinator.rendererView, isNull);
    controller.enqueueRendererRequest(
      NativeFullSnapshotUpdate(
        r3Snapshot(generation: 2, viewportGeneration: 3, updated: true),
      ),
    );
    coordinator.updateApplicationLifecycle(BrowserHostLifecycle.resumed);
    for (
      var attempt = 0;
      attempt < 50 &&
          coordinator.rendererView?.commit.revision.sourceGeneration != 2;
      attempt++
    ) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }

    final resumed = coordinator.rendererView!;
    expect(resumed.commit.commitId, greaterThan(first.commit.commitId));
    expect(resumed.commit.revision.sourceGeneration, 2);
    expect(resumed.commit.revision, isNot(first.commit.revision));
    coordinator.rendererCommitPresented(resumed);
    for (var attempt = 0; attempt < 50 && !first.isRetired; attempt++) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }
    expect(first.isRetired, isTrue);
    await coordinator.close();
  });

  test(
    'startup creates exactly one about:vixen context when session is empty',
    () async {
      final controller = ScriptedBrowserController();
      final coordinator = ShellCoordinator(controller);

      await coordinator.start();
      await flushEvents();

      expect(controller.startCount, 1);
      expect(coordinator.contexts, hasLength(1));
      expect(coordinator.selectedContext?.url, vixenStartUrl);
      expect(controller.commands.map((command) => command.type), [
        'load_profile_session',
        'browser_snapshot',
        'create_context',
        'navigate',
        'activate_context',
        'browser_snapshot',
      ]);
      await coordinator.close();
    },
  );

  test('startup accepts one explicit initial URL for native smokes', () async {
    final controller = ScriptedBrowserController();
    final coordinator = ShellCoordinator(
      controller,
      initialUrl: 'file:///accessibility.html',
    );

    await coordinator.start();
    await flushEvents();

    expect(coordinator.selectedContext?.url, 'file:///accessibility.html');
    await coordinator.close();
  });

  test(
    'startup restores session tabs and selected index through commands',
    () async {
      final controller = ScriptedBrowserController(
        session: const ProfileSessionState(
          tabs: ['https://one.test', 'https://two.test'],
          activeIndex: 1,
        ),
      );
      final coordinator = ShellCoordinator(controller);

      await coordinator.start();
      await flushEvents();

      expect(coordinator.contexts.map((tab) => tab.url), [
        'https://one.test',
        'https://two.test',
      ]);
      expect(coordinator.selectedContext?.url, 'https://two.test');
      expect(
        controller.commands
            .where((command) => command.type == 'navigate')
            .map((command) => command.toWire()),
        [
          {
            'v': 1,
            'type': 'navigate',
            'context_id': 1,
            'url': 'https://one.test',
          },
          {
            'v': 1,
            'type': 'navigate',
            'context_id': 2,
            'url': 'https://two.test',
          },
        ],
      );
      await coordinator.close();
    },
  );

  test('tab actions route to exact context command paths', () async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 4,
        contexts: [contextState(id: 4, url: 'https://current.test')],
      ),
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();

    await coordinator.navigate('example.com/path');
    await coordinator.reload();
    await coordinator.goBack();
    await coordinator.goForward();
    await coordinator.stop();

    expect(controller.commands.skip(2).map((command) => command.toWire()), [
      {
        'v': 1,
        'type': 'navigate',
        'context_id': 4,
        'url': 'https://example.com/path',
      },
      {'v': 1, 'type': 'reload', 'context_id': 4},
      {'v': 1, 'type': 'traverse_history', 'context_id': 4, 'delta': -1},
      {'v': 1, 'type': 'traverse_history', 'context_id': 4, 'delta': 1},
      {'v': 1, 'type': 'stop', 'context_id': 4},
    ]);
    await coordinator.close();
  });

  test('closing the sole tab resets its BrowserCore context', () async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 5,
        contexts: [contextState(id: 5, url: 'https://old.test')],
      ),
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();

    await coordinator.closeTab(5);
    await flushEvents();

    expect(coordinator.contexts, hasLength(1));
    expect(coordinator.selectedContext?.url, vixenStartUrl);
    expect(controller.commands.skip(2).map((command) => command.type), [
      'navigate',
    ]);
    expect(coordinator.selectedContext?.contextId, 5);
    await coordinator.close();
  });

  test(
    'event sequence discontinuity reconciles from authoritative snapshot',
    () async {
      final initial = contextState(id: 1, url: 'https://before.test');
      final controller = ScriptedBrowserController(
        snapshot: BrowserSnapshot(activeContextId: 1, contexts: [initial]),
      );
      final coordinator = ShellCoordinator(controller);
      await coordinator.start();

      final reconciled = contextState(id: 1, url: 'https://snapshot.test');
      controller.replaceSnapshot(
        BrowserSnapshot(activeContextId: 1, contexts: [reconciled]),
      );
      controller.emitEvent(
        BrowserEvent.contextStateChanged(
          contextState(id: 1, url: 'https://skipped-event.test'),
        ),
        sequence: 8,
      );
      await flushEvents();

      expect(controller.snapshotCount, 2);
      expect(coordinator.selectedContext?.url, 'https://snapshot.test');
      expect(coordinator.lastEventSequence, 8);
      await coordinator.close();
    },
  );

  test('late events cannot resurrect a closed context', () async {
    final state = contextState(id: 2, url: 'https://closed.test');
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(activeContextId: 2, contexts: [state]),
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();

    controller.emitEvent(BrowserEvent.contextClosed(2));
    controller.emitEvent(
      BrowserEvent.contextStateChanged(
        contextState(id: 2, url: 'https://stale.test'),
      ),
    );
    await flushEvents();

    expect(coordinator.contexts, isEmpty);
    await coordinator.close();
  });

  test(
    'navigation exposes pending, settlement, and structured failure status',
    () async {
      final controller = ScriptedBrowserController(
        snapshot: BrowserSnapshot(
          activeContextId: 3,
          contexts: [contextState(id: 3, url: 'https://start.test')],
        ),
      );
      final coordinator = ShellCoordinator(controller);
      await coordinator.start();

      await coordinator.navigate('https://next.test');
      await flushEvents();
      expect(coordinator.selectedContext?.isLoading, isTrue);
      expect(coordinator.selectedStatus, contains('Loading'));

      controller.settleNavigation(3, title: 'Next');
      await flushEvents();
      expect(coordinator.selectedContext?.isLoading, isFalse);
      expect(coordinator.selectedStatus, 'Done');

      await coordinator.navigate('https://broken.test');
      controller.failNavigation(
        3,
        error: const BrowserFailure('navigation.load', 'connection refused'),
      );
      await flushEvents();
      expect(coordinator.errorMessage, contains('connection refused'));
      expect(coordinator.selectedStatus, 'Navigation failed');
      await coordinator.close();
    },
  );

  test('teardown saves and shuts down once', () async {
    final controller = ScriptedBrowserController();
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();

    await Future.wait([coordinator.close(), coordinator.close()]);

    expect(controller.shutdownCount, 1);
    expect(
      controller.commands.where(
        (command) => command.type == 'save_current_profile_session',
      ),
      hasLength(1),
    );
    coordinator.dispose();
    await flushEvents();
    expect(controller.shutdownCount, 1);
  });

  test('accessibility capture retries twice and recovers', () async {
    var accessibilityAttempts = 0;
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 8,
        contexts: [contextState(id: 8, url: 'https://a11y-recovery.test')],
      ),
      onAccessibilitySnapshot: (contextId, documentId, width, height) {
        accessibilityAttempts++;
        if (accessibilityAttempts <=
            ShellCoordinator.maxAccessibilityCaptureRetries) {
          throw const BrowserFailure(
            'renderer.accessibility',
            'projection unavailable',
          );
        }
        return BrowserAccessibilitySnapshot(
          sourceGeneration: 1,
          generation: 1,
          contextId: contextId,
          documentId: documentId,
          viewportWidth: width,
          viewportHeight: height,
          nodes: const [],
          truncated: false,
        );
      },
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();

    coordinator.updatePhysicalViewport(320, 180);
    await flushEvents();
    await flushEvents();

    expect(accessibilityAttempts, 3);
    expect(coordinator.accessibility, isNotNull);
    expect(coordinator.errorMessage, isNull);
    await coordinator.close();
  });

  test('physical viewport is bounded before renderer projection', () async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 3,
        contexts: [contextState(id: 3, url: 'https://bounds.test')],
      ),
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();

    coordinator.updatePhysicalViewport(100000, 100000);
    await flushEvents();

    final request = controller.commands
        .singleWhere((command) => command.type == 'accessibility_snapshot')
        .toWire();
    final viewport = (request['viewport']! as Map).cast<String, int>();
    expect(viewport['width'], browserMaxViewportDimension);
    expect(viewport['height'], browserMaxViewportDimension);
    expect(
      viewport['width']! * viewport['height']! * 4,
      lessThanOrEqualTo(browserMaxViewportBytes),
    );
    await coordinator.close();
  });

  test('renderer publication waits for the pending host view update', () async {
    final state = contextState(id: 3, url: 'https://ordered.test');
    final hostView = Completer<BrowserResponse>();
    final controller = ScriptedBrowserController(
      rendererUpdatesEnabled: true,
      snapshot: BrowserSnapshot(activeContextId: 3, contexts: [state]),
      onCommand: (command, _) => switch (command.type) {
        'browser_snapshot' => BrowserSnapshotResponse(
          BrowserSnapshot(activeContextId: 3, contexts: [state]),
        ),
        'update_host_view_state' => hostView.future,
        'accessibility_snapshot' => AccessibilitySnapshotResponse(
          BrowserAccessibilitySnapshot(
            sourceGeneration: 1,
            generation: 1,
            contextId: 3,
            documentId: state.documentId,
            viewportWidth: 320,
            viewportHeight: 180,
            nodes: const [],
            truncated: false,
          ),
        ),
        'publish_renderer_snapshot' => const AcceptedResponse(),
        _ => throw StateError('unexpected command ${command.type}'),
      },
    );
    final coordinator = ShellCoordinator(controller, useProfileSession: false);
    await coordinator.start();

    coordinator.updatePhysicalViewport(320, 180);
    await flushEvents();
    expect(
      controller.commands.map((command) => command.type),
      isNot(contains('publish_renderer_snapshot')),
    );

    hostView.complete(InputDispatchedResponse.empty());
    for (
      var attempt = 0;
      attempt < 50 &&
          !controller.commands.any(
            (command) => command.type == 'publish_renderer_snapshot',
          );
      attempt++
    ) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }
    expect(
      controller.commands.map((command) => command.type),
      containsAllInOrder([
        'update_host_view_state',
        'publish_renderer_snapshot',
      ]),
    );
    await coordinator.close();
  });

  test(
    'commit-independent input carries the selected generation and viewport',
    () async {
      final controller = ScriptedBrowserController(
        snapshot: BrowserSnapshot(
          activeContextId: 4,
          contexts: [contextState(id: 4, url: 'https://input.test')],
        ),
      );
      final coordinator = ShellCoordinator(controller);
      await coordinator.start();
      coordinator.updatePhysicalViewport(640, 360);
      coordinator.updateContentFocus(true);

      await coordinator.dispatchMouseEvent(
        'mousedown',
        const BrowserMouseEvent(x: 10, y: 20, button: 0, buttons: 0, detail: 1),
      );
      await coordinator.dispatchKeyEvent(
        'keydown',
        const BrowserKeyEvent(
          key: 'a',
          code: 'Key A',
          text: 'a',
          applyText: true,
        ),
      );
      await coordinator.dispatchTextInput(
        const BrowserTextInputState(
          text: 'に',
          selection: BrowserAccessibilityTextSelection(
            baseOffset: 1,
            extentOffset: 1,
          ),
          composing: BrowserAccessibilityTextSelection(
            baseOffset: 0,
            extentOffset: 1,
          ),
        ),
      );

      final input = controller.commands
          .where((command) => command.type.startsWith('dispatch_'))
          .map((command) => command.toWire())
          .toList();
      expect(input, hasLength(2));
      for (final command in input) {
        expect(command['context_id'], 4);
        expect(command['document_id'], 400);
        expect(command['runtime_context_id'], 4000);
        expect(command['viewport'], {'width': 640, 'height': 360});
      }
      expect(coordinator.lastInputResult, isA<InputDispatchedResponse>());
      await coordinator.close();
    },
  );

  test('host focus visibility and lifecycle use monotonic commands', () async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 14,
        contexts: [contextState(id: 14, url: 'https://host-view.test')],
      ),
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();
    coordinator.updatePhysicalViewport(640, 360, 2);
    coordinator.updateContentFocus(true);
    await flushEvents();

    coordinator.updateApplicationLifecycle(BrowserHostLifecycle.hidden);
    await flushEvents();
    await coordinator.dispatchKeyEvent(
      'keydown',
      const BrowserKeyEvent(key: 'a', code: 'KeyA'),
    );
    coordinator.updateApplicationLifecycle(BrowserHostLifecycle.resumed);
    await flushEvents();

    final updates = controller.commands
        .where((command) => command.type == 'update_host_view_state')
        .map((command) => command.toWire())
        .toList();
    expect(updates.map((command) => command['generation']), [1, 2, 3, 4]);
    expect(updates[1]['focused'], isTrue);
    expect(updates[1]['scale_factor'], 2.0);
    expect(updates[2]['lifecycle'], 'hidden');
    expect(updates[2]['visible'], isFalse);
    expect(updates[2]['focused'], isFalse);
    expect(updates[3]['lifecycle'], 'resumed');
    expect(updates[3]['visible'], isTrue);
    expect(updates[3]['focused'], isTrue);
    expect(
      controller.commands.where(
        (command) => command.type == 'dispatch_key_event',
      ),
      isEmpty,
    );
    await coordinator.close();
  });

  test('accessibility snapshot is generation and viewport matched', () async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 5,
        contexts: [contextState(id: 5, url: 'https://semantics.test')],
      ),
      onAccessibilitySnapshot: (contextId, documentId, width, height) =>
          BrowserAccessibilitySnapshot(
            sourceGeneration: 1,
            generation: 1,
            contextId: contextId,
            documentId: documentId,
            viewportWidth: width,
            viewportHeight: height,
            nodes: [
              semanticButton(id: 11),
              semanticTextbox(id: 12),
              semanticRange(id: 13),
            ],
            truncated: false,
          ),
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();
    coordinator.updatePhysicalViewport(320, 180);
    coordinator.updateContentFocus(true);
    await flushEvents();

    expect(coordinator.accessibility?.contextId, 5);
    expect(coordinator.accessibility?.generation, 1);
    expect(coordinator.accessibility?.documentId, 500);
    expect(coordinator.accessibility?.viewportWidth, 320);
    expect(coordinator.accessibility?.nodes.first.label, 'Open');

    final semantics = coordinator.accessibility!;
    await coordinator.dispatchSemanticFocus(semantics, semantics.nodes.first);
    final focus = controller.commands
        .where((command) => command.type == 'dispatch_accessibility_action')
        .single
        .toWire();
    expect(focus, {
      'v': 1,
      'type': 'dispatch_accessibility_action',
      'context_id': 5,
      'document_id': 500,
      'runtime_context_id': 5000,
      'viewport': {'width': 320, 'height': 180},
      'source_generation': 1,
      'generation': 1,
      'node_id': 11,
      'action': 'focus',
    });

    await flushEvents();
    final focused = coordinator.accessibility!;
    final textbox = focused.nodes.singleWhere((node) => node.id == 12);
    await coordinator.dispatchSemanticSetValue(focused, textbox, 'Ada');
    final setValue = controller.commands
        .where((command) => command.type == 'dispatch_accessibility_action')
        .last
        .toWire();
    expect(setValue['action'], 'set_value');
    expect(setValue['node_id'], 12);
    expect(setValue['value'], 'Ada');

    await flushEvents();
    final adjusted = coordinator.accessibility!;
    final range = adjusted.nodes.singleWhere((node) => node.id == 13);
    await coordinator.dispatchSemanticAdjustment(
      adjusted,
      range,
      increase: true,
    );
    final increase = controller.commands
        .where((command) => command.type == 'dispatch_accessibility_action')
        .last
        .toWire();
    expect(increase['action'], 'increase');
    expect(increase['node_id'], 13);

    await flushEvents();
    final refreshed = coordinator.accessibility!;
    final button = refreshed.nodes.singleWhere((node) => node.id == 11);
    await coordinator.dispatchSemanticTap(refreshed, button);
    expect(
      controller.commands.where(
        (command) => command.type == 'dispatch_renderer_mouse_event',
      ),
      isEmpty,
    );
    await coordinator.close();
  });

  test('runtime effects refresh renderer and semantic projections', () async {
    var sourceGeneration = 0;
    final controller = ScriptedBrowserController(
      rendererUpdatesEnabled: true,
      snapshot: BrowserSnapshot(
        activeContextId: 8,
        contexts: [contextState(id: 8, url: 'https://live.test')],
      ),
      onAccessibilitySnapshot: (contextId, documentId, width, height) =>
          BrowserAccessibilitySnapshot(
            sourceGeneration: ++sourceGeneration,
            generation: sourceGeneration,
            contextId: contextId,
            documentId: documentId,
            viewportWidth: width,
            viewportHeight: height,
            nodes: [semanticButton(id: 1)],
            truncated: false,
          ),
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();
    coordinator.updatePhysicalViewport(320, 180);
    await flushEvents();
    final firstGeneration = coordinator.accessibility!.sourceGeneration;

    controller.emitEvent(
      BrowserEvent.fromWire({
        'type': 'runtime_effects',
        'context_id': 8,
        'document_id': 800,
        'runtime_context_id': 8000,
        'effects': {
          'console': <Object?>[],
          'dialogs': <Object?>[],
          'bindings': <Object?>[],
          'network': <Object?>[],
          'exceptions': <Object?>[],
        },
      }),
    );
    await flushEvents();

    for (
      var attempt = 0;
      attempt < 50 &&
          controller.commands
                  .where(
                    (command) => command.type == 'publish_renderer_snapshot',
                  )
                  .length <
              2;
      attempt++
    ) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }

    expect(
      coordinator.accessibility!.sourceGeneration,
      greaterThan(firstGeneration),
    );
    expect(
      controller.commands.where(
        (command) => command.type == 'accessibility_snapshot',
      ),
      hasLength(2),
    );
    final viewportGenerations = controller.commands
        .where((command) => command.type == 'publish_renderer_snapshot')
        .map((command) => command.toWire()['viewport_generation'])
        .toList();
    expect(viewportGenerations, [1, 1]);
    await coordinator.close();
  });
}

BrowserAccessibilityNode semanticButton({required int id}) =>
    BrowserAccessibilityNode(
      id: id,
      role: 'button',
      label: 'Open',
      bounds: const BrowserAccessibilityRect(
        x: 10,
        y: 20,
        width: 100,
        height: 40,
      ),
      focused: false,
      disabled: false,
      selected: false,
      hidden: false,
      focusable: true,
      actions: const ['tap', 'focus'],
    );

BrowserAccessibilityNode semanticTextbox({required int id}) =>
    BrowserAccessibilityNode(
      id: id,
      role: 'textbox',
      label: 'Name',
      value: '',
      bounds: const BrowserAccessibilityRect(
        x: 10,
        y: 70,
        width: 140,
        height: 40,
      ),
      focused: false,
      disabled: false,
      selected: false,
      hidden: false,
      focusable: true,
      actions: const ['focus', 'set_value'],
    );

BrowserAccessibilityNode semanticRange({required int id}) =>
    BrowserAccessibilityNode(
      id: id,
      role: 'slider',
      label: 'Volume',
      value: '4',
      range: const BrowserAccessibilityRange(
        current: 4,
        minimum: 0,
        maximum: 10,
        step: 2,
      ),
      bounds: const BrowserAccessibilityRect(
        x: 10,
        y: 120,
        width: 140,
        height: 40,
      ),
      focused: false,
      disabled: false,
      selected: false,
      hidden: false,
      focusable: true,
      actions: const ['focus', 'increase', 'decrease'],
    );

Future<void> flushEvents() async {
  await Future<void>.delayed(Duration.zero);
  await Future<void>.delayed(Duration.zero);
}
