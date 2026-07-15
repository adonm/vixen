import 'dart:io';

import 'package:flutter/material.dart';
import 'package:yaru/yaru.dart';

import 'src/app/vixen_app.dart';
import 'src/automation/automation_app.dart';
import 'src/automation/automation_config.dart';
import 'src/automation/cdp_automation_app.dart';
import 'src/bridge/native/native_browser_controller.dart';
import 'src/bridge/browser_models.dart';
import 'src/shell/shell_coordinator.dart';

Future<void> main(List<String> arguments) async {
  if (isCdpAutomationInvocation(arguments)) {
    WidgetsFlutterBinding.ensureInitialized();
    late final CdpAutomationConfig config;
    try {
      config = CdpAutomationConfig.parse(arguments);
    } on FormatException catch (error) {
      stderr.writeln(error.message);
      exit(64);
    }
    runApp(
      VixenCdpAutomationApp(
        config: config,
        coordinator: ShellCoordinator(
          NativeBrowserController(),
          initialUrl: config.url,
          useProfileSession: false,
          externalRendererUpdates: true,
        ),
        onFinished: exit,
      ),
    );
    return;
  }
  if (isAutomationInvocation(arguments)) {
    WidgetsFlutterBinding.ensureInitialized();
    late final AutomationConfig config;
    try {
      config = AutomationConfig.parse(arguments);
    } on FormatException catch (error) {
      stderr.writeln(error.message);
      exit(64);
    }
    runApp(
      VixenAutomationApp(
        config: config,
        coordinator: ShellCoordinator(
          NativeBrowserController(),
          initialUrl: config.url,
          useProfileSession: false,
        ),
        onFinished: exit,
      ),
    );
    return;
  }
  await YaruWindowTitleBar.ensureInitialized();
  runApp(
    VixenApp(
      coordinator: ShellCoordinator(
        NativeBrowserController(),
        initialUrl: Platform.environment['VIXEN_START_URL'] ?? vixenStartUrl,
      ),
    ),
  );
}
