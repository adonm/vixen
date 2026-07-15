import 'dart:convert';

import '../bridge/browser_models.dart';

const String vixenAutomationFlag = '--vixen-automation';
const int vixenAutomationMaxUrlBytes = 16 * 1024;
const int vixenAutomationMaxOutputPathBytes = 4096;

final class AutomationConfig {
  const AutomationConfig({
    required this.url,
    required this.width,
    required this.height,
    required this.outputPath,
  });

  final String url;
  final int width;
  final int height;
  final String outputPath;

  static AutomationConfig parse(List<String> arguments) {
    String? url;
    String? viewport;
    String? outputPath;
    var automationFlags = 0;
    for (final argument in arguments) {
      if (argument == vixenAutomationFlag) {
        automationFlags++;
      } else if (argument.startsWith('--vixen-url=')) {
        if (url != null) _invalid('duplicate --vixen-url');
        url = argument.substring('--vixen-url='.length);
      } else if (argument.startsWith('--vixen-viewport=')) {
        if (viewport != null) _invalid('duplicate --vixen-viewport');
        viewport = argument.substring('--vixen-viewport='.length);
      } else if (argument.startsWith('--vixen-output=')) {
        if (outputPath != null) _invalid('duplicate --vixen-output');
        outputPath = argument.substring('--vixen-output='.length);
      } else {
        _invalid('unknown argument $argument');
      }
    }
    if (automationFlags != 1) {
      _invalid('exactly one $vixenAutomationFlag is required');
    }
    final parsedUrl = _parseUrl(url);
    final dimensions = _parseViewport(viewport);
    final parsedOutputPath = _parseOutputPath(outputPath);
    return AutomationConfig(
      url: parsedUrl,
      width: dimensions.width,
      height: dimensions.height,
      outputPath: parsedOutputPath,
    );
  }

  static String _parseUrl(String? value) {
    if (value == null ||
        value.isEmpty ||
        value.contains('\u0000') ||
        utf8.encode(value).length > vixenAutomationMaxUrlBytes) {
      _invalid(
        '--vixen-url must contain 1 to $vixenAutomationMaxUrlBytes UTF-8 bytes',
      );
    }
    final uri = Uri.tryParse(value);
    final validPort = uri == null || !uri.hasPort || uri.port <= 65535;
    final validHttp =
        uri != null &&
        const {'http', 'https'}.contains(uri.scheme) &&
        uri.host.isNotEmpty &&
        validPort;
    final validFile =
        uri != null &&
        uri.scheme == 'file' &&
        uri.host.isEmpty &&
        value.startsWith('file:/') &&
        uri.path.startsWith('/');
    if (!validHttp && !validFile) {
      _invalid('--vixen-url must be an absolute file, http, or https URL');
    }
    return value;
  }

  static ({int width, int height}) _parseViewport(String? value) {
    final match = value == null
        ? null
        : RegExp(r'^([1-9][0-9]*)x([1-9][0-9]*)$').firstMatch(value);
    final width = match == null ? null : int.tryParse(match.group(1)!);
    final height = match == null ? null : int.tryParse(match.group(2)!);
    if (width == null ||
        height == null ||
        width > browserMaxFrameDimension ||
        height > browserMaxFrameDimension ||
        width * height * 4 > browserMaxFrameBytes) {
      _invalid(
        '--vixen-viewport must be WIDTHxHEIGHT within the renderer bounds',
      );
    }
    return (width: width, height: height);
  }

  static String _parseOutputPath(String? value) {
    if (value == null ||
        value.isEmpty ||
        value.contains('\u0000') ||
        !value.startsWith('/') ||
        !value.toLowerCase().endsWith('.png') ||
        utf8.encode(value).length > vixenAutomationMaxOutputPathBytes) {
      _invalid(
        '--vixen-output must be an absolute .png path no longer than '
        '$vixenAutomationMaxOutputPathBytes UTF-8 bytes',
      );
    }
    return value;
  }

  static Never _invalid(String message) =>
      throw FormatException('invalid Vixen automation configuration: $message');
}

bool isAutomationInvocation(List<String> arguments) =>
    arguments.contains(vixenAutomationFlag);
