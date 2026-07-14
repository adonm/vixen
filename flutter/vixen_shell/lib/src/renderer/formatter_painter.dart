import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';

import '../bridge/render_models.dart';
import 'formatter.dart';

final class RenderCommitPainter extends CustomPainter {
  const RenderCommitPainter(this.view);
  final FormatterCommitView view;

  @override
  void paint(Canvas canvas, Size size) => view.paint(canvas);

  @override
  bool? hitTest(Offset position) =>
      position.dx >= 0 &&
      position.dy >= 0 &&
      position.dx < view.viewport.width &&
      position.dy < view.viewport.height;

  @override
  SemanticsBuilderCallback get semanticsBuilder =>
      (size) => view.semanticRegions
          .map(
            (region) => CustomPainterSemantics(
              key: ValueKey(region.descriptor.id),
              rect: region.rect,
              properties: _properties(region.descriptor),
            ),
          )
          .toList(growable: false);

  @override
  bool shouldRepaint(RenderCommitPainter oldDelegate) =>
      oldDelegate.view.commit.commitId != view.commit.commitId;

  @override
  bool shouldRebuildSemantics(RenderCommitPainter oldDelegate) =>
      shouldRepaint(oldDelegate);
}

SemanticsProperties _properties(RenderSemanticDescriptor descriptor) =>
    switch (descriptor.role) {
      'heading' => SemanticsProperties(
        label: descriptor.name,
        textDirection: TextDirection.ltr,
        header: true,
        headingLevel: 1,
      ),
      'link' => SemanticsProperties(
        label: descriptor.name,
        textDirection: TextDirection.ltr,
        link: true,
      ),
      'text' => SemanticsProperties(
        label: descriptor.name,
        textDirection: TextDirection.ltr,
      ),
      _ => SemanticsProperties(
        label: descriptor.name,
        value: descriptor.value,
        textDirection: TextDirection.ltr,
      ),
    };
