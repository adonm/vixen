import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/native/native_protocol.dart';
import 'package:vixen_shell/src/bridge/native/native_renderer_protocol.dart';
import 'package:vixen_shell/src/bridge/render_models.dart';

void main() {
  test('Rust ensure-layout golden decodes to exact immutable revision', () {
    final envelope = decodeNativeJson(
      Uint8List.fromList(
        utf8.encode(
          '{"v":1,"type":"renderer_request","request_id":7,'
          '"request":{"type":"ensure_layout","required_revision":{'
          '"context_id":1,"document_id":2,"source_generation":3,'
          '"style_generation":4,"viewport_generation":5,'
          '"resource_generation":6}}}',
        ),
      ),
    );
    final request = decodeRendererRequest(envelope);
    expect(request, isA<NativeEnsureLayoutRequest>());
    final ensure = request as NativeEnsureLayoutRequest;
    expect(ensure.requestId, 7);
    expect(
      ensure.requiredRevision,
      const RenderRevision(
        contextId: 1,
        documentId: 2,
        sourceGeneration: 3,
        styleGeneration: 4,
        viewportGeneration: 5,
        resourceGeneration: 6,
      ),
    );
  });

  test('Dart cancellation golden matches strict Rust response shape', () {
    final response = rendererCancelledResponse(7, 'stop');
    expect(jsonDecode(utf8.decode(encodeRendererResponse(response))), {
      'v': 1,
      'type': 'renderer_response',
      'request_id': 7,
      'response': {'type': 'cancelled', 'reason': 'stop'},
    });
    expect(
      () => rendererCancelledResponse(7, 'unknown'),
      throwsA(isA<RenderProtocolException>()),
    );
  });

  test('Rust hit-test request and Dart target response preserve identity', () {
    final request = decodeRendererRequest(
      decodeNativeJson(
        Uint8List.fromList(
          utf8.encode(
            '{"v":1,"type":"renderer_request","request_id":8,'
            '"request":{"type":"hit_test","context_id":1,'
            '"document_id":2,"displayed_commit_id":9,"revision":{'
            '"context_id":1,"document_id":2,"source_generation":3,'
            '"style_generation":4,"viewport_generation":5,'
            '"resource_generation":6},"handle":10,"query_id":11,'
            '"point":{"x":12.5,"y":13.5}}}',
          ),
        ),
      ),
    );
    final hit = request as NativeHitTestRequest;
    expect(hit.query.point.x, 12.5);
    final response = rendererHitTestResponse(
      hit.requestId,
      RenderInputTarget(
        queryId: hit.query.queryId,
        contextId: hit.query.contextId,
        documentId: hit.query.documentId,
        displayedCommitId: hit.query.displayedCommitId,
        revision: hit.query.revision,
        handle: hit.query.handle,
        nodeId: 14,
        fragmentId: 15,
        viewportPoint: hit.query.point,
        localPoint: const RenderPoint(1.5, 2.5),
      ),
    );
    expect(response['request_id'], 8);
    expect(
      (response['response']! as Map<String, Object?>)['target'],
      containsPair('fragment_id', 15),
    );
  });

  test('Rust text-query variants decode and Dart results encode exactly', () {
    final request = decodeRendererRequest(
      decodeNativeJson(
        Uint8List.fromList(
          utf8.encode(
            '{"v":1,"type":"renderer_request","request_id":12,'
            '"request":{"type":"text_query","context_id":1,'
            '"document_id":2,"commit_id":9,"revision":{'
            '"context_id":1,"document_id":2,"source_generation":3,'
            '"style_generation":4,"viewport_generation":5,'
            '"resource_generation":6},"handle":10,'
            '"allow_truncation":false,"queries":['
            '{"query_id":20,"node_id":30,"kind":{'
            '"type":"offset_for_point","point":{"x":1.0,"y":2.0}}},'
            '{"query_id":21,"node_id":30,"kind":{'
            '"type":"caret_for_offset","utf16_offset":3,'
            '"affinity":"downstream"}},'
            '{"query_id":22,"node_id":30,"kind":{'
            '"type":"range_boxes","utf16_start":1,"utf16_end":4}}]}}',
          ),
        ),
      ),
    ) as NativeTextQueryRequest;
    expect(request.batch.queries.map((query) => query.kind), [
      isA<RenderOffsetForPoint>(),
      isA<RenderCaretForOffset>(),
      isA<RenderRangeBoxes>(),
    ]);

    final response = rendererTextQueryResponse(
      request.requestId,
      RenderTextQueryBatchResult(
        contextId: 1,
        documentId: 2,
        commitId: 9,
        revision: request.batch.revision,
        results: [
          const RenderTextQueryResult(
            queryId: 20,
            value: RenderTextOffsetValue(2, RenderTextAffinity.downstream),
          ),
          const RenderTextQueryResult(
            queryId: 21,
            value: RenderTextCaretValue(
              RenderRect(4, 5, 1, 10),
              RenderTextAffinity.downstream,
            ),
          ),
          RenderTextQueryResult(
            queryId: 22,
            value: RenderTextRangeBoxesValue([
              const RenderTextBox(
                rect: RenderRect(4, 5, 6, 7),
                direction: RenderTextDirection.ltr,
              ),
            ]),
          ),
        ],
      ),
    );
    expect(jsonDecode(utf8.decode(encodeRendererResponse(response))), response);
  });

  test(
    'renderer request decoder rejects stale identity and unknown fields',
    () {
      expect(
        () => decodeRendererRequest({
          'v': 1,
          'type': 'renderer_request',
          'request_id': 1,
          'request': {
            'type': 'hit_test',
            'context_id': 9,
            'document_id': 2,
            'displayed_commit_id': 3,
            'revision': const RenderRevision(
              contextId: 1,
              documentId: 2,
              sourceGeneration: 1,
              styleGeneration: 1,
              viewportGeneration: 1,
              resourceGeneration: 1,
            ).toWire(),
            'handle': 4,
            'query_id': 5,
            'point': {'x': 0, 'y': 0},
          },
        }),
        throwsA(isA<RenderProtocolException>()),
      );
    },
  );

  test('Rust full-snapshot update decodes every source field', () {
    final update = decodeRendererMessage(
      decodeNativeJson(
        Uint8List.fromList(
          utf8.encode(
            '{"v":1,"type":"renderer_update","update":{'
            '"type":"full_snapshot","revision":{"context_id":1,'
            '"document_id":2,"source_generation":3,"style_generation":4,'
            '"viewport_generation":5,"resource_generation":6},'
            '"viewport":{"width":240,"height":160,"device_scale":1.0,'
            '"page_zoom":1.0},"nodes":[{"id":10,"parent_id":null,'
            '"sibling_index":0,"depth":0,"kind":{"type":"element",'
            '"local_name":"main"},"styles":[{"name":"display",'
            '"value":"block"}],"resource_ids":[9],"semantic":null}],'
            '"resources":[{"id":9,"kind":"image","mime":"image/png",'
            '"bytes":"AAEC"}],"scroll_intents":[]}}',
          ),
        ),
      ),
    ) as NativeFullSnapshotUpdate;
    expect(update.snapshot.nodes.single.name, 'main');
    expect(update.snapshot.nodes.single.styles, {'display': 'block'});
    expect(update.snapshot.resources.single.bytes, [0, 1, 2]);
  });

  test('mutation variants and submissions retain exact identity', () {
    final update = decodeRendererUpdate({
      'v': 1,
      'type': 'renderer_update',
      'update': {
        'type': 'mutation_batch',
        'base_revision': _revision(1),
        'target_revision': {
          ..._revision(1),
          'source_generation': 2,
          'viewport_generation': 2,
          'resource_generation': 2,
        },
        'mutations': [
          {
            'type': 'set_viewport',
            'viewport': {
              'width': 320,
              'height': 200,
              'device_scale': 1.0,
              'page_zoom': 1.0,
            },
          },
          {
            'type': 'upsert_node',
            'node': {
              'id': 3,
              'parent_id': null,
              'sibling_index': 0,
              'depth': 0,
              'kind': {'type': 'element', 'local_name': 'main'},
              'styles': <Object?>[],
              'resource_ids': <Object?>[],
              'semantic': null,
            },
          },
          {'type': 'remove_node', 'node_id': 3},
          {
            'type': 'upsert_resource',
            'resource': {
              'id': 4,
              'kind': 'image',
              'mime': 'image/png',
              'bytes': 'AA==',
            },
          },
          {'type': 'remove_resource', 'resource_id': 4},
          {
            'type': 'set_scroll_intent',
            'intent': {
              'scroll_node_id': 5,
              'node_id': 3,
              'kind': 'to',
              'point': {'x': 0, 'y': 10},
            },
          },
          {'type': 'remove_scroll_intent', 'scroll_node_id': 5},
        ],
      },
    }) as NativeMutationBatchUpdate;
    expect(update.batch.mutations, [
      isA<SetRenderViewport>(),
      isA<UpsertRenderNode>(),
      isA<RemoveRenderNode>(),
      isA<UpsertRenderResource>(),
      isA<RemoveRenderResource>(),
      isA<SetRenderScrollIntent>(),
      isA<RemoveRenderScrollIntent>(),
    ]);
    final presented = rendererPresentedSubmission(
      const RenderPresented(
        contextId: 1,
        documentId: 2,
        commitId: 9,
        revision: _wireRevision,
      ),
    );
    expect(presented['type'], 'renderer_submission');
    expect((presented['submission']! as Map<String, Object?>)['commit_id'], 9);
  });
}

Map<String, Object?> _revision(int generation) => {
  'context_id': 1,
  'document_id': 2,
  'source_generation': generation,
  'style_generation': generation,
  'viewport_generation': 1,
  'resource_generation': generation,
};

const _wireRevision = RenderRevision(
  contextId: 1,
  documentId: 2,
  sourceGeneration: 2,
  styleGeneration: 1,
  viewportGeneration: 2,
  resourceGeneration: 2,
);
