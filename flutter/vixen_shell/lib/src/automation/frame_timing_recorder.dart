import 'dart:convert';
import 'dart:ui' as ui;

import '../renderer/formatter.dart';

const String vixenMeasurementPrefix = 'Vixen measurement ';

typedef FrameMeasurementReporter = void Function(String message);
typedef FrameMeasurementClock = int Function();

final class PresentedFrameTimingRecorder {
  PresentedFrameTimingRecorder({
    required this.limit,
    required this.report,
    FrameMeasurementClock? clock,
  }) : _clock = clock ?? _wallTimeMicroseconds {
    if (limit < 1) throw ArgumentError.value(limit, 'limit');
  }

  final int limit;
  final FrameMeasurementReporter report;
  final FrameMeasurementClock _clock;
  final Map<int, _PaintedCommit> _paintedByCommit = {};
  final Map<int, ui.FrameTiming> _timingsByFrame = {};
  final Map<int, _PresentedCommit> _presentedByFrame = {};
  var _sequence = 0;
  var _limitReported = false;

  void recordPaint(FormatterCommitView view, int frameNumber) {
    if (frameNumber < 0 || view.isRetired) return;
    _paintedByCommit[view.commit.commitId] = _PaintedCommit(view, frameNumber);
    _trim(_paintedByCommit);
  }

  void recordPresented(FormatterCommitView view, double refreshRateHz) {
    if (_sequence >= limit) {
      if (!_limitReported) {
        _limitReported = true;
        _emit({'v': 1, 'type': 'frame_timing_limit_reached', 'limit': limit});
      }
      return;
    }
    final painted = _paintedByCommit[view.commit.commitId];
    if (painted == null || !identical(painted.view, view) || view.isRetired) {
      throw StateError(
        'presented commit was not painted in a live Flutter frame',
      );
    }
    _sequence++;
    final presented = _PresentedCommit(
      sequence: _sequence,
      view: view,
      frameNumber: painted.frameNumber,
      coordinatorReturnWallUs: _clock(),
      refreshRateHz: refreshRateHz.isFinite && refreshRateHz > 0
          ? refreshRateHz
          : null,
    );
    _presentedByFrame[presented.frameNumber] = presented;
    _emit({
      'v': 1,
      'type': 'presented_commit',
      'sequence': presented.sequence,
      'context_id': view.commit.revision.contextId,
      'document_id': view.commit.revision.documentId,
      'commit_id': view.commit.commitId,
      'revision': view.commit.revision.toWire(),
      'frame_number': presented.frameNumber,
      'coordinator_return_wall_us': presented.coordinatorReturnWallUs,
    });
    _emitTimingIfReady(presented.frameNumber);
  }

  void recordTimings(List<ui.FrameTiming> timings) {
    for (final timing in timings) {
      if (timing.frameNumber < 0) continue;
      _timingsByFrame[timing.frameNumber] = timing;
      _trim(_timingsByFrame);
      _emitTimingIfReady(timing.frameNumber);
    }
  }

  void dispose() {
    _paintedByCommit.clear();
    _timingsByFrame.clear();
    _presentedByFrame.clear();
  }

  void _emitTimingIfReady(int frameNumber) {
    final presented = _presentedByFrame[frameNumber];
    final timing = _timingsByFrame[frameNumber];
    if (presented == null || timing == null) return;
    final view = presented.view;
    if (view.isRetired) return;
    _emit({
      'v': 1,
      'type': 'presented_commit_frame_timing',
      'sequence': presented.sequence,
      'context_id': view.commit.revision.contextId,
      'document_id': view.commit.revision.documentId,
      'commit_id': view.commit.commitId,
      'frame_number': frameNumber,
      'refresh_rate_hz': presented.refreshRateHz,
      'engine_timestamps_us': {
        'vsync_start': timing.timestampInMicroseconds(ui.FramePhase.vsyncStart),
        'build_start': timing.timestampInMicroseconds(ui.FramePhase.buildStart),
        'build_finish': timing.timestampInMicroseconds(
          ui.FramePhase.buildFinish,
        ),
        'raster_start': timing.timestampInMicroseconds(
          ui.FramePhase.rasterStart,
        ),
        'raster_finish': timing.timestampInMicroseconds(
          ui.FramePhase.rasterFinish,
        ),
      },
      'raster_finish_wall_us': timing.timestampInMicroseconds(
        ui.FramePhase.rasterFinishWallTime,
      ),
      'durations_us': {
        'vsync_overhead': timing.vsyncOverhead.inMicroseconds,
        'build': timing.buildDuration.inMicroseconds,
        'raster': timing.rasterDuration.inMicroseconds,
        'total_span': timing.totalSpan.inMicroseconds,
      },
    });
    _presentedByFrame.remove(frameNumber);
    _timingsByFrame.remove(frameNumber);
    _paintedByCommit.remove(view.commit.commitId);
  }

  void _emit(Map<String, Object?> record) {
    report('$vixenMeasurementPrefix${jsonEncode(record)}');
  }

  void _trim<T>(Map<int, T> values) {
    final capacity = limit * 2;
    while (values.length > capacity) {
      values.remove(values.keys.first);
    }
  }
}

final class _PaintedCommit {
  const _PaintedCommit(this.view, this.frameNumber);
  final FormatterCommitView view;
  final int frameNumber;
}

final class _PresentedCommit {
  const _PresentedCommit({
    required this.sequence,
    required this.view,
    required this.frameNumber,
    required this.coordinatorReturnWallUs,
    required this.refreshRateHz,
  });

  final int sequence;
  final FormatterCommitView view;
  final int frameNumber;
  final int coordinatorReturnWallUs;
  final double? refreshRateHz;
}

int _wallTimeMicroseconds() => DateTime.now().microsecondsSinceEpoch;
