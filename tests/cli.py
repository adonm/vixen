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
                (["browse", "--format", f"{base}/ok"], "usage: vixen browse"),
                (["browse", "--dump", "--format", "xml", f"{base}/ok"], "usage: vixen browse"),
                (["browse", "--dump", "--format=xml", f"{base}/ok"], "usage: vixen browse"),
                (["browse", "--format", "json", f"{base}/ok"], "usage: vixen browse"),
                (["browse", f"{base}/ok", f"{base}/ok"], "usage: vixen browse"),
                (["browse"], "usage: vixen browse"),
                (["fetch", "--bogus", f"{base}/ok"], "usage: vixen fetch"),
                (["fetch", "--profile"], "usage: vixen fetch"),
                (["fetch", f"{base}/ok", f"{base}/ok"], "usage: vixen fetch"),
                (["fetch"], "usage: vixen fetch"),
                (["render", "--bogus", "corpus/form.html"], "usage: vixen render"),
                (["render", "--out"], "usage: vixen render"),
                (["render", "--meta"], "usage: vixen render"),
                (["render", "--profile"], "usage: vixen render"),
                (["render", "--base-url"], "usage: vixen render"),
                (["render", "--width", "0", "corpus/form.html"], "usage: vixen render"),
                (["render", "a.html", "b.html"], "usage: vixen render"),
                (["render"], "usage: vixen render"),
                (["tui", "--bogus", "corpus/form.html"], "usage: vixen tui"),
                (["tui", "--width", "xx", "corpus/form.html"], "usage: vixen tui"),
                (["tui", "--profile"], "usage: vixen tui"),
                (["tui", "--base-url"], "usage: vixen tui"),
                (["tui", "--meta"], "usage: vixen tui"),
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
                (["version", "extra"], "usage: vixen version"),
                (["--version", "extra"], "usage: vixen version"),
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

            for argv in (["version"], ["--version"], ["-V"]):
                p = run(binary, *argv)
                assert p.returncode == 0, (argv, p.returncode)
                assert p.stdout.startswith(b"vixen ") and b"odin " in p.stdout, (argv, p.stdout)
            print("PASS cli version reports build identity")

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
            assert b"\x1b_G" not in p.stdout and b"\x1b[" not in p.stdout
            out = work / "ok.png"
            p = run(binary, "render", "--out", str(out), "--width", "600", "corpus/form.html")
            assert p.returncode == 0 and out.is_file() and out.stat().st_size > 100, p.stderr
            p = run(binary, "browse", "--dump", "--profile", str(prof), "--", f"{base}/ok")
            assert p.returncode == 0, ("-- separator", p.stderr)
            print("PASS cli success paths through strict parser")

            # M3 headless: JSON dump, render meta/PNG geometry, truncation,
            # outside-checkout execution, byte reproducibility.
            import json as _json, struct as _struct
            def png_size(path):
                with open(path, "rb") as f:
                    head = f.read(24)
                assert head[:8] == b"\x89PNG\r\n\x1a\n" and head[12:16] == b"IHDR"
                return _struct.unpack(">II", head[16:24])
            p = run(binary, "browse", "--dump", "--format", "json", "--profile", str(prof),
                    "--width", "600", f"{base}/ok")
            assert p.returncode == 0, p.stderr
            assert b"\x1b_G" not in p.stdout
            doc = _json.loads(p.stdout.decode())
            assert doc["title"] == "OK" and "cli fixture" in " ".join(doc["lines"]), doc
            assert doc["width"] == 600 and doc["is_error"] is False
            assert isinstance(doc["lines"], list) and isinstance(doc["links"], list)
            p = run(binary, "browse", "--dump", "--format=json", "--profile", str(prof), f"{base}/missing")
            assert p.returncode == 1, ("dump json 404", p.returncode)
            doc = _json.loads(p.stdout.decode())
            assert doc["is_error"] is True
            print("PASS cli dump --format json (success + error shapes)")

            meta, img = work / "r.json", work / "r.png"
            p = run(binary, "render", "--out", str(img), "--meta", str(meta), "--width", "600",
                    "--profile", str(prof), "corpus/form.html")
            assert p.returncode == 0, p.stderr
            w, h = png_size(img)
            assert (w, h) == (600, _json.loads(meta.read_text())["height"]), (w, h)
            m = _json.loads(meta.read_text())
            assert m["source"] == "corpus/form.html" and m["base_url"] == ""
            assert m["width"] == 600 and m["truncated"] is False and m["lines"] > 0
            # Outside the checkout: absolute paths, unrelated cwd, no repo assets.
            outside = work / "cwd"
            outside.mkdir()
            img2, meta2 = work / "o2.png", work / "o2.json"
            p = run(binary, "render", "--out", str(img2), "--meta", str(meta2), "--width", "600",
                    "--profile", str(prof), str(ROOT / "corpus" / "form.html"), cwd=outside)
            assert p.returncode == 0, p.stderr
            assert png_size(img2) == (w, h)
            assert img.read_bytes() == img2.read_bytes(), "render not reproducible"
            print("PASS cli render meta/PNG geometry, outside checkout, reproducibility")

            timg, tmeta = work / "t.png", work / "t.json"
            p = run(binary, "render", "--out", str(timg), "--meta", str(tmeta), "--width", "400",
                    "--profile", str(prof), "corpus/tall.html")
            assert p.returncode == 0, p.stderr
            assert b"truncated to 4000px" in p.stderr, p.stderr[-300:]
            w, h = png_size(timg)
            m = _json.loads(tmeta.read_text())
            assert (w, h) == (400, 4000) and m["truncated"] is True
            assert m["full_height"] > 4000 and m["lines"] > 100
            print("PASS cli render truncation warns, caps at 4000px, meta flags it")

            p = run(binary, "tui", "--width", "600", "--profile", str(prof),
                    "--meta", str(work / "tui.json"), "corpus/form.html")
            assert p.returncode == 0, p.stderr
            assert b"\x1b_G" in p.stdout, "one-shot Kitty missing"
            m = _json.loads((work / "tui.json").read_text())
            assert m["width"] == 600 and m["lines"] > 0
            print("PASS cli one-shot tui carries base/profile/meta pipeline")
    finally:
        server.shutdown()
        thread.join(timeout=5)
        server.server_close()

if __name__ == "__main__":
    main()
