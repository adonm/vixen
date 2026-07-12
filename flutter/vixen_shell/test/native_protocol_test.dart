import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/bridge/browser_models.dart';
import 'package:vixen_shell/src/bridge/native/native_bindings.dart';
import 'package:vixen_shell/src/bridge/native/native_paths.dart';
import 'package:vixen_shell/src/bridge/native/native_protocol.dart';

void main() {
  test('VixenBuffer matches the 64-bit Linux C layout', () {
    expect(sizeOf<Uint64>(), 8);
    expect(sizeOf<Size>(), 8);
    expect(sizeOf<Pointer<Uint8>>(), 8);
    expect(sizeOf<VixenBuffer>(), 24);
  });

  test('VixenFrame matches the exact 64-bit Linux C layout', () {
    expect(sizeOf<VixenFrame>(), 64);
  });

  test('validates exact packed native frame descriptors', () {
    expect(
      () => validateNativeFrameDescriptor(
        token: 1,
        pointerAddress: 1,
        length: 8 * 6 * 4,
        width: 8,
        height: 6,
        rowStride: 32,
        frameId: 2,
        contextId: 3,
        documentId: 4,
        expectedWidth: 8,
        expectedHeight: 6,
        expectedContextId: 3,
        expectedDocumentId: 4,
      ),
      returnsNormally,
    );
    expect(
      () => validateNativeFrameDescriptor(
        token: 1,
        pointerAddress: 1,
        length: 8 * 6 * 4,
        width: 8,
        height: 6,
        rowStride: 31,
        frameId: 2,
        contextId: 3,
        documentId: 4,
        expectedWidth: 8,
        expectedHeight: 6,
        expectedContextId: 3,
        expectedDocumentId: 4,
      ),
      throwsA(isA<NativeProtocolException>()),
    );
  });

  test('profile path follows XDG and HOME precedence', () {
    expect(
      resolveProfilePath(
        environment: const <String, String>{
          'XDG_DATA_HOME': '/xdg',
          'HOME': '/home/tester',
        },
      ),
      '/xdg/dev.adonm.vixen/profile.redb',
    );
    expect(
      resolveProfilePath(
        environment: const <String, String>{'HOME': '/home/tester'},
      ),
      '/home/tester/.local/share/dev.adonm.vixen/profile.redb',
    );
    expect(
      resolveProfilePath(
        environment: const <String, String>{
          'VIXEN_PROFILE_PATH': '/profiles/custom.redb',
        },
      ),
      '/profiles/custom.redb',
    );
  });

  test('library override fails closed for relative and missing paths', () {
    expect(
      () => resolveNativeLibraryPath(
        environment: const <String, String>{
          'VIXEN_FFI_LIBRARY': 'libvixen_ffi.so',
        },
      ),
      throwsA(isA<NativeBridgeException>()),
    );
    expect(
      () => resolveNativeLibraryPath(
        environment: const <String, String>{
          'VIXEN_FFI_LIBRARY': '/definitely/missing/libvixen_ffi.so',
        },
      ),
      throwsA(isA<NativeBridgeException>()),
    );
  });

  group('native JSON parser', () {
    test('accepts a copied v1 event envelope', () {
      final bytes = Uint8List.fromList(
        utf8.encode(
          '{"v":1,"type":"event","sequence":7,'
          '"event":{"type":"diagnostic"}}',
        ),
      );

      final parsed = decodeNativeJson(bytes);

      expect(parsed['sequence'], 7);
      expect((parsed['event']! as Map<String, Object?>)['type'], 'diagnostic');
    });

    test('rejects malformed UTF-8', () {
      expect(
        () => decodeNativeJson(Uint8List.fromList(<int>[0xc3, 0x28])),
        throwsA(isA<NativeProtocolException>()),
      );
    });

    test('rejects an unknown envelope version', () {
      expect(
        () => decodeNativeJson(
          Uint8List.fromList(utf8.encode('{"v":2,"type":"opened"}')),
        ),
        throwsA(isA<NativeProtocolException>()),
      );
    });

    test('rejects output above the ABI bound before parsing', () {
      expect(
        () => decodeNativeJson(Uint8List(vixenMaxOutputBytes + 1)),
        throwsA(isA<NativeProtocolException>()),
      );
    });
  });

  group('native command parser', () {
    test('encodes an exact navigate command', () {
      final encoded = encodeNativeCommand(
        nativeCommand('navigate', <String, Object?>{
          'context_id': 4,
          'url': 'https://example.test/',
        }),
      );

      expect(jsonDecode(utf8.decode(encoded)), <String, Object?>{
        'v': 1,
        'type': 'navigate',
        'context_id': 4,
        'url': 'https://example.test/',
      });
    });

    test('rejects unknown fields and zero context ids', () {
      expect(
        () => normalizeNativeCommand(<String, Object?>{
          ...nativeCommand('reload', <String, Object?>{'context_id': 1}),
          'extra': true,
        }),
        throwsA(isA<NativeBridgeException>()),
      );
      expect(
        () => normalizeNativeCommand(
          nativeCommand('reload', <String, Object?>{'context_id': 0}),
        ),
        throwsA(isA<NativeBridgeException>()),
      );
    });

    test('accepts the production accessibility and input commands', () {
      final commands = <BrowserCommand>[
        BrowserCommand.updateHostViewState(
          contextId: 1,
          generation: 1,
          viewportWidth: 320,
          viewportHeight: 200,
          scaleFactor: 2,
          focused: true,
          visible: true,
          lifecycle: BrowserHostLifecycle.resumed,
        ),
        BrowserCommand.accessibilitySnapshot(
          contextId: 1,
          documentId: 2,
          viewportWidth: 320,
          viewportHeight: 200,
        ),
        BrowserCommand.dispatchAccessibilityFocus(
          contextId: 1,
          documentId: 2,
          runtimeContextId: 3,
          viewportWidth: 320,
          viewportHeight: 200,
          sourceGeneration: 4,
          generation: 5,
          nodeId: 6,
        ),
        BrowserCommand.dispatchAccessibilitySetValue(
          contextId: 1,
          documentId: 2,
          runtimeContextId: 3,
          viewportWidth: 320,
          viewportHeight: 200,
          sourceGeneration: 4,
          generation: 5,
          nodeId: 6,
          value: 'Ada',
        ),
        BrowserCommand.dispatchMouseEvent(
          contextId: 1,
          documentId: 2,
          runtimeContextId: 3,
          viewportWidth: 320,
          viewportHeight: 200,
          eventType: 'mousedown',
          event: const BrowserMouseEvent(x: 12.5, y: 9, button: 0, buttons: 1),
        ),
        BrowserCommand.dispatchMouseEvent(
          contextId: 1,
          documentId: 2,
          runtimeContextId: 3,
          viewportWidth: 320,
          viewportHeight: 200,
          eventType: 'cancel',
          event: const BrowserMouseEvent(x: 12.5, y: 9, button: 0, buttons: 0),
        ),
        BrowserCommand.dispatchKeyEvent(
          contextId: 1,
          documentId: 2,
          runtimeContextId: 3,
          viewportWidth: 320,
          viewportHeight: 200,
          eventType: 'keydown',
          event: const BrowserKeyEvent(
            key: 'a',
            code: 'KeyA',
            text: 'a',
            applyText: true,
          ),
        ),
      ];

      for (final command in commands) {
        expect(normalizeNativeCommand(command.toWire()), command.toWire());
      }
    });

    test('strictly rejects malformed production input commands', () {
      final mouse = BrowserCommand.dispatchMouseEvent(
        contextId: 1,
        documentId: 2,
        runtimeContextId: 3,
        viewportWidth: 320,
        viewportHeight: 200,
        eventType: 'mousedown',
        event: const BrowserMouseEvent(x: 12, y: 9, button: 0, buttons: 1),
      ).toWire();
      expect(
        () => normalizeNativeCommand(<String, Object?>{
          ...mouse,
          'event_type': 'pointerdown',
        }),
        throwsA(isA<NativeBridgeException>()),
      );
      expect(
        () => normalizeNativeCommand(<String, Object?>{
          ...mouse,
          'viewport': <String, Object?>{'width': 4096, 'height': 4097},
        }),
        throwsA(isA<NativeBridgeException>()),
      );
      expect(
        () => normalizeNativeCommand(<String, Object?>{
          ...mouse,
          'event': <String, Object?>{
            ...(mouse['event']! as Map<String, Object?>),
            'x': double.nan,
          },
        }),
        throwsA(isA<NativeBridgeException>()),
      );
      final focus = BrowserCommand.dispatchAccessibilityFocus(
        contextId: 1,
        documentId: 2,
        runtimeContextId: 3,
        viewportWidth: 320,
        viewportHeight: 200,
        sourceGeneration: 4,
        generation: 5,
        nodeId: 6,
      ).toWire();
      expect(
        () => normalizeNativeCommand(<String, Object?>{
          ...focus,
          'generation': 0,
        }),
        throwsA(isA<NativeBridgeException>()),
      );
      expect(
        () => normalizeNativeCommand(<String, Object?>{
          ...focus,
          'action': 'set_value',
        }),
        throwsA(isA<NativeBridgeException>()),
      );
      final setValue = BrowserCommand.dispatchAccessibilitySetValue(
        contextId: 1,
        documentId: 2,
        runtimeContextId: 3,
        viewportWidth: 320,
        viewportHeight: 200,
        sourceGeneration: 4,
        generation: 5,
        nodeId: 6,
        value: 'x' * (vixenMaxAccessibilityValueBytes + 1),
      ).toWire();
      expect(
        () => normalizeNativeCommand(setValue),
        throwsA(isA<NativeBridgeException>()),
      );
    });

    test('maps every stable status value', () {
      for (var value = 0; value <= 13; value++) {
        expect(NativeStatus.fromValue(value).value, value);
      }
      expect(
        () => NativeStatus.fromValue(14),
        throwsA(isA<NativeProtocolException>()),
      );
    });
  });
}
