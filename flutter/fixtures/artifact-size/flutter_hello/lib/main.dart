import 'package:flutter/material.dart';

void main() => runApp(const HelloFlutterApp());

final class HelloFlutterApp extends StatelessWidget {
  const HelloFlutterApp({super.key});

  @override
  Widget build(BuildContext context) {
    return const MaterialApp(
      home: Scaffold(body: Center(child: Text('Hello, Flutter'))),
    );
  }
}
