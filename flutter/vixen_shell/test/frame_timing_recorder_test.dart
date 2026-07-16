import 'dart:convert';
import 'dart:ui' as ui;

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/automation/frame_timing_recorder.dart';
import 'package:vixen_shell/src/renderer/formatter.dart';
import 'package:vixen_shell/src/renderer/formatter_painter.dart';

import 'support/r3_fixture.dart';

void main() {
  test(
    'joins exact painted, presented, and raster-finished frame identity',
    () async {
      final formatter = VixenFormatter();
      addTearDown(formatter.dispose);
      final view = (await formatter.acceptFullSnapshot(
        r3Snapshot(),
      ) as RenderApplied).view;
      final reports = <String>[];
      final recorder = PresentedFrameTimingRecorder(
        limit: 4,
        report: reports.add,
        clock: () => 2_000_000,
      );
      addTearDown(recorder.dispose);
      final timing = _timing(frameNumber: 7);

      recorder.recordPaint(view, 7);
      recorder.recordTimings([timing]);
      recorder.recordPresented(view, 60);

      expect(reports, hasLength(2));
      final presented = _record(reports[0]);
      final frame = _record(reports[1]);
      expect(presented['type'], 'presented_commit');
      expect(presented['commit_id'], view.commit.commitId);
      expect(presented['frame_number'], 7);
      expect(presented['coordinator_return_wall_us'], 2_000_000);
      expect(frame['type'], 'presented_commit_frame_timing');
      expect(frame['sequence'], presented['sequence']);
      expect(frame['commit_id'], view.commit.commitId);
      expect(frame['frame_number'], 7);
      expect(frame['refresh_rate_hz'], 60);
      expect(frame['raster_finish_wall_us'], 1_999_000);
      expect((frame['durations_us'] as Map)['build'], 2000);
      expect((frame['durations_us'] as Map)['raster'], 2500);
      expect((frame['durations_us'] as Map)['total_span'], 5500);
    },
  );

  test(
    'emits timing after acknowledgement and bounds retained commits',
    () async {
      final formatter = VixenFormatter();
      addTearDown(formatter.dispose);
      final view = (await formatter.acceptFullSnapshot(
        r3Snapshot(),
      ) as RenderApplied).view;
      final reports = <String>[];
      final recorder = PresentedFrameTimingRecorder(
        limit: 1,
        report: reports.add,
        clock: () => 2_000_000,
      );
      addTearDown(recorder.dispose);

      recorder.recordPaint(view, 9);
      recorder.recordPresented(view, double.nan);
      expect(reports, hasLength(1));
      recorder.recordTimings([_timing(frameNumber: 9)]);
      expect(reports, hasLength(2));
      expect(_record(reports[1])['refresh_rate_hz'], isNull);

      recorder.recordPaint(view, 10);
      recorder.recordPresented(view, 60);
      recorder.recordPresented(view, 60);
      expect(
        reports
            .map(_record)
            .where((record) => record['type'] == 'frame_timing_limit_reached'),
        hasLength(1),
      );
    },
  );

  test(
    'requires a live exact paint and ignores callback-only repaint changes',
    () async {
      final formatter = VixenFormatter();
      addTearDown(formatter.dispose);
      final view = (await formatter.acceptFullSnapshot(
        r3Snapshot(),
      ) as RenderApplied).view;
      final recorder = PresentedFrameTimingRecorder(limit: 1, report: (_) {});
      addTearDown(recorder.dispose);

      expect(() => recorder.recordPresented(view, 60), throwsStateError);
      expect(
        RenderCommitPainter(
          view,
          onPaint: (_, _) {},
        ).shouldRepaint(RenderCommitPainter(view)),
        isFalse,
      );
    },
  );
}

ui.FrameTiming _timing({required int frameNumber}) => ui.FrameTiming(
  vsyncStart: 1_000,
  buildStart: 1_500,
  buildFinish: 3_500,
  rasterStart: 4_000,
  rasterFinish: 6_500,
  rasterFinishWallTime: 1_999_000,
  frameNumber: frameNumber,
);

Map<String, Object?> _record(String line) {
  expect(line, startsWith(vixenMeasurementPrefix));
  return (jsonDecode(line.substring(vixenMeasurementPrefix.length)) as Map)
      .cast<String, Object?>();
}
