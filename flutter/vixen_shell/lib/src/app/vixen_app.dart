import 'package:flutter/material.dart';

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
    return MaterialApp(
      title: 'Vixen',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: const Color(0xff6957c2),
          brightness: Brightness.light,
        ),
        useMaterial3: true,
      ),
      darkTheme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: const Color(0xff9e8cff),
          brightness: Brightness.dark,
        ),
        useMaterial3: true,
      ),
      home: BrowserShell(coordinator: widget.coordinator),
    );
  }
}
