import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/fake/scripted_browser_controller.dart';
import 'package:vixen_shell/src/bridge/native/native_renderer_protocol.dart';
import 'package:vixen_shell/src/bridge/render_models.dart';
import 'package:vixen_shell/src/renderer/formatter.dart';
import 'package:vixen_shell/src/renderer/renderer_broker_service.dart';

import 'support/r3_fixture.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('ordinary snapshot and mutation flow submit atomic commits', () async {
    final transport = ScriptedBrowserController();
    final formatter = VixenFormatter();
    addTearDown(formatter.dispose);
    final service = RendererBrokerService(
      transport: transport,
      formatter: formatter,
    );

    transport.enqueueRendererRequest(NativeFullSnapshotUpdate(r3Snapshot()));
    expect(await service.serviceNext(), isTrue);
    expect(transport.rendererResponses.single['type'], 'renderer_submission');
    final first = formatter.acceptedView!;

    transport.enqueueRendererRequest(NativeMutationBatchUpdate(r3Mutation()));
    expect(await service.serviceNext(), isTrue);
    expect(transport.rendererResponses.length, 2);
    expect(formatter.sourceRevision, r3Revision(2));
    expect(first.isRetired, isTrue);
    final latest = formatter.acceptedView!;
    final release = RenderHandleRelease(
      commitId: latest.commit.commitId,
      hitTestHandle: latest.commit.hitTestHandle,
      textQueryHandle: latest.commit.textQueryHandle,
    );
    transport.enqueueRendererRequest(NativeHandleReleaseUpdate(release));
    await service.serviceNext();
    expect(latest.isRetired, isTrue);
    expect(formatter.acceptedView, isNull);
    transport.enqueueRendererRequest(NativeHandleReleaseUpdate(release));
    await service.serviceNext();
  });

  test(
    'broker requests use exact accepted and displayed commit identities',
    () async {
      final transport = ScriptedBrowserController();
      final formatter = VixenFormatter();
      addTearDown(formatter.dispose);
      final service = RendererBrokerService(
        transport: transport,
        formatter: formatter,
      );
      transport.enqueueRendererRequest(NativeFullSnapshotUpdate(r3Snapshot()));
      await service.serviceNext();
      final view = formatter.acceptedView!;

      transport.enqueueRendererRequest(
        NativeEnsureLayoutRequest(10, view.commit.revision),
      );
      await service.serviceNext();
      expect(_responseType(transport.rendererResponses.last), 'commit');

      formatter.present(
        RenderPresented(
          contextId: 1,
          documentId: 2,
          commitId: view.commit.commitId,
          revision: view.commit.revision,
        ),
      );
      final image = view.commit.geometry.singleWhere(
        (entry) => entry.nodeId == 9,
      );
      transport.enqueueRendererRequest(
        NativeHitTestRequest(
          11,
          RenderHitTestQuery(
            queryId: 12,
            contextId: 1,
            documentId: 2,
            displayedCommitId: view.commit.commitId,
            revision: view.commit.revision,
            handle: view.commit.hitTestHandle,
            point: RenderPoint(image.borderBox.x + 1, image.borderBox.y + 1),
          ),
        ),
      );
      await service.serviceNext();
      expect(_responseType(transport.rendererResponses.last), 'hit_test');

      transport.enqueueRendererRequest(
        NativeEnsureLayoutRequest(13, r3Revision(2)),
      );
      await service.serviceNext();
      expect(_responseType(transport.rendererResponses.last), 'failed');
    },
  );

  test('fake renderer transport enforces its queue bound', () {
    final transport = ScriptedBrowserController();
    for (var index = 0; index < renderBrokerQueueCapacity; index++) {
      transport.enqueueRendererRequest(
        NativeEnsureLayoutRequest(index + 1, r3Revision(1)),
      );
    }
    expect(
      () => transport.enqueueRendererRequest(
        NativeEnsureLayoutRequest(100, r3Revision(1)),
      ),
      throwsA(isA<RenderProtocolException>()),
    );
  });
}

String? _responseType(Map<String, Object?> envelope) =>
    (envelope['response'] as Map<String, Object?>?)?['type'] as String?;
