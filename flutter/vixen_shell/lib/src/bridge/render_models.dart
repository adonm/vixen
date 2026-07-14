import 'dart:typed_data';

const int renderProtocolVersion = 1;

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
}

enum RenderNodeKind { element, text, pseudoBefore, pseudoAfter }

enum RenderSemanticRole { heading, link, text }

final class RenderSemanticDescriptor {
  const RenderSemanticDescriptor({
    required this.id,
    required this.role,
    required this.name,
    required this.actionGeneration,
  });
  final int id;
  final RenderSemanticRole role;
  final String name;
  final int actionGeneration;
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

final class RenderResource {
  RenderResource({
    required this.id,
    required this.mime,
    required Uint8List bytes,
  }) : bytes = Uint8List.fromList(bytes);
  final int id;
  final String mime;
  final Uint8List bytes;
}

final class FullRenderSnapshot {
  FullRenderSnapshot({
    required this.revision,
    required this.viewport,
    required List<RenderNode> nodes,
    required List<RenderResource> resources,
  }) : nodes = List.unmodifiable(nodes),
       resources = List.unmodifiable(resources);
  final RenderRevision revision;
  final RenderViewport viewport;
  final List<RenderNode> nodes;
  final List<RenderResource> resources;
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
  }) : geometry = List.unmodifiable(geometry),
       scroll = List.unmodifiable(scroll),
       semantics = List.unmodifiable(semantics);

  final int commitId;
  final RenderRevision revision;
  final RenderViewport viewport;
  final List<RenderGeometryEntry> geometry;
  final int hitTestHandle;
  final int textQueryHandle;
  final List<RenderScrollState> scroll;
  final List<RenderSemanticBounds> semantics;

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
    'truncations': const <Object?>[],
  };
}

final class RenderPresented {
  const RenderPresented({required this.commitId, required this.revision});
  final int commitId;
  final RenderRevision revision;
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
