#!/usr/bin/env python3
"""Deterministic net-test server (stdlib only). Endpoints:
  /etag        ETag "v1"; 304 on If-None-Match: "v1"; counts body sends
  /lastmod     fixed Last-Modified; 304 on If-Modified-Since match
  /maxage?n=N  Cache-Control: max-age=N
  /nostore     Cache-Control: no-store
  /vary        Vary: X-Foo; echoes X-Foo value in body
  /setcookies  multiple Set-Cookie (path/domain/secure variants)
  /redir/<n>   302 chain, Set-Cookie per hop, ends at /echo
  /echo        body = received Cookie header + X-Foo value
  /post        echoes METHOD + body
  /stats       JSON hit counts per path (proves cache behavior)
Usage: testserver.py <portfile>
"""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs

COUNTS = {}
LOCK = threading.Lock()
FIXED_DATE = "Wed, 01 Jan 2025 00:00:00 GMT"


def bump(path):
    with LOCK:
        COUNTS[path] = COUNTS.get(path, 0) + 1


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _send(self, code, headers, body):
        if isinstance(body, str):
            body = body.encode()
        self.send_response(code)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def _route(self, body=None):
        u = urlparse(self.path)
        p = u.path
        bump(p)
        q = parse_qs(u.query)
        if p == "/etag":
            if self.headers.get("If-None-Match") == '"v1"':
                self._send(304, [("ETag", '"v1"')], b"")
            else:
                self._send(200, [("ETag", '"v1"'), ("Content-Type", "text/plain")], b"etag-body")
        elif p == "/lastmod":
            if self.headers.get("If-Modified-Since") == FIXED_DATE:
                self._send(304, [], b"")
            else:
                self._send(200, [("Last-Modified", FIXED_DATE), ("Content-Type", "text/plain")], b"lm-body")
        elif p == "/maxage":
            n = q.get("n", ["60"])[0]
            self._send(200, [("Cache-Control", f"max-age={n}"), ("Content-Type", "text/plain")], b"maxage-body")
        elif p == "/nostore":
            self._send(200, [("Cache-Control", "no-store"), ("Content-Type", "text/plain")], b"nostore-body")
        elif p == "/vary":
            foo = self.headers.get("X-Foo", "-")
            self._send(200, [("Vary", "X-Foo"), ("Cache-Control", "max-age=60"), ("Content-Type", "text/plain")], f"foo={foo}")
        elif p == "/setcookies":
            self.send_response(200)
            self.send_header("Set-Cookie", "sess=abc123; Path=/; HttpOnly")
            self.send_header("Set-Cookie", "pref=dark; Path=/prefs; Max-Age=3600")
            self.send_header("Set-Cookie", "evil=x; Domain=com; Path=/")
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", "2")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(b"ok")
        elif p.startswith("/redir/"):
            try:
                n = int(p.split("/")[2])
            except (IndexError, ValueError):
                n = 0
            nxt = "/echo" if n <= 1 else f"/redir/{n - 1}"
            self.send_response(302)
            self.send_header("Set-Cookie", f"hop{n}=1; Path=/")
            self.send_header("Location", nxt)
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "close")
            self.end_headers()
        elif p == "/echo":
            ck = self.headers.get("Cookie", "-")
            foo = self.headers.get("X-Foo", "-")
            self._send(200, [("Content-Type", "text/plain")], f"cookie=[{ck}] xfoo=[{foo}]")
        elif p == "/post":
            ln = int(self.headers.get("Content-Length", "0") or 0)
            data = self.rfile.read(ln) if ln else b""
            self._send(200, [("Content-Type", "text/plain")], f"{self.command}:{data.decode()}")
        elif p == "/stats":
            with LOCK:
                snap = dict(COUNTS)
            self._send(200, [("Content-Type", "application/json")], json.dumps(snap))
        elif p in ("/article", "/relayout"):
            with open(f"corpus{p}.html", "rb") as f:
                html = f.read()
            self._send(200, [("Content-Type", "text/html; charset=utf-8"), ("Cache-Control", "max-age=60")], html)
        elif p == "/imgpage":
            self._send(200, [("Content-Type", "text/html; charset=utf-8")],
                          "<html><head><title>Pics</title></head><body>"
                          "<h1>Gallery</h1>"
                          '<img src="/img.png" alt="tiny" width="8" height="6">'
                          '<img src="/missing.png" alt="gone">'
                          '<img src="/img.png" alt="dup">'
                          '<img src="/img.png" width="4" height="3">'
                          "</body></html>")
        elif p == "/search":
            self._send(200, [("Content-Type", "text/html; charset=utf-8")],
                          f"<html><head><title>Results</title></head><body><p>results for [{u.query}]</p></body></html>")
        elif p == "/form":
            with open("corpus/form.html", "rb") as f:
                html = f.read()
            self._send(200, [("Content-Type", "text/html; charset=utf-8")], html)
        elif p == "/postform":
            ln = int(self.headers.get("Content-Length", "0") or 0)
            data = self.rfile.read(ln) if ln else b""
            self._send(200, [("Content-Type", "text/html; charset=utf-8")],
                          f"<html><head><title>Posted</title></head><body><p>got [{data.decode()}]</p></body></html>")
        elif p == "/img.png":
            import struct, zlib
            w, h = 8, 6
            raw = b"".join(b"\x00" + bytes([c for x in range(w) for c in (x * 32 % 256, y * 43 % 256, 128, 255)]) for y in range(h))
            ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
            def chunk(t, d):
                c = t + d
                return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c))
            png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) +
                   chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))
            self._send(200, [("Content-Type", "image/png"), ("Cache-Control", "max-age=60")], png)
        else:
            self._send(404, [("Content-Type", "text/plain")], b"nope")

    do_GET = _route

    def do_POST(self):
        self._route()


def main():
    srv = HTTPServer(("127.0.0.1", 0), H)
    with open(sys.argv[1], "w") as f:
        f.write(str(srv.server_address[1]))
    srv.serve_forever()


main()
