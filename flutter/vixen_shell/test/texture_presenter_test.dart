import 'dart:async';

import 'package:flutter/gestures.dart';
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

  test('viewport transform shares DPR across frame, input, and semantics', () {
    final transform = BrowserViewportTransform.fromLogical(
      const Size(400, 500),
      2,
    );

    expect((transform.width, transform.height), (800, 1000));
    expect(transform.scaleFactor, 2);
    expect(
      transform.localToPhysical(const Offset(200, 250)),
      const Offset(400, 500),
    );
    expect(
      transform.logicalDeltaToPhysical(const Offset(4, 6)),
      const Offset(8, 12),
    );
    expect(
      transform.physicalRectToLocal(const Rect.fromLTWH(100, 120, 200, 80)),
      const Rect.fromLTWH(50, 60, 100, 40),
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

  testWidgets('texture presentation recreates twice after transient loss', (
    tester,
  ) async {
    final controller = _FlakyTextureController(failuresRemaining: 2);

    await tester.pumpWidget(
      MaterialApp(
        home: BrowserContentSurface(
          contextState: null,
          frame: testFrame(frameId: 6),
          textureController: controller,
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(controller.createCount, 3);
    expect(controller.publishCount, 3);
    expect(controller.disposeCount, 2);
    expect(find.byKey(const Key('browser-texture')), findsOneWidget);
    expect(find.byKey(const Key('surface-recovery-failed')), findsNothing);

    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pumpAndSettle();
  });

  testWidgets('texture presentation bounds failure and accepts a newer frame', (
    tester,
  ) async {
    final controller = _FlakyTextureController(failuresRemaining: 10);
    Widget surface(BrowserFrame frame) => MaterialApp(
      home: BrowserContentSurface(
        contextState: null,
        frame: frame,
        textureController: controller,
      ),
    );

    await tester.pumpWidget(surface(testFrame(frameId: 7)));
    await tester.pumpAndSettle();

    expect(controller.publishCount, 3);
    expect(find.byKey(const Key('surface-recovery-failed')), findsOneWidget);

    controller.failuresRemaining = 0;
    await tester.pumpWidget(surface(testFrame(frameId: 8)));
    await tester.pumpAndSettle();

    expect(controller.publishCount, 4);
    expect(find.byKey(const Key('browser-texture')), findsOneWidget);
    expect(find.byKey(const Key('surface-recovery-failed')), findsNothing);

    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pumpAndSettle();
  });

  testWidgets(
    'detach and resume reject stale publish and recover newer texture loss',
    (tester) async {
      final controller = _LifecycleFaultTextureController(
        blockedFrameId: 31,
        failingFrameId: 32,
      );
      Widget surface(BrowserFrame frame, BrowserHostLifecycle lifecycle) =>
          MaterialApp(
            home: BrowserContentSurface(
              contextState: null,
              frame: frame,
              lifecycle: lifecycle,
              textureController: controller,
            ),
          );

      await tester.pumpWidget(
        surface(testFrame(frameId: 30), BrowserHostLifecycle.resumed),
      );
      await tester.pumpAndSettle();
      expect(tester.widget<Texture>(find.byType(Texture)).textureId, 1);

      await tester.pumpWidget(
        surface(testFrame(frameId: 31), BrowserHostLifecycle.resumed),
      );
      await tester.pump();
      expect(controller.publishedFrameIds, [30, 31]);

      await tester.pumpWidget(
        surface(testFrame(frameId: 31), BrowserHostLifecycle.detached),
      );
      await tester.pumpWidget(
        surface(testFrame(frameId: 32), BrowserHostLifecycle.resumed),
      );
      expect(find.byKey(const Key('browser-texture')), findsNothing);

      controller.releaseBlockedPublish();
      await tester.pumpAndSettle();

      expect(controller.publishedFrameIds, [30, 31, 32, 32]);
      expect(controller.createCount, 3);
      expect(controller.disposeCount, 2);
      expect(tester.widget<Texture>(find.byType(Texture)).textureId, 3);
      expect(find.byKey(const Key('surface-recovery-failed')), findsNothing);

      await tester.pumpWidget(const SizedBox.shrink());
      await tester.pumpAndSettle();
    },
  );

  testWidgets('content surface normalizes pointer and keyboard input', (
    tester,
  ) async {
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.resetDevicePixelRatio);
    final mouseEvents = <(String, BrowserMouseEvent)>[];
    final keyEvents = <(String, BrowserKeyEvent)>[];
    var viewport = (width: 0, height: 0);
    var scaleFactor = 0.0;
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
              onPhysicalViewportChanged: (width, height, scale) {
                viewport = (width: width, height: height);
                scaleFactor = scale;
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
    await tester.sendEventToBinding(
      PointerScrollEvent(
        position: tester.getCenter(find.byKey(const Key('content-surface'))),
        scrollDelta: const Offset(4, 6),
      ),
    );
    await tester.pump();
    final gesture = await tester.startGesture(
      tester.getCenter(find.byKey(const Key('content-surface'))),
    );
    await gesture.cancel();
    final touchScroll = await tester.startGesture(
      tester.getCenter(find.byKey(const Key('content-surface'))),
      pointer: 2,
    );
    await touchScroll.moveBy(const Offset(-20, -30));
    await touchScroll.up();
    await tester.sendKeyDownEvent(LogicalKeyboardKey.keyA);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.keyA);
    await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.keyF);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);

    expect(mouseEvents.map((entry) => entry.$1), [
      'mousedown',
      'mouseup',
      'wheel',
      'mousemove',
      'mousedown',
      'cancel',
      'mousedown',
      'cancel',
      'wheel',
    ]);
    expect(mouseEvents.first.$2.x, closeTo(viewport.width / 2, 0.01));
    expect(mouseEvents.first.$2.y, closeTo(viewport.height / 2, 0.01));
    expect(scaleFactor, 2);
    final wheels = mouseEvents
        .where((entry) => entry.$1 == 'wheel')
        .map((entry) => entry.$2)
        .toList();
    expect(wheels.first.deltaX, 8);
    expect(wheels.first.deltaY, 12);
    expect(wheels.last.deltaX, 40);
    expect(wheels.last.deltaY, 60);
    final aEvents = keyEvents.where((entry) => entry.$2.key == 'a').toList();
    expect(aEvents.map((entry) => entry.$1), ['keydown', 'keyup']);
    expect(aEvents.first.$2.code, 'KeyA');
    expect(keyEvents.where((entry) => entry.$2.key == 'f'), isEmpty);
  });

  testWidgets('focused BrowserCore text controls accept platform IME state', (
    tester,
  ) async {
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetDevicePixelRatio);
    final states = <BrowserTextInputState>[];
    final keyEvents = <(String, BrowserKeyEvent)>[];
    final context = BrowsingContextState.initial(
      10,
    ).copyWith(documentId: 20, runtimeContextId: 30);
    final snapshot = BrowserAccessibilitySnapshot(
      sourceGeneration: 1,
      generation: 1,
      contextId: 10,
      documentId: 20,
      viewportWidth: 320,
      viewportHeight: 200,
      nodes: [
        BrowserAccessibilityNode(
          id: 5,
          role: 'textbox',
          label: 'Name',
          value: '',
          textSelection: const BrowserAccessibilityTextSelection(
            baseOffset: 0,
            extentOffset: 0,
          ),
          textInputType: BrowserTextInputType.email,
          textInputAction: BrowserTextInputAction.send,
          bounds: const BrowserAccessibilityRect(
            x: 0,
            y: 0,
            width: 320,
            height: 40,
          ),
          focused: true,
          disabled: false,
          selected: false,
          hidden: false,
          focusable: true,
          actions: const ['focus', 'set_value'],
        ),
      ],
      truncated: false,
    );
    await tester.pumpWidget(
      MaterialApp(
        home: SizedBox(
          width: 320,
          height: 200,
          child: BrowserContentSurface(
            contextState: context,
            frame: sizedTestFrame(frameId: 9, width: 320, height: 200),
            accessibility: snapshot,
            textureController: _TestTextureController(),
            onTextInput: states.add,
            onKeyEvent: (type, event) => keyEvents.add((type, event)),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('content-surface')));
    await tester.pump();
    expect(tester.testTextInput.isVisible, isTrue);
    expect(
      tester.testTextInput.setClientArgs?['inputAction'],
      'TextInputAction.send',
    );
    expect(
      (tester.testTextInput.setClientArgs?['inputType'] as Map)['name'],
      'TextInputType.emailAddress',
    );

    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'に',
        selection: TextSelection.collapsed(offset: 1),
        composing: TextRange(start: 0, end: 1),
      ),
    );
    await tester.pump();

    expect(states, hasLength(1));
    expect(states.single.text, 'に');
    expect(states.single.selection.baseOffset, 1);
    expect(states.single.composing?.baseOffset, 0);
    expect(states.single.composing?.extentOffset, 1);

    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'に',
        selection: TextSelection.collapsed(offset: 1),
      ),
    );
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'に🦊',
        selection: TextSelection.collapsed(offset: 3),
        composing: TextRange(start: 1, end: 3),
      ),
    );
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'に🦊',
        selection: TextSelection.collapsed(offset: 3),
      ),
    );
    await tester.pump();
    expect(states, hasLength(4));
    expect(states[1].composing, isNull);
    expect(states[2].text, 'に🦊');
    expect(states[2].selection.baseOffset, 3);
    expect(states[2].composing?.baseOffset, 1);
    expect(states[2].composing?.extentOffset, 3);
    expect(states[3].composing, isNull);

    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'に🦊',
        selection: TextSelection.collapsed(offset: 3),
        composing: TextRange(start: 3, end: 1),
      ),
    );
    await tester.pump();
    expect(states, hasLength(4));

    await tester.sendKeyDownEvent(LogicalKeyboardKey.keyA);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.keyA);
    expect(keyEvents, hasLength(2));
    expect(keyEvents.first.$2.applyText, isFalse);
    expect(keyEvents.first.$2.text, isEmpty);
    keyEvents.clear();

    await tester.testTextInput.receiveAction(TextInputAction.send);
    await tester.pump();
    expect(keyEvents.map((entry) => entry.$1), ['keydown', 'keyup']);
    expect(keyEvents.map((entry) => entry.$2.key), everyElement('Enter'));
    await tester.testTextInput.receiveAction(TextInputAction.newline);
    await tester.pump();
    expect(keyEvents, hasLength(2));
  });

  testWidgets('BrowserCore nodes project into actionable Flutter Semantics', (
    tester,
  ) async {
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetDevicePixelRatio);
    BrowserAccessibilityNode? tapped;
    BrowserAccessibilityNode? focused;
    final semanticKeyEvents = <(String, BrowserKeyEvent)>[];
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
    final heading = BrowserAccessibilityNode(
      id: 45,
      parentId: 7,
      role: 'heading',
      label: 'Preferences',
      headingLevel: 2,
      bounds: const BrowserAccessibilityRect(
        x: 150,
        y: 20,
        width: 120,
        height: 30,
      ),
      focused: false,
      disabled: false,
      selected: false,
      hidden: false,
      focusable: false,
      actions: const [],
    );
    final mixed = BrowserAccessibilityNode(
      id: 46,
      parentId: 7,
      role: 'checkbox',
      label: 'Some selected',
      checked: false,
      mixed: true,
      bounds: const BrowserAccessibilityRect(
        x: 150,
        y: 60,
        width: 120,
        height: 30,
      ),
      focused: false,
      disabled: false,
      selected: false,
      hidden: false,
      focusable: true,
      actions: const ['tap', 'focus'],
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
                nodes: [parent, node, textbox, slider, heading, mixed],
                truncated: false,
              ),
              onSemanticTap: (_, value) => tapped = value,
              onSemanticFocus: (_, value) => focused = value,
              onKeyEvent: (type, event) => semanticKeyEvents.add((type, event)),
              onSemanticSetValue: (_, node, value) => setValue = (node, value),
              onSemanticAdjustment: (_, node, increase) =>
                  adjusted = (node, increase),
            ),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    final finder = find.byWidgetPredicate(
      (widget) =>
          widget is Semantics && widget.properties.label == 'Open settings',
    );
    final parentFinder = find.byWidgetPredicate(
      (widget) =>
          widget is Semantics && widget.properties.label == 'Page content',
    );
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
    await tester.pump();
    await tester.sendKeyDownEvent(LogicalKeyboardKey.keyA);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.keyA);
    expect(tapped?.id, 42);
    expect(focused?.id, 42);
    expect(semanticKeyEvents.map((event) => event.$1), ['keydown', 'keyup']);
    final textboxFinder = find.byWidgetPredicate(
      (widget) => widget is Semantics && widget.properties.label == 'Name',
    );
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
    final sliderFinder = find.byWidgetPredicate(
      (widget) => widget is Semantics && widget.properties.label == 'Volume',
    );
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
    expect(
      tester
          .widget<Semantics>(
            find.byWidgetPredicate(
              (widget) =>
                  widget is Semantics &&
                  widget.properties.label == 'Preferences',
            ),
          )
          .properties
          .headingLevel,
      2,
    );
    expect(
      tester
          .widget<Semantics>(
            find.byWidgetPredicate(
              (widget) =>
                  widget is Semantics &&
                  widget.properties.label == 'Some selected',
            ),
          )
          .properties
          .mixed,
      isTrue,
    );
    semantics.dispose();
  });

  testWidgets('semantic reconciliation keys only changed nodes', (
    tester,
  ) async {
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetDevicePixelRatio);
    final controller = _TestTextureController();

    Future<Key?> pumpNode(int generation, String label) async {
      final node = BrowserAccessibilityNode(
        id: 1,
        role: 'button',
        label: label,
        bounds: const BrowserAccessibilityRect(
          x: 10,
          y: 10,
          width: 100,
          height: 40,
        ),
        focused: false,
        disabled: false,
        selected: false,
        hidden: false,
        focusable: true,
        actions: const ['tap', 'focus'],
      );
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
                frame: sizedTestFrame(
                  frameId: generation,
                  width: 400,
                  height: 300,
                ),
                textureController: controller,
                accessibility: BrowserAccessibilitySnapshot(
                  sourceGeneration: generation,
                  generation: generation,
                  contextId: 10,
                  documentId: 20,
                  viewportWidth: 400,
                  viewportHeight: 300,
                  nodes: [node],
                  truncated: false,
                ),
              ),
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();
      return tester
          .widget<Semantics>(
            find.byWidgetPredicate(
              (widget) =>
                  widget is Semantics && widget.properties.label == label,
            ),
          )
          .key;
    }

    final first = await pumpNode(1, 'Stable');
    final unchanged = await pumpNode(2, 'Stable');
    final changed = await pumpNode(3, 'Changed');
    expect(unchanged, first);
    expect(changed, isNot(unchanged));
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

final class _FlakyTextureController implements BrowserTextureController {
  _FlakyTextureController({required this.failuresRemaining});

  int failuresRemaining;
  int createCount = 0;
  int publishCount = 0;
  int disposeCount = 0;

  @override
  Future<int> create() async => ++createCount;

  @override
  Future<void> publish(BrowserFrame frame) async {
    publishCount++;
    if (failuresRemaining > 0) {
      failuresRemaining--;
      throw StateError('surface lost');
    }
  }

  @override
  Future<void> dispose() async {
    disposeCount++;
  }
}

final class _LifecycleFaultTextureController
    implements BrowserTextureController {
  _LifecycleFaultTextureController({
    required this.blockedFrameId,
    required this.failingFrameId,
  });

  final int blockedFrameId;
  final int failingFrameId;
  final Completer<void> _blockedPublish = Completer<void>();
  final List<int> publishedFrameIds = [];
  int createCount = 0;
  int disposeCount = 0;
  bool _failedNewerFrame = false;

  @override
  Future<int> create() async => ++createCount;

  @override
  Future<void> publish(BrowserFrame frame) async {
    publishedFrameIds.add(frame.frameId);
    if (frame.frameId == blockedFrameId && !_blockedPublish.isCompleted) {
      await _blockedPublish.future;
    }
    if (frame.frameId == failingFrameId && !_failedNewerFrame) {
      _failedNewerFrame = true;
      throw StateError('texture lost after resume');
    }
  }

  void releaseBlockedPublish() => _blockedPublish.complete();

  @override
  Future<void> dispose() async {
    disposeCount++;
  }
}
