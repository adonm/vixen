import 'dart:ui' as ui;
import 'dart:typed_data';

import '../bridge/render_models.dart';

sealed class RenderApplyResult {
  const RenderApplyResult();
}

final class RenderApplied extends RenderApplyResult {
  const RenderApplied(this.view);
  final FormatterCommitView view;
}

final class RenderResyncRequired extends RenderApplyResult {
  const RenderResyncRequired(this.request);
  final RenderResyncRequest request;
}

final class FormatterHit {
  const FormatterHit({
    required this.nodeId,
    required this.fragmentId,
    required this.viewportPoint,
    required this.localPoint,
  });
  final int nodeId;
  final int fragmentId;
  final ui.Offset viewportPoint;
  final ui.Offset localPoint;
}

final class FormatterSemanticRegion {
  const FormatterSemanticRegion({required this.descriptor, required this.rect});
  final RenderSemanticDescriptor descriptor;
  final ui.Rect rect;
}

final class FormatterCommitView {
  FormatterCommitView._({
    required this.commit,
    required this._picture,
    required List<_ParagraphState> paragraphs,
    required List<ui.Image> images,
    required List<FormatterSemanticRegion> semanticRegions,
  }) : _paragraphs = List.unmodifiable(paragraphs),
       _images = List.unmodifiable(images),
       semanticRegions = List.unmodifiable(semanticRegions);

  final RenderCommit commit;
  final ui.Picture _picture;
  final List<_ParagraphState> _paragraphs;
  final List<ui.Image> _images;
  final List<FormatterSemanticRegion> semanticRegions;
  bool _retired = false;

  bool get isRetired => _retired;
  ui.Size get viewport => ui.Size(
    commit.viewport.width.toDouble(),
    commit.viewport.height.toDouble(),
  );

  void paint(ui.Canvas canvas) {
    _requireLive();
    canvas.drawPicture(_picture);
  }

  FormatterHit? hitTest(ui.Offset point, {required int handle}) {
    _requireLive();
    if (handle != commit.hitTestHandle ||
        !point.dx.isFinite ||
        !point.dy.isFinite ||
        !(ui.Offset.zero & viewport).contains(point)) {
      return null;
    }
    final entries = commit.geometry.toList()
      ..sort((a, b) => b.paintOrder.compareTo(a.paintOrder));
    for (final entry in entries) {
      final rect = entry.borderBox.uiRect;
      final clip = entry.clip?.uiRect;
      if (rect.contains(point) && (clip == null || clip.contains(point))) {
        return FormatterHit(
          nodeId: entry.nodeId,
          fragmentId: entry.fragmentId,
          viewportPoint: point,
          localPoint: point - rect.topLeft,
        );
      }
    }
    return null;
  }

  List<RenderRect> rangeBoxes({
    required int handle,
    required int nodeId,
    required int start,
    required int end,
  }) {
    _requireLive();
    if (handle != commit.textQueryHandle) {
      throw const RenderProtocolException(
        'render.stale',
        'text query handle is stale',
      );
    }
    final state = _paragraphs
        .where((entry) => entry.ranges.containsKey(nodeId))
        .firstOrNull;
    final range = state?.ranges[nodeId];
    if (state == null ||
        range == null ||
        start < 0 ||
        end < start ||
        end > range.length) {
      throw const RenderProtocolException(
        'render.invalid-geometry',
        'text range is outside the source text',
      );
    }
    return state.paragraph
        .getBoxesForRange(range.start + start, range.start + end)
        .map(
          (box) => RenderRect(
            state.origin.dx + box.left,
            state.origin.dy + box.top,
            box.right - box.left,
            box.bottom - box.top,
          ),
        )
        .toList(growable: false);
  }

  int offsetForPoint({
    required int handle,
    required int nodeId,
    required ui.Offset point,
  }) {
    _requireLive();
    if (handle != commit.textQueryHandle) {
      throw const RenderProtocolException(
        'render.stale',
        'text handle is stale',
      );
    }
    final state = _paragraphs
        .where((entry) => entry.ranges.containsKey(nodeId))
        .firstOrNull;
    if (state == null) {
      throw const RenderProtocolException(
        'render.unknown-id',
        'unknown text node',
      );
    }
    final range = state.ranges[nodeId]!;
    return (state.paragraph.getPositionForOffset(point - state.origin).offset -
            range.start)
        .clamp(0, range.length);
  }

  Future<ui.Image> capture() async {
    _requireLive();
    final scene = (ui.SceneBuilder()..addPicture(ui.Offset.zero, _picture))
        .build();
    try {
      return await scene.toImage(commit.viewport.width, commit.viewport.height);
    } finally {
      scene.dispose();
    }
  }

  void retire() {
    if (_retired) return;
    _retired = true;
    _picture.dispose();
    for (final paragraph in _paragraphs) {
      paragraph.paragraph.dispose();
    }
    for (final image in _images) {
      image.dispose();
    }
  }

  void _requireLive() {
    if (_retired) {
      throw const RenderProtocolException('render.stale', 'commit is retired');
    }
  }
}

final class VixenFormatter {
  final Map<int, RenderNode> _nodes = {};
  final Map<int, RenderResource> _resources = {};
  RenderRevision? _revision;
  RenderViewport? _viewport;
  FormatterCommitView? _staged;
  FormatterCommitView? _presented;
  int _nextCommitId = 1;
  int _nextHandle = 1;

  RenderRevision? get sourceRevision => _revision;
  FormatterCommitView? get displayedView => _presented;

  Future<RenderApplyResult> acceptFullSnapshot(
    FullRenderSnapshot snapshot,
  ) async {
    _validateSnapshot(snapshot);
    if (_revision case final current?) {
      if (snapshot.revision == current && !_snapshotMatches(snapshot)) {
        throw const RenderProtocolException(
          'render.revision',
          'equal revision contains different state',
        );
      }
      if (_regresses(snapshot.revision, current)) {
        throw const RenderProtocolException(
          'render.stale',
          'full snapshot regresses the current revision',
        );
      }
    }
    _nodes
      ..clear()
      ..addEntries(snapshot.nodes.map((node) => MapEntry(node.id, node)));
    _resources
      ..clear()
      ..addEntries(
        snapshot.resources.map((resource) => MapEntry(resource.id, resource)),
      );
    _revision = snapshot.revision;
    _viewport = snapshot.viewport;
    return RenderApplied(await _stage());
  }

  Future<RenderApplyResult> applyMutationBatch(
    RenderMutationBatch batch,
  ) async {
    final current = _revision;
    if (current == null || batch.baseRevision != current) {
      return RenderResyncRequired(
        RenderResyncRequest(
          contextId: batch.targetRevision.contextId,
          documentId: batch.targetRevision.documentId,
          currentRevision: current,
          rejectedBaseRevision: batch.baseRevision,
          reason: current == null ? 'missing_state' : 'missed_base_revision',
        ),
      );
    }
    if (!batch.targetRevision.succeeds(batch.baseRevision)) {
      throw const RenderProtocolException(
        'render.revision',
        'mutation target is not an exact successor',
      );
    }
    final next = Map<int, RenderNode>.of(_nodes);
    for (final mutation in batch.mutations) {
      switch (mutation) {
        case UpsertRenderNode(:final node):
          next[node.id] = node;
        case RemoveRenderNode(:final nodeId):
          if (next.remove(nodeId) == null) {
            throw const RenderProtocolException(
              'render.unknown-id',
              'mutation removes an unknown node',
            );
          }
      }
    }
    _validateNodeGraph(
      next.values.toList(growable: false),
      _resources.keys.toSet(),
    );
    _nodes
      ..clear()
      ..addAll(next);
    _revision = batch.targetRevision;
    return RenderApplied(await _stage());
  }

  FormatterCommitView present(RenderPresented presented) {
    final staged = _staged;
    if (staged == null ||
        staged.commit.commitId != presented.commitId ||
        staged.commit.revision != presented.revision) {
      throw const RenderProtocolException(
        'render.stale',
        'presentation does not match the staged commit',
      );
    }
    if (identical(_presented, staged)) return staged;
    _presented?.retire();
    _presented = staged;
    return staged;
  }

  RenderResyncRequest reset({required int contextId, required int documentId}) {
    _staged?.retire();
    if (!identical(_presented, _staged)) _presented?.retire();
    _staged = null;
    _presented = null;
    _nodes.clear();
    _resources.clear();
    _revision = null;
    _viewport = null;
    return RenderResyncRequest(
      contextId: contextId,
      documentId: documentId,
      currentRevision: null,
      rejectedBaseRevision: null,
      reason: 'renderer_reset',
    );
  }

  void dispose() => reset(contextId: 1, documentId: 1);

  Future<FormatterCommitView> _stage() async {
    final revision = _revision!;
    final viewport = _viewport!;
    final decodedImages = <int, ui.Image>{};
    try {
      for (final resource in _resources.values) {
        if (resource.mime != 'image/png') {
          throw const RenderProtocolException(
            'render.resource',
            'R3 accepts only policy-approved PNG resources',
          );
        }
        decodedImages[resource.id] = await _decodePng(resource.bytes);
      }
      final layout = _FixtureLayout(
        nodes: _nodes,
        images: decodedImages,
        viewport: viewport,
      ).build();
      final recorder = ui.PictureRecorder();
      final canvas = ui.Canvas(
        recorder,
        ui.Rect.fromLTWH(
          0,
          0,
          viewport.width.toDouble(),
          viewport.height.toDouble(),
        ),
      );
      canvas.save();
      canvas.clipRect(
        ui.Rect.fromLTWH(
          0,
          0,
          viewport.width.toDouble(),
          viewport.height.toDouble(),
        ),
      );
      layout.paint(canvas);
      canvas.restore();
      final picture = recorder.endRecording();
      final commitId = _nextCommitId++;
      final view = FormatterCommitView._(
        commit: RenderCommit(
          commitId: commitId,
          revision: revision,
          viewport: viewport,
          geometry: layout.geometry,
          hitTestHandle: _nextHandle++,
          textQueryHandle: _nextHandle++,
          scroll: [
            RenderScrollState(
              scrollNodeId: 1,
              nodeId: layout.rootNodeId,
              offsetX: 0,
              offsetY: 0,
              maxOffsetX: 0,
              maxOffsetY: (layout.contentHeight - viewport.height).clamp(
                0,
                double.infinity,
              ),
              viewport: RenderRect(
                0,
                0,
                viewport.width.toDouble(),
                viewport.height.toDouble(),
              ),
              contentWidth: viewport.width.toDouble(),
              contentHeight: layout.contentHeight,
            ),
          ],
          semantics: layout.semanticBounds,
        ),
        picture: picture,
        paragraphs: layout.paragraphs,
        images: decodedImages.values.toList(growable: false),
        semanticRegions: layout.semanticRegions,
      );
      if (_staged != null && !identical(_staged, _presented)) {
        _staged!.retire();
      }
      _staged = view;
      return view;
    } catch (_) {
      for (final image in decodedImages.values) {
        image.dispose();
      }
      rethrow;
    }
  }

  void _validateSnapshot(FullRenderSnapshot snapshot) {
    if (snapshot.viewport.width <= 0 || snapshot.viewport.height <= 0) {
      throw const RenderProtocolException(
        'render.invalid-geometry',
        'viewport must be positive',
      );
    }
    _validateNodeGraph(
      snapshot.nodes,
      snapshot.resources.map((resource) => resource.id).toSet(),
    );
  }

  void _validateNodeGraph(Iterable<RenderNode> nodes, Set<int> resourceIds) {
    final byId = <int, RenderNode>{};
    for (final node in nodes) {
      if (node.id <= 0 || byId.containsKey(node.id)) {
        throw const RenderProtocolException(
          'render.duplicate-id',
          'render node ids must be unique and nonzero',
        );
      }
      byId[node.id] = node;
      if (node.resourceIds.any((id) => !resourceIds.contains(id))) {
        throw const RenderProtocolException(
          'render.unknown-id',
          'render node references an unknown resource',
        );
      }
    }
    for (final node in byId.values) {
      final parent = node.parentId == null ? null : byId[node.parentId];
      if ((node.parentId != null && parent == null) ||
          (node.parentId == null && node.depth != 0) ||
          (parent != null && node.depth != parent.depth + 1)) {
        throw const RenderProtocolException(
          'render.invalid-graph',
          'render node depth does not match its parent',
        );
      }
    }
  }

  bool _snapshotMatches(FullRenderSnapshot snapshot) {
    if (_viewport != snapshot.viewport ||
        _nodes.length != snapshot.nodes.length ||
        _resources.length != snapshot.resources.length) {
      return false;
    }
    final nodesMatch = snapshot.nodes.every((node) {
      final current = _nodes[node.id];
      return current != null &&
          current.parentId == node.parentId &&
          current.siblingIndex == node.siblingIndex &&
          current.depth == node.depth &&
          current.kind == node.kind &&
          current.name == node.name &&
          current.text == node.text &&
          _mapEquals(current.styles, node.styles) &&
          _listEquals(current.resourceIds, node.resourceIds) &&
          _semanticsEqual(current.semantic, node.semantic);
    });
    return nodesMatch &&
        snapshot.resources.every((resource) {
          final current = _resources[resource.id];
          return current != null &&
              current.mime == resource.mime &&
              _listEquals(current.bytes, resource.bytes);
        });
  }

  bool _regresses(RenderRevision next, RenderRevision current) =>
      next.contextId == current.contextId &&
      next.documentId == current.documentId &&
      (next.sourceGeneration < current.sourceGeneration ||
          next.styleGeneration < current.styleGeneration ||
          next.viewportGeneration < current.viewportGeneration ||
          next.resourceGeneration < current.resourceGeneration);
}

Future<ui.Image> _decodePng(Uint8List bytes) async {
  final buffer = await ui.ImmutableBuffer.fromUint8List(bytes);
  try {
    final descriptor = await ui.ImageDescriptor.encoded(buffer);
    try {
      if (descriptor.width <= 0 ||
          descriptor.height <= 0 ||
          descriptor.width > 4096 ||
          descriptor.height > 4096) {
        throw const RenderProtocolException(
          'render.resource',
          'decoded image dimensions are invalid',
        );
      }
      final codec = await descriptor.instantiateCodec();
      try {
        if (codec.frameCount != 1) {
          throw const RenderProtocolException(
            'render.resource',
            'animated PNG is outside the R3 vertical',
          );
        }
        return (await codec.getNextFrame()).image;
      } finally {
        codec.dispose();
      }
    } finally {
      descriptor.dispose();
    }
  } finally {
    buffer.dispose();
  }
}

final class _ParagraphState {
  const _ParagraphState({
    required this.paragraph,
    required this.origin,
    required this.ranges,
  });
  final ui.Paragraph paragraph;
  final ui.Offset origin;
  final Map<int, _TextRange> ranges;
}

final class _TextRange {
  const _TextRange(this.start, this.length);
  final int start;
  final int length;
}

sealed class _PaintItem {
  const _PaintItem();
  void paint(ui.Canvas canvas);
}

final class _RectPaint extends _PaintItem {
  const _RectPaint(this.rect, this.color);
  final ui.Rect rect;
  final ui.Color color;
  @override
  void paint(ui.Canvas canvas) =>
      canvas.drawRect(rect, ui.Paint()..color = color);
}

final class _ParagraphPaint extends _PaintItem {
  const _ParagraphPaint(this.paragraph, this.origin);
  final ui.Paragraph paragraph;
  final ui.Offset origin;
  @override
  void paint(ui.Canvas canvas) => canvas.drawParagraph(paragraph, origin);
}

final class _ImagePaint extends _PaintItem {
  const _ImagePaint(this.image, this.rect);
  final ui.Image image;
  final ui.Rect rect;
  @override
  void paint(ui.Canvas canvas) {
    canvas.drawImageRect(
      image,
      ui.Rect.fromLTWH(0, 0, image.width.toDouble(), image.height.toDouble()),
      rect,
      ui.Paint()..filterQuality = ui.FilterQuality.none,
    );
  }
}

final class _LayoutResult {
  const _LayoutResult({
    required this.rootNodeId,
    required this.contentHeight,
    required this.items,
    required this.geometry,
    required this.paragraphs,
    required this.semanticBounds,
    required this.semanticRegions,
  });
  final int rootNodeId;
  final double contentHeight;
  final List<_PaintItem> items;
  final List<RenderGeometryEntry> geometry;
  final List<_ParagraphState> paragraphs;
  final List<RenderSemanticBounds> semanticBounds;
  final List<FormatterSemanticRegion> semanticRegions;
  void paint(ui.Canvas canvas) {
    for (final item in items) {
      item.paint(canvas);
    }
  }
}

final class _FixtureLayout {
  _FixtureLayout({
    required this.nodes,
    required this.images,
    required this.viewport,
  });
  final Map<int, RenderNode> nodes;
  final Map<int, ui.Image> images;
  final RenderViewport viewport;
  final List<_PaintItem> _items = [];
  final List<RenderGeometryEntry> _geometry = [];
  final List<_ParagraphState> _paragraphs = [];
  final List<RenderSemanticBounds> _semanticBounds = [];
  final List<FormatterSemanticRegion> _semanticRegions = [];
  int _nextFragment = 1;
  int _paintOrder = 0;

  _LayoutResult build() {
    final roots = nodes.values.where((node) => node.parentId == null).toList();
    if (roots.length != 1) {
      throw const RenderProtocolException(
        'render.invalid-graph',
        'R3 requires one render root',
      );
    }
    final root = roots.single;
    final rootBackground = _color(root.styles['background'], 0xffffffff);
    final contentHeight = _number(root.styles['height'], 208);
    _items.add(
      _RectPaint(
        ui.Rect.fromLTWH(0, 0, viewport.width.toDouble(), contentHeight),
        rootBackground,
      ),
    );
    _addGeometry(
      root,
      ui.Rect.fromLTWH(0, 0, viewport.width.toDouble(), contentHeight),
      clip: ui.Rect.fromLTWH(
        0,
        0,
        viewport.width.toDouble(),
        viewport.height.toDouble(),
      ),
    );
    var y = 0.0;
    for (final child in _children(root.id)) {
      y = _layoutElement(child, 0, y, viewport.width.toDouble());
    }
    return _LayoutResult(
      rootNodeId: root.id,
      contentHeight: contentHeight.clamp(y, double.infinity),
      items: List.unmodifiable(_items),
      geometry: List.unmodifiable(_geometry),
      paragraphs: List.unmodifiable(_paragraphs),
      semanticBounds: List.unmodifiable(_semanticBounds),
      semanticRegions: List.unmodifiable(_semanticRegions),
    );
  }

  double _layoutElement(RenderNode node, double x, double y, double width) {
    final paintStart = _items.length;
    final margin = _number(node.styles['margin'], 0);
    final padding = _number(node.styles['padding'], 0);
    final left = x + margin;
    final top = y + margin;
    final innerWidth = width - margin * 2 - padding * 2;
    if (node.name == 'img') {
      final image = images[node.resourceIds.single];
      if (image == null) {
        throw const RenderProtocolException(
          'render.resource',
          'image is missing',
        );
      }
      final imageWidth = _number(node.styles['width'], image.width.toDouble());
      final imageHeight = imageWidth * image.height / image.width;
      final rect = ui.Rect.fromLTWH(left, top, imageWidth, imageHeight);
      _items.add(_ImagePaint(image, rect));
      _addGeometry(node, rect);
      _addSemantic(node, rect);
      return rect.bottom + margin;
    }

    final elementPaintOrder = _paintOrder++;
    final children = _children(node.id);
    final textChildren = children
        .where((child) => child.kind == RenderNodeKind.text)
        .toList();
    var cursor = top + padding;
    if (textChildren.isNotEmpty) {
      final (paragraph, ranges) = _paragraph(textChildren, innerWidth);
      final origin = ui.Offset(left + padding, cursor);
      _items.add(_ParagraphPaint(paragraph, origin));
      final rect = ui.Rect.fromLTWH(
        origin.dx,
        origin.dy,
        innerWidth,
        paragraph.height,
      );
      _paragraphs.add(
        _ParagraphState(
          paragraph: paragraph,
          origin: origin,
          ranges: Map.unmodifiable(ranges),
        ),
      );
      for (final text in textChildren) {
        _addGeometry(text, rect);
      }
      cursor = rect.bottom;
    }
    for (final child in children.where(
      (child) => child.kind != RenderNodeKind.text,
    )) {
      cursor = _layoutElement(child, left + padding, cursor, innerWidth);
    }
    final explicitHeight = _number(node.styles['height'], 0);
    final height = explicitHeight > 0
        ? explicitHeight
        : (cursor - top + padding).clamp(padding * 2, double.infinity);
    final rect = ui.Rect.fromLTWH(left, top, width - margin * 2, height);
    final background = node.styles['background'];
    if (background != null) {
      _items.insert(
        paintStart,
        _RectPaint(rect, _color(background, 0x00000000)),
      );
    }
    _addGeometry(node, rect, paintOrder: elementPaintOrder);
    _addSemantic(node, rect);
    return rect.bottom + margin;
  }

  (ui.Paragraph, Map<int, _TextRange>) _paragraph(
    List<RenderNode> nodes,
    double width,
  ) {
    final builder = ui.ParagraphBuilder(
      ui.ParagraphStyle(
        textDirection: ui.TextDirection.ltr,
        fontFamily: nodes.first.styles['font-family'],
      ),
    );
    final ranges = <int, _TextRange>{};
    var offset = 0;
    for (final node in nodes) {
      builder.pushStyle(
        ui.TextStyle(
          color: _color(node.styles['color'], 0xff111111),
          fontSize: _number(node.styles['font-size'], 16),
          fontWeight: node.styles['font-weight'] == 'bold'
              ? ui.FontWeight.bold
              : ui.FontWeight.normal,
        ),
      );
      builder.addText(node.text);
      builder.pop();
      final length = node.text.codeUnits.length;
      ranges[node.id] = _TextRange(offset, length);
      offset += length;
    }
    final paragraph = builder.build();
    paragraph.layout(ui.ParagraphConstraints(width: width));
    return (paragraph, ranges);
  }

  void _addGeometry(
    RenderNode node,
    ui.Rect rect, {
    ui.Rect? clip,
    int? paintOrder,
  }) {
    _geometry.add(
      RenderGeometryEntry(
        nodeId: node.id,
        fragmentId: _nextFragment++,
        borderBox: rect.renderRect,
        paddingBox: rect.renderRect,
        contentBox: rect.renderRect,
        clip: clip?.renderRect,
        scrollNodeId: 1,
        paintOrder: paintOrder ?? _paintOrder++,
      ),
    );
  }

  void _addSemantic(RenderNode node, ui.Rect rect) {
    final semantic = node.semantic;
    if (semantic == null) return;
    _semanticBounds.add(
      RenderSemanticBounds(
        semanticNodeId: semantic.id,
        nodeId: node.id,
        rects: [rect.renderRect],
      ),
    );
    _semanticRegions.add(
      FormatterSemanticRegion(descriptor: semantic, rect: rect),
    );
  }

  List<RenderNode> _children(int parentId) {
    final children = nodes.values
        .where((node) => node.parentId == parentId)
        .toList();
    children.sort((a, b) => a.siblingIndex.compareTo(b.siblingIndex));
    return children;
  }
}

ui.Color _color(String? value, int fallback) {
  if (value == null) return ui.Color(fallback);
  final normalized = value.startsWith('#') ? value.substring(1) : value;
  final parsed = int.tryParse(normalized, radix: 16);
  if (parsed == null || (normalized.length != 6 && normalized.length != 8)) {
    throw RenderProtocolException('render.style', 'invalid color $value');
  }
  return ui.Color(normalized.length == 6 ? 0xff000000 | parsed : parsed);
}

double _number(String? value, double fallback) =>
    value == null ? fallback : double.parse(value.replaceAll('px', ''));

bool _mapEquals(Map<String, String> a, Map<String, String> b) {
  if (a.length != b.length) return false;
  return a.entries.every((entry) => b[entry.key] == entry.value);
}

bool _listEquals<T>(List<T> a, List<T> b) {
  if (a.length != b.length) return false;
  for (var index = 0; index < a.length; index++) {
    if (a[index] != b[index]) return false;
  }
  return true;
}

bool _semanticsEqual(
  RenderSemanticDescriptor? a,
  RenderSemanticDescriptor? b,
) =>
    a == null && b == null ||
    a != null &&
        b != null &&
        a.id == b.id &&
        a.role == b.role &&
        a.name == b.name &&
        a.actionGeneration == b.actionGeneration;

extension on RenderRect {
  ui.Rect get uiRect => ui.Rect.fromLTWH(x, y, width, height);
}

extension on ui.Rect {
  RenderRect get renderRect => RenderRect(left, top, width, height);
}

extension<T> on Iterable<T> {
  T? get firstOrNull {
    final iterator = this.iterator;
    return iterator.moveNext() ? iterator.current : null;
  }
}
