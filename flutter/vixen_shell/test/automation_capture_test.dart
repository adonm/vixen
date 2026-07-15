import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/automation/automation_capture.dart';
import 'package:vixen_shell/src/bridge/browser_models.dart';
import 'package:vixen_shell/src/bridge/fake/scripted_browser_controller.dart';
import 'package:vixen_shell/src/bridge/native/native_renderer_protocol.dart';
import 'package:vixen_shell/src/bridge/render_models.dart';
import 'package:vixen_shell/src/shell/shell_coordinator.dart';

import 'support/r3_fixture.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test(
    'writes one exact presented commit through the Flutter renderer',
    () async {
      final state = BrowsingContextState(
        contextId: 1,
        mainFrameId: 10,
        documentId: 2,
        runtimeContextId: 100,
        activeNavigationId: null,
        url: 'file:///fixture.html',
        title: 'Fixture',
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
      final coordinator = ShellCoordinator(
        controller,
        initialUrl: state.url,
        useProfileSession: false,
      );
      addTearDown(coordinator.close);
      await coordinator.start();
      controller.enqueueRendererRequest(NativeFullSnapshotUpdate(r3Snapshot()));
      coordinator.updatePhysicalViewport(240, 160);
      for (
        var attempt = 0;
        attempt < 100 && coordinator.rendererView == null;
        attempt++
      ) {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }

      final view = coordinator.rendererView!;
      final png = await coordinator.capturePresentedRendererCommitPng(view);
      expect(identical(coordinator.presentedRendererView, view), isTrue);
      expect(
        controller.commands.map((command) => command.type),
        contains('accessibility_snapshot'),
      );
      expect(
        controller.commands.map((command) => command.type),
        isNot(contains('load_profile_session')),
      );
      final presented = controller.rendererResponses.singleWhere(
        (response) =>
            response['type'] == 'renderer_submission' &&
            (response['submission'] as Map)['type'] == 'presented',
      );
      expect(
        (presented['submission'] as Map)['commit_id'],
        view.commit.commitId,
      );
      expect(png.sublist(0, 8), [137, 80, 78, 71, 13, 10, 26, 10]);
      expect(_uint32(png, 16), 240);
      expect(_uint32(png, 20), 160);

      final directory = await Directory.systemTemp.createTemp(
        'vixen-automation-capture-',
      );
      addTearDown(() => directory.delete(recursive: true));
      final output = File('${directory.path}/capture.png');
      await const AutomationCaptureWriter().write(output.path, png);
      expect(await output.length(), png.length);

      coordinator.updateApplicationLifecycle(BrowserHostLifecycle.hidden);
      await expectLater(
        coordinator.capturePresentedRendererCommitPng(view),
        throwsA(
          isA<RenderProtocolException>().having(
            (error) => error.code,
            'code',
            'render.stale',
          ),
        ),
      );
      await coordinator.close();
      expect(
        controller.commands.map((command) => command.type),
        isNot(contains('save_current_profile_session')),
      );
    },
  );
}

int _uint32(List<int> bytes, int offset) =>
    bytes[offset] << 24 |
    bytes[offset + 1] << 16 |
    bytes[offset + 2] << 8 |
    bytes[offset + 3];
