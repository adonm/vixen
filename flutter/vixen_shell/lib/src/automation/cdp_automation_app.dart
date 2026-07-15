import 'dart:async';
import 'dart:io';

import 'package:flutter/widgets.dart';

import '../renderer/formatter.dart';
import '../renderer/formatter_painter.dart';
import '../shell/shell_coordinator.dart';
import 'automation_app.dart';
import 'automation_config.dart';

const Duration vixenCdpRendererPollInterval = Duration(milliseconds: 4);

final class VixenCdpAutomationApp extends StatefulWidget {
  const VixenCdpAutomationApp({
    required this.config,
    required this.coordinator,
    this.onFinished,
    this.onError = _stderrReport,
    super.key,
  });

  final CdpAutomationConfig config;
  final ShellCoordinator coordinator;
  final ValueChanged<int>? onFinished;
  final ValueChanged<String> onError;

  @override
  State<VixenCdpAutomationApp> createState() => _VixenCdpAutomationAppState();
}

final class _VixenCdpAutomationAppState extends State<VixenCdpAutomationApp> {
  Timer? _rendererPoll;
  StreamSubscription<ProcessSignal>? _signalSubscription;
  FormatterCommitView? _scheduledPresentation;
  bool _finished = false;
  bool _serviceInFlight = false;

  @override
  void initState() {
    super.initState();
    widget.coordinator.addListener(_coordinatorChanged);
    widget.coordinator.updatePhysicalViewport(
      widget.config.width,
      widget.config.height,
    );
    widget.coordinator.updateContentFocus(true);
    _signalSubscription = ProcessSignal.sigterm.watch().listen((_) {
      unawaited(_finish(0));
    });
    unawaited(_start());
  }

  Future<void> _start() async {
    await widget.coordinator.start();
    if (_finished) return;
    final error = widget.coordinator.errorMessage;
    if (error != null) {
      await _fail(StateError(error));
      return;
    }
    try {
      await widget.coordinator.controller.startCdp(widget.config.port);
      _rendererPoll = Timer.periodic(vixenCdpRendererPollInterval, (_) {
        if (!_serviceInFlight && !_finished) unawaited(_serviceRenderer());
      });
    } catch (error, stackTrace) {
      await _fail(error, stackTrace);
    }
  }

  Future<void> _serviceRenderer() async {
    _serviceInFlight = true;
    try {
      await widget.coordinator.serviceExternalRendererUpdates();
    } catch (error, stackTrace) {
      await _fail(error, stackTrace);
    } finally {
      _serviceInFlight = false;
    }
  }

  void _coordinatorChanged() {
    if (!mounted || _finished) return;
    setState(() {});
    final error = widget.coordinator.errorMessage;
    if (error != null) {
      unawaited(_fail(StateError(error)));
      return;
    }
    final view = widget.coordinator.rendererView;
    if (view == null ||
        view.isRetired ||
        identical(view, _scheduledPresentation) ||
        identical(view, widget.coordinator.presentedRendererView)) {
      return;
    }
    _scheduledPresentation = view;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted ||
          _finished ||
          !identical(view, widget.coordinator.rendererView) ||
          view.isRetired) {
        if (identical(view, _scheduledPresentation)) {
          _scheduledPresentation = null;
        }
        return;
      }
      unawaited(_present(view));
    });
  }

  Future<void> _present(FormatterCommitView view) async {
    try {
      await widget.coordinator.rendererCommitPresented(view);
    } catch (error, stackTrace) {
      await _fail(error, stackTrace);
    } finally {
      if (identical(view, _scheduledPresentation)) {
        _scheduledPresentation = null;
      }
    }
  }

  Future<void> _fail(Object error, [StackTrace? stackTrace]) async {
    if (_finished) return;
    widget.onError('Vixen CDP automation failed: $error');
    if (stackTrace != null) widget.onError('$stackTrace');
    await _finish(1);
  }

  Future<void> _finish(int status) async {
    if (_finished) return;
    _finished = true;
    _rendererPoll?.cancel();
    await _signalSubscription?.cancel();
    var exitStatus = status;
    try {
      await widget.coordinator.close().timeout(vixenAutomationShutdownTimeout);
    } catch (error, stackTrace) {
      exitStatus = 1;
      widget.onError('Vixen CDP automation shutdown failed: $error');
      widget.onError('$stackTrace');
    } finally {
      widget.onFinished?.call(exitStatus);
    }
  }

  @override
  void dispose() {
    _rendererPoll?.cancel();
    unawaited(_signalSubscription?.cancel());
    widget.coordinator.removeListener(_coordinatorChanged);
    widget.coordinator.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final view = widget.coordinator.rendererView;
    final validView = view != null && !view.isRetired;
    return Directionality(
      textDirection: TextDirection.ltr,
      child: ColoredBox(
        color: const Color(0xff000000),
        child: Align(
          alignment: Alignment.topLeft,
          child: validView
              ? SizedBox(
                  width: view.commit.viewport.width.toDouble(),
                  height: view.commit.viewport.height.toDouble(),
                  child: CustomPaint(
                    key: const Key('cdp-automation-renderer-view'),
                    painter: RenderCommitPainter(view),
                  ),
                )
              : const SizedBox.shrink(
                  key: Key('cdp-automation-renderer-pending'),
                ),
        ),
      ),
    );
  }
}

void _stderrReport(String message) => stderr.writeln(message);
