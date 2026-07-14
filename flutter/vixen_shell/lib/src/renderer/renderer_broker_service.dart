import '../bridge/native/native_renderer_protocol.dart';
import '../bridge/render_models.dart';
import '../bridge/renderer_transport.dart';
import 'formatter.dart';

/// Services one bounded renderer message without invoking BrowserCore or the
/// serialized browser command path.
final class RendererBrokerService {
  const RendererBrokerService({
    required this.transport,
    required this.formatter,
  });

  final RendererTransport transport;
  final VixenFormatter formatter;

  Future<bool> serviceNext({int timeoutMilliseconds = 0}) async {
    final message = transport.pollRenderer(
      timeoutMilliseconds: timeoutMilliseconds,
    );
    if (message == null) return false;
    switch (message) {
      case NativeFullSnapshotUpdate(:final snapshot):
        final result = await formatter.acceptFullSnapshot(snapshot);
        _submitApplyResult(result);
      case NativeMutationBatchUpdate(:final batch):
        final result = await formatter.applyMutationBatch(batch);
        _submitApplyResult(result);
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
    }
    return true;
  }

  void _submitApplyResult(RenderApplyResult result) {
    switch (result) {
      case RenderApplied(:final view):
        transport.submitRenderer(rendererCommitSubmission(view.commit));
      case RenderResyncRequired(:final request):
        transport.submitRenderer(rendererResyncSubmission(request));
    }
  }

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
}
