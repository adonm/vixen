import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/browser_models.dart';
import 'package:vixen_shell/src/bridge/native/native_browser_controller.dart';

void main() {
  final libraryPath = Platform.environment['VIXEN_FFI_LIBRARY'];

  test(
    'opens, navigates, captures, and shuts down through the production C ABI',
    () async {
      final profile = await Directory.systemTemp.createTemp('vixen-ffi-test-');
      final controller = NativeBrowserController(
        libraryPath: libraryPath,
        profilePath: '${profile.path}/profile.redb',
      );
      try {
        await controller.start();
        final contextId = await controller.createContext();
        final settled = controller.events.firstWhere(
          (envelope) =>
              envelope.event.contextId == contextId &&
              envelope.event.type == 'navigation_phase_changed' &&
              envelope.event.phase == 'settled',
        );
        final fixture = File(
          '../../crates/vixen-ffi/tests/fixtures/frame.html',
        ).absolute.uri.toString();
        await controller.navigate(contextId, fixture);
        await settled.timeout(const Duration(seconds: 30));
        final snapshot = await controller.browserSnapshot();
        final state = await controller.contextState(contextId);

        expect(contextId, greaterThan(0));
        expect(
          snapshot.contexts.map((context) => context.contextId),
          contains(contextId),
        );
        final accessibility = await controller.accessibilitySnapshot(
          contextId: contextId,
          documentId: state.documentId,
          viewportWidth: 64,
          viewportHeight: 48,
        );
        final sample = accessibility.nodes.singleWhere(
          (node) => node.label == 'Vixen sample',
        );
        expect(sample.actions, contains('focus'));
        await controller.dispatchAccessibilityFocus(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 64,
          viewportHeight: 48,
          sourceGeneration: accessibility.sourceGeneration,
          generation: accessibility.generation,
          nodeId: sample.id,
        );
        final focusedAccessibility = await controller.accessibilitySnapshot(
          contextId: contextId,
          documentId: state.documentId,
          viewportWidth: 64,
          viewportHeight: 48,
        );
        expect(
          focusedAccessibility.nodes.any(
            (node) => node.id == sample.id && node.focused,
          ),
          isTrue,
        );
        final name = focusedAccessibility.nodes.singleWhere(
          (node) => node.label == 'Name',
        );
        expect(name.actions, contains('set_value'));
        await controller.dispatchAccessibilitySetValue(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 64,
          viewportHeight: 48,
          sourceGeneration: focusedAccessibility.sourceGeneration,
          generation: focusedAccessibility.generation,
          nodeId: name.id,
          value: 'Ada',
        );
        final valuedAccessibility = await controller.accessibilitySnapshot(
          contextId: contextId,
          documentId: state.documentId,
          viewportWidth: 64,
          viewportHeight: 48,
        );
        expect(
          valuedAccessibility.nodes.any(
            (node) => node.id == name.id && node.value == 'Ada',
          ),
          isTrue,
        );
        try {
          final frame = await controller.captureFrame(
            contextId: contextId,
            documentId: state.documentId,
            width: 64,
            height: 48,
          );
          expect(frame, isNotNull);
          expect(frame?.rgba, hasLength(64 * 48 * 4));
          expect(frame?.contextId, contextId);
          expect(frame?.documentId, state.documentId);
        } on BrowserFailure catch (error) {
          if (error.code != 'unsupported.screenshot') rethrow;
        }
      } finally {
        await controller.shutdown();
        await profile.delete(recursive: true);
      }
    },
    skip: libraryPath == null
        ? 'Set VIXEN_FFI_LIBRARY to run the native integration smoke test.'
        : false,
  );
}
