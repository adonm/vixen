#!/usr/bin/env python3
"""CLI failure semantics: invalid args exit 2, failed loads/exports exit 1."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import sys
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[1]

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        if self.path == "/ok":
            body = b"<html><title>OK</title><p>cli fixture</p></html>"
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.send_header("Content-Length", "4")
            self.end_headers()
            self.wfile.write(b"nope")

def run(binary, *args, cwd=None):
    return subprocess.run([str(binary), *args], cwd=cwd or ROOT,
                          capture_output=True, timeout=15)

def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "vixen").resolve()
    (ROOT / ".tmp").mkdir(exist_ok=True)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        base = f"http://127.0.0.1:{server.server_port}"
        with tempfile.TemporaryDirectory(prefix="vixen-cli-", dir=ROOT / ".tmp") as tmp:
            work = Path(tmp)
            prof = work / "prof"
            # Unknown flags, missing values, invalid widths, extra positionals.
            bad_argv = [
                (["browse", "--bogus", f"{base}/ok"], "usage: vixen browse"),
                (["browse", "--profile"], "usage: vixen browse"),
                (["browse", "--profile="], "usage: vixen browse"),
                (["browse", "--width"], "usage: vixen browse"),
                (["browse", "--width", "banana", f"{base}/ok"], "usage: vixen browse"),
                (["browse", "--width=0", f"{base}/ok"], "usage: vixen browse"),
                (["browse", "--width=99", f"{base}/ok"], "usage: vixen browse"),
                (["browse", "--width=8193", f"{base}/ok"], "usage: vixen browse"),
                (["browse", "--dump=x", f"{base}/ok"], "usage: vixen browse"),
                (["browse", f"{base}/ok", f"{base}/ok"], "usage: vixen browse"),
                (["browse"], "usage: vixen browse"),
                (["fetch", "--bogus", f"{base}/ok"], "usage: vixen fetch"),
                (["fetch", "--profile"], "usage: vixen fetch"),
                (["fetch", f"{base}/ok", f"{base}/ok"], "usage: vixen fetch"),
                (["fetch"], "usage: vixen fetch"),
                (["render", "--bogus", "corpus/form.html"], "usage: vixen render"),
                (["render", "--out"], "usage: vixen render"),
                (["render", "--width", "0", "corpus/form.html"], "usage: vixen render"),
                (["render", "a.html", "b.html"], "usage: vixen render"),
                (["render"], "usage: vixen render"),
                (["tui", "--bogus", "corpus/form.html"], "usage: vixen tui"),
                (["tui", "--width", "xx", "corpus/form.html"], "usage: vixen tui"),
                (["tui"], "usage: vixen tui"),
                (["show", "a.html", "b.html"], "usage: vixen show"),
                (["show"], "usage: vixen show"),
                (["parse"], "usage: vixen parse"),
                (["js"], "usage: vixen js"),
                (["rss", "extra"], "usage: vixen rss"),
                (["shapetest", "extra"], "usage: vixen shapetest"),
                (["nettest", "extra"], "usage: vixen nettest"),
                (["termtest", "extra"], "usage: vixen termtest"),
                (["browsetest", "extra"], "usage: vixen browsetest"),
                (["domtest", "a", "b"], "usage: vixen domtest"),
                (["wasmtest"], "usage: vixen wasmtest"),
                (["wasmtest", "a", "b"], "usage: vixen wasmtest"),
                (["frobnicate"], "unknown command"),
                ([], "usage: vixen"),
            ]
            for argv, needle in bad_argv:
                p = run(binary, *argv)
                assert p.returncode == 2, (argv, p.returncode, p.stderr)
                assert needle.encode() in p.stderr, (argv, p.stderr)
            print(f"PASS cli {len(bad_argv)} invalid invocations exit 2 with usage")

            for argv in (["browse", "--help"], ["fetch", "--help"], ["render", "--help"],
                        ["tui", "--help"], ["help"], ["--help"]):
                p = run(binary, *argv)
                assert p.returncode == 0, (argv, p.returncode)
                assert b"usage:" in p.stdout or b"Vixen" in p.stdout, argv
            print("PASS cli help exits 0")

            # Failed loads/exports exit 1 (not 0, not 2, no panic).
            p = run(binary, "fetch", "--profile", str(prof), f"{base}/missing")
            assert p.returncode == 1, ("fetch 404", p.returncode)
            p = run(binary, "fetch", "--profile", str(prof), "::::")
            assert p.returncode == 1, ("fetch bad-url", p.returncode)
            p = run(binary, "browse", "--dump", "--profile", str(prof), f"{base}/missing")
            assert p.returncode == 1, ("dump 404", p.returncode, p.stdout)
            assert b"404" in p.stdout or b"404" in p.stderr
            p = run(binary, "browse", "--dump", "--profile", str(prof), "http://127.0.0.1:1/none")
            assert p.returncode == 1, ("dump refused", p.returncode)
            p = run(binary, "parse", str(work / "does-not-exist.html"))
            assert p.returncode == 1, ("parse missing", p.returncode)
            p = run(binary, "js", str(work / "does-not-exist.js"))
            assert p.returncode == 1, ("js missing", p.returncode)
            p = run(binary, "render", "--out", str(work / "o.png"), str(work / "missing.html"))
            assert p.returncode == 1, ("render missing", p.returncode)
            p = run(binary, "render", "--out", str(work / "no-dir" / "o.png"), "corpus/form.html")
            assert p.returncode == 1, ("render bad-out", p.returncode)
            p = run(binary, "tui", str(work / "missing.html"))
            assert p.returncode == 1, ("tui missing", p.returncode)
            print("PASS cli failed loads/exports exit 1")

            # Success still works through the strict parser (both = and space forms).
            p = run(binary, "fetch", "--profile", str(prof), f"{base}/ok")
            assert p.returncode == 0, p.stderr
            p = run(binary, "browse", "--dump", f"--profile={prof}", "--width=600", f"{base}/ok")
            assert p.returncode == 0, p.stderr
            assert b"cli fixture" in p.stdout
            out = work / "ok.png"
            p = run(binary, "render", "--out", str(out), "--width", "600", "corpus/form.html")
            assert p.returncode == 0 and out.is_file() and out.stat().st_size > 100, p.stderr
            p = run(binary, "browse", "--dump", "--profile", str(prof), "--", f"{base}/ok")
            assert p.returncode == 0, ("-- separator", p.stderr)
            print("PASS cli success paths through strict parser")
    finally:
        server.shutdown()
        thread.join(timeout=5)
        server.server_close()

if __name__ == "__main__":
    main()
