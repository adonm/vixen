import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';

import '../bridge/render_models.dart';
import 'formatter.dart';

typedef FormatterSemanticActionCallback = void Function(
  RenderSemanticDescriptor descriptor,
  RenderSemanticActionKind action,
  String? value,
);

final class RenderCommitPainter extends CustomPainter {
  const RenderCommitPainter(
    this.view, {
    this.findResult,
    this.onSemanticAction,
  });
  final FormatterCommitView view;
  final FormatterFindResult? findResult;
  final FormatterSemanticActionCallback? onSemanticAction;

  @override
  void paint(Canvas canvas, Size size) {
    view.paint(canvas);
    final result = findResult;
    if (result == null ||
        result.commitId != view.commit.commitId ||
        result.revision != view.commit.revision) {
      return;
    }
    final paint = Paint()..color = const Color(0x66ffd54f);
    for (final box in result.boxes) {
      canvas.drawRect(
        Rect.fromLTWH(box.x, box.y, box.width, box.height),
        paint,
      );
    }
  }

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
              properties: _properties(region.descriptor, onSemanticAction),
            ),
          )
          .toList(growable: false);

  @override
  bool shouldRepaint(RenderCommitPainter oldDelegate) =>
      oldDelegate.view.commit.commitId != view.commit.commitId ||
      oldDelegate.findResult != findResult;

  @override
  bool shouldRebuildSemantics(RenderCommitPainter oldDelegate) =>
      oldDelegate.view.commit.commitId != view.commit.commitId ||
      oldDelegate.onSemanticAction != onSemanticAction;
}

SemanticsProperties _properties(
  RenderSemanticDescriptor descriptor,
  FormatterSemanticActionCallback? callback,
) {
  final actions = descriptor.actions;
  final onTap =
      callback != null && actions.contains(RenderSemanticActionKind.activate)
      ? () => callback.call(descriptor, RenderSemanticActionKind.activate, null)
      : null;
  final onFocus =
      callback != null && actions.contains(RenderSemanticActionKind.focus)
      ? () => callback.call(descriptor, RenderSemanticActionKind.focus, null)
      : null;
  final onSetText =
      callback != null && actions.contains(RenderSemanticActionKind.setValue)
      ? (String value) =>
            callback.call(descriptor, RenderSemanticActionKind.setValue, value)
      : null;
  final onIncrease =
      callback != null && actions.contains(RenderSemanticActionKind.increase)
      ? () => callback.call(descriptor, RenderSemanticActionKind.increase, null)
      : null;
  final onDecrease =
      callback != null && actions.contains(RenderSemanticActionKind.decrease)
      ? () => callback.call(descriptor, RenderSemanticActionKind.decrease, null)
      : null;
  return SemanticsProperties(
    label: descriptor.name,
    value: descriptor.value,
    textDirection: TextDirection.ltr,
    header: descriptor.role == 'heading',
    headingLevel: descriptor.role == 'heading' ? 1 : null,
    link: descriptor.role == 'link',
    textField: descriptor.role == 'textbox',
    button: descriptor.role == 'button',
    onTap: onTap,
    onFocus: onFocus,
    onSetText: onSetText,
    onIncrease: onIncrease,
    onDecrease: onDecrease,
  );
}
