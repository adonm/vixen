import 'dart:io';

import 'package:flutter/material.dart';
import 'package:yaru/yaru.dart';

import 'src/app/vixen_app.dart';
import 'src/bridge/native/native_browser_controller.dart';
import 'src/bridge/browser_models.dart';
import 'src/shell/shell_coordinator.dart';

Future<void> main() async {
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
