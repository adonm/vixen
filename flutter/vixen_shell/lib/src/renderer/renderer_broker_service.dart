import 'dart:ui' as ui;

import '../bridge/native/native_renderer_protocol.dart';
import '../bridge/render_models.dart';
import '../bridge/renderer_transport.dart';
import 'formatter.dart';

/// Services one bounded renderer message without invoking BrowserCore or the
/// serialized browser command path.
final class RendererBrokerService {
  RendererBrokerService({required this.transport, required this.formatter});

  final RendererTransport transport;
  final VixenFormatter formatter;
  bool _servicing = false;

  Future<bool> serviceNext({int timeoutMilliseconds = 0}) async {
    if (_servicing) {
      throw const RenderProtocolException(
        'render.busy',
        'renderer service already has one message in flight',
      );
    }
    _servicing = true;
    try {
      final message = transport.pollRenderer(
        timeoutMilliseconds: timeoutMilliseconds,
      );
      if (message == null) return false;
      switch (message) {
        case NativeFullSnapshotUpdate(:final snapshot):
          await formatter.acceptFullSnapshot(
            snapshot,
            beforePublish: _submitCommit,
          );
        case NativeMutationBatchUpdate(:final batch):
          final result = await formatter.applyMutationBatch(
            batch,
            beforePublish: _submitCommit,
          );
          if (result case RenderResyncRequired(:final request)) {
            transport.submitRenderer(rendererResyncSubmission(request));
          }
        case NativeHandleReleaseUpdate(:final release):
          formatter.releaseHandles(release);
        case NativeEnsureLayoutRequest():
          _answerRequest(message, () {
            final view = formatter.acceptedView;
            if (view == null ||
                view.commit.revision != message.requiredRevision) {
              throw const RenderProtocolException(
                'render.stale',
                'required revision has no accepted formatter commit',
              );
            }
            return rendererCommitResponse(message.requestId, view.commit);
          });
        case NativeHitTestRequest():
          _answerRequest(message, () {
            final view = formatter.displayedView;
            if (view == null) {
              throw const RenderProtocolException(
                'render.stale',
                'hit testing requires a displayed commit',
              );
            }
            return rendererHitTestResponse(
              message.requestId,
              view.answerHitTest(message.query),
            );
          });
        case NativeTextQueryRequest():
          _answerRequest(message, () {
            final view = formatter.acceptedView;
            if (view == null) {
              throw const RenderProtocolException(
                'render.stale',
                'text queries require an accepted commit',
              );
            }
            return rendererTextQueryResponse(
              message.requestId,
              view.answerTextQueries(message.batch),
            );
          });
        case NativeCaptureSceneRequest():
          await _answerCaptureRequest(message);
        case NativeResetRendererRequest():
          formatter.reset(
            contextId: message.contextId,
            documentId: message.documentId,
          );
          transport.respondRenderer(rendererResetResponse(message.requestId));
      }
      return true;
    } finally {
      _servicing = false;
    }
  }

  void _submitCommit(RenderCommit commit) =>
      transport.submitRenderer(rendererCommitSubmission(commit));

  void _answerRequest(
    NativeRendererRequest request,
    Map<String, Object?> Function() answer,
  ) {
    try {
      transport.respondRenderer(answer());
    } on RenderProtocolException catch (error) {
      transport.respondRenderer(
        rendererFailedResponse(
          request.requestId,
          code: error.code,
          message: error.message,
        ),
      );
    }
  }

  Future<void> _answerCaptureRequest(NativeCaptureSceneRequest request) async {
    try {
      final view = formatter.displayedView;
      if (view == null ||
          view.isRetired ||
          view.commit.revision.contextId != request.contextId ||
          view.commit.revision.documentId != request.documentId ||
          view.commit.commitId != request.displayedCommitId ||
          view.commit.revision != request.revision ||
          view.commit.viewport != request.viewport) {
        throw const RenderProtocolException(
          'render.stale',
          'capture requires the exact displayed formatter commit',
        );
      }
      final image = await view.capture();
      try {
        final data = await image.toByteData(format: ui.ImageByteFormat.png);
        if (data == null) {
          throw const RenderProtocolException(
            'render.capture',
            'Flutter did not encode the displayed scene as PNG',
          );
        }
        if (!identical(view, formatter.displayedView) || view.isRetired) {
          throw const RenderProtocolException(
            'render.stale',
            'displayed formatter commit changed during capture',
          );
        }
        final png = data.buffer.asUint8List(
          data.offsetInBytes,
          data.lengthInBytes,
        );
        if (png.length < 24 || png.length > renderMaxCaptureBytes) {
          throw const RenderProtocolException(
            'render.limit',
            'captured PNG exceeds the renderer capture bound',
          );
        }
        transport.respondRendererCapture(request.requestId, png);
      } finally {
        image.dispose();
      }
    } on RenderProtocolException catch (error) {
      transport.respondRenderer(
        rendererFailedResponse(
          request.requestId,
          code: error.code,
          message: error.message,
        ),
      );
    } catch (error) {
      transport.respondRenderer(
        rendererFailedResponse(
          request.requestId,
          code: 'render.capture',
          message: '$error',
        ),
      );
    }
  }
}
