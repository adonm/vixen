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
