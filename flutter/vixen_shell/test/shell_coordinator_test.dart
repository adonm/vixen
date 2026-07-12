import 'dart:async';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/browser_models.dart';
import 'package:vixen_shell/src/bridge/fake/scripted_browser_controller.dart';
import 'package:vixen_shell/src/shell/shell_coordinator.dart';

import 'browser_models_test.dart' show contextState;

void main() {
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

  test(
    'frame capture coalesces to one in flight and one replacement',
    () async {
      final first = Completer<BrowserFrame?>();
      final second = Completer<BrowserFrame?>();
      var captureCount = 0;
      final controller = ScriptedBrowserController(
        snapshot: BrowserSnapshot(
          activeContextId: 1,
          contexts: [contextState(id: 1, url: 'https://frame.test')],
        ),
        onCaptureFrame: (contextId, documentId, width, height) {
          captureCount++;
          return captureCount == 1 ? first.future : second.future;
        },
      );
      final coordinator = ShellCoordinator(controller);
      await coordinator.start();

      coordinator.updatePhysicalViewport(100, 50);
      coordinator.updatePhysicalViewport(200, 100);
      coordinator.updatePhysicalViewport(300, 150);
      expect(controller.frameRequests, hasLength(1));

      first.complete(
        browserFrame(
          width: 100,
          height: 50,
          frameId: 1,
          contextId: 1,
          documentId: 100,
        ),
      );
      await flushEvents();
      expect(coordinator.frame, isNull);
      expect(controller.frameRequests, hasLength(2));
      expect(controller.frameRequests.last.width, 300);

      second.complete(
        browserFrame(
          width: 300,
          height: 150,
          frameId: 2,
          contextId: 1,
          documentId: 100,
        ),
      );
      await flushEvents();
      expect(coordinator.frame?.frameId, 2);
      await coordinator.close();
    },
  );

  test('stale document capture is rejected and replaced', () async {
    final oldCapture = Completer<BrowserFrame?>();
    final newCapture = Completer<BrowserFrame?>();
    var captureCount = 0;
    final initial = contextState(id: 2, url: 'https://generation.test');
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(activeContextId: 2, contexts: [initial]),
      onCaptureFrame: (contextId, documentId, width, height) {
        captureCount++;
        return captureCount == 1 ? oldCapture.future : newCapture.future;
      },
    );
    final coordinator = ShellCoordinator(controller);
    await coordinator.start();
    coordinator.updatePhysicalViewport(4, 3);

    controller.replaceContext(initial.copyWith(documentId: 201));
    await flushEvents();
    oldCapture.complete(
      browserFrame(
        width: 4,
        height: 3,
        frameId: 1,
        contextId: 2,
        documentId: 200,
      ),
    );
    await flushEvents();
    expect(coordinator.frame, isNull);
    expect(controller.frameRequests.last.documentId, 201);

    newCapture.complete(
      browserFrame(
        width: 4,
        height: 3,
        frameId: 2,
        contextId: 2,
        documentId: 201,
      ),
    );
    await flushEvents();
    expect(coordinator.frame?.documentId, 201);
    await coordinator.close();
  });

  test('physical viewport is bounded before capture', () async {
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

    expect(controller.frameRequests.single.width, browserMaxFrameDimension);
    expect(controller.frameRequests.single.height, browserMaxFrameDimension);
    expect(
      controller.frameRequests.single.width *
          controller.frameRequests.single.height *
          4,
      lessThanOrEqualTo(browserMaxFrameBytes),
    );
    await coordinator.close();
  });

  test(
    'input carries the selected BrowserCore generation and viewport',
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
    var nextFrameId = 0;
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
      onCaptureFrame: (contextId, documentId, width, height) => browserFrame(
        width: width,
        height: height,
        frameId: ++nextFrameId,
        contextId: contextId,
        documentId: documentId,
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
    final mouseCommands = controller.commands
        .where((command) => command.type == 'dispatch_mouse_event')
        .map((command) => command.toWire())
        .toList();
    expect(mouseCommands.map((command) => command['event_type']), [
      'mousedown',
      'mouseup',
    ]);
    expect((mouseCommands.first['event']! as Map<Object?, Object?>)['x'], 60.0);
    await coordinator.close();
  });

  test(
    'accessibility is withheld until its exact frame generation arrives',
    () async {
      final frame = Completer<BrowserFrame?>();
      final controller = ScriptedBrowserController(
        snapshot: BrowserSnapshot(
          activeContextId: 6,
          contexts: [contextState(id: 6, url: 'https://paired.test')],
        ),
        onAccessibilitySnapshot: (contextId, documentId, width, height) =>
            BrowserAccessibilitySnapshot(
              sourceGeneration: 2,
              generation: 2,
              contextId: contextId,
              documentId: documentId,
              viewportWidth: width,
              viewportHeight: height,
              nodes: [semanticButton(id: 12)],
              truncated: false,
            ),
        onCaptureFrame: (contextId, documentId, width, height) => frame.future,
      );
      final coordinator = ShellCoordinator(controller);
      await coordinator.start();
      coordinator.updatePhysicalViewport(200, 100);
      await flushEvents();

      expect(coordinator.accessibility, isNull);
      frame.complete(
        browserFrame(
          width: 200,
          height: 100,
          frameId: 1,
          contextId: 6,
          documentId: 600,
        ),
      );
      await flushEvents();

      expect(coordinator.accessibility?.generation, 2);
      expect(coordinator.frame?.frameId, 1);
      await coordinator.close();
    },
  );
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

BrowserFrame browserFrame({
  required int width,
  required int height,
  required int frameId,
  required int contextId,
  required int documentId,
}) => BrowserFrame(
  rgba: Uint8List(width * height * 4),
  width: width,
  height: height,
  frameId: frameId,
  contextId: contextId,
  documentId: documentId,
);

Future<void> flushEvents() async {
  await Future<void>.delayed(Duration.zero);
  await Future<void>.delayed(Duration.zero);
}
