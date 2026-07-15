import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/browser_models.dart';
import 'package:vixen_shell/src/shell/content_surface.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('physical viewport preserves bounds and byte cap', () {
    final viewport = physicalRendererViewport(const Size(10000, 9000), 3);
    expect(viewport.width, lessThanOrEqualTo(browserMaxViewportDimension));
    expect(viewport.height, lessThanOrEqualTo(browserMaxViewportDimension));
    expect(
      viewport.width * viewport.height * 4,
      lessThanOrEqualTo(browserMaxViewportBytes),
    );
  });

  test('viewport transform shares DPR across renderer input and geometry', () {
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
    final context = BrowsingContextState.initial(10)
        .copyWith(documentId: 20, runtimeContextId: 30);
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
            accessibility: snapshot,
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
}
