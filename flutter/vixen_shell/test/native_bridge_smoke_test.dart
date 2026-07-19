import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/browser_models.dart';
import 'package:vixen_shell/src/bridge/native/native_browser_controller.dart';
import 'package:vixen_shell/src/bridge/native/native_renderer_protocol.dart';
import 'package:vixen_shell/src/bridge/render_models.dart';
import 'package:vixen_shell/src/renderer/formatter.dart';

void main() {
  final libraryPath = Platform.environment['VIXEN_FFI_LIBRARY'];

  test(
    'navigates, presents, clicks, revises, and shuts down through the production C ABI',
    () async {
      final profile = await Directory.systemTemp.createTemp('vixen-ffi-test-');
      final controller = NativeBrowserController(
        libraryPath: libraryPath,
        profilePath: '${profile.path}/profile.redb',
      );
      final formatter = VixenFormatter();
      try {
        await controller.start();
        final contextId = await controller.createContext();
        final settled = controller.events.firstWhere(
          (envelope) =>
              envelope.event.contextId == contextId &&
              envelope.event.type == 'navigation_phase_changed' &&
              envelope.event.phase == 'settled',
        );
        final fixture = File('test/fixtures/native_bridge.html').absolute.uri
            .toString();
        await controller.navigate(contextId, fixture);
        await settled.timeout(const Duration(seconds: 30));
        final snapshot = await controller.browserSnapshot();
        final state = await controller.contextState(contextId);
        await controller.publishRendererSnapshot(
          contextId: contextId,
          documentId: state.documentId,
          viewportWidth: 240,
          viewportHeight: 160,
          viewportGeneration: 1,
          pageZoom: state.pageZoom,
        );
        final rendererUpdate = controller.pollRenderer();
        expect(rendererUpdate, isA<NativeFullSnapshotUpdate>());
        final fullSnapshot = rendererUpdate! as NativeFullSnapshotUpdate;
        expect(fullSnapshot.snapshot.scrollIntents, hasLength(1));
        expect(
          fullSnapshot.snapshot.scrollIntents.single.kind,
          RenderScrollIntentKind.to,
        );
        expect(fullSnapshot.snapshot.scrollIntents.single.point.y, 0);
        expect(
          fullSnapshot.snapshot.nodes
              .where((node) => node.kind == RenderNodeKind.text)
              .map((node) => node.text)
              .join(' '),
          'Vixen draft Tail',
        );
        expect(
          fullSnapshot.snapshot.nodes
              .firstWhere((node) => node.semantic?.name == 'Vixen sample')
              .styles['width'],
          '32px',
        );
        final view = (await formatter.acceptFullSnapshot(
          fullSnapshot.snapshot,
          beforePublish: (commit) {
            controller.submitRenderer(rendererCommitSubmission(commit));
          },
        ) as RenderApplied).view;
        await controller.flushRendererSubmissions();
        final presented = RenderPresented(
          contextId: contextId,
          documentId: state.documentId,
          commitId: view.commit.commitId,
          revision: view.commit.revision,
        );
        controller.submitRenderer(rendererPresentedSubmission(presented));
        await controller.flushRendererSubmissions();
        formatter.present(presented);
        await controller.updateHostViewState(
          contextId: contextId,
          generation: 1,
          viewportWidth: 240,
          viewportHeight: 160,
          scaleFactor: 1,
          focused: true,
          visible: true,
          lifecycle: BrowserHostLifecycle.resumed,
        );
        final targetRect = view.semanticRegions
            .singleWhere((region) => region.descriptor.name == 'Vixen sample')
            .rect;
        final targetPoint = RenderPoint(
          targetRect.left + 4,
          targetRect.top + 4,
        );
        final query = RenderHitTestQuery(
          queryId: 1,
          contextId: contextId,
          documentId: state.documentId,
          displayedCommitId: view.commit.commitId,
          revision: view.commit.revision,
          handle: view.commit.hitTestHandle,
          point: targetPoint,
        );
        await controller.dispatchRendererMouseEvent(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 240,
          viewportHeight: 160,
          eventType: 'mousedown',
          event: BrowserMouseEvent(
            x: targetPoint.x,
            y: targetPoint.y,
            button: 0,
            buttons: 1,
            detail: 1,
          ),
          query: query,
          target: view.answerHitTest(query),
        );
        final mouseUpQuery = RenderHitTestQuery(
          queryId: 2,
          contextId: contextId,
          documentId: state.documentId,
          displayedCommitId: view.commit.commitId,
          revision: view.commit.revision,
          handle: view.commit.hitTestHandle,
          point: targetPoint,
        );
        await controller.dispatchRendererMouseEvent(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 240,
          viewportHeight: 160,
          eventType: 'mouseup',
          event: BrowserMouseEvent(
            x: targetPoint.x,
            y: targetPoint.y,
            button: 0,
            buttons: 0,
            detail: 1,
          ),
          query: mouseUpQuery,
          target: view.answerHitTest(mouseUpQuery),
        );
        expect(
          (await controller.contextState(contextId)).title,
          'Renderer click',
        );
        final zoomedState = await controller.setPageZoom(contextId, 1.25);
        await controller.publishRendererSnapshot(
          contextId: contextId,
          documentId: state.documentId,
          viewportWidth: 240,
          viewportHeight: 160,
          viewportGeneration: 2,
          pageZoom: zoomedState.pageZoom,
        );
        final zoomedUpdate =
            controller.pollRenderer()! as NativeMutationBatchUpdate;
        expect(zoomedUpdate.batch.targetRevision.viewportGeneration, 2);
        expect(
          zoomedUpdate.batch.mutations
              .whereType<SetRenderViewport>()
              .single
              .viewport
              .pageZoom,
          1.25,
        );
        expect(
          zoomedUpdate.batch.mutations
              .whereType<SetRenderScrollIntent>()
              .single
              .intent
              .point
              .y,
          75,
        );
        final zoomedView = await _applyMutation(
          controller,
          formatter,
          zoomedUpdate,
        );
        final zoomedPresented = RenderPresented(
          contextId: contextId,
          documentId: state.documentId,
          commitId: zoomedView.commit.commitId,
          revision: zoomedView.commit.revision,
        );
        controller.submitRenderer(rendererPresentedSubmission(zoomedPresented));
        await controller.flushRendererSubmissions();
        formatter.present(zoomedPresented);
        final firstRelease =
            controller.pollRenderer()! as NativeHandleReleaseUpdate;
        formatter.releaseHandles(firstRelease.release);
        expect(zoomedView.commit.commitId, greaterThan(view.commit.commitId));
        expect(zoomedView.commit.scroll.single.offsetY, 75);
        expect(view.isRetired, isTrue);

        for (final (queryId, shiftKey) in [(3, true), (4, false)]) {
          final query = RenderHitTestQuery(
            queryId: queryId,
            contextId: contextId,
            documentId: state.documentId,
            displayedCommitId: zoomedView.commit.commitId,
            revision: zoomedView.commit.revision,
            handle: zoomedView.commit.hitTestHandle,
            point: const RenderPoint(200, 140),
          );
          await controller.dispatchRendererMouseEvent(
            contextId: contextId,
            documentId: state.documentId,
            runtimeContextId: state.runtimeContextId!,
            viewportWidth: 240,
            viewportHeight: 160,
            eventType: 'wheel',
            event: BrowserMouseEvent(
              x: 200,
              y: 140,
              button: 0,
              buttons: 0,
              shiftKey: shiftKey,
              deltaY: 40,
            ),
            query: query,
            target: zoomedView.answerHitTest(query),
          );
        }
        await controller.publishRendererSnapshot(
          contextId: contextId,
          documentId: state.documentId,
          viewportWidth: 240,
          viewportHeight: 160,
          viewportGeneration: 3,
          pageZoom: zoomedState.pageZoom,
        );
        final wheelUpdate =
            controller.pollRenderer()! as NativeMutationBatchUpdate;
        expect(
          wheelUpdate.batch.mutations
              .whereType<SetRenderScrollIntent>()
              .single
              .intent
              .point
              .y,
          115,
        );
        final wheelView = await _applyMutation(
          controller,
          formatter,
          wheelUpdate,
        );
        final wheelPresented = RenderPresented(
          contextId: contextId,
          documentId: state.documentId,
          commitId: wheelView.commit.commitId,
          revision: wheelView.commit.revision,
        );
        controller.submitRenderer(rendererPresentedSubmission(wheelPresented));
        await controller.flushRendererSubmissions();
        formatter.present(wheelPresented);
        final zoomRelease =
            controller.pollRenderer()! as NativeHandleReleaseUpdate;
        formatter.releaseHandles(zoomRelease.release);
        expect(wheelView.commit.scroll.single.offsetY, 115);
        expect(zoomedView.isRetired, isTrue);

        await controller.dispatchKeyEvent(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 240,
          viewportHeight: 160,
          eventType: 'keydown',
          event: const BrowserKeyEvent(key: 'PageDown', code: 'PageDown'),
        );
        await controller.publishRendererSnapshot(
          contextId: contextId,
          documentId: state.documentId,
          viewportWidth: 240,
          viewportHeight: 160,
          viewportGeneration: 4,
          pageZoom: zoomedState.pageZoom,
        );
        final keyUpdate =
            controller.pollRenderer()! as NativeMutationBatchUpdate;
        final keyScroll = keyUpdate.batch.mutations
            .whereType<SetRenderScrollIntent>()
            .single
            .intent;
        expect(
          keyScroll.point.y,
          greaterThan(wheelView.commit.scroll.single.offsetY),
        );
        final keyView = await _applyMutation(controller, formatter, keyUpdate);
        final keyPresented = RenderPresented(
          contextId: contextId,
          documentId: state.documentId,
          commitId: keyView.commit.commitId,
          revision: keyView.commit.revision,
        );
        controller.submitRenderer(rendererPresentedSubmission(keyPresented));
        await controller.flushRendererSubmissions();
        formatter.present(keyPresented);
        final wheelRelease =
            controller.pollRenderer()! as NativeHandleReleaseUpdate;
        formatter.releaseHandles(wheelRelease.release);
        expect(keyView.commit.scroll.single.offsetY, keyScroll.point.y);
        expect(wheelView.isRetired, isTrue);

        await controller.publishRendererSnapshot(
          contextId: contextId,
          documentId: state.documentId,
          viewportWidth: 300,
          viewportHeight: 180,
          viewportGeneration: 5,
          pageZoom: zoomedState.pageZoom,
        );
        final resizedUpdate =
            controller.pollRenderer()! as NativeMutationBatchUpdate;
        expect(resizedUpdate.batch.targetRevision.viewportGeneration, 5);
        final resizedViewport = resizedUpdate.batch.mutations
            .whereType<SetRenderViewport>()
            .single
            .viewport;
        expect(resizedViewport.width, 300);
        expect(resizedViewport.height, 180);
        final resizedView = await _applyMutation(
          controller,
          formatter,
          resizedUpdate,
        );
        final resizedPresented = RenderPresented(
          contextId: contextId,
          documentId: state.documentId,
          commitId: resizedView.commit.commitId,
          revision: resizedView.commit.revision,
        );
        controller.submitRenderer(
          rendererPresentedSubmission(resizedPresented),
        );
        await controller.flushRendererSubmissions();
        formatter.present(resizedPresented);
        final keyRelease =
            controller.pollRenderer()! as NativeHandleReleaseUpdate;
        formatter.releaseHandles(keyRelease.release);
        expect(
          resizedView.commit.commitId,
          greaterThan(zoomedView.commit.commitId),
        );
        expect(resizedView.commit.viewport.width, 300);
        expect(resizedView.commit.viewport.height, 180);
        expect(keyView.isRetired, isTrue);

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
        final valuedName = valuedAccessibility.nodes.singleWhere(
          (node) => node.id == name.id,
        );
        await controller.dispatchAccessibilityFocus(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 64,
          viewportHeight: 48,
          sourceGeneration: valuedAccessibility.sourceGeneration,
          generation: valuedAccessibility.generation,
          nodeId: valuedName.id,
        );
        await controller.dispatchTextInput(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 64,
          viewportHeight: 48,
          state: const BrowserTextInputState(
            text: 'Adaに',
            selection: BrowserAccessibilityTextSelection(
              baseOffset: 4,
              extentOffset: 4,
            ),
            composing: BrowserAccessibilityTextSelection(
              baseOffset: 3,
              extentOffset: 4,
            ),
          ),
        );
        await controller.dispatchTextInput(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 64,
          viewportHeight: 48,
          state: const BrowserTextInputState(
            text: 'Adaに',
            selection: BrowserAccessibilityTextSelection(
              baseOffset: 4,
              extentOffset: 4,
            ),
          ),
        );
        final composedInput = await controller.accessibilitySnapshot(
          contextId: contextId,
          documentId: state.documentId,
          viewportWidth: 64,
          viewportHeight: 48,
        );
        expect(
          composedInput.nodes.any(
            (node) =>
                node.id == name.id &&
                node.value == 'Adaに' &&
                node.textSelection?.baseOffset == 4,
          ),
          isTrue,
        );

        final editor = composedInput.nodes.singleWhere(
          (node) => node.label == 'Editor',
        );
        await controller.dispatchAccessibilityFocus(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 64,
          viewportHeight: 48,
          sourceGeneration: composedInput.sourceGeneration,
          generation: composedInput.generation,
          nodeId: editor.id,
        );
        await controller.dispatchTextInput(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 64,
          viewportHeight: 48,
          state: const BrowserTextInputState(
            text: 'draft🦊',
            selection: BrowserAccessibilityTextSelection(
              baseOffset: 7,
              extentOffset: 7,
            ),
            composing: BrowserAccessibilityTextSelection(
              baseOffset: 5,
              extentOffset: 7,
            ),
          ),
        );
        await controller.dispatchTextInput(
          contextId: contextId,
          documentId: state.documentId,
          runtimeContextId: state.runtimeContextId!,
          viewportWidth: 64,
          viewportHeight: 48,
          state: const BrowserTextInputState(
            text: 'draft🦊',
            selection: BrowserAccessibilityTextSelection(
              baseOffset: 7,
              extentOffset: 7,
            ),
          ),
        );
        final composedEditor = await controller.accessibilitySnapshot(
          contextId: contextId,
          documentId: state.documentId,
          viewportWidth: 64,
          viewportHeight: 48,
        );
        expect(
          composedEditor.nodes.any(
            (node) =>
                node.id == editor.id &&
                node.value == 'draft🦊' &&
                node.textSelection?.baseOffset == 7,
          ),
          isTrue,
        );
      } finally {
        formatter.dispose();
        await controller.shutdown();
        await profile.delete(recursive: true);
      }
    },
    skip: libraryPath == null
        ? 'Set VIXEN_FFI_LIBRARY to run the native integration smoke test.'
        : false,
  );
}

Future<FormatterCommitView> _applyMutation(
  NativeBrowserController controller,
  VixenFormatter formatter,
  NativeMutationBatchUpdate update,
) async {
  final result = await formatter.applyMutationBatch(
    update.batch,
    beforePublish: (commit) {
      controller.submitRenderer(rendererCommitSubmission(commit));
    },
  );
  expect(result, isA<RenderApplied>());
  await controller.flushRendererSubmissions();
  return (result as RenderApplied).view;
}
