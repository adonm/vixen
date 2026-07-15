import 'dart:ui' as ui;
import 'dart:convert';
import 'dart:typed_data';

import '../bridge/render_models.dart';

const int _maxDecodedImagePixels = 64 * 1024 * 1024;

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

final class FormatterTextMatch {
  FormatterTextMatch({
    required this.nodeId,
    required this.utf16Start,
    required this.utf16End,
    required this.startCaret,
    required this.endCaret,
    required List<RenderRect> boxes,
  }) : boxes = List.unmodifiable(boxes);

  final int nodeId;
  final int utf16Start;
  final int utf16End;
  final RenderRect startCaret;
  final RenderRect endCaret;
  final List<RenderRect> boxes;
}

final class FormatterFindResult {
  FormatterFindResult({
    required this.commitId,
    required this.revision,
    required this.query,
    required List<FormatterTextMatch> matches,
  }) : matches = List.unmodifiable(matches),
       boxes = List.unmodifiable(matches.expand((match) => match.boxes));

  final int commitId;
  final RenderRevision revision;
  final String query;
  final List<FormatterTextMatch> matches;
  final List<RenderRect> boxes;
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

  RenderInputTarget? answerHitTest(RenderHitTestQuery query) {
    _requireLive();
    if (query.contextId != commit.revision.contextId ||
        query.documentId != commit.revision.documentId ||
        query.displayedCommitId != commit.commitId ||
        query.revision != commit.revision ||
        query.handle != commit.hitTestHandle) {
      throw const RenderProtocolException(
        'render.stale',
        'hit-test query does not name this displayed commit',
      );
    }
    final point = ui.Offset(query.point.x, query.point.y);
    final hit = hitTest(point, handle: query.handle);
    if (hit == null) return null;
    return RenderInputTarget(
      queryId: query.queryId,
      contextId: query.contextId,
      documentId: query.documentId,
      displayedCommitId: query.displayedCommitId,
      revision: query.revision,
      handle: query.handle,
      nodeId: hit.nodeId,
      fragmentId: hit.fragmentId,
      viewportPoint: query.point,
      localPoint: RenderPoint(hit.localPoint.dx, hit.localPoint.dy),
    );
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

  RenderTextQueryBatchResult answerTextQueries(RenderTextQueryBatch batch) {
    _requireLive();
    if (batch.contextId != commit.revision.contextId ||
        batch.documentId != commit.revision.documentId ||
        batch.commitId != commit.commitId ||
        batch.revision != commit.revision ||
        batch.handle != commit.textQueryHandle) {
      throw const RenderProtocolException(
        'render.stale',
        'text query does not name this accepted commit',
      );
    }
    if (batch.queries.length > renderMaxTextQueries) {
      throw const RenderProtocolException(
        'render.limit',
        'text query batch exceeds the query limit',
      );
    }
    final seen = <int>{};
    var textBoxCount = 0;
    final results = batch.queries
        .map((query) {
          if (!seen.add(query.queryId)) {
            throw const RenderProtocolException(
              'render.duplicate-id',
              'text query repeats a query id',
            );
          }
          final value = switch (query.kind) {
            RenderOffsetForPoint(:final point) => () {
              final state = _paragraphState(query.nodeId);
              final range = state.ranges[query.nodeId]!;
              final position = state.paragraph.getPositionForOffset(
                ui.Offset(point.x, point.y) - state.origin,
              );
              return RenderTextOffsetValue(
                (position.offset - range.start).clamp(0, range.length),
                position.affinity == ui.TextAffinity.upstream
                    ? RenderTextAffinity.upstream
                    : RenderTextAffinity.downstream,
              );
            }(),
            RenderCaretForOffset(:final utf16Offset, :final affinity) =>
              RenderTextCaretValue(
                _caretRect(query.nodeId, utf16Offset, affinity),
                affinity,
              ),
            RenderRangeBoxes(:final utf16Start, :final utf16End) => () {
              final boxes = _rangeTextBoxes(query.nodeId, utf16Start, utf16End);
              textBoxCount += boxes.length;
              if (textBoxCount > renderMaxTextBoxes) {
                throw const RenderProtocolException(
                  'render.limit',
                  'text query response exceeds the text box limit',
                );
              }
              return RenderTextRangeBoxesValue(boxes);
            }(),
          };
          return RenderTextQueryResult(queryId: query.queryId, value: value);
        })
        .toList(growable: false);
    return RenderTextQueryBatchResult(
      contextId: batch.contextId,
      documentId: batch.documentId,
      commitId: batch.commitId,
      revision: batch.revision,
      results: results,
    );
  }

  FormatterFindResult findText(String query, {bool caseSensitive = false}) {
    _requireLive();
    if (utf8.encode(query).length > renderMaxStringBytes) {
      throw const RenderProtocolException(
        'render.limit',
        'find query exceeds the renderer string limit',
      );
    }
    if (query.isEmpty) {
      return FormatterFindResult(
        commitId: commit.commitId,
        revision: commit.revision,
        query: query,
        matches: const [],
      );
    }
    final needle = caseSensitive ? query : _foldFindText(query);
    final matches = <FormatterTextMatch>[];
    var textBoxCount = 0;
    for (final paragraph in _paragraphs) {
      for (final entry in paragraph.textByNode.entries) {
        final haystack = caseSensitive
            ? entry.value
            : _foldFindText(entry.value);
        var start = 0;
        while (start <= haystack.length - needle.length) {
          final found = haystack.indexOf(needle, start);
          if (found < 0) break;
          final end = found + needle.length;
          final boxes = rangeBoxes(
            handle: commit.textQueryHandle,
            nodeId: entry.key,
            start: found,
            end: end,
          );
          textBoxCount += boxes.length;
          if (matches.length >= renderMaxTextQueries ||
              textBoxCount > renderMaxTextBoxes) {
            throw const RenderProtocolException(
              'render.limit',
              'find geometry exceeds renderer query limits',
            );
          }
          matches.add(
            FormatterTextMatch(
              nodeId: entry.key,
              utf16Start: found,
              utf16End: end,
              startCaret: _caretRect(
                entry.key,
                found,
                RenderTextAffinity.downstream,
              ),
              endCaret: _caretRect(entry.key, end, RenderTextAffinity.upstream),
              boxes: boxes,
            ),
          );
          start = end;
        }
      }
    }
    return FormatterFindResult(
      commitId: commit.commitId,
      revision: commit.revision,
      query: query,
      matches: matches,
    );
  }

  _ParagraphState _paragraphState(int nodeId) {
    final state = _paragraphs
        .where((entry) => entry.ranges.containsKey(nodeId))
        .firstOrNull;
    if (state == null) {
      throw const RenderProtocolException(
        'render.unknown-id',
        'text query names an unknown text node',
      );
    }
    return state;
  }

  List<RenderTextBox> _rangeTextBoxes(int nodeId, int start, int end) {
    final state = _paragraphState(nodeId);
    final range = state.ranges[nodeId]!;
    if (start < 0 || end < start || end > range.length) {
      throw const RenderProtocolException(
        'render.invalid-geometry',
        'text range is outside the source text',
      );
    }
    return state.paragraph
        .getBoxesForRange(range.start + start, range.start + end)
        .map(
          (box) => RenderTextBox(
            rect: RenderRect(
              state.origin.dx + box.left,
              state.origin.dy + box.top,
              box.right - box.left,
              box.bottom - box.top,
            ),
            direction: box.direction == ui.TextDirection.rtl
                ? RenderTextDirection.rtl
                : RenderTextDirection.ltr,
          ),
        )
        .toList(growable: false);
  }

  RenderRect _caretRect(int nodeId, int offset, RenderTextAffinity affinity) {
    final state = _paragraphState(nodeId);
    final range = state.ranges[nodeId]!;
    if (offset < 0 || offset > range.length) {
      throw const RenderProtocolException(
        'render.invalid-geometry',
        'caret offset is outside the source text',
      );
    }
    if (range.length == 0) {
      return RenderRect(
        state.origin.dx,
        state.origin.dy,
        1,
        state.paragraph.height,
      );
    }
    final usePrevious =
        offset == range.length ||
        (affinity == RenderTextAffinity.upstream && offset > 0);
    final glyphOffset = range.start + (usePrevious ? offset - 1 : offset);
    final boxes = state.paragraph.getBoxesForRange(
      glyphOffset,
      glyphOffset + 1,
    );
    if (boxes.isEmpty) {
      return RenderRect(
        state.origin.dx,
        state.origin.dy,
        1,
        state.paragraph.height,
      );
    }
    final box = boxes.first;
    final x = state.origin.dx + (usePrevious ? box.right : box.left);
    return RenderRect(x, state.origin.dy + box.top, 1, box.bottom - box.top);
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

final class _FormatterSource {
  _FormatterSource({
    required this.revision,
    required this.viewport,
    required Map<int, RenderNode> nodes,
    required Map<int, RenderResource> resources,
    required Map<int, RenderScrollIntent> scrollIntents,
    int? sourceNodeCount,
    int? sourceResourceCount,
    int? sourceScrollIntentCount,
  }) : nodes = Map.unmodifiable(nodes),
       resources = Map.unmodifiable(resources),
       scrollIntents = Map.unmodifiable(scrollIntents),
       sourceNodeCount = sourceNodeCount ?? nodes.length,
       sourceResourceCount = sourceResourceCount ?? resources.length,
       sourceScrollIntentCount =
           sourceScrollIntentCount ?? scrollIntents.length;

  final RenderRevision revision;
  final RenderViewport viewport;
  final Map<int, RenderNode> nodes;
  final Map<int, RenderResource> resources;
  final Map<int, RenderScrollIntent> scrollIntents;
  final int sourceNodeCount;
  final int sourceResourceCount;
  final int sourceScrollIntentCount;
}

final class VixenFormatter {
  _FormatterSource? _source;
  FormatterCommitView? _staged;
  FormatterCommitView? _presented;
  int _nextCommitId = 1;
  int _nextHandle = 1;
  int _stageEpoch = 0;
  bool _disposed = false;

  RenderRevision? get sourceRevision => _source?.revision;
  FormatterCommitView? get acceptedView => _staged;
  FormatterCommitView? get displayedView => _presented;

  Future<RenderApplyResult> acceptFullSnapshot(
    FullRenderSnapshot snapshot, {
    void Function(RenderCommit commit)? beforePublish,
  }) async {
    _requireOpen();
    _validateSnapshot(snapshot);
    final candidate = _FormatterSource(
      revision: snapshot.revision,
      viewport: snapshot.viewport,
      nodes: {for (final node in snapshot.nodes) node.id: node},
      resources: {
        for (final resource in snapshot.resources) resource.id: resource,
      },
      scrollIntents: {
        for (final intent in snapshot.scrollIntents)
          intent.scrollNodeId: intent,
      },
    );
    if (_source case final current?) {
      if (snapshot.revision == current.revision &&
          !_sourceMatches(candidate, current)) {
        throw const RenderProtocolException(
          'render.revision',
          'equal revision contains different state',
        );
      }
      if (snapshot.revision == current.revision) {
        final staged = _staged;
        if (staged == null) {
          throw const RenderProtocolException(
            'render.stale',
            'equal revision has no retained commit',
          );
        }
        return RenderApplied(staged);
      }
      if (_regresses(snapshot.revision, current.revision)) {
        throw const RenderProtocolException(
          'render.stale',
          'full snapshot regresses the current revision',
        );
      }
      if (snapshot.revision.contextId == current.revision.contextId &&
          snapshot.revision.documentId == current.revision.documentId &&
          snapshot.revision.viewportGeneration ==
              current.revision.viewportGeneration &&
          snapshot.viewport != current.viewport) {
        throw const RenderProtocolException(
          'render.revision',
          'snapshot changed viewport without advancing its generation',
        );
      }
    }
    return RenderApplied(
      await _stageAndPublish(candidate, beforePublish: beforePublish),
    );
  }

  Future<RenderApplyResult> applyMutationBatch(
    RenderMutationBatch batch, {
    void Function(RenderCommit commit)? beforePublish,
  }) async {
    _requireOpen();
    if (batch.mutations.length > renderMaxMutations) {
      throw const RenderProtocolException(
        'render.limit',
        'mutation batch exceeds the mutation limit',
      );
    }
    final current = _source;
    if (current == null || batch.baseRevision != current.revision) {
      return RenderResyncRequired(
        RenderResyncRequest(
          contextId: batch.targetRevision.contextId,
          documentId: batch.targetRevision.documentId,
          currentRevision: current?.revision,
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
    final next = Map<int, RenderNode>.of(current.nodes);
    final resources = Map<int, RenderResource>.of(current.resources);
    final scrollIntents = Map<int, RenderScrollIntent>.of(
      current.scrollIntents,
    );
    var viewport = current.viewport;
    var viewportMutations = 0;
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
        case SetRenderViewport(viewport: final nextViewport):
          viewport = nextViewport;
          viewportMutations++;
          if (viewportMutations > 1) {
            throw const RenderProtocolException(
              'render.invalid-graph',
              'mutation batch repeats the viewport',
            );
          }
        case UpsertRenderResource(:final resource):
          resources[resource.id] = resource;
        case RemoveRenderResource(:final resourceId):
          if (resources.remove(resourceId) == null) {
            throw const RenderProtocolException(
              'render.unknown-id',
              'mutation removes an unknown resource',
            );
          }
        case SetRenderScrollIntent(:final intent):
          scrollIntents[intent.scrollNodeId] = intent;
        case RemoveRenderScrollIntent(:final scrollNodeId):
          if (scrollIntents.remove(scrollNodeId) == null) {
            throw const RenderProtocolException(
              'render.unknown-id',
              'mutation removes an unknown scroll intent',
            );
          }
      }
    }
    final viewportAdvanced =
        batch.targetRevision.viewportGeneration !=
        batch.baseRevision.viewportGeneration;
    if (viewportAdvanced != (viewportMutations == 1)) {
      throw const RenderProtocolException(
        'render.revision',
        'viewport generation and mutation must advance together',
      );
    }
    final candidate = _FormatterSource(
      revision: batch.targetRevision,
      viewport: viewport,
      nodes: next,
      resources: resources,
      scrollIntents: scrollIntents,
    );
    _validateSource(candidate);
    return RenderApplied(
      await _stageAndPublish(candidate, beforePublish: beforePublish),
    );
  }

  FormatterCommitView present(RenderPresented presented) {
    final staged = _staged;
    if (staged == null ||
        presented.contextId != staged.commit.revision.contextId ||
        presented.documentId != staged.commit.revision.documentId ||
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

  bool releaseHandles(RenderHandleRelease release) {
    final staged = _staged;
    final presented = _presented;
    final view = staged?.commit.commitId == release.commitId
        ? staged
        : presented?.commit.commitId == release.commitId
        ? presented
        : null;
    if (view == null || view.isRetired) return false;
    if (view.commit.hitTestHandle != release.hitTestHandle ||
        view.commit.textQueryHandle != release.textQueryHandle) {
      throw const RenderProtocolException(
        'render.stale',
        'handle release does not match its commit',
      );
    }
    if (identical(_staged, view)) _staged = null;
    if (identical(_presented, view)) _presented = null;
    view.retire();
    return true;
  }

  RenderResyncRequest reset({required int contextId, required int documentId}) {
    _stageEpoch++;
    _staged?.retire();
    if (!identical(_presented, _staged)) _presented?.retire();
    _staged = null;
    _presented = null;
    _source = null;
    return RenderResyncRequest(
      contextId: contextId,
      documentId: documentId,
      currentRevision: null,
      rejectedBaseRevision: null,
      reason: 'renderer_reset',
    );
  }

  void dispose() {
    if (_disposed) return;
    reset(contextId: 1, documentId: 1);
    _disposed = true;
  }

  Future<FormatterCommitView> _stageAndPublish(
    _FormatterSource candidate, {
    void Function(RenderCommit commit)? beforePublish,
  }) async {
    final epoch = ++_stageEpoch;
    final view = await _stage(candidate);
    if (_disposed || epoch != _stageEpoch) {
      view.retire();
      throw const RenderProtocolException(
        'render.stale',
        'formatter build was superseded before publication',
      );
    }
    try {
      beforePublish?.call(view.commit);
    } catch (_) {
      view.retire();
      rethrow;
    }
    if (_staged != null && !identical(_staged, _presented)) {
      _staged!.retire();
    }
    _source = candidate;
    _staged = view;
    return view;
  }

  Future<FormatterCommitView> _stage(_FormatterSource source) async {
    final revision = source.revision;
    final viewport = source.viewport;
    final decodedImages = <int, ui.Image>{};
    _LayoutResult? layout;
    ui.Picture? picture;
    try {
      var decodedPixels = 0;
      for (final resource in source.resources.values) {
        if (resource.kind != RenderResourceKind.image ||
            resource.mime != 'image/png') {
          throw const RenderProtocolException(
            'render.resource',
            'R3 accepts only policy-approved PNG resources',
          );
        }
        final image = await _decodePng(resource.bytes);
        decodedPixels += image.width * image.height;
        if (decodedPixels > _maxDecodedImagePixels) {
          image.dispose();
          throw const RenderProtocolException(
            'render.limit',
            'decoded images exceed the formatter pixel limit',
          );
        }
        decodedImages[resource.id] = image;
      }
      layout = _FixtureLayout(
        nodes: source.nodes,
        images: decodedImages,
        viewport: viewport,
      ).build();
      final maxScrollY = (layout.contentHeight - viewport.height)
          .clamp(0, double.infinity)
          .toDouble();
      final scrollOffset = _resolveScrollOffset(
        source,
        rootNodeId: layout.rootNodeId,
        maxScrollY: maxScrollY,
      );
      final geometry = layout.geometry
          .map((entry) => _translateGeometry(entry, scrollOffset))
          .toList(growable: false);
      final paragraphs = layout.paragraphs
          .map(
            (paragraph) => _ParagraphState(
              paragraph: paragraph.paragraph,
              origin: paragraph.origin - scrollOffset,
              ranges: paragraph.ranges,
              textByNode: paragraph.textByNode,
            ),
          )
          .toList(growable: false);
      final semanticBounds = layout.semanticBounds
          .map((bounds) => _translateSemanticBounds(bounds, scrollOffset))
          .toList(growable: false);
      final semanticRegions = layout.semanticRegions
          .map(
            (region) => FormatterSemanticRegion(
              descriptor: region.descriptor,
              rect: region.rect.shift(-scrollOffset),
            ),
          )
          .toList(growable: false);
      final semanticRectCount = semanticBounds.fold(
        0,
        (count, bounds) => count + bounds.rects.length,
      );
      if (geometry.length > renderMaxGeometryEntries ||
          semanticBounds.length > renderMaxSemanticBounds ||
          semanticRectCount > renderMaxGeometryEntries) {
        throw const RenderProtocolException(
          'render.limit',
          'formatter output exceeds a commit geometry limit',
        );
      }
      for (final entry in geometry) {
        _validateCommitRect(entry.borderBox);
        _validateCommitRect(entry.paddingBox);
        _validateCommitRect(entry.contentBox);
        if (entry.clip case final clip?) _validateCommitRect(clip);
      }
      for (final bounds in semanticBounds) {
        for (final rect in bounds.rects) {
          _validateCommitRect(rect);
        }
      }
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
      canvas.translate(-scrollOffset.dx, -scrollOffset.dy);
      layout.paint(canvas);
      canvas.restore();
      picture = recorder.endRecording();
      final commitId = _nextCommitId++;
      final view = FormatterCommitView._(
        commit: RenderCommit(
          commitId: commitId,
          revision: revision,
          viewport: viewport,
          geometry: geometry,
          hitTestHandle: _nextHandle++,
          textQueryHandle: _nextHandle++,
          scroll: [
            RenderScrollState(
              scrollNodeId: 1,
              nodeId: layout.rootNodeId,
              offsetX: scrollOffset.dx,
              offsetY: scrollOffset.dy,
              maxOffsetX: 0,
              maxOffsetY: maxScrollY,
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
          semantics: semanticBounds,
        ),
        picture: picture,
        paragraphs: paragraphs,
        images: decodedImages.values.toList(growable: false),
        semanticRegions: semanticRegions,
      );
      return view;
    } catch (_) {
      picture?.dispose();
      layout?.disposeParagraphs();
      for (final image in decodedImages.values) {
        image.dispose();
      }
      rethrow;
    }
  }

  void _validateSnapshot(FullRenderSnapshot snapshot) {
    _validateSource(
      _FormatterSource(
        revision: snapshot.revision,
        viewport: snapshot.viewport,
        nodes: {for (final node in snapshot.nodes) node.id: node},
        resources: {
          for (final resource in snapshot.resources) resource.id: resource,
        },
        scrollIntents: {
          for (final intent in snapshot.scrollIntents)
            intent.scrollNodeId: intent,
        },
        sourceNodeCount: snapshot.nodes.length,
        sourceResourceCount: snapshot.resources.length,
        sourceScrollIntentCount: snapshot.scrollIntents.length,
      ),
    );
  }

  void _validateSource(_FormatterSource source) {
    source.revision.validate();
    final viewport = source.viewport;
    if (viewport.width <= 0 ||
        viewport.height <= 0 ||
        viewport.width > renderMaxViewportDimension ||
        viewport.height > renderMaxViewportDimension ||
        !viewport.deviceScale.isFinite ||
        !viewport.pageZoom.isFinite ||
        viewport.deviceScale <= 0 ||
        viewport.pageZoom <= 0 ||
        viewport.deviceScale > renderMaxScale ||
        viewport.pageZoom > renderMaxScale) {
      throw const RenderProtocolException(
        'render.invalid-geometry',
        'viewport is outside the renderer limits',
      );
    }
    if (source.sourceNodeCount > renderMaxNodes ||
        source.sourceResourceCount > renderMaxResources ||
        source.sourceScrollIntentCount > renderMaxScrollEntries ||
        source.nodes.length != source.sourceNodeCount ||
        source.resources.length != source.sourceResourceCount ||
        source.scrollIntents.length != source.sourceScrollIntentCount) {
      throw const RenderProtocolException(
        'render.limit',
        'renderer source count exceeds a protocol limit or repeats an id',
      );
    }
    var totalResourceBytes = 0;
    for (final resource in source.resources.values) {
      if (resource.id <= 0 ||
          resource.bytes.length > renderMaxResourceBytes ||
          utf8.encode(resource.mime).length > renderMaxStringBytes) {
        throw const RenderProtocolException(
          'render.limit',
          'renderer resource exceeds a protocol limit',
        );
      }
      totalResourceBytes += resource.bytes.length;
      if (totalResourceBytes > renderMaxTotalResourceBytes) {
        throw const RenderProtocolException(
          'render.limit',
          'renderer resources exceed the aggregate byte limit',
        );
      }
    }
    var totalStringBytes = 0;
    for (final node in source.nodes.values) {
      if (node.depth > renderMaxTreeDepth ||
          node.styles.length > renderMaxStylesPerNode ||
          node.resourceIds.length > renderMaxResourcesPerNode ||
          (node.semantic?.actions.length ?? 0) >
              renderMaxSemanticActionsPerNode) {
        throw const RenderProtocolException(
          'render.limit',
          'renderer node exceeds a protocol limit',
        );
      }
      final strings = <String>[
        node.name,
        node.text,
        ...node.styles.keys,
        ...node.styles.values,
        if (node.semantic case final semantic?) semantic.name,
        if (node.semantic case final semantic?) semantic.role,
        ?node.semantic?.value,
      ];
      for (final value in strings) {
        final length = utf8.encode(value).length;
        if (length > renderMaxStringBytes) {
          throw const RenderProtocolException(
            'render.limit',
            'renderer string exceeds its byte limit',
          );
        }
        totalStringBytes += length;
        if (totalStringBytes > renderMaxTotalStringBytes) {
          throw const RenderProtocolException(
            'render.limit',
            'renderer strings exceed the aggregate byte limit',
          );
        }
      }
    }
    _validateNodeGraph(source.nodes.values, source.resources.keys.toSet());
    final semanticIds = <int>{};
    for (final node in source.nodes.values) {
      final semantic = node.semantic;
      if (semantic != null &&
          (semantic.id <= 0 ||
              semantic.actionGeneration <= 0 ||
              !semanticIds.add(semantic.id) ||
              semantic.actions.toSet().length != semantic.actions.length)) {
        throw const RenderProtocolException(
          'render.invalid-graph',
          'semantic ids, generations, and actions must be valid and unique',
        );
      }
    }
    for (final intent in source.scrollIntents.values) {
      if (intent.scrollNodeId <= 0 ||
          !source.nodes.containsKey(intent.nodeId) ||
          !intent.point.x.isFinite ||
          !intent.point.y.isFinite ||
          intent.point.x.abs() > renderMaxCoordinate ||
          intent.point.y.abs() > renderMaxCoordinate) {
        throw const RenderProtocolException(
          'render.invalid-geometry',
          'scroll intent is invalid',
        );
      }
    }
  }

  ui.Offset _resolveScrollOffset(
    _FormatterSource source, {
    required int rootNodeId,
    required double maxScrollY,
  }) {
    if (source.scrollIntents.isEmpty) return ui.Offset.zero;
    if (source.scrollIntents.length != 1) {
      throw const RenderProtocolException(
        'render.unsupported',
        'R4 formatter supports exactly one root scroll intent',
      );
    }
    final intent = source.scrollIntents.values.single;
    if (intent.scrollNodeId != 1 || intent.nodeId != rootNodeId) {
      throw const RenderProtocolException(
        'render.unsupported',
        'R4 formatter scroll intent must name the root scroll node',
      );
    }
    var previous = ui.Offset.zero;
    final staged = _staged;
    if (staged != null &&
        staged.commit.revision.contextId == source.revision.contextId &&
        staged.commit.revision.documentId == source.revision.documentId &&
        staged.commit.scroll.length == 1) {
      final scroll = staged.commit.scroll.single;
      previous = ui.Offset(scroll.offsetX, scroll.offsetY);
    }
    final requested = switch (intent.kind) {
      RenderScrollIntentKind.by =>
        previous + ui.Offset(intent.point.x, intent.point.y),
      RenderScrollIntentKind.to || RenderScrollIntentKind.restore => ui.Offset(
        intent.point.x,
        intent.point.y,
      ),
    };
    return ui.Offset(0, requested.dy.clamp(0, maxScrollY).toDouble());
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
      if (node.resourceIds.toSet().length != node.resourceIds.length) {
        throw const RenderProtocolException(
          'render.duplicate-id',
          'render node repeats a resource reference',
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

  bool _sourceMatches(_FormatterSource next, _FormatterSource current) {
    if (current.viewport != next.viewport ||
        current.nodes.length != next.nodes.length ||
        current.resources.length != next.resources.length ||
        current.scrollIntents.length != next.scrollIntents.length) {
      return false;
    }
    final nodesMatch = next.nodes.values.every((node) {
      final existing = current.nodes[node.id];
      return existing != null &&
          existing.parentId == node.parentId &&
          existing.siblingIndex == node.siblingIndex &&
          existing.depth == node.depth &&
          existing.kind == node.kind &&
          existing.name == node.name &&
          existing.text == node.text &&
          _mapEquals(existing.styles, node.styles) &&
          _listEquals(existing.resourceIds, node.resourceIds) &&
          _semanticsEqual(existing.semantic, node.semantic);
    });
    final resourcesMatch = next.resources.values.every((resource) {
      final existing = current.resources[resource.id];
      return existing != null &&
          existing.kind == resource.kind &&
          existing.mime == resource.mime &&
          _listEquals(existing.bytes, resource.bytes);
    });
    return nodesMatch &&
        resourcesMatch &&
        _scrollIntentsEqual(next.scrollIntents, current.scrollIntents);
  }

  bool _regresses(RenderRevision next, RenderRevision current) =>
      next.contextId == current.contextId &&
      next.documentId == current.documentId &&
      (next.sourceGeneration < current.sourceGeneration ||
          next.styleGeneration < current.styleGeneration ||
          next.viewportGeneration < current.viewportGeneration ||
          next.resourceGeneration < current.resourceGeneration);

  void _requireOpen() {
    if (_disposed) {
      throw const RenderProtocolException(
        'render.closed',
        'formatter is disposed',
      );
    }
  }
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
    required this.textByNode,
  });
  final ui.Paragraph paragraph;
  final ui.Offset origin;
  final Map<int, _TextRange> ranges;
  final Map<int, String> textByNode;
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

  void disposeParagraphs() {
    for (final paragraph in paragraphs) {
      paragraph.paragraph.dispose();
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
    try {
      return _build();
    } catch (_) {
      for (final paragraph in _paragraphs) {
        paragraph.paragraph.dispose();
      }
      rethrow;
    }
  }

  _LayoutResult _build() {
    final roots = nodes.values.where((node) => node.parentId == null).toList();
    if (roots.length != 1) {
      throw const RenderProtocolException(
        'render.invalid-graph',
        'R3 requires one render root',
      );
    }
    final root = roots.single;
    final rootBackground = _color(
      root.styles['background-color'] ?? root.styles['background'],
      0xffffffff,
    );
    final contentHeight =
        _scaledLength(root.styles['height']) ?? viewport.height.toDouble();
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
    for (final child in _children(
      root.id,
    ).where((child) => !_isHidden(child))) {
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
    if (_isHidden(node)) return y;
    final paintStart = _items.length;
    final defaultMargin = node.name == 'body' ? 8.0 : 0.0;
    final marginTop =
        _edge(node.styles, 'margin', 'top', defaultMargin) * viewport.pageZoom;
    final marginRight =
        _edge(node.styles, 'margin', 'right', defaultMargin) *
        viewport.pageZoom;
    final marginBottom =
        _edge(node.styles, 'margin', 'bottom', defaultMargin) *
        viewport.pageZoom;
    final marginLeft =
        _edge(node.styles, 'margin', 'left', defaultMargin) * viewport.pageZoom;
    final paddingTop =
        _edge(node.styles, 'padding', 'top', 0) * viewport.pageZoom;
    final paddingRight =
        _edge(node.styles, 'padding', 'right', 0) * viewport.pageZoom;
    final paddingBottom =
        _edge(node.styles, 'padding', 'bottom', 0) * viewport.pageZoom;
    final paddingLeft =
        _edge(node.styles, 'padding', 'left', 0) * viewport.pageZoom;
    final left = x + marginLeft;
    final top = y + marginTop;
    final availableWidth = (width - marginLeft - marginRight)
        .clamp(0, double.infinity)
        .toDouble();
    final authoredWidth = _scaledLength(node.styles['width']);
    final boxWidth = authoredWidth ?? availableWidth;
    final innerWidth = (boxWidth - paddingLeft - paddingRight)
        .clamp(0, double.infinity)
        .toDouble();
    if (node.name == 'img') {
      final image = images[node.resourceIds.single];
      if (image == null) {
        throw const RenderProtocolException(
          'render.resource',
          'image is missing',
        );
      }
      final imageWidth = authoredWidth ?? image.width.toDouble();
      final imageHeight =
          _scaledLength(node.styles['height']) ??
          imageWidth * image.height / image.width;
      final rect = ui.Rect.fromLTWH(left, top, imageWidth, imageHeight);
      _items.add(_ImagePaint(image, rect));
      _addGeometry(node, rect);
      _addSemantic(node, [rect]);
      return rect.bottom + marginBottom;
    }

    final elementPaintOrder = _paintOrder++;
    final children = _children(node.id);
    final textChildren = children
        .where((child) => child.kind == RenderNodeKind.text)
        .toList();
    var semanticRects = <ui.Rect>[];
    var cursor = top + paddingTop;
    if (textChildren.isNotEmpty) {
      final (paragraph, ranges, textByNode) = _paragraph(
        textChildren,
        innerWidth,
      );
      final origin = ui.Offset(left + paddingLeft, cursor);
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
          textByNode: Map.unmodifiable(textByNode),
        ),
      );
      final textLength = ranges.values.fold(
        0,
        (length, range) => length + range.length,
      );
      semanticRects = paragraph
          .getBoxesForRange(0, textLength)
          .map(
            (box) => ui.Rect.fromLTRB(
              origin.dx + box.left,
              origin.dy + box.top,
              origin.dx + box.right,
              origin.dy + box.bottom,
            ),
          )
          .toList(growable: false);
      for (final text in textChildren) {
        final range = ranges[text.id]!;
        final boxes = paragraph.getBoxesForRange(
          range.start,
          range.start + range.length,
        );
        if (boxes.isEmpty) {
          _addGeometry(
            text,
            ui.Rect.fromLTWH(origin.dx, origin.dy, 0, paragraph.height),
          );
        } else {
          for (final box in boxes) {
            _addGeometry(
              text,
              ui.Rect.fromLTRB(
                origin.dx + box.left,
                origin.dy + box.top,
                origin.dx + box.right,
                origin.dy + box.bottom,
              ),
            );
          }
        }
      }
      cursor = rect.bottom;
    }
    for (final child in children.where(
      (child) => child.kind != RenderNodeKind.text && !_isHidden(child),
    )) {
      cursor = _layoutElement(child, left + paddingLeft, cursor, innerWidth);
    }
    final explicitHeight = _scaledLength(node.styles['height']);
    final height =
        explicitHeight ??
        (cursor - top + paddingBottom)
            .clamp(paddingTop + paddingBottom, double.infinity)
            .toDouble();
    final rect = ui.Rect.fromLTWH(left, top, boxWidth, height);
    final background =
        node.styles['background-color'] ?? node.styles['background'];
    if (background != null) {
      _items.insert(
        paintStart,
        _RectPaint(rect, _color(background, 0x00000000)),
      );
    }
    final paddingRect = rect;
    final contentRect = ui.Rect.fromLTWH(
      rect.left + paddingLeft,
      rect.top + paddingTop,
      (rect.width - paddingLeft - paddingRight)
          .clamp(0, double.infinity)
          .toDouble(),
      (rect.height - paddingTop - paddingBottom)
          .clamp(0, double.infinity)
          .toDouble(),
    );
    _addGeometry(
      node,
      rect,
      paddingBox: paddingRect,
      contentBox: contentRect,
      paintOrder: elementPaintOrder,
    );
    _addSemantic(node, semanticRects.isEmpty ? [rect] : semanticRects);
    return rect.bottom + marginBottom;
  }

  (ui.Paragraph, Map<int, _TextRange>, Map<int, String>) _paragraph(
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
    final textByNode = <int, String>{};
    var offset = 0;
    for (final node in nodes) {
      builder.pushStyle(
        ui.TextStyle(
          color: _color(node.styles['color'], 0xff111111),
          fontSize: _number(node.styles['font-size'], 16) * viewport.pageZoom,
          fontWeight: node.styles['font-weight'] == 'bold'
              ? ui.FontWeight.bold
              : ui.FontWeight.normal,
        ),
      );
      builder.addText(node.text);
      builder.pop();
      final length = node.text.codeUnits.length;
      ranges[node.id] = _TextRange(offset, length);
      textByNode[node.id] = node.text;
      offset += length;
    }
    final paragraph = builder.build();
    try {
      paragraph.layout(ui.ParagraphConstraints(width: width));
    } catch (_) {
      paragraph.dispose();
      rethrow;
    }
    return (paragraph, ranges, textByNode);
  }

  void _addGeometry(
    RenderNode node,
    ui.Rect rect, {
    ui.Rect? paddingBox,
    ui.Rect? contentBox,
    ui.Rect? clip,
    int? paintOrder,
  }) {
    _geometry.add(
      RenderGeometryEntry(
        nodeId: node.id,
        fragmentId: _nextFragment++,
        borderBox: rect.renderRect,
        paddingBox: (paddingBox ?? rect).renderRect,
        contentBox: (contentBox ?? rect).renderRect,
        clip: clip?.renderRect,
        scrollNodeId: 1,
        paintOrder: paintOrder ?? _paintOrder++,
      ),
    );
  }

  void _addSemantic(RenderNode node, List<ui.Rect> rects) {
    final semantic = node.semantic;
    if (semantic == null) return;
    _semanticBounds.add(
      RenderSemanticBounds(
        semanticNodeId: semantic.id,
        nodeId: node.id,
        rects: rects.map((rect) => rect.renderRect).toList(growable: false),
      ),
    );
    final union = rects
        .skip(1)
        .fold(rects.first, (bounds, rect) => bounds.expandToInclude(rect));
    _semanticRegions.add(
      FormatterSemanticRegion(descriptor: semantic, rect: union),
    );
  }

  List<RenderNode> _children(int parentId) {
    final children = nodes.values
        .where((node) => node.parentId == parentId)
        .toList();
    children.sort((a, b) => a.siblingIndex.compareTo(b.siblingIndex));
    return children;
  }

  double? _scaledLength(String? value) {
    final parsed = _length(value);
    return parsed == null ? null : parsed * viewport.pageZoom;
  }
}

ui.Color _color(String? value, int fallback) {
  if (value == null) return ui.Color(fallback);
  final keyword = value.trim().toLowerCase();
  if (keyword == 'transparent') return const ui.Color(0x00000000);
  const named = <String, int>{
    'black': 0xff000000,
    'white': 0xffffffff,
    'red': 0xffff0000,
    'green': 0xff008000,
    'blue': 0xff0000ff,
    'yellow': 0xffffff00,
    'gray': 0xff808080,
    'grey': 0xff808080,
    'purple': 0xff800080,
  };
  if (named[keyword] case final color?) return ui.Color(color);
  var normalized = keyword.startsWith('#') ? keyword.substring(1) : keyword;
  if (normalized.length == 3 || normalized.length == 4) {
    normalized = normalized.split('').map((digit) => '$digit$digit').join();
  }
  final parsed = int.tryParse(normalized, radix: 16);
  if (parsed == null || (normalized.length != 6 && normalized.length != 8)) {
    throw RenderProtocolException('render.style', 'invalid color $value');
  }
  return ui.Color(normalized.length == 6 ? 0xff000000 | parsed : parsed);
}

bool _isHidden(RenderNode node) =>
    const {
      'head',
      'title',
      'meta',
      'link',
      'style',
      'script',
      'template',
      'source',
    }.contains(node.name) ||
    node.styles['display'] == 'none' ||
    node.styles['visibility'] == 'hidden';

double _edge(
  Map<String, String> styles,
  String property,
  String side,
  double fallback,
) {
  final explicit = _length(styles['$property-$side']);
  if (explicit != null) return explicit;
  final shorthand = styles[property];
  if (shorthand == null) return fallback;
  final values = shorthand
      .trim()
      .split(RegExp(r'\s+'))
      .map(_length)
      .toList(growable: false);
  if (values.any((value) => value == null) ||
      values.isEmpty ||
      values.length > 4) {
    return fallback;
  }
  final top = values[0]!;
  final right = values.length == 1 ? top : values[1]!;
  final bottom = values.length <= 2 ? top : values[2]!;
  final left = switch (values.length) {
    1 => top,
    2 || 3 => right,
    _ => values[3]!,
  };
  return switch (side) {
    'top' => top,
    'right' => right,
    'bottom' => bottom,
    'left' => left,
    _ => fallback,
  };
}

double? _length(String? value) {
  if (value == null) return null;
  final normalized = value.trim().toLowerCase();
  if (normalized == 'auto' ||
      normalized.endsWith('%') ||
      normalized.endsWith('em') ||
      normalized.endsWith('rem') ||
      normalized.startsWith('calc(')) {
    return null;
  }
  final parsed = double.tryParse(normalized.replaceAll('px', ''));
  if (parsed == null ||
      !parsed.isFinite ||
      parsed.abs() > renderMaxCoordinate) {
    return null;
  }
  return parsed;
}

double _number(String? value, double fallback) {
  if (value == null) return fallback;
  final parsed = double.tryParse(value.replaceAll('px', ''));
  if (parsed == null ||
      !parsed.isFinite ||
      parsed.abs() > renderMaxCoordinate) {
    throw RenderProtocolException(
      'render.style',
      'invalid finite numeric style $value',
    );
  }
  return parsed;
}

String _foldFindText(String value) => String.fromCharCodes(
  value.codeUnits.map(
    (unit) => unit >= 0x41 && unit <= 0x5a ? unit + 0x20 : unit,
  ),
);

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
        a.value == b.value &&
        a.actionGeneration == b.actionGeneration &&
        _listEquals(a.actions, b.actions);

bool _scrollIntentsEqual(
  Map<int, RenderScrollIntent> a,
  Map<int, RenderScrollIntent> b,
) {
  if (a.length != b.length) return false;
  return a.entries.every((entry) {
    final other = b[entry.key];
    return other != null &&
        entry.value.nodeId == other.nodeId &&
        entry.value.kind == other.kind &&
        entry.value.point.x == other.point.x &&
        entry.value.point.y == other.point.y;
  });
}

RenderGeometryEntry _translateGeometry(
  RenderGeometryEntry entry,
  ui.Offset offset,
) => RenderGeometryEntry(
  nodeId: entry.nodeId,
  fragmentId: entry.fragmentId,
  borderBox: entry.borderBox.shift(-offset),
  paddingBox: entry.paddingBox.shift(-offset),
  contentBox: entry.contentBox.shift(-offset),
  clip: entry.clip,
  scrollNodeId: entry.scrollNodeId,
  paintOrder: entry.paintOrder,
);

RenderSemanticBounds _translateSemanticBounds(
  RenderSemanticBounds bounds,
  ui.Offset offset,
) => RenderSemanticBounds(
  semanticNodeId: bounds.semanticNodeId,
  nodeId: bounds.nodeId,
  rects: bounds.rects
      .map((rect) => rect.shift(-offset))
      .toList(growable: false),
);

void _validateCommitRect(RenderRect rect) {
  if (!rect.x.isFinite ||
      !rect.y.isFinite ||
      !rect.width.isFinite ||
      !rect.height.isFinite ||
      rect.width < 0 ||
      rect.height < 0 ||
      rect.x.abs() > renderMaxCoordinate ||
      rect.y.abs() > renderMaxCoordinate ||
      rect.width > renderMaxCoordinate ||
      rect.height > renderMaxCoordinate) {
    throw const RenderProtocolException(
      'render.invalid-geometry',
      'formatter produced geometry outside the protocol limits',
    );
  }
}

extension on RenderRect {
  ui.Rect get uiRect => ui.Rect.fromLTWH(x, y, width, height);

  RenderRect shift(ui.Offset offset) =>
      RenderRect(x + offset.dx, y + offset.dy, width, height);
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
