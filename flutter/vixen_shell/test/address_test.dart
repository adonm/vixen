import 'package:flutter_test/flutter_test.dart';
import 'package:vixen_shell/src/shell/address.dart';

void main() {
  test('normalizes addresses and searches without browser ownership', () {
    expect(
      normalizeAddress('  https://example.com/a  '),
      'https://example.com/a',
    );
    expect(normalizeAddress('about:vixen'), 'about:vixen');
    expect(normalizeAddress('file:///tmp/page.html'), 'file:///tmp/page.html');
    expect(normalizeAddress('example.com/docs'), 'https://example.com/docs');
    expect(normalizeAddress('localhost:8080'), 'https://localhost:8080');
    expect(normalizeAddress('127.0.0.1:9000'), 'https://127.0.0.1:9000');
    expect(
      normalizeAddress('rust browser'),
      'https://duckduckgo.com/?q=rust+browser',
    );
    expect(normalizeAddress('vixen'), 'https://duckduckgo.com/?q=vixen');
    expect(normalizeAddress('   '), 'about:blank');
  });
}
