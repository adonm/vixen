import 'dart:io';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/render_models.dart';
import 'package:vixen_shell/src/renderer/formatter.dart';
import 'package:vixen_shell/src/renderer/formatter_painter.dart';

import 'support/r3_fixture.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('full snapshot produces one atomic formatter commit', () async {
    final formatter = VixenFormatter();
    addTearDown(formatter.dispose);
    final result = await formatter.acceptFullSnapshot(r3Snapshot());
    final view = (result as RenderApplied).view;

    expect(view.commit.revision, r3Revision(1));
    expect(view.commit.viewport.width, 240);
    expect(
      view.commit.geometry.map((entry) => entry.nodeId),
      containsAll([1, 2, 4, 6, 8, 9]),
    );
    expect(view.commit.scroll.single.maxOffsetY, greaterThan(0));
    expect(view.commit.semantics.map((entry) => entry.semanticNodeId), [
      1,
      2,
      3,
    ]);
    expect(
      view.commit.semantics
          .singleWhere((entry) => entry.semanticNodeId == 2)
          .rects
          .length,
      greaterThan(1),
    );
    expect(view.commit.hitTestHandle, isPositive);
    expect(view.commit.textQueryHandle, isPositive);
    expect(formatter.displayedView, isNull);
    final article = view.commit.geometry.singleWhere(
      (entry) => entry.nodeId == 2,
    );
    expect(article.contentBox.x, article.borderBox.x + 12);
    expect(article.contentBox.y, article.borderBox.y + 12);
  });

  test('Paragraph owns wrapping, UTF-16 ranges, and point offsets', () async {
    final formatter = VixenFormatter();
    addTearDown(formatter.dispose);
    final view = (await formatter.acceptFullSnapshot(
      r3Snapshot(),
    ) as RenderApplied).view;
    final boxes = view.rangeBoxes(
      handle: view.commit.textQueryHandle,
      nodeId: 6,
      start: 0,
      end: 80,
    );
    expect(boxes.length, greaterThan(1));
    final offset = view.offsetForPoint(
      handle: view.commit.textQueryHandle,
      nodeId: 6,
      point: ui.Offset(boxes.first.x + 2, boxes.first.y + 2),
    );
    expect(offset, inInclusiveRange(0, 4));
    final batchResult = view.answerTextQueries(
      RenderTextQueryBatch(
        contextId: 1,
        documentId: 2,
        commitId: view.commit.commitId,
        revision: view.commit.revision,
        handle: view.commit.textQueryHandle,
        allowTruncation: false,
        queries: [
          RenderTextQuery(
            queryId: 1,
            nodeId: 6,
            kind: RenderOffsetForPoint(
              RenderPoint(boxes.first.x + 2, boxes.first.y + 2),
            ),
          ),
          const RenderTextQuery(
            queryId: 2,
            nodeId: 6,
            kind: RenderCaretForOffset(3, RenderTextAffinity.downstream),
          ),
          const RenderTextQuery(
            queryId: 3,
            nodeId: 6,
            kind: RenderRangeBoxes(0, 80),
          ),
        ],
      ),
    );
    expect(batchResult.results[0].value, isA<RenderTextOffsetValue>());
    final caret = batchResult.results[1].value as RenderTextCaretValue;
    expect(caret.rect.width, 1);
    expect(
      (batchResult.results[2].value as RenderTextRangeBoxesValue).boxes.length,
      greaterThan(1),
    );
    expect(
      () => view.rangeBoxes(
        handle: view.commit.textQueryHandle + 1,
        nodeId: 6,
        start: 0,
        end: 1,
      ),
      throwsA(isA<RenderProtocolException>()),
    );
  });

  test(
    'scene capture contains Canvas backgrounds and decoded PNG pixels',
    () async {
      const requireImpeller = bool.fromEnvironment('VIXEN_REQUIRE_IMPELLER');
      if (requireImpeller) {
        expect(Platform.executableArguments, contains('--enable-impeller'));
      }
      final formatter = VixenFormatter();
      addTearDown(formatter.dispose);
      final view = (await formatter.acceptFullSnapshot(
        r3Snapshot(),
      ) as RenderApplied).view;
      final image = await view.capture();
      addTearDown(image.dispose);
      final bytes = (await image.toByteData(
        format: ui.ImageByteFormat.rawRgba,
      ))!;
      final rgba = bytes.buffer.asUint8List();

      expect(_pixel(rgba, 240, 1, 1), [240, 244, 248, 255]);
      expect(_pixel(rgba, 240, 13, 13), [32, 48, 64, 255]);
      final imageRect = view.commit.geometry
          .singleWhere((entry) => entry.nodeId == 9)
          .borderBox;
      final left = imageRect.x.toInt();
      final top = imageRect.y.toInt();
      expect(_pixel(rgba, 240, left + 4, top + 4).take(3), [255, 0, 0]);
      expect(_pixel(rgba, 240, left + 27, top + 4).take(3), [0, 255, 0]);
      expect(_pixel(rgba, 240, left + 4, top + 27).take(3), [0, 0, 255]);
      expect(_pixel(rgba, 240, left + 27, top + 27).take(3), [255, 255, 0]);
      expect(
        _fnv1a64(rgba),
        requireImpeller ? 757077222971174478 : 6568249825582439392,
      );
    },
  );

  test(
    'geometry hit testing uses the displayed commit handle and clips',
    () async {
      final formatter = VixenFormatter();
      addTearDown(formatter.dispose);
      final view = (await formatter.acceptFullSnapshot(
        r3Snapshot(),
      ) as RenderApplied).view;
      formatter.present(
        RenderPresented(
          contextId: 1,
          documentId: 2,
          commitId: view.commit.commitId,
          revision: view.commit.revision,
        ),
      );
      final imageRect = view.commit.geometry
          .singleWhere((entry) => entry.nodeId == 9)
          .borderBox;
      final hit = view.hitTest(
        ui.Offset(imageRect.x + 16, imageRect.y + 16),
        handle: view.commit.hitTestHandle,
      );
      expect(hit?.nodeId, 9);
      final target = view.answerHitTest(
        RenderHitTestQuery(
          queryId: 7,
          contextId: 1,
          documentId: 2,
          displayedCommitId: view.commit.commitId,
          revision: view.commit.revision,
          handle: view.commit.hitTestHandle,
          point: RenderPoint(imageRect.x + 16, imageRect.y + 16),
        ),
      );
      expect(target?.nodeId, 9);
      expect(target?.queryId, 7);
      final styledRun = view.commit.geometry.firstWhere(
        (entry) =>
            entry.nodeId == 6 &&
            entry.borderBox.width > 0 &&
            entry.borderBox.y < view.viewport.height,
      );
      expect(
        view
            .answerHitTest(
              RenderHitTestQuery(
                queryId: 8,
                contextId: 1,
                documentId: 2,
                displayedCommitId: view.commit.commitId,
                revision: view.commit.revision,
                handle: view.commit.hitTestHandle,
                point: RenderPoint(
                  styledRun.borderBox.x + styledRun.borderBox.width / 2,
                  styledRun.borderBox.y + styledRun.borderBox.height / 2,
                ),
              ),
            )
            ?.nodeId,
        6,
      );
      expect(
        view.hitTest(
          ui.Offset(imageRect.x + 16, imageRect.y + 16),
          handle: view.commit.hitTestHandle + 1,
        ),
        isNull,
      );
      expect(
        view.hitTest(
          const ui.Offset(10, 170),
          handle: view.commit.hitTestHandle,
        ),
        isNull,
      );
    },
  );

  test('mutation stages and presentation replaces one whole commit', () async {
    final formatter = VixenFormatter();
    addTearDown(formatter.dispose);
    final first = (await formatter.acceptFullSnapshot(
      r3Snapshot(),
    ) as RenderApplied).view;
    formatter.present(
      RenderPresented(
        contextId: 1,
        documentId: 2,
        commitId: first.commit.commitId,
        revision: first.commit.revision,
      ),
    );
    final second = (await formatter.applyMutationBatch(
      r3Mutation(),
    ) as RenderApplied).view;
    expect(second.commit.revision, r3Revision(2));
    expect(formatter.displayedView?.commit.commitId, first.commit.commitId);
    expect(first.isRetired, isFalse);

    formatter.present(
      RenderPresented(
        contextId: 1,
        documentId: 2,
        commitId: second.commit.commitId,
        revision: second.commit.revision,
      ),
    );
    expect(formatter.displayedView?.commit.commitId, second.commit.commitId);
    expect(first.isRetired, isTrue);
    expect(second.semanticRegions.first.descriptor.name, 'Updated Vixen');
  });

  test(
    'missed base requests deterministic resync without changing display',
    () async {
      final formatter = VixenFormatter();
      addTearDown(formatter.dispose);
      final first = (await formatter.acceptFullSnapshot(
        r3Snapshot(),
      ) as RenderApplied).view;
      formatter.present(
        RenderPresented(
          contextId: 1,
          documentId: 2,
          commitId: first.commit.commitId,
          revision: first.commit.revision,
        ),
      );
      final second = (await formatter.applyMutationBatch(
        r3Mutation(),
      ) as RenderApplied).view;
      formatter.present(
        RenderPresented(
          contextId: 1,
          documentId: 2,
          commitId: second.commit.commitId,
          revision: second.commit.revision,
        ),
      );
      final result = await formatter.applyMutationBatch(r3Mutation());
      final resync = (result as RenderResyncRequired).request;
      expect(resync.reason, 'missed_base_revision');
      expect(resync.currentRevision, r3Revision(2));
      expect(resync.rejectedBaseRevision, r3Revision(1));
      expect(formatter.displayedView?.commit.commitId, second.commit.commitId);
    },
  );

  test('reset retires state and accepts a full resync', () async {
    final formatter = VixenFormatter();
    addTearDown(formatter.dispose);
    final first = (await formatter.acceptFullSnapshot(
      r3Snapshot(),
    ) as RenderApplied).view;
    formatter.present(
      RenderPresented(
        contextId: 1,
        documentId: 2,
        commitId: first.commit.commitId,
        revision: first.commit.revision,
      ),
    );
    final reset = formatter.reset(contextId: 1, documentId: 2);
    expect(reset.reason, 'renderer_reset');
    expect(formatter.displayedView, isNull);
    expect(first.isRetired, isTrue);
    final fresh = (await formatter.acceptFullSnapshot(
      r3Snapshot(generation: 2),
    ) as RenderApplied).view;
    expect(fresh.commit.commitId, greaterThan(first.commit.commitId));
  });

  test('equal conflicting and regressing snapshots fail closed', () async {
    final formatter = VixenFormatter();
    addTearDown(formatter.dispose);
    final first = (await formatter.acceptFullSnapshot(
      r3Snapshot(),
    ) as RenderApplied).view;
    final idempotent = (await formatter.acceptFullSnapshot(
      r3Snapshot(),
    ) as RenderApplied).view;
    expect(identical(idempotent, first), isTrue);
    await expectLater(
      formatter.acceptFullSnapshot(r3Snapshot(updated: true)),
      throwsA(isA<RenderProtocolException>()),
    );
    expect(first.isRetired, isFalse);
    await formatter.applyMutationBatch(r3Mutation());
    await expectLater(
      formatter.acceptFullSnapshot(r3Snapshot()),
      throwsA(isA<RenderProtocolException>()),
    );
    expect(formatter.sourceRevision, r3Revision(2));
  });

  test(
    'failed and superseded builds never publish mixed source state',
    () async {
      final formatter = VixenFormatter();
      addTearDown(formatter.dispose);
      final first = (await formatter.acceptFullSnapshot(
        r3Snapshot(),
      ) as RenderApplied).view;
      final invalidArticle = r3Snapshot(generation: 2).nodes[1].copyWith(
        styles: const {
          'margin': '12',
          'padding': '12',
          'background': 'not-a-color',
        },
      );
      await expectLater(
        formatter.applyMutationBatch(
          RenderMutationBatch(
            baseRevision: r3Revision(1),
            targetRevision: r3Revision(2),
            mutations: [UpsertRenderNode(invalidArticle)],
          ),
        ),
        throwsA(isA<RenderProtocolException>()),
      );
      expect(formatter.sourceRevision, r3Revision(1));
      expect(first.isRetired, isFalse);

      final superseded = formatter.acceptFullSnapshot(
        r3Snapshot(generation: 2),
      );
      final latest = formatter.acceptFullSnapshot(r3Snapshot(generation: 3));
      await expectLater(superseded, throwsA(isA<RenderProtocolException>()));
      final latestView = (await latest as RenderApplied).view;
      expect(formatter.sourceRevision, r3Revision(3));
      expect(latestView.commit.revision, r3Revision(3));
    },
  );

  test('CustomPainter semantics use only the displayed commit', () async {
    final formatter = VixenFormatter();
    addTearDown(formatter.dispose);
    final first = (await formatter.acceptFullSnapshot(
      r3Snapshot(),
    ) as RenderApplied).view;
    formatter.present(
      RenderPresented(
        contextId: 1,
        documentId: 2,
        commitId: first.commit.commitId,
        revision: first.commit.revision,
      ),
    );
    final firstSemantics = RenderCommitPainter(formatter.displayedView!)
        .semanticsBuilder(first.viewport);
    expect(
      firstSemantics.map((entry) => entry.properties.label),
      containsAll(['Vixen renderer', 'Read more']),
    );
    expect(firstSemantics.first.properties.header, isTrue);

    final second = (await formatter.applyMutationBatch(
      r3Mutation(),
    ) as RenderApplied).view;
    final stagedSemantics = RenderCommitPainter(formatter.displayedView!)
        .semanticsBuilder(first.viewport);
    expect(stagedSemantics.first.properties.label, 'Vixen renderer');
    formatter.present(
      RenderPresented(
        contextId: 1,
        documentId: 2,
        commitId: second.commit.commitId,
        revision: second.commit.revision,
      ),
    );
    final secondSemantics = RenderCommitPainter(formatter.displayedView!)
        .semanticsBuilder(second.viewport);
    expect(secondSemantics.first.properties.label, 'Updated Vixen');
  });
}

List<int> _pixel(Uint8List rgba, int width, int x, int y) {
  final offset = (y * width + x) * 4;
  return rgba.sublist(offset, offset + 4);
}

int _fnv1a64(Uint8List bytes) {
  var hash = 0xcbf29ce484222325;
  for (final byte in bytes) {
    hash ^= byte;
    hash = (hash * 0x100000001b3) & 0x7fffffffffffffff;
  }
  return hash;
}
