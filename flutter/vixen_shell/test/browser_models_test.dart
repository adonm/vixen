import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/browser_models.dart';

void main() {
  test('BrowserFrame copies input and exposes immutable packed pixels', () {
    final source = Uint8List.fromList([1, 2, 3, 4]);
    final frame = BrowserFrame(
      rgba: source,
      width: 1,
      height: 1,
      frameId: 1,
      contextId: 2,
      documentId: 3,
    );
    source[0] = 9;
    expect(frame.rgba, [1, 2, 3, 4]);
    expect(() => frame.rgba[0] = 8, throwsUnsupportedError);
  });

  test('commands use exact ABI v1 command fields', () {
    expect(BrowserCommand.loadProfileSession().toWire(), {
      'v': 1,
      'type': 'load_profile_session',
    });
    expect(BrowserCommand.saveCurrentProfileSession().toWire(), {
      'v': 1,
      'type': 'save_current_profile_session',
    });
    expect(BrowserCommand.browserSnapshot().toWire(), {
      'v': 1,
      'type': 'browser_snapshot',
    });
    expect(BrowserCommand.createContext().toWire(), {
      'v': 1,
      'type': 'create_context',
    });
    expect(BrowserCommand.closeContext(7).toWire(), {
      'v': 1,
      'type': 'close_context',
      'context_id': 7,
    });
    expect(BrowserCommand.activateContext(7).toWire(), {
      'v': 1,
      'type': 'activate_context',
      'context_id': 7,
    });
    expect(BrowserCommand.navigate(7, 'https://example.test').toWire(), {
      'v': 1,
      'type': 'navigate',
      'context_id': 7,
      'url': 'https://example.test',
    });
    expect(BrowserCommand.reload(7).toWire(), {
      'v': 1,
      'type': 'reload',
      'context_id': 7,
    });
    expect(BrowserCommand.stop(7).toWire(), {
      'v': 1,
      'type': 'stop',
      'context_id': 7,
    });
    expect(BrowserCommand.traverseHistory(7, -1).toWire(), {
      'v': 1,
      'type': 'traverse_history',
      'context_id': 7,
      'delta': -1,
    });
    expect(BrowserCommand.contextState(7).toWire(), {
      'v': 1,
      'type': 'context_state',
      'context_id': 7,
    });
    expect(
      BrowserCommand.updateHostViewState(
        contextId: 7,
        generation: 3,
        viewportWidth: 800,
        viewportHeight: 600,
        scaleFactor: 2,
        focused: false,
        visible: false,
        lifecycle: BrowserHostLifecycle.hidden,
      ).toWire(),
      {
        'v': 1,
        'type': 'update_host_view_state',
        'context_id': 7,
        'generation': 3,
        'viewport': {'width': 800, 'height': 600},
        'scale_factor': 2.0,
        'focused': false,
        'visible': false,
        'lifecycle': 'hidden',
      },
    );
    expect(
      BrowserCommand.dispatchMouseEvent(
        contextId: 7,
        documentId: 70,
        runtimeContextId: 700,
        viewportWidth: 800,
        viewportHeight: 600,
        eventType: 'mousedown',
        event: const BrowserMouseEvent(
          x: 12.5,
          y: 20,
          button: 0,
          buttons: 0,
          detail: 1,
        ),
      ).toWire(),
      {
        'v': 1,
        'type': 'dispatch_mouse_event',
        'context_id': 7,
        'document_id': 70,
        'runtime_context_id': 700,
        'viewport': {'width': 800, 'height': 600},
        'event_type': 'mousedown',
        'event': {
          'x': 12.5,
          'y': 20.0,
          'button': 0,
          'buttons': 0,
          'detail': 1,
          'bubbles': true,
          'ctrl_key': false,
          'shift_key': false,
          'alt_key': false,
          'meta_key': false,
          'delta_x': 0.0,
          'delta_y': 0.0,
        },
      },
    );
    expect(
      BrowserCommand.dispatchKeyEvent(
        contextId: 7,
        documentId: 70,
        runtimeContextId: 700,
        viewportWidth: 800,
        viewportHeight: 600,
        eventType: 'keydown',
        event: const BrowserKeyEvent(
          key: 'a',
          code: 'Key A',
          text: 'a',
          applyText: true,
        ),
      ).toWire()['event'],
      {
        'key': 'a',
        'code': 'Key A',
        'text': 'a',
        'apply_text': true,
        'ctrl_key': false,
        'shift_key': false,
        'alt_key': false,
        'meta_key': false,
        'repeat': false,
        'location': 0,
      },
    );
  });

  test(
    'input response preserves BrowserCore effects and navigation actions',
    () {
      final response = BrowserResponse.fromWire({
        'type': 'input_dispatched',
        'effects': {
          'console': <Object?>[],
          'dialogs': [
            {'kind': 'alert', 'message': 'hello'},
          ],
          'bindings': <Object?>[],
          'network': <Object?>[],
          'exceptions': <Object?>[],
        },
        'navigation_actions': [
          {'type': 'same_document', 'url': 'https://example.test/#next'},
        ],
      }) as InputDispatchedResponse;

      expect(response.effects['dialogs'], hasLength(1));
      expect(response.navigationActions.single['type'], 'same_document');
      expect(
        () => response.navigationActions.add(const {}),
        throwsUnsupportedError,
      );
    },
  );

  test('accessibility snapshot preserves bounded semantic fields', () {
    final response = BrowserResponse.fromWire({
      'type': 'accessibility_snapshot',
      'source_generation': 8,
      'generation': 99,
      'context_id': 7,
      'document_id': 70,
      'viewport': {'width': 800, 'height': 600},
      'nodes': [
        {
          'id': 9,
          'parent_id': null,
          'controls_ids': [],
          'role': 'checkbox',
          'label': 'Remember me',
          'value': null,
          'range': null,
          'bbox': {'x': 10.0, 'y': 20.0, 'width': 100.0, 'height': 30.0},
          'focused': true,
          'disabled': false,
          'checked': true,
          'selected': false,
          'expanded': null,
          'hidden': false,
          'focusable': true,
          'actions': ['tap'],
        },
      ],
      'truncated': false,
    }) as AccessibilitySnapshotResponse;

    expect(response.snapshot.nodes.single.role, 'checkbox');
    expect(response.snapshot.nodes.single.parentId, isNull);
    expect(response.snapshot.sourceGeneration, 8);
    expect(response.snapshot.generation, 99);
    expect(response.snapshot.nodes.single.checked, isTrue);
    expect(response.snapshot.nodes.single.bounds?.width, 100);
    expect(
      BrowserCommand.dispatchAccessibilityFocus(
        contextId: 7,
        documentId: 70,
        runtimeContextId: 700,
        viewportWidth: 800,
        viewportHeight: 600,
        sourceGeneration: 8,
        generation: 99,
        nodeId: 9,
      ).toWire(),
      {
        'v': 1,
        'type': 'dispatch_accessibility_action',
        'context_id': 7,
        'document_id': 70,
        'runtime_context_id': 700,
        'viewport': {'width': 800, 'height': 600},
        'source_generation': 8,
        'generation': 99,
        'node_id': 9,
        'action': 'focus',
      },
    );
    expect(
      BrowserCommand.accessibilitySnapshot(
        contextId: 7,
        documentId: 70,
        viewportWidth: 800,
        viewportHeight: 600,
      ).toWire(),
      {
        'v': 1,
        'type': 'accessibility_snapshot',
        'context_id': 7,
        'document_id': 70,
        'viewport': {'width': 800, 'height': 600},
      },
    );
    expect(
      BrowserCommand.dispatchAccessibilitySetValue(
        contextId: 7,
        documentId: 70,
        runtimeContextId: 700,
        viewportWidth: 800,
        viewportHeight: 600,
        sourceGeneration: 8,
        generation: 99,
        nodeId: 9,
        value: 'Ada',
      ).toWire(),
      {
        'v': 1,
        'type': 'dispatch_accessibility_action',
        'context_id': 7,
        'document_id': 70,
        'runtime_context_id': 700,
        'viewport': {'width': 800, 'height': 600},
        'source_generation': 8,
        'generation': 99,
        'node_id': 9,
        'action': 'set_value',
        'value': 'Ada',
      },
    );
    expect(
      BrowserCommand.dispatchAccessibilityAdjustment(
        contextId: 7,
        documentId: 70,
        runtimeContextId: 700,
        viewportWidth: 800,
        viewportHeight: 600,
        sourceGeneration: 8,
        generation: 99,
        nodeId: 9,
        increase: true,
      ).toWire(),
      {
        'v': 1,
        'type': 'dispatch_accessibility_action',
        'context_id': 7,
        'document_id': 70,
        'runtime_context_id': 700,
        'viewport': {'width': 800, 'height': 600},
        'source_generation': 8,
        'generation': 99,
        'node_id': 9,
        'action': 'increase',
      },
    );
  });

  test('accessibility snapshot validates parent hierarchy', () {
    BrowserAccessibilityNode node(
      int id, {
      int? parentId,
      List<int> controlsIds = const [],
    }) => BrowserAccessibilityNode(
      id: id,
      parentId: parentId,
      controlsIds: controlsIds,
      role: 'generic',
      label: 'node $id',
      focused: false,
      disabled: false,
      selected: false,
      hidden: false,
      focusable: false,
      actions: const [],
    );

    expect(
      () => BrowserAccessibilitySnapshot(
        sourceGeneration: 1,
        generation: 1,
        contextId: 1,
        documentId: 1,
        viewportWidth: 100,
        viewportHeight: 100,
        nodes: [node(2, parentId: 1), node(1)],
        truncated: false,
      ),
      throwsFormatException,
    );
    expect(
      () => BrowserAccessibilitySnapshot(
        sourceGeneration: 1,
        generation: 1,
        contextId: 1,
        documentId: 1,
        viewportWidth: 100,
        viewportHeight: 100,
        nodes: [node(1), node(1)],
        truncated: false,
      ),
      throwsFormatException,
    );
    expect(
      BrowserAccessibilitySnapshot(
        sourceGeneration: 1,
        generation: 1,
        contextId: 1,
        documentId: 1,
        viewportWidth: 100,
        viewportHeight: 100,
        nodes: [node(1), node(2, parentId: 1)],
        truncated: false,
      ).nodes.last.parentId,
      1,
    );
    expect(
      BrowserAccessibilitySnapshot(
        sourceGeneration: 1,
        generation: 1,
        contextId: 1,
        documentId: 1,
        viewportWidth: 100,
        viewportHeight: 100,
        nodes: [
          node(1),
          node(2, controlsIds: const [1]),
        ],
        truncated: false,
      ).nodes.last.controlsIds,
      [1],
    );
  });

  test('snapshot and event envelope round trip exact wire names', () {
    final state = contextState(
      id: 9,
      url: 'https://example.test',
      loading: true,
      progress: 0.4,
    );
    final snapshot = BrowserSnapshot.fromWire({
      'active_context_id': 9,
      'contexts': [state.toWire()],
    });
    expect(snapshot.activeContextId, 9);
    expect(snapshot.contexts.single.activeNavigationId, 90);
    expect(snapshot.toWire(), {
      'active_context_id': 9,
      'contexts': [state.toWire()],
    });

    final envelope = SequencedBrowserEvent.fromWire({
      'v': 1,
      'type': 'event',
      'sequence': 3,
      'event': {
        'type': 'browsing_context_state_changed',
        'state': state.toWire(),
      },
    });
    expect(envelope.sequence, 3);
    expect(envelope.event.state?.contextId, 9);
    expect(envelope.toWire()['sequence'], 3);
  });
}

BrowsingContextState contextState({
  required int id,
  required String url,
  bool loading = false,
  double progress = 1,
}) {
  return BrowsingContextState(
    contextId: id,
    mainFrameId: id * 10,
    documentId: id * 100,
    runtimeContextId: id * 1000,
    activeNavigationId: loading ? id * 10 : null,
    url: url,
    title: null,
    historyLength: 2,
    historyIndex: 1,
    canGoBack: true,
    canGoForward: false,
    isLoading: loading,
    loadProgress: progress,
  );
}
