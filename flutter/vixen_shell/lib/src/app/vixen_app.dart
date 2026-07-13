import 'package:flutter/material.dart';
import 'package:yaru/yaru.dart';

import '../shell/browser_shell.dart';
import '../shell/shell_coordinator.dart';

final class VixenApp extends StatefulWidget {
  const VixenApp({required this.coordinator, super.key});

  final ShellCoordinator coordinator;

  @override
  State<VixenApp> createState() => _VixenAppState();
}

final class _VixenAppState extends State<VixenApp> {
  @override
  void dispose() {
    widget.coordinator.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    const theme = YaruThemeData(
      variant: YaruVariant.adwaitaBlue,
      useMaterial3: true,
      visualDensity: VisualDensity.standard,
    );
    const highContrastTheme = YaruThemeData(
      variant: YaruVariant.adwaitaBlue,
      highContrast: true,
      useMaterial3: true,
      visualDensity: VisualDensity.standard,
    );
    return MaterialApp(
      title: 'Vixen',
      debugShowCheckedModeBanner: false,
      theme: theme.theme,
      darkTheme: theme.darkTheme,
      highContrastTheme: highContrastTheme.theme,
      highContrastDarkTheme: highContrastTheme.darkTheme,
      themeMode: ThemeMode.system,
      home: BrowserShell(coordinator: widget.coordinator),
    );
  }
}
