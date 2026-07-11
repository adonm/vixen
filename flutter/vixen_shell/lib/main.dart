import 'package:flutter/material.dart';

import 'src/app/vixen_app.dart';
import 'src/bridge/native/native_browser_controller.dart';
import 'src/shell/shell_coordinator.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(VixenApp(coordinator: ShellCoordinator(NativeBrowserController())));
}
