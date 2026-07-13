import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/app/vixen_app.dart';
import 'package:vixen_shell/src/bridge/browser_models.dart';
import 'package:vixen_shell/src/bridge/fake/scripted_browser_controller.dart';
import 'package:vixen_shell/src/shell/shell_coordinator.dart';

import 'browser_models_test.dart' show contextState;

void main() {
  testWidgets('renders Material browser chrome and honest empty frame seam', (
    tester,
  ) async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 1,
        contexts: [contextState(id: 1, url: 'https://example.test')],
      ),
    );
    final coordinator = ShellCoordinator(controller);

    await tester.pumpWidget(VixenApp(coordinator: coordinator));
    await pumpStartup(tester);

    expect(find.byKey(const Key('back')), findsOneWidget);
    expect(find.byKey(const Key('forward')), findsOneWidget);
    expect(find.byKey(const Key('reload-stop')), findsOneWidget);
    expect(find.byKey(const Key('new-tab')), findsOneWidget);
    expect(find.byKey(const Key('address-field')), findsOneWidget);
    expect(find.byKey(const Key('content-surface')), findsOneWidget);
    expect(find.text('Renderer frame unavailable'), findsOneWidget);
    expect(find.byKey(const Key('status-bar')), findsOneWidget);

    await tester.pumpWidget(const SizedBox.shrink());
    coordinator.dispose();
    await tester.pump();
  });

  testWidgets('keyboard shortcuts focus address and route tab commands', (
    tester,
  ) async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 6,
        contexts: [contextState(id: 6, url: 'https://example.test')],
      ),
    );
    final coordinator = ShellCoordinator(controller);
    await tester.pumpWidget(VixenApp(coordinator: coordinator));
    await pumpStartup(tester);

    await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.keyL);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
    await tester.pump();
    final field = tester.widget<EditableText>(find.byType(EditableText));
    expect(field.focusNode.hasFocus, isTrue);
    expect(
      field.controller.selection,
      const TextSelection(baseOffset: 0, extentOffset: 20),
    );

    await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.keyT);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
    await tester.pump();
    await tester.pump();
    expect(
      controller.commands.where((command) => command.type == 'create_context'),
      hasLength(1),
    );

    await tester.pumpWidget(const SizedBox.shrink());
    coordinator.dispose();
    await tester.pump();
  });

  testWidgets('menu opens shortcuts and about dialogs', (tester) async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 1,
        contexts: [contextState(id: 1, url: vixenStartUrl)],
      ),
    );
    final coordinator = ShellCoordinator(controller);
    await tester.pumpWidget(VixenApp(coordinator: coordinator));
    await pumpStartup(tester);

    await tester.tap(find.byKey(const Key('main-menu')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Keyboard shortcuts'));
    await tester.pumpAndSettle();
    expect(
      find.widgetWithText(AlertDialog, 'Keyboard shortcuts'),
      findsOneWidget,
    );
    await tester.tap(find.text('Close'));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('main-menu')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('About Vixen'));
    await tester.pumpAndSettle();
    expect(find.text('Vixen'), findsWidgets);

    await tester.pumpWidget(const SizedBox.shrink());
    coordinator.dispose();
    await tester.pump();
  });

  testWidgets('Ctrl+F opens BrowserCore-backed find bar', (tester) async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 1,
        contexts: [contextState(id: 1, url: 'https://example.test')],
      ),
    );
    final coordinator = ShellCoordinator(controller);
    await tester.pumpWidget(VixenApp(coordinator: coordinator));
    await pumpStartup(tester);

    await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.keyF);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
    await tester.pump();
    expect(find.byKey(const Key('find-bar')), findsOneWidget);

    await tester.enterText(find.byKey(const Key('find-field')), 'Vixen');
    await tester.pump();
    await tester.pump();
    expect(find.text('0 matches'), findsOneWidget);
    final command = controller.commands.lastWhere(
      (command) => command.type == 'find_text',
    );
    expect(command.toWire(), {
      'v': 1,
      'type': 'find_text',
      'context_id': 1,
      'document_id': 100,
      'query': 'Vixen',
      'case_sensitive': false,
    });

    await tester.tap(find.byTooltip('Close find'));
    await tester.pump();
    expect(find.byKey(const Key('find-bar')), findsNothing);

    await tester.pumpWidget(const SizedBox.shrink());
    coordinator.dispose();
    await tester.pump();
  });

  testWidgets('zoom shortcuts update BrowserCore-owned tab zoom', (
    tester,
  ) async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 1,
        contexts: [contextState(id: 1, url: 'https://example.test')],
      ),
    );
    final coordinator = ShellCoordinator(controller);
    await tester.pumpWidget(VixenApp(coordinator: coordinator));
    await pumpStartup(tester);

    await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.equal);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
    await tester.pump();
    await tester.pump();
    expect(coordinator.selectedContext?.pageZoom, 1.1);
    expect(
      controller.commands
          .lastWhere((command) => command.type == 'set_page_zoom')
          .toWire(),
      {'v': 1, 'type': 'set_page_zoom', 'context_id': 1, 'zoom': 1.1},
    );

    await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.digit0);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
    await tester.pump();
    await tester.pump();
    expect(coordinator.selectedContext?.pageZoom, 1);

    await tester.pumpWidget(const SizedBox.shrink());
    coordinator.dispose();
    await tester.pump();
  });

  testWidgets('navigation failure displays dismissible error banner', (
    tester,
  ) async {
    final controller = ScriptedBrowserController(
      snapshot: BrowserSnapshot(
        activeContextId: 1,
        contexts: [contextState(id: 1, url: vixenStartUrl)],
      ),
    );
    final coordinator = ShellCoordinator(controller);
    await tester.pumpWidget(VixenApp(coordinator: coordinator));
    await pumpStartup(tester);

    await coordinator.navigate('https://broken.test');
    controller.failNavigation(1);
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('error-banner')), findsOneWidget);
    await tester.tap(find.byTooltip('Dismiss error'));
    await tester.pump();
    expect(find.byKey(const Key('error-banner')), findsNothing);

    await tester.pumpWidget(const SizedBox.shrink());
    coordinator.dispose();
    await tester.pump();
  });
}

Future<void> pumpStartup(WidgetTester tester) async {
  await tester.pump();
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 50));
}
