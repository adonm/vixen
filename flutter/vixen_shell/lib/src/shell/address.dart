const String _searchUrl = 'https://duckduckgo.com/';

String normalizeAddress(String input) {
  final value = input.trim();
  if (value.isEmpty) return 'about:blank';
  if (_isProbableWebAddress(value)) return 'https://$value';
  if (_hasScheme(value)) return value;
  final query = Uri.encodeQueryComponent(value).replaceAll('%20', '+');
  return '$_searchUrl?q=$query';
}

bool _hasScheme(String value) {
  final separator = value.indexOf(':');
  if (separator <= 0) return false;
  final scheme = value.substring(0, separator);
  if (!_isAsciiLetter(scheme.codeUnitAt(0))) return false;
  return scheme.codeUnits
      .skip(1)
      .every(
        (code) =>
            _isAsciiLetter(code) ||
            _isAsciiDigit(code) ||
            code == 0x2b ||
            code == 0x2d ||
            code == 0x2e,
      );
}

bool _isProbableWebAddress(String value) {
  if (value.codeUnits.any((code) => code <= 0x20)) return false;
  final authority = value.split(RegExp(r'[/\?#]')).first;
  if (authority.isEmpty ||
      authority.startsWith('.') ||
      authority.endsWith('.')) {
    return false;
  }
  final hostPort = authority.split('@').last;
  if (hostPort.startsWith('[')) {
    final close = hostPort.indexOf(']');
    if (close < 1) return false;
    final suffix = hostPort.substring(close + 1);
    return suffix.isEmpty ||
        (suffix.startsWith(':') && _isPort(suffix.substring(1)));
  }
  final separator = hostPort.lastIndexOf(':');
  final hasPort = separator >= 0;
  final host = hasPort ? hostPort.substring(0, separator) : hostPort;
  if (hasPort && !_isPort(hostPort.substring(separator + 1))) return false;
  if (host.toLowerCase() == 'localhost' || _isIpAddress(host)) return true;
  final labels = host.split('.');
  return labels.length > 1 &&
      labels.every(
        (label) =>
            label.isNotEmpty &&
            label.codeUnits.every(
              (code) =>
                  _isAsciiLetter(code) || _isAsciiDigit(code) || code == 0x2d,
            ),
      );
}

bool _isPort(String value) =>
    value.isNotEmpty && value.codeUnits.every(_isAsciiDigit);

bool _isIpAddress(String value) {
  final pieces = value.split('.');
  return pieces.length == 4 &&
      pieces.every((piece) {
        final number = int.tryParse(piece);
        return number != null && number >= 0 && number <= 255;
      });
}

bool _isAsciiLetter(int code) =>
    (code >= 0x41 && code <= 0x5a) || (code >= 0x61 && code <= 0x7a);
bool _isAsciiDigit(int code) => code >= 0x30 && code <= 0x39;
