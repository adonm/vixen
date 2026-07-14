import 'dart:typed_data';

const int renderProtocolVersion = 1;
const int renderBrokerQueueCapacity = 64;
const int renderBrokerMaxUpdateSourceBytes = 512 * 1024;
const int renderMaxNodes = 16384;
const int renderMaxTreeDepth = 256;
const int renderMaxMutations = 4096;
const int renderMaxResources = 512;
const int renderMaxResourceBytes = 16 * 1024 * 1024;
const int renderMaxTotalResourceBytes = 64 * 1024 * 1024;
const int renderMaxStringBytes = 64 * 1024;
const int renderMaxTotalStringBytes = 4 * 1024 * 1024;
const int renderMaxStylesPerNode = 512;
const int renderMaxResourcesPerNode = 32;
const int renderMaxSemanticActionsPerNode = 32;
const int renderMaxGeometryEntries = 65536;
const int renderMaxScrollEntries = 4096;
const int renderMaxSemanticBounds = 16384;
const int renderMaxTextQueries = 256;
const int renderMaxTextBoxes = 4096;
const int renderMaxViewportDimension = 16384;
const double renderMaxCoordinate = 16777216;
const double renderMaxScale = 16;

final class RenderProtocolException implements Exception {
  const RenderProtocolException(this.code, this.message);
  final String code;
  final String message;

  @override
  String toString() => 'RenderProtocolException[$code]: $message';
}

final class RenderRevision {
  const RenderRevision({
    required this.contextId,
    required this.documentId,
    required this.sourceGeneration,
    required this.styleGeneration,
    required this.viewportGeneration,
    required this.resourceGeneration,
  });

  final int contextId;
  final int documentId;
  final int sourceGeneration;
  final int styleGeneration;
  final int viewportGeneration;
  final int resourceGeneration;

  Map<String, Object?> toWire() => {
    'context_id': contextId,
    'document_id': documentId,
    'source_generation': sourceGeneration,
    'style_generation': styleGeneration,
    'viewport_generation': viewportGeneration,
    'resource_generation': resourceGeneration,
  };

  factory RenderRevision.fromWire(Object? value) {
    final wire = renderObject(value, 'revision');
    renderKeys(wire, const {
      'context_id',
      'document_id',
      'source_generation',
      'style_generation',
      'viewport_generation',
      'resource_generation',
    });
    return RenderRevision(
      contextId: renderPositiveInt(wire['context_id'], 'context_id'),
      documentId: renderPositiveInt(wire['document_id'], 'document_id'),
      sourceGeneration: renderPositiveInt(
        wire['source_generation'],
        'source_generation',
      ),
      styleGeneration: renderPositiveInt(
        wire['style_generation'],
        'style_generation',
      ),
      viewportGeneration: renderPositiveInt(
        wire['viewport_generation'],
        'viewport_generation',
      ),
      resourceGeneration: renderPositiveInt(
        wire['resource_generation'],
        'resource_generation',
      ),
    );
  }

  bool succeeds(RenderRevision base) =>
      contextId == base.contextId &&
      documentId == base.documentId &&
      sourceGeneration >= base.sourceGeneration &&
      styleGeneration >= base.styleGeneration &&
      viewportGeneration >= base.viewportGeneration &&
      resourceGeneration >= base.resourceGeneration &&
      this != base;

  void validate() {
    if (contextId <= 0 ||
        documentId <= 0 ||
        sourceGeneration <= 0 ||
        styleGeneration <= 0 ||
        viewportGeneration <= 0 ||
        resourceGeneration <= 0) {
      throw const RenderProtocolException(
        'render.revision',
        'renderer revision ids and generations must be positive',
      );
    }
  }

  @override
  bool operator ==(Object other) =>
      other is RenderRevision &&
      contextId == other.contextId &&
      documentId == other.documentId &&
      sourceGeneration == other.sourceGeneration &&
      styleGeneration == other.styleGeneration &&
      viewportGeneration == other.viewportGeneration &&
      resourceGeneration == other.resourceGeneration;

  @override
  int get hashCode => Object.hash(
    contextId,
    documentId,
    sourceGeneration,
    styleGeneration,
    viewportGeneration,
    resourceGeneration,
  );
}

final class RenderViewport {
  const RenderViewport({
    required this.width,
    required this.height,
    this.deviceScale = 1,
    this.pageZoom = 1,
  });
  final int width;
  final int height;
  final double deviceScale;
  final double pageZoom;

  Map<String, Object?> toWire() => {
    'width': width,
    'height': height,
    'device_scale': deviceScale,
    'page_zoom': pageZoom,
  };

  factory RenderViewport.fromWire(Object? value) {
    final wire = renderObject(value, 'viewport');
    renderKeys(wire, const {'width', 'height', 'device_scale', 'page_zoom'});
    return RenderViewport(
      width: renderUnsigned32(wire['width'], 'viewport.width'),
      height: renderUnsigned32(wire['height'], 'viewport.height'),
      deviceScale: renderFiniteDouble(
        wire['device_scale'],
        'viewport.device_scale',
      ),
      pageZoom: renderFiniteDouble(wire['page_zoom'], 'viewport.page_zoom'),
    );
  }

  @override
  bool operator ==(Object other) =>
      other is RenderViewport &&
      width == other.width &&
      height == other.height &&
      deviceScale == other.deviceScale &&
      pageZoom == other.pageZoom;

  @override
  int get hashCode => Object.hash(width, height, deviceScale, pageZoom);
}

enum RenderNodeKind { element, text, pseudoBefore, pseudoAfter }

enum RenderSemanticActionKind {
  activate,
  focus,
  setValue,
  setSelection,
  increase,
  decrease,
  scrollIntoView,
}

final class RenderSemanticDescriptor {
  RenderSemanticDescriptor({
    required this.id,
    required this.role,
    required this.name,
    this.value,
    required this.actionGeneration,
    List<RenderSemanticActionKind> actions = const [],
  }) : actions = List.unmodifiable(actions);
  final int id;
  final String role;
  final String name;
  final String? value;
  final int actionGeneration;
  final List<RenderSemanticActionKind> actions;
}

final class RenderNode {
  RenderNode({
    required this.id,
    required this.parentId,
    required this.siblingIndex,
    required this.depth,
    required this.kind,
    required this.name,
    this.text = '',
    Map<String, String> styles = const {},
    List<int> resourceIds = const [],
    this.semantic,
  }) : styles = Map.unmodifiable(styles),
       resourceIds = List.unmodifiable(resourceIds);

  final int id;
  final int? parentId;
  final int siblingIndex;
  final int depth;
  final RenderNodeKind kind;
  final String name;
  final String text;
  final Map<String, String> styles;
  final List<int> resourceIds;
  final RenderSemanticDescriptor? semantic;

  RenderNode copyWith({String? text, Map<String, String>? styles}) =>
      RenderNode(
        id: id,
        parentId: parentId,
        siblingIndex: siblingIndex,
        depth: depth,
        kind: kind,
        name: name,
        text: text ?? this.text,
        styles: styles ?? this.styles,
        resourceIds: resourceIds,
        semantic: semantic,
      );
}

enum RenderResourceKind { image, font }

final class RenderResource {
  RenderResource({
    required this.id,
    this.kind = RenderResourceKind.image,
    required this.mime,
    required Uint8List bytes,
  }) : bytes = Uint8List.fromList(bytes);
  final int id;
  final RenderResourceKind kind;
  final String mime;
  final Uint8List bytes;
}

final class FullRenderSnapshot {
  FullRenderSnapshot({
    required this.revision,
    required this.viewport,
    required List<RenderNode> nodes,
    required List<RenderResource> resources,
    List<RenderScrollIntent> scrollIntents = const [],
  }) : nodes = List.unmodifiable(nodes),
       resources = List.unmodifiable(resources),
       scrollIntents = List.unmodifiable(scrollIntents);
  final RenderRevision revision;
  final RenderViewport viewport;
  final List<RenderNode> nodes;
  final List<RenderResource> resources;
  final List<RenderScrollIntent> scrollIntents;
}

enum RenderScrollIntentKind { by, to, restore }

final class RenderScrollIntent {
  const RenderScrollIntent({
    required this.scrollNodeId,
    required this.nodeId,
    required this.kind,
    required this.point,
  });
  final int scrollNodeId;
  final int nodeId;
  final RenderScrollIntentKind kind;
  final RenderPoint point;
}

sealed class RenderMutation {
  const RenderMutation();
}

final class UpsertRenderNode extends RenderMutation {
  const UpsertRenderNode(this.node);
  final RenderNode node;
}

final class RemoveRenderNode extends RenderMutation {
  const RemoveRenderNode(this.nodeId);
  final int nodeId;
}

final class SetRenderViewport extends RenderMutation {
  const SetRenderViewport(this.viewport);
  final RenderViewport viewport;
}

final class UpsertRenderResource extends RenderMutation {
  const UpsertRenderResource(this.resource);
  final RenderResource resource;
}

final class RemoveRenderResource extends RenderMutation {
  const RemoveRenderResource(this.resourceId);
  final int resourceId;
}

final class SetRenderScrollIntent extends RenderMutation {
  const SetRenderScrollIntent(this.intent);
  final RenderScrollIntent intent;
}

final class RemoveRenderScrollIntent extends RenderMutation {
  const RemoveRenderScrollIntent(this.scrollNodeId);
  final int scrollNodeId;
}

final class RenderMutationBatch {
  RenderMutationBatch({
    required this.baseRevision,
    required this.targetRevision,
    required List<RenderMutation> mutations,
  }) : mutations = List.unmodifiable(mutations);
  final RenderRevision baseRevision;
  final RenderRevision targetRevision;
  final List<RenderMutation> mutations;
}

final class RenderResyncRequest {
  const RenderResyncRequest({
    required this.contextId,
    required this.documentId,
    required this.currentRevision,
    required this.rejectedBaseRevision,
    required this.reason,
  });
  final int contextId;
  final int documentId;
  final RenderRevision? currentRevision;
  final RenderRevision? rejectedBaseRevision;
  final String reason;

  Map<String, Object?> toWire() => {
    'type': 'resync',
    'context_id': contextId,
    'document_id': documentId,
    'current_revision': currentRevision?.toWire(),
    'rejected_base_revision': rejectedBaseRevision?.toWire(),
    'reason': reason,
  };
}

final class RenderRect {
  const RenderRect(this.x, this.y, this.width, this.height);
  final double x;
  final double y;
  final double width;
  final double height;

  Map<String, Object?> toWire() => {
    'x': x,
    'y': y,
    'width': width,
    'height': height,
  };
}

final class RenderGeometryEntry {
  const RenderGeometryEntry({
    required this.nodeId,
    required this.fragmentId,
    required this.borderBox,
    required this.paddingBox,
    required this.contentBox,
    required this.clip,
    required this.scrollNodeId,
    required this.paintOrder,
  });
  final int nodeId;
  final int fragmentId;
  final RenderRect borderBox;
  final RenderRect paddingBox;
  final RenderRect contentBox;
  final RenderRect? clip;
  final int? scrollNodeId;
  final int paintOrder;

  Map<String, Object?> toWire() => {
    'node_id': nodeId,
    'fragment_id': fragmentId,
    'border_box': borderBox.toWire(),
    'padding_box': paddingBox.toWire(),
    'content_box': contentBox.toWire(),
    'clip': clip?.toWire(),
    'scroll_node_id': scrollNodeId,
    'paint_order': paintOrder,
  };
}

final class RenderScrollState {
  const RenderScrollState({
    required this.scrollNodeId,
    required this.nodeId,
    required this.offsetX,
    required this.offsetY,
    required this.maxOffsetX,
    required this.maxOffsetY,
    required this.viewport,
    required this.contentWidth,
    required this.contentHeight,
  });
  final int scrollNodeId;
  final int nodeId;
  final double offsetX;
  final double offsetY;
  final double maxOffsetX;
  final double maxOffsetY;
  final RenderRect viewport;
  final double contentWidth;
  final double contentHeight;

  Map<String, Object?> toWire() => {
    'scroll_node_id': scrollNodeId,
    'node_id': nodeId,
    'offset': {'x': offsetX, 'y': offsetY},
    'max_offset': {'x': maxOffsetX, 'y': maxOffsetY},
    'viewport': viewport.toWire(),
    'content_size': {'width': contentWidth, 'height': contentHeight},
  };
}

final class RenderSemanticBounds {
  RenderSemanticBounds({
    required this.semanticNodeId,
    required this.nodeId,
    required List<RenderRect> rects,
  }) : rects = List.unmodifiable(rects);
  final int semanticNodeId;
  final int nodeId;
  final List<RenderRect> rects;

  Map<String, Object?> toWire() => {
    'semantic_node_id': semanticNodeId,
    'node_id': nodeId,
    'rects': rects.map((rect) => rect.toWire()).toList(growable: false),
  };
}

final class RenderCommit {
  RenderCommit({
    required this.commitId,
    required this.revision,
    required this.viewport,
    required List<RenderGeometryEntry> geometry,
    required this.hitTestHandle,
    required this.textQueryHandle,
    required List<RenderScrollState> scroll,
    required List<RenderSemanticBounds> semantics,
    List<RenderTruncationDiagnostic> truncations = const [],
  }) : geometry = List.unmodifiable(geometry),
       scroll = List.unmodifiable(scroll),
       semantics = List.unmodifiable(semantics),
       truncations = List.unmodifiable(truncations);

  final int commitId;
  final RenderRevision revision;
  final RenderViewport viewport;
  final List<RenderGeometryEntry> geometry;
  final int hitTestHandle;
  final int textQueryHandle;
  final List<RenderScrollState> scroll;
  final List<RenderSemanticBounds> semantics;
  final List<RenderTruncationDiagnostic> truncations;

  Map<String, Object?> toWire() => {
    'type': 'commit',
    'commit_id': commitId,
    'revision': revision.toWire(),
    'viewport': viewport.toWire(),
    'geometry_index': geometry
        .map((entry) => entry.toWire())
        .toList(growable: false),
    'hit_test_handle': hitTestHandle,
    'text_query_handle': textQueryHandle,
    'scroll_snapshot': scroll
        .map((entry) => entry.toWire())
        .toList(growable: false),
    'semantic_bounds': semantics
        .map((entry) => entry.toWire())
        .toList(growable: false),
    'truncations': truncations
        .map((truncation) => truncation.toWire())
        .toList(growable: false),
  };
}

final class RenderPresented {
  const RenderPresented({
    required this.contextId,
    required this.documentId,
    required this.commitId,
    required this.revision,
  });
  final int contextId;
  final int documentId;
  final int commitId;
  final RenderRevision revision;

  Map<String, Object?> toWire() => {
    'type': 'presented',
    'context_id': contextId,
    'document_id': documentId,
    'commit_id': commitId,
    'revision': revision.toWire(),
  };
}

final class RenderHandleRelease {
  const RenderHandleRelease({
    required this.commitId,
    required this.hitTestHandle,
    required this.textQueryHandle,
  });
  final int commitId;
  final int hitTestHandle;
  final int textQueryHandle;
}

final class RenderPoint {
  const RenderPoint(this.x, this.y);
  final double x;
  final double y;

  Map<String, Object?> toWire() => {'x': x, 'y': y};

  factory RenderPoint.fromWire(Object? value, String name) {
    final wire = renderObject(value, name);
    renderKeys(wire, const {'x', 'y'});
    return RenderPoint(
      renderFiniteDouble(wire['x'], '$name.x'),
      renderFiniteDouble(wire['y'], '$name.y'),
    );
  }
}

final class RenderHitTestQuery {
  const RenderHitTestQuery({
    required this.queryId,
    required this.contextId,
    required this.documentId,
    required this.displayedCommitId,
    required this.revision,
    required this.handle,
    required this.point,
  });
  final int queryId;
  final int contextId;
  final int documentId;
  final int displayedCommitId;
  final RenderRevision revision;
  final int handle;
  final RenderPoint point;

  Map<String, Object?> toWire() => {
    'v': renderProtocolVersion,
    'query_id': queryId,
    'context_id': contextId,
    'document_id': documentId,
    'displayed_commit_id': displayedCommitId,
    'revision': revision.toWire(),
    'handle': handle,
    'point': point.toWire(),
  };
}

final class RenderInputTarget {
  const RenderInputTarget({
    required this.queryId,
    required this.contextId,
    required this.documentId,
    required this.displayedCommitId,
    required this.revision,
    required this.handle,
    required this.nodeId,
    required this.fragmentId,
    required this.viewportPoint,
    required this.localPoint,
  });
  final int queryId;
  final int contextId;
  final int documentId;
  final int displayedCommitId;
  final RenderRevision revision;
  final int handle;
  final int nodeId;
  final int fragmentId;
  final RenderPoint viewportPoint;
  final RenderPoint localPoint;

  Map<String, Object?> toWire() => {
    'v': renderProtocolVersion,
    'query_id': queryId,
    'context_id': contextId,
    'document_id': documentId,
    'displayed_commit_id': displayedCommitId,
    'revision': revision.toWire(),
    'handle': handle,
    'node_id': nodeId,
    'fragment_id': fragmentId,
    'viewport_point': viewportPoint.toWire(),
    'local_point': localPoint.toWire(),
  };
}

enum RenderTextAffinity { upstream, downstream }

enum RenderTextDirection { ltr, rtl }

sealed class RenderTextQueryKind {
  const RenderTextQueryKind();
}

final class RenderOffsetForPoint extends RenderTextQueryKind {
  const RenderOffsetForPoint(this.point);
  final RenderPoint point;
}

final class RenderCaretForOffset extends RenderTextQueryKind {
  const RenderCaretForOffset(this.utf16Offset, this.affinity);
  final int utf16Offset;
  final RenderTextAffinity affinity;
}

final class RenderRangeBoxes extends RenderTextQueryKind {
  const RenderRangeBoxes(this.utf16Start, this.utf16End);
  final int utf16Start;
  final int utf16End;
}

final class RenderTextQuery {
  const RenderTextQuery({
    required this.queryId,
    required this.nodeId,
    required this.kind,
  });
  final int queryId;
  final int nodeId;
  final RenderTextQueryKind kind;
}

final class RenderTextQueryBatch {
  RenderTextQueryBatch({
    required this.contextId,
    required this.documentId,
    required this.commitId,
    required this.revision,
    required this.handle,
    required this.allowTruncation,
    required List<RenderTextQuery> queries,
  }) : queries = List.unmodifiable(queries);
  final int contextId;
  final int documentId;
  final int commitId;
  final RenderRevision revision;
  final int handle;
  final bool allowTruncation;
  final List<RenderTextQuery> queries;
}

final class RenderTextBox {
  const RenderTextBox({required this.rect, required this.direction});
  final RenderRect rect;
  final RenderTextDirection direction;

  Map<String, Object?> toWire() => {
    'rect': rect.toWire(),
    'direction': direction.name,
  };
}

sealed class RenderTextQueryValue {
  const RenderTextQueryValue();
  Map<String, Object?> toWire();
}

final class RenderTextOffsetValue extends RenderTextQueryValue {
  const RenderTextOffsetValue(this.utf16Offset, this.affinity);
  final int utf16Offset;
  final RenderTextAffinity affinity;

  @override
  Map<String, Object?> toWire() => {
    'type': 'offset',
    'utf16_offset': utf16Offset,
    'affinity': affinity.name,
  };
}

final class RenderTextCaretValue extends RenderTextQueryValue {
  const RenderTextCaretValue(this.rect, this.affinity);
  final RenderRect rect;
  final RenderTextAffinity affinity;

  @override
  Map<String, Object?> toWire() => {
    'type': 'caret',
    'rect': rect.toWire(),
    'affinity': affinity.name,
  };
}

final class RenderTextRangeBoxesValue extends RenderTextQueryValue {
  RenderTextRangeBoxesValue(List<RenderTextBox> boxes)
    : boxes = List.unmodifiable(boxes);
  final List<RenderTextBox> boxes;

  @override
  Map<String, Object?> toWire() => {
    'type': 'range_boxes',
    'boxes': boxes.map((box) => box.toWire()).toList(growable: false),
  };
}

final class RenderTextQueryResult {
  const RenderTextQueryResult({required this.queryId, required this.value});
  final int queryId;
  final RenderTextQueryValue value;

  Map<String, Object?> toWire() => {
    'query_id': queryId,
    'value': value.toWire(),
  };
}

enum RenderLimitDomain {
  nodes,
  treeDepth,
  mutations,
  resources,
  resourceBytes,
  stringBytes,
  geometry,
  scrollEntries,
  semanticBounds,
  textQueries,
  textBoxes,
}

final class RenderTruncationDiagnostic {
  const RenderTruncationDiagnostic({
    required this.domain,
    required this.limit,
    required this.omitted,
    required this.required,
  });
  final RenderLimitDomain domain;
  final int limit;
  final int omitted;
  final bool required;

  Map<String, Object?> toWire() => {
    'domain': switch (domain) {
      RenderLimitDomain.treeDepth => 'tree_depth',
      RenderLimitDomain.resourceBytes => 'resource_bytes',
      RenderLimitDomain.stringBytes => 'string_bytes',
      RenderLimitDomain.scrollEntries => 'scroll_entries',
      RenderLimitDomain.semanticBounds => 'semantic_bounds',
      RenderLimitDomain.textQueries => 'text_queries',
      RenderLimitDomain.textBoxes => 'text_boxes',
      _ => domain.name,
    },
    'limit': limit,
    'omitted': omitted,
    'required': required,
  };
}

final class RenderTextQueryBatchResult {
  RenderTextQueryBatchResult({
    required this.contextId,
    required this.documentId,
    required this.commitId,
    required this.revision,
    required List<RenderTextQueryResult> results,
    List<RenderTruncationDiagnostic> truncations = const [],
  }) : results = List.unmodifiable(results),
       truncations = List.unmodifiable(truncations);
  final int contextId;
  final int documentId;
  final int commitId;
  final RenderRevision revision;
  final List<RenderTextQueryResult> results;
  final List<RenderTruncationDiagnostic> truncations;

  Map<String, Object?> toWire() => {
    'type': 'text_query',
    'context_id': contextId,
    'document_id': documentId,
    'commit_id': commitId,
    'revision': revision.toWire(),
    'results': results.map((result) => result.toWire()).toList(growable: false),
    'truncations': truncations
        .map((truncation) => truncation.toWire())
        .toList(growable: false),
  };
}

Map<String, Object?> renderObject(Object? value, String name) {
  if (value is! Map<Object?, Object?> ||
      value.keys.any((key) => key is! String)) {
    throw RenderProtocolException(
      'render.invalid-wire',
      '$name must be an object',
    );
  }
  return value.cast<String, Object?>();
}

void renderKeys(Map<String, Object?> value, Set<String> expected) {
  if (value.length != expected.length ||
      !value.keys.toSet().containsAll(expected)) {
    throw const RenderProtocolException(
      'render.invalid-wire',
      'renderer object has missing or unknown fields',
    );
  }
}

int renderPositiveInt(Object? value, String name) {
  if (value is! int || value <= 0 || value > 0x7fffffffffffffff) {
    throw RenderProtocolException(
      'render.invalid-wire',
      '$name must be a positive integer',
    );
  }
  return value;
}

int renderUnsigned32(Object? value, String name) {
  if (value is! int || value < 0 || value > 0xffffffff) {
    throw RenderProtocolException(
      'render.invalid-wire',
      '$name must be an unsigned 32-bit integer',
    );
  }
  return value;
}

double renderFiniteDouble(Object? value, String name) {
  if (value is! num || !value.isFinite) {
    throw RenderProtocolException(
      'render.invalid-wire',
      '$name must be a finite number',
    );
  }
  return value.toDouble();
}
