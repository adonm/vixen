import 'dart:convert';
import 'dart:io';

import 'native_protocol.dart';

const String vixenApplicationId = 'dev.adonm.vixen';
const String vixenLibraryFileName = 'libvixen_ffi.so';

String resolveNativeLibraryPath({
  Map<String, String>? environment,
  String? resolvedExecutable,
}) {
  final env = environment ?? Platform.environment;
  final override = env['VIXEN_FFI_LIBRARY'];
  late final String path;
  if (override != null && override.isNotEmpty) {
    path = override;
  } else {
    final executable = File(resolvedExecutable ?? Platform.resolvedExecutable);
    path = executable.parent.uri
        .resolve('lib/$vixenLibraryFileName')
        .toFilePath();
  }
  return validateNativeLibraryPath(path);
}

String validateNativeLibraryPath(String path) {
  if (!Platform.isLinux) {
    throw const NativeBridgeException(
      'the Vixen native bridge is only available on Linux',
      code: 'ffi.unsupported-platform',
    );
  }
  if (!path.startsWith('/')) {
    throw const NativeBridgeException(
      'VIXEN_FFI_LIBRARY must be an absolute path',
      code: 'ffi.invalid-library-path',
    );
  }
  if (!File(path).existsSync()) {
    throw NativeBridgeException(
      'native library does not exist at $path',
      code: 'ffi.library-not-found',
    );
  }
  return path;
}

String resolveProfilePath({Map<String, String>? environment}) {
  final env = environment ?? Platform.environment;
  final override = env['VIXEN_PROFILE_PATH'];
  if (override != null && override.isNotEmpty) {
    return override;
  }

  final xdgDataHome = env['XDG_DATA_HOME'];
  final String dataHome;
  if (xdgDataHome != null && xdgDataHome.isNotEmpty) {
    dataHome = xdgDataHome;
  } else {
    final home = env['HOME'];
    if (home == null || home.isEmpty) {
      throw const NativeBridgeException(
        'HOME or XDG_DATA_HOME is required to locate the Vixen profile',
        code: 'ffi.profile-path-unavailable',
      );
    }
    dataHome = '$home/.local/share';
  }
  return '$dataHome/$vixenApplicationId/profile.redb';
}

void ensureProfileParentExists(String profilePath) {
  final encodedLength = utf8.encode(profilePath).length;
  if (profilePath.isEmpty || encodedLength > vixenMaxProfilePathBytes) {
    throw NativeBridgeException(
      'profile path must contain 1 to $vixenMaxProfilePathBytes UTF-8 bytes',
      code: NativeStatus.invalidArgument.defaultCode,
      status: NativeStatus.invalidArgument,
    );
  }
  try {
    File(profilePath).parent.createSync(recursive: true);
  } on FileSystemException catch (error) {
    throw NativeBridgeException(
      'could not create the profile directory: ${error.message}',
      code: 'ffi.profile-directory',
    );
  }
}
