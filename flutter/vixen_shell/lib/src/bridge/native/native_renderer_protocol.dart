import 'dart:convert';
import 'dart:typed_data';

import '../render_models.dart';
import 'native_protocol.dart';

sealed class NativeRendererMessage {
  const NativeRendererMessage();
}

sealed class NativeRendererRequest extends NativeRendererMessage {
  const NativeRendererRequest(this.requestId);
  final int requestId;
}

final class NativeEnsureLayoutRequest extends NativeRendererRequest {
  const NativeEnsureLayoutRequest(super.requestId, this.requiredRevision);
  final RenderRevision requiredRevision;
}

final class NativeHitTestRequest extends NativeRendererRequest {
  const NativeHitTestRequest(super.requestId, this.query);
  final RenderHitTestQuery query;
}

final class NativeTextQueryRequest extends NativeRendererRequest {
  const NativeTextQueryRequest(super.requestId, this.batch);
  final RenderTextQueryBatch batch;
}

final class NativeCaptureSceneRequest extends NativeRendererRequest {
  const NativeCaptureSceneRequest({
    required int requestId,
    required this.contextId,
    required this.documentId,
    required this.displayedCommitId,
    required this.revision,
    required this.viewport,
  }) : super(requestId);

  final int contextId;
  final int documentId;
  final int displayedCommitId;
  final RenderRevision revision;
  final RenderViewport viewport;
}

final class NativeResetRendererRequest extends NativeRendererRequest {
  const NativeResetRendererRequest(
    super.requestId,
    this.contextId,
    this.documentId,
  );

  final int contextId;
  final int documentId;
}

sealed class NativeRendererUpdate extends NativeRendererMessage {
  const NativeRendererUpdate();
}

NativeRendererMessage decodeRendererMessage(Map<String, Object?> envelope) =>
    switch (envelope['type']) {
      'renderer_request' => decodeRendererRequest(envelope),
      'renderer_update' => decodeRendererUpdate(envelope),
      _ => throw const RenderProtocolException(
        'render.invalid-wire',
        'unsupported renderer message envelope',
      ),
    };

void validateRendererMessagePayload(NativeRendererMessage message) {
  final size = switch (message) {
    NativeFullSnapshotUpdate(:final snapshot) =>
      snapshot.nodes.fold(0, (size, node) => size + _nodeSourceSize(node)) +
          snapshot.resources.fold(
            0,
            (size, resource) => size + _resourceSourceSize(resource),
          ),
    NativeMutationBatchUpdate(:final batch) => batch.mutations.fold(
      0,
      (size, mutation) =>
          size +
          switch (mutation) {
            UpsertRenderNode(:final node) => _nodeSourceSize(node),
            UpsertRenderResource(:final resource) => _resourceSourceSize(
              resource,
            ),
            _ => 0,
          },
    ),
    _ => 0,
  };
  if (size > renderBrokerMaxUpdateSourceBytes) {
    throw const RenderProtocolException(
      'render.payload-too-large',
      'renderer update exceeds the transport source-byte limit',
    );
  }
}

final class NativeFullSnapshotUpdate extends NativeRendererUpdate {
  const NativeFullSnapshotUpdate(this.snapshot);
  final FullRenderSnapshot snapshot;
}

final class NativeMutationBatchUpdate extends NativeRendererUpdate {
  const NativeMutationBatchUpdate(this.batch);
  final RenderMutationBatch batch;
}

final class NativeHandleReleaseUpdate extends NativeRendererUpdate {
  const NativeHandleReleaseUpdate(this.release);
  final RenderHandleRelease release;
}

NativeRendererUpdate decodeRendererUpdate(Map<String, Object?> envelope) {
  renderKeys(envelope, const {'v', 'type', 'update'});
  if (envelope['v'] != renderProtocolVersion ||
      envelope['type'] != 'renderer_update') {
    throw const RenderProtocolException(
      'render.invalid-wire',
      'unsupported renderer update envelope',
    );
  }
  final update = renderObject(envelope['update'], 'renderer update');
  return switch (update['type']) {
    'full_snapshot' => NativeFullSnapshotUpdate(_decodeSnapshot(update)),
    'mutation_batch' => NativeMutationBatchUpdate(_decodeMutationBatch(update)),
    'handle_release' => NativeHandleReleaseUpdate(_decodeHandleRelease(update)),
    _ => throw const RenderProtocolException(
      'render.invalid-wire',
      'unsupported renderer update',
    ),
  };
}

FullRenderSnapshot _decodeSnapshot(Map<String, Object?> update) {
  renderKeys(update, const {
    'type',
    'revision',
    'viewport',
    'nodes',
    'resources',
    'scroll_intents',
  });
  final nodes = _boundedList(
    update['nodes'],
    'nodes',
    renderMaxNodes,
  ).map(_decodeNode).toList(growable: false);
  final resources = _boundedList(
    update['resources'],
    'resources',
    renderMaxResources,
  ).map(_decodeResource).toList(growable: false);
  final scrollIntents = _boundedList(
    update['scroll_intents'],
    'scroll_intents',
    renderMaxScrollEntries,
  ).map(_decodeScrollIntent).toList(growable: false);
  return FullRenderSnapshot(
    revision: RenderRevision.fromWire(update['revision']),
    viewport: RenderViewport.fromWire(update['viewport']),
    nodes: nodes,
    resources: resources,
    scrollIntents: scrollIntents,
  );
}

RenderMutationBatch _decodeMutationBatch(Map<String, Object?> update) {
  renderKeys(update, const {
    'type',
    'base_revision',
    'target_revision',
    'mutations',
  });
  final mutations =
      _boundedList(update['mutations'], 'mutations', renderMaxMutations)
          .map((value) {
            final mutation = renderObject(value, 'mutation');
            return switch (mutation['type']) {
              'set_viewport' => () {
                renderKeys(mutation, const {'type', 'viewport'});
                return SetRenderViewport(
                  RenderViewport.fromWire(mutation['viewport']),
                );
              }(),
              'upsert_node' => () {
                renderKeys(mutation, const {'type', 'node'});
                return UpsertRenderNode(_decodeNode(mutation['node']));
              }(),
              'remove_node' => () {
                renderKeys(mutation, const {'type', 'node_id'});
                return RemoveRenderNode(
                  renderPositiveInt(mutation['node_id'], 'node_id'),
                );
              }(),
              'upsert_resource' => () {
                renderKeys(mutation, const {'type', 'resource'});
                return UpsertRenderResource(
                  _decodeResource(mutation['resource']),
                );
              }(),
              'remove_resource' => () {
                renderKeys(mutation, const {'type', 'resource_id'});
                return RemoveRenderResource(
                  renderPositiveInt(mutation['resource_id'], 'resource_id'),
                );
              }(),
              'set_scroll_intent' => () {
                renderKeys(mutation, const {'type', 'intent'});
                return SetRenderScrollIntent(
                  _decodeScrollIntent(mutation['intent']),
                );
              }(),
              'remove_scroll_intent' => () {
                renderKeys(mutation, const {'type', 'scroll_node_id'});
                return RemoveRenderScrollIntent(
                  renderPositiveInt(
                    mutation['scroll_node_id'],
                    'scroll_node_id',
                  ),
                );
              }(),
              _ => throw const RenderProtocolException(
                'render.invalid-wire',
                'unsupported renderer mutation',
              ),
            };
          })
          .toList(growable: false);
  return RenderMutationBatch(
    baseRevision: RenderRevision.fromWire(update['base_revision']),
    targetRevision: RenderRevision.fromWire(update['target_revision']),
    mutations: mutations,
  );
}

RenderNode _decodeNode(Object? value) {
  final node = renderObject(value, 'render node');
  renderKeys(node, const {
    'id',
    'parent_id',
    'sibling_index',
    'depth',
    'kind',
    'styles',
    'resource_ids',
    'semantic',
  });
  final kind = renderObject(node['kind'], 'render node kind');
  late final RenderNodeKind nodeKind;
  late final String name;
  var text = '';
  switch (kind['type']) {
    case 'element':
      renderKeys(kind, const {'type', 'local_name'});
      nodeKind = RenderNodeKind.element;
      name = _boundedString(kind['local_name'], 'local_name');
    case 'text':
      renderKeys(kind, const {'type', 'text'});
      nodeKind = RenderNodeKind.text;
      name = '#text';
      text = _boundedString(kind['text'], 'text');
    case 'pseudo_before':
      renderKeys(kind, const {'type', 'text'});
      nodeKind = RenderNodeKind.pseudoBefore;
      name = '::before';
      text = _boundedString(kind['text'], 'text');
    case 'pseudo_after':
      renderKeys(kind, const {'type', 'text'});
      nodeKind = RenderNodeKind.pseudoAfter;
      name = '::after';
      text = _boundedString(kind['text'], 'text');
    default:
      throw const RenderProtocolException(
        'render.invalid-wire',
        'unsupported render node kind',
      );
  }
  final styles = <String, String>{};
  for (final value in _boundedList(
    node['styles'],
    'styles',
    renderMaxStylesPerNode,
  )) {
    final style = renderObject(value, 'style');
    renderKeys(style, const {'name', 'value'});
    final name = _boundedString(style['name'], 'style.name');
    if (styles.containsKey(name)) {
      throw const RenderProtocolException(
        'render.invalid-graph',
        'render node repeats a style name',
      );
    }
    styles[name] = _boundedString(style['value'], 'style.value');
  }
  final resourceIds = _boundedList(
    node['resource_ids'],
    'resource_ids',
    renderMaxResourcesPerNode,
  ).map((id) => renderPositiveInt(id, 'resource_id')).toList(growable: false);
  final parent = node['parent_id'];
  return RenderNode(
    id: renderPositiveInt(node['id'], 'node.id'),
    parentId: parent == null
        ? null
        : renderPositiveInt(parent, 'node.parent_id'),
    siblingIndex: renderUnsigned32(node['sibling_index'], 'sibling_index'),
    depth: renderUnsigned32(node['depth'], 'depth'),
    kind: nodeKind,
    name: name,
    text: text,
    styles: styles,
    resourceIds: resourceIds,
    semantic: node['semantic'] == null
        ? null
        : _decodeSemantic(node['semantic']),
  );
}

RenderSemanticDescriptor _decodeSemantic(Object? value) {
  final semantic = renderObject(value, 'semantic node');
  renderKeys(semantic, const {
    'id',
    'role',
    'name',
    'value',
    'action_generation',
    'actions',
  });
  final rawValue = semantic['value'];
  if (rawValue != null && rawValue is! String) {
    throw const RenderProtocolException(
      'render.invalid-wire',
      'semantic value must be a string or null',
    );
  }
  final actions =
      _boundedList(
            semantic['actions'],
            'semantic.actions',
            renderMaxSemanticActionsPerNode,
          )
          .map(
            (action) => switch (action) {
              'activate' => RenderSemanticActionKind.activate,
              'focus' => RenderSemanticActionKind.focus,
              'set_value' => RenderSemanticActionKind.setValue,
              'set_selection' => RenderSemanticActionKind.setSelection,
              'increase' => RenderSemanticActionKind.increase,
              'decrease' => RenderSemanticActionKind.decrease,
              'scroll_into_view' => RenderSemanticActionKind.scrollIntoView,
              _ => throw const RenderProtocolException(
                'render.invalid-wire',
                'unsupported semantic action',
              ),
            },
          )
          .toList(growable: false);
  return RenderSemanticDescriptor(
    id: renderPositiveInt(semantic['id'], 'semantic.id'),
    role: _boundedString(semantic['role'], 'semantic.role'),
    name: _boundedString(semantic['name'], 'semantic.name'),
    value: rawValue as String?,
    actionGeneration: renderPositiveInt(
      semantic['action_generation'],
      'semantic.action_generation',
    ),
    actions: actions,
  );
}

RenderResource _decodeResource(Object? value) {
  final resource = renderObject(value, 'render resource');
  renderKeys(resource, const {'id', 'kind', 'mime', 'bytes'});
  final encoded = resource['bytes'];
  if (encoded is! String || encoded.length > renderMaxResourceBytes * 2) {
    throw const RenderProtocolException(
      'render.limit',
      'encoded resource exceeds its transport limit',
    );
  }
  late final Uint8List bytes;
  try {
    bytes = base64Decode(encoded);
  } on FormatException {
    throw const RenderProtocolException(
      'render.invalid-wire',
      'resource bytes are not valid base64',
    );
  }
  if (bytes.length > renderMaxResourceBytes) {
    throw const RenderProtocolException(
      'render.limit',
      'resource bytes exceed the protocol limit',
    );
  }
  return RenderResource(
    id: renderPositiveInt(resource['id'], 'resource.id'),
    kind: switch (resource['kind']) {
      'image' => RenderResourceKind.image,
      'font' => RenderResourceKind.font,
      _ => throw const RenderProtocolException(
        'render.invalid-wire',
        'unsupported resource kind',
      ),
    },
    mime: _boundedString(resource['mime'], 'resource.mime'),
    bytes: bytes,
  );
}

RenderScrollIntent _decodeScrollIntent(Object? value) {
  final intent = renderObject(value, 'scroll intent');
  renderKeys(intent, const {'scroll_node_id', 'node_id', 'kind', 'point'});
  return RenderScrollIntent(
    scrollNodeId: renderPositiveInt(intent['scroll_node_id'], 'scroll_node_id'),
    nodeId: renderPositiveInt(intent['node_id'], 'node_id'),
    kind: switch (intent['kind']) {
      'by' => RenderScrollIntentKind.by,
      'to' => RenderScrollIntentKind.to,
      'restore' => RenderScrollIntentKind.restore,
      _ => throw const RenderProtocolException(
        'render.invalid-wire',
        'unsupported scroll intent kind',
      ),
    },
    point: RenderPoint.fromWire(intent['point'], 'scroll intent point'),
  );
}

RenderHandleRelease _decodeHandleRelease(Map<String, Object?> update) {
  renderKeys(update, const {
    'type',
    'commit_id',
    'hit_test_handle',
    'text_query_handle',
  });
  return RenderHandleRelease(
    commitId: renderPositiveInt(update['commit_id'], 'commit_id'),
    hitTestHandle: renderPositiveInt(
      update['hit_test_handle'],
      'hit_test_handle',
    ),
    textQueryHandle: renderPositiveInt(
      update['text_query_handle'],
      'text_query_handle',
    ),
  );
}

NativeRendererRequest decodeRendererRequest(Map<String, Object?> envelope) {
  renderKeys(envelope, const {'v', 'type', 'request_id', 'request'});
  if (envelope['v'] != renderProtocolVersion ||
      envelope['type'] != 'renderer_request') {
    throw const RenderProtocolException(
      'render.invalid-wire',
      'unsupported renderer request envelope',
    );
  }
  final requestId = renderPositiveInt(envelope['request_id'], 'request_id');
  final request = renderObject(envelope['request'], 'request');
  return switch (request['type']) {
    'ensure_layout' => _decodeEnsureLayout(requestId, request),
    'hit_test' => _decodeHitTest(requestId, request),
    'text_query' => _decodeTextQuery(requestId, request),
    'capture_scene' => _decodeCaptureScene(requestId, request),
    'reset' => _decodeReset(requestId, request),
    _ => throw RenderProtocolException(
      'render.invalid-wire',
      'unsupported renderer request ${request['type']}',
    ),
  };
}

NativeResetRendererRequest _decodeReset(
  int requestId,
  Map<String, Object?> request,
) {
  renderKeys(request, const {'type', 'context_id', 'document_id'});
  return NativeResetRendererRequest(
    requestId,
    renderPositiveInt(request['context_id'], 'context_id'),
    renderPositiveInt(request['document_id'], 'document_id'),
  );
}

NativeCaptureSceneRequest _decodeCaptureScene(
  int requestId,
  Map<String, Object?> request,
) {
  renderKeys(request, const {
    'type',
    'context_id',
    'document_id',
    'displayed_commit_id',
    'revision',
    'viewport',
  });
  final contextId = renderPositiveInt(request['context_id'], 'context_id');
  final documentId = renderPositiveInt(request['document_id'], 'document_id');
  final revision = RenderRevision.fromWire(request['revision']);
  _requireRevisionIdentity(revision, contextId, documentId);
  return NativeCaptureSceneRequest(
    requestId: requestId,
    contextId: contextId,
    documentId: documentId,
    displayedCommitId: renderPositiveInt(
      request['displayed_commit_id'],
      'displayed_commit_id',
    ),
    revision: revision,
    viewport: RenderViewport.fromWire(request['viewport']),
  );
}

NativeEnsureLayoutRequest _decodeEnsureLayout(
  int requestId,
  Map<String, Object?> request,
) {
  renderKeys(request, const {'type', 'required_revision'});
  return NativeEnsureLayoutRequest(
    requestId,
    RenderRevision.fromWire(request['required_revision']),
  );
}

NativeHitTestRequest _decodeHitTest(
  int requestId,
  Map<String, Object?> request,
) {
  renderKeys(request, const {
    'type',
    'context_id',
    'document_id',
    'displayed_commit_id',
    'revision',
    'handle',
    'query_id',
    'point',
  });
  final revision = RenderRevision.fromWire(request['revision']);
  final contextId = renderPositiveInt(request['context_id'], 'context_id');
  final documentId = renderPositiveInt(request['document_id'], 'document_id');
  _requireRevisionIdentity(revision, contextId, documentId);
  return NativeHitTestRequest(
    requestId,
    RenderHitTestQuery(
      queryId: renderPositiveInt(request['query_id'], 'query_id'),
      contextId: contextId,
      documentId: documentId,
      displayedCommitId: renderPositiveInt(
        request['displayed_commit_id'],
        'displayed_commit_id',
      ),
      revision: revision,
      handle: renderPositiveInt(request['handle'], 'handle'),
      point: RenderPoint.fromWire(request['point'], 'point'),
    ),
  );
}

NativeTextQueryRequest _decodeTextQuery(
  int requestId,
  Map<String, Object?> request,
) {
  renderKeys(request, const {
    'type',
    'context_id',
    'document_id',
    'commit_id',
    'revision',
    'handle',
    'allow_truncation',
    'queries',
  });
  final revision = RenderRevision.fromWire(request['revision']);
  final contextId = renderPositiveInt(request['context_id'], 'context_id');
  final documentId = renderPositiveInt(request['document_id'], 'document_id');
  _requireRevisionIdentity(revision, contextId, documentId);
  final allowTruncation = request['allow_truncation'];
  if (allowTruncation is! bool) {
    throw const RenderProtocolException(
      'render.invalid-wire',
      'allow_truncation must be a boolean',
    );
  }
  final values = request['queries'];
  if (values is! List<Object?> || values.length > 256) {
    throw const RenderProtocolException(
      'render.limit',
      'text query batch exceeds 256 entries',
    );
  }
  final seen = <int>{};
  final queries = values
      .map((value) {
        final wire = renderObject(value, 'text query');
        renderKeys(wire, const {'query_id', 'node_id', 'kind'});
        final queryId = renderPositiveInt(wire['query_id'], 'query_id');
        if (!seen.add(queryId)) {
          throw const RenderProtocolException(
            'render.duplicate-id',
            'text query id is duplicated',
          );
        }
        final kind = renderObject(wire['kind'], 'text query kind');
        return RenderTextQuery(
          queryId: queryId,
          nodeId: renderPositiveInt(wire['node_id'], 'node_id'),
          kind: _decodeTextQueryKind(kind),
        );
      })
      .toList(growable: false);
  return NativeTextQueryRequest(
    requestId,
    RenderTextQueryBatch(
      contextId: contextId,
      documentId: documentId,
      commitId: renderPositiveInt(request['commit_id'], 'commit_id'),
      revision: revision,
      handle: renderPositiveInt(request['handle'], 'handle'),
      allowTruncation: allowTruncation,
      queries: queries,
    ),
  );
}

RenderTextQueryKind _decodeTextQueryKind(Map<String, Object?> kind) {
  return switch (kind['type']) {
    'offset_for_point' => () {
      renderKeys(kind, const {'type', 'point'});
      return RenderOffsetForPoint(
        RenderPoint.fromWire(kind['point'], 'text query point'),
      );
    }(),
    'caret_for_offset' => () {
      renderKeys(kind, const {'type', 'utf16_offset', 'affinity'});
      return RenderCaretForOffset(
        renderUnsigned32(kind['utf16_offset'], 'utf16_offset'),
        _decodeAffinity(kind['affinity']),
      );
    }(),
    'range_boxes' => () {
      renderKeys(kind, const {'type', 'utf16_start', 'utf16_end'});
      final start = renderUnsigned32(kind['utf16_start'], 'utf16_start');
      final end = renderUnsigned32(kind['utf16_end'], 'utf16_end');
      if (start > end) {
        throw const RenderProtocolException(
          'render.invalid-geometry',
          'text query range is reversed',
        );
      }
      return RenderRangeBoxes(start, end);
    }(),
    _ => throw const RenderProtocolException(
      'render.invalid-wire',
      'unsupported text query kind',
    ),
  };
}

RenderTextAffinity _decodeAffinity(Object? value) => switch (value) {
  'upstream' => RenderTextAffinity.upstream,
  'downstream' => RenderTextAffinity.downstream,
  _ => throw const RenderProtocolException(
    'render.invalid-wire',
    'unsupported text affinity',
  ),
};

Map<String, Object?> rendererCommitResponse(
  int requestId,
  RenderCommit commit,
) => _response(requestId, commit.toWire());

Map<String, Object?> rendererHitTestResponse(
  int requestId,
  RenderInputTarget? target,
) => _response(requestId, {'type': 'hit_test', 'target': target?.toWire()});

Map<String, Object?> rendererTextQueryResponse(
  int requestId,
  RenderTextQueryBatchResult result,
) => _response(requestId, result.toWire());

Map<String, Object?> rendererResetResponse(int requestId) =>
    _response(requestId, const {'type': 'reset'});

Map<String, Object?> rendererCancelledResponse(int requestId, String reason) {
  if (!const {
    'navigation',
    'stop',
    'context_closed',
    'shutdown',
    'deadline',
  }.contains(reason)) {
    throw const RenderProtocolException(
      'render.invalid-wire',
      'unsupported renderer cancellation reason',
    );
  }
  return _response(requestId, {'type': 'cancelled', 'reason': reason});
}

Map<String, Object?> rendererFailedResponse(
  int requestId, {
  required String code,
  required String message,
}) {
  if (code.isEmpty ||
      utf8.encode(code).length > 256 ||
      utf8.encode(message).length > 4096) {
    throw const RenderProtocolException(
      'render.limit',
      'renderer failure text exceeds its wire limit',
    );
  }
  return _response(requestId, {
    'type': 'failed',
    'code': code,
    'message': message,
  });
}

Map<String, Object?> rendererCommitSubmission(RenderCommit commit) =>
    _submission(commit.toWire());

Map<String, Object?> rendererPresentedSubmission(RenderPresented presented) {
  _requireRevisionIdentity(
    presented.revision,
    presented.contextId,
    presented.documentId,
  );
  renderPositiveInt(presented.commitId, 'commit_id');
  return _submission(presented.toWire());
}

Map<String, Object?> rendererResyncSubmission(RenderResyncRequest request) {
  renderPositiveInt(request.contextId, 'context_id');
  renderPositiveInt(request.documentId, 'document_id');
  if (!const {
    'missing_state',
    'missed_base_revision',
    'renderer_reset',
  }.contains(request.reason)) {
    throw const RenderProtocolException(
      'render.invalid-wire',
      'unsupported renderer resync reason',
    );
  }
  for (final revision in [
    ?request.currentRevision,
    ?request.rejectedBaseRevision,
  ]) {
    _requireRevisionIdentity(revision, request.contextId, request.documentId);
  }
  return _submission(request.toWire());
}

Map<String, Object?> _submission(Map<String, Object?> submission) => {
  'v': renderProtocolVersion,
  'type': 'renderer_submission',
  'submission': submission,
};

Map<String, Object?> _response(int requestId, Map<String, Object?> response) =>
    {
      'v': renderProtocolVersion,
      'type': 'renderer_response',
      'request_id': renderPositiveInt(requestId, 'request_id'),
      'response': response,
    };

Uint8List encodeRendererResponse(Map<String, Object?> response) {
  final bytes = JsonUtf8Encoder().convert(response);
  if (bytes.length > vixenMaxMessageBytes) {
    throw NativeBridgeException(
      'renderer response exceeds $vixenMaxMessageBytes bytes',
      code: NativeStatus.inputTooLarge.defaultCode,
      status: NativeStatus.inputTooLarge,
    );
  }
  return Uint8List.fromList(bytes);
}

Uint8List encodeRendererSubmission(Map<String, Object?> submission) =>
    encodeRendererResponse(submission);

void _requireRevisionIdentity(
  RenderRevision revision,
  int contextId,
  int documentId,
) {
  if (revision.contextId != contextId || revision.documentId != documentId) {
    throw const RenderProtocolException(
      'render.stale',
      'renderer request identity does not match its revision',
    );
  }
}

List<Object?> _boundedList(Object? value, String name, int limit) {
  if (value is! List<Object?>) {
    throw RenderProtocolException(
      'render.invalid-wire',
      '$name must be an array',
    );
  }
  if (value.length > limit) {
    throw RenderProtocolException(
      'render.limit',
      '$name exceeds the limit $limit',
    );
  }
  return value;
}

String _boundedString(Object? value, String name) {
  if (value is! String) {
    throw RenderProtocolException(
      'render.invalid-wire',
      '$name must be a string',
    );
  }
  if (utf8.encode(value).length > renderMaxStringBytes) {
    throw RenderProtocolException(
      'render.limit',
      '$name exceeds the string byte limit',
    );
  }
  return value;
}

int _nodeSourceSize(RenderNode node) {
  var size = utf8.encode(node.name).length + utf8.encode(node.text).length;
  for (final entry in node.styles.entries) {
    size += utf8.encode(entry.key).length + utf8.encode(entry.value).length;
  }
  final semantic = node.semantic;
  if (semantic != null) {
    size += utf8.encode(semantic.role).length;
    size += utf8.encode(semantic.name).length;
    size += utf8.encode(semantic.value ?? '').length;
  }
  return size;
}

int _resourceSourceSize(RenderResource resource) =>
    utf8.encode(resource.mime).length + resource.bytes.length;
