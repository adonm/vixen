import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/browser_models.dart';
import 'package:vixen_shell/src/shell/texture_presenter.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('physical viewport preserves bounds and byte cap', () {
    final viewport = physicalFrameViewport(const Size(10000, 9000), 3);
    expect(viewport.width, lessThanOrEqualTo(browserMaxFrameDimension));
    expect(viewport.height, lessThanOrEqualTo(browserMaxFrameDimension));
    expect(
      viewport.width * viewport.height * 4,
      lessThanOrEqualTo(browserMaxFrameBytes),
    );
  });

  testWidgets('texture lifecycle creates, publishes, displays, and disposes', (
    tester,
  ) async {
    const channel = MethodChannel('dev.adonm.vixen/texture-test');
    final calls = <MethodCall>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      calls.add(call);
      return call.method == 'create' ? 77 : null;
    });
    final controller = LinuxTextureController(channel: channel, isLinux: true);
    final frame = testFrame(frameId: 5);

    await tester.pumpWidget(
      MaterialApp(
        home: SizedBox(
          width: 200,
          height: 100,
          child: BrowserContentSurface(
            contextState: null,
            frame: frame,
            textureController: controller,
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(calls.map((call) => call.method), ['create', 'publish']);
    final publish = calls[1].arguments! as Map<Object?, Object?>;
    expect(publish['width'], 2);
    expect(publish['height'], 1);
    expect(publish['rgba'], isA<Uint8List>());
    expect(find.byKey(const Key('browser-texture')), findsOneWidget);
    expect(tester.widget<Texture>(find.byType(Texture)).textureId, 77);

    await tester.pumpWidget(
      MaterialApp(
        home: SizedBox(
          width: 200,
          height: 100,
          child: BrowserContentSurface(
            contextState: null,
            frame: testFrame(frameId: 4),
            textureController: controller,
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(calls.where((call) => call.method == 'publish'), hasLength(1));

    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pumpAndSettle();
    expect(calls.last.method, 'dispose');
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      channel,
      null,
    );
  });

  test('non-Linux texture controller fails closed', () {
    final controller = LinuxTextureController(isLinux: false);
    expect(controller.create, throwsUnsupportedError);
  });

  testWidgets('content surface normalizes pointer and keyboard input', (
    tester,
  ) async {
    final mouseEvents = <(String, BrowserMouseEvent)>[];
    final keyEvents = <(String, BrowserKeyEvent)>[];
    var viewport = (width: 0, height: 0);
    await tester.pumpWidget(
      MaterialApp(
        home: Center(
          child: SizedBox(
            width: 400,
            height: 500,
            child: BrowserContentSurface(
              contextState: null,
              frame: testFrame(frameId: 8),
              textureController: _TestTextureController(),
              onPhysicalViewportChanged: (width, height, _) {
                viewport = (width: width, height: height);
              },
              onMouseEvent: (type, event) => mouseEvents.add((type, event)),
              onKeyEvent: (type, event) => keyEvents.add((type, event)),
            ),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('content-surface')));
    await tester.sendEventToBinding(
      PointerHoverEvent(
        position: tester.getCenter(find.byKey(const Key('content-surface'))),
      ),
    );
    await tester.pump();
    final gesture = await tester.startGesture(
      tester.getCenter(find.byKey(const Key('content-surface'))),
    );
    await gesture.cancel();
    await tester.sendKeyDownEvent(LogicalKeyboardKey.keyA);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.keyA);

    expect(mouseEvents.map((entry) => entry.$1), [
      'mousedown',
      'mouseup',
      'mousemove',
      'mousedown',
      'cancel',
    ]);
    expect(mouseEvents.first.$2.x, closeTo(viewport.width / 2, 0.01));
    expect(mouseEvents.first.$2.y, closeTo(viewport.height / 2, 0.01));
    expect(keyEvents.map((entry) => entry.$1), ['keydown', 'keyup']);
    expect(keyEvents.first.$2.key, 'a');
    expect(keyEvents.first.$2.code, 'KeyA');
  });

  testWidgets('BrowserCore nodes project into actionable Flutter Semantics', (
    tester,
  ) async {
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetDevicePixelRatio);
    BrowserAccessibilityNode? tapped;
    BrowserAccessibilityNode? focused;
    (BrowserAccessibilityNode, String)? setValue;
    (BrowserAccessibilityNode, bool)? adjusted;
    final parent = BrowserAccessibilityNode(
      id: 7,
      controlsIds: const [43],
      role: 'main',
      label: 'Page content',
      bounds: const BrowserAccessibilityRect(
        x: 0,
        y: 0,
        width: 300,
        height: 200,
      ),
      focused: false,
      disabled: false,
      selected: false,
      hidden: false,
      focusable: false,
      actions: const [],
    );
    final node = BrowserAccessibilityNode(
      id: 42,
      parentId: 7,
      role: 'button',
      label: 'Open settings',
      description: 'Opens browser preferences',
      bounds: const BrowserAccessibilityRect(
        x: 10,
        y: 20,
        width: 100,
        height: 40,
      ),
      focused: true,
      disabled: false,
      selected: false,
      hidden: false,
      liveRegion: true,
      focusable: true,
      actions: const ['tap', 'focus'],
    );
    final textbox = BrowserAccessibilityNode(
      id: 43,
      parentId: 7,
      role: 'textbox',
      label: 'Name',
      value: '',
      textSelection: const BrowserAccessibilityTextSelection(
        baseOffset: 0,
        extentOffset: 0,
      ),
      bounds: const BrowserAccessibilityRect(
        x: 10,
        y: 70,
        width: 120,
        height: 40,
      ),
      focused: false,
      disabled: false,
      selected: false,
      hidden: false,
      focusable: true,
      actions: const ['focus', 'set_value'],
    );
    final slider = BrowserAccessibilityNode(
      id: 44,
      parentId: 7,
      role: 'slider',
      label: 'Volume',
      value: '4',
      range: const BrowserAccessibilityRange(
        current: 4,
        minimum: 0,
        maximum: 10,
        step: 2,
      ),
      bounds: const BrowserAccessibilityRect(
        x: 10,
        y: 120,
        width: 120,
        height: 40,
      ),
      focused: false,
      disabled: false,
      selected: false,
      hidden: false,
      focusable: true,
      actions: const ['focus', 'increase', 'decrease'],
    );
    final semantics = tester.ensureSemantics();
    await tester.pumpWidget(
      MaterialApp(
        home: Center(
          child: SizedBox(
            width: 400,
            height: 300,
            child: BrowserContentSurface(
              contextState: BrowsingContextState.initial(
                10,
              ).copyWith(documentId: 20),
              frame: sizedTestFrame(frameId: 9, width: 400, height: 300),
              textureController: _TestTextureController(),
              accessibility: BrowserAccessibilitySnapshot(
                sourceGeneration: 9,
                generation: 9,
                contextId: 10,
                documentId: 20,
                viewportWidth: 400,
                viewportHeight: 300,
                nodes: [parent, node, textbox, slider],
                truncated: false,
              ),
              onSemanticTap: (_, value) => tapped = value,
              onSemanticFocus: (_, value) => focused = value,
              onSemanticSetValue: (_, node, value) => setValue = (node, value),
              onSemanticAdjustment: (_, node, increase) =>
                  adjusted = (node, increase),
            ),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    final finder = find.byKey(const ValueKey('semantic-9-42'));
    final parentFinder = find.byKey(const ValueKey('semantic-9-7'));
    expect(find.descendant(of: parentFinder, matching: finder), findsOneWidget);
    expect(
      tester.getSemantics(finder),
      matchesSemantics(
        label: 'Open settings',
        hint: 'Opens browser preferences',
        isButton: true,
        hasEnabledState: true,
        isEnabled: true,
        isFocusable: true,
        isFocused: true,
        isLiveRegion: true,
        hasTapAction: true,
        hasFocusAction: true,
      ),
    );
    tester.widget<Semantics>(finder).properties.onTap!();
    tester.widget<Semantics>(finder).properties.onFocus!();
    expect(tapped?.id, 42);
    expect(focused?.id, 42);
    final textboxFinder = find.byKey(const ValueKey('semantic-9-43'));
    expect(
      tester.getSemantics(textboxFinder),
      matchesSemantics(
        label: 'Name',
        value: '',
        isTextField: true,
        hasEnabledState: true,
        isEnabled: true,
        isFocusable: true,
        hasFocusAction: true,
        hasSetTextAction: true,
      ),
    );
    expect(
      tester.getSemantics(textboxFinder).getSemanticsData().textSelection,
      const TextSelection.collapsed(offset: 0),
    );
    tester.widget<Semantics>(textboxFinder).properties.onSetText!('Ada');
    expect(setValue?.$1.id, 43);
    expect(setValue?.$2, 'Ada');
    expect(tester.widget<Semantics>(parentFinder).properties.controlsNodes, {
      'vixen-10-20-43',
    });
    expect(
      tester.widget<Semantics>(textboxFinder).properties.identifier,
      'vixen-10-20-43',
    );
    final sliderFinder = find.byKey(const ValueKey('semantic-9-44'));
    expect(
      tester.getSemantics(sliderFinder),
      matchesSemantics(
        label: 'Volume',
        value: '4',
        isSlider: true,
        hasEnabledState: true,
        isEnabled: true,
        isFocusable: true,
        hasFocusAction: true,
        hasIncreaseAction: true,
        hasDecreaseAction: true,
      ),
    );
    final sliderWidget = tester.widget<Semantics>(sliderFinder);
    expect(sliderWidget.properties.minValue, '0');
    expect(sliderWidget.properties.maxValue, '10');
    expect(sliderWidget.properties.increasedValue, '6');
    expect(sliderWidget.properties.decreasedValue, '2');
    sliderWidget.properties.onIncrease!();
    expect(adjusted?.$1.id, 44);
    expect(adjusted?.$2, isTrue);
    semantics.dispose();
  });
}

BrowserFrame testFrame({required int frameId}) => BrowserFrame(
  rgba: Uint8List.fromList([1, 2, 3, 255, 4, 5, 6, 255]),
  width: 2,
  height: 1,
  frameId: frameId,
  contextId: 10,
  documentId: 20,
);

BrowserFrame sizedTestFrame({
  required int frameId,
  required int width,
  required int height,
}) => BrowserFrame(
  rgba: Uint8List(width * height * 4),
  width: width,
  height: height,
  frameId: frameId,
  contextId: 10,
  documentId: 20,
);

final class _TestTextureController implements BrowserTextureController {
  @override
  Future<int> create() async => 1;

  @override
  Future<void> publish(BrowserFrame frame) async {}

  @override
  Future<void> dispose() async {}
}
