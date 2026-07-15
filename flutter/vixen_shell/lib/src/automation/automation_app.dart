import 'dart:async';
import 'dart:io';

import 'package:flutter/widgets.dart';

import '../renderer/formatter.dart';
import '../renderer/formatter_painter.dart';
import '../shell/shell_coordinator.dart';
import 'automation_capture.dart';
import 'automation_config.dart';

const Duration vixenAutomationStartupTimeout = Duration(seconds: 60);
const Duration vixenAutomationShutdownTimeout = Duration(seconds: 5);

final class VixenAutomationApp extends StatefulWidget {
  const VixenAutomationApp({
    required this.config,
    required this.coordinator,
    this.writer = const AutomationCaptureWriter(),
    this.timeout = vixenAutomationStartupTimeout,
    this.onFinished,
    this.onReport = _stdoutReport,
    this.onError = _stderrReport,
    super.key,
  });

  final AutomationConfig config;
  final ShellCoordinator coordinator;
  final AutomationCaptureWriter writer;
  final Duration timeout;
  final ValueChanged<int>? onFinished;
  final ValueChanged<String> onReport;
  final ValueChanged<String> onError;

  @override
  State<VixenAutomationApp> createState() => _VixenAutomationAppState();
}

final class _VixenAutomationAppState extends State<VixenAutomationApp> {
  Timer? _timeout;
  FormatterCommitView? _scheduledView;
  bool _capturePending = false;
  bool _finished = false;

  @override
  void initState() {
    super.initState();
    widget.coordinator.addListener(_coordinatorChanged);
    widget.coordinator.updatePhysicalViewport(
      widget.config.width,
      widget.config.height,
    );
    _timeout = Timer(widget.timeout, () {
      unawaited(
        _finishFailure(
          TimeoutException(
            'automation capture did not produce a presented commit',
            widget.timeout,
          ),
        ),
      );
    });
    unawaited(widget.coordinator.start());
  }

  void _coordinatorChanged() {
    if (!mounted || _finished) return;
    setState(() {});
    final error = widget.coordinator.errorMessage;
    if (error != null) {
      unawaited(_finishFailure(StateError(error)));
      return;
    }
    final view = widget.coordinator.rendererView;
    if (view == null ||
        view.isRetired ||
        view.commit.viewport.width != widget.config.width ||
        view.commit.viewport.height != widget.config.height ||
        _capturePending ||
        identical(view, _scheduledView)) {
      return;
    }
    _scheduledView = view;
    _capturePending = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted ||
          _finished ||
          !identical(view, widget.coordinator.rendererView)) {
        _rescheduleCapture(view);
        return;
      }
      Timer.run(() {
        if (!mounted ||
            _finished ||
            !identical(view, widget.coordinator.rendererView)) {
          _rescheduleCapture(view);
          return;
        }
        unawaited(_capture(view));
      });
    });
  }

  void _rescheduleCapture(FormatterCommitView view) {
    if (!identical(view, _scheduledView)) return;
    _scheduledView = null;
    _capturePending = false;
    if (mounted && !_finished) _coordinatorChanged();
  }

  Future<void> _capture(FormatterCommitView view) async {
    try {
      final png = await widget.coordinator.capturePresentedRendererCommitPng(
        view,
      );
      await widget.writer.write(
        widget.config.outputPath,
        png,
        canPublish: () =>
            mounted &&
            !_finished &&
            identical(view, _scheduledView) &&
            identical(view, widget.coordinator.presentedRendererView),
      );
      widget.onReport(
        'Vixen automation captured context='
        '${view.commit.revision.contextId} '
        'document=${view.commit.revision.documentId} '
        'commit=${view.commit.commitId} '
        'viewport=${view.commit.viewport.width}x'
        '${view.commit.viewport.height} '
        'output=${widget.config.outputPath}',
      );
      await _finish(0);
    } catch (error, stackTrace) {
      await _finishFailure(error, stackTrace);
    }
  }

  Future<void> _finishFailure(Object error, [StackTrace? stackTrace]) async {
    if (_finished) return;
    widget.onError('Vixen automation failed: $error');
    if (stackTrace != null) widget.onError('$stackTrace');
    await _finish(1);
  }

  Future<void> _finish(int status) async {
    if (_finished) return;
    _finished = true;
    _timeout?.cancel();
    var exitStatus = status;
    try {
      await widget.coordinator.close().timeout(vixenAutomationShutdownTimeout);
    } catch (error, stackTrace) {
      exitStatus = 1;
      widget.onError('Vixen automation shutdown failed: $error');
      widget.onError('$stackTrace');
    } finally {
      widget.onFinished?.call(exitStatus);
    }
  }

  @override
  void dispose() {
    _timeout?.cancel();
    widget.coordinator.removeListener(_coordinatorChanged);
    widget.coordinator.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final view = _capturePending
        ? _scheduledView
        : widget.coordinator.rendererView;
    final validView =
        view != null &&
        !view.isRetired &&
        view.commit.viewport.width == widget.config.width &&
        view.commit.viewport.height == widget.config.height;
    return Directionality(
      textDirection: TextDirection.ltr,
      child: ColoredBox(
        color: const Color(0xff000000),
        child: Align(
          alignment: Alignment.topLeft,
          child: SizedBox(
            width: widget.config.width.toDouble(),
            height: widget.config.height.toDouble(),
            child: validView
                ? CustomPaint(
                    key: const Key('automation-renderer-view'),
                    painter: RenderCommitPainter(view),
                  )
                : const ColoredBox(
                    key: Key('automation-renderer-pending'),
                    color: Color(0xff000000),
                  ),
          ),
        ),
      ),
    );
  }
}

void _stdoutReport(String message) => stdout.writeln(message);

void _stderrReport(String message) => stderr.writeln(message);
