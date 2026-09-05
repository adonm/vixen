#!/usr/bin/env python3
"""Repeatable startup/render measurements (no thresholds, just evidence).

Measures wall time for local parse/js/render, headless fetch/dump (cold vs
warm profile), and PTY time-to-first-Kitty-frame. Records version, fixture
hashes, and cache state. Prints JSON + table; exits nonzero only if a
command fails (not on timing).
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))
import tui_protocol

HAVE_GNU_TIME = shutil.which("/usr/bin/time") is not None

def sha256(path):
    h = hashlib.sha256()
    h.update(Path(path).read_bytes())
    return h.hexdigest()[:12]

def run_wall(binary, args, cwd=None, env=None, timeout=30):
    t0 = time.monotonic()
    p = subprocess.run([str(binary), *args], cwd=cwd or ROOT,
                       capture_output=True, timeout=timeout, env=env)
    dt = (time.monotonic() - t0) * 1000
    return p, dt

def run_rss_kb(binary, args):
    # Peak RSS via GNU time; None when unavailable.
    if not HAVE_GNU_TIME:
        return None
    p = subprocess.run(["/usr/bin/time", "-v", str(binary), *args],
                       cwd=ROOT, capture_output=True, timeout=30)
    for line in p.stderr.decode(errors="replace").splitlines():
        if "Maximum resident set size" in line:
            return int("".join(c for c in line if c.isdigit()))
    return None

def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "vixen").resolve()
    (ROOT / ".tmp").mkdir(exist_ok=True)
    out = {"binary": str(binary), "results": {}}
    p, _ = run_wall(binary, ["version"])
    assert p.returncode == 0, p.stderr
    out["version"] = p.stdout.decode().strip().replace("\n", " | ")
    out["fixtures"] = {name: sha256(ROOT / "corpus" / name)
                       for name in ("article.html", "relayout.html", "form.html",
                                    "example.html", "bench.js", "app-shell.html")}
    def measure(name, args, runs=5, rss=False):
        times = []
        for _ in range(runs):
            p, dt = run_wall(binary, args)
            assert p.returncode == 0, (name, args, p.returncode, p.stderr[-500:])
            times.append(dt)
        rec = {"ms_min": round(min(times), 1),
               "ms_p50": round(statistics.median(times), 1),
               "ms_max": round(max(times), 1)}
        if rss:
            rec["rss_kb"] = run_rss_kb(binary, args)
        out["results"][name] = rec
        print(f"{name:28s} min={rec['ms_min']:8.1f} p50={rec['ms_p50']:8.1f} max={rec['ms_max']:8.1f} ms"
              + (f" rss={rec['rss_kb']} KB" if rss else ""))

    measure("rss-cold-start", ["rss"], rss=True)
    measure("parse-corpus", ["parse", "corpus/example.html", "corpus/github.html"])
    measure("js-bench", ["js", "corpus/bench.js"])

    with tempfile.TemporaryDirectory(prefix="vixen-bench-", dir=ROOT / ".tmp") as tmp:
        work = Path(tmp)
        png = work / "bench.png"
        # Renderarel local fixture (full-page PNG).
        times = []
        for _ in range(3):
            p, dt = run_wall(binary, ["render", "--out", str(png), "--width", "900", "corpus/article.html"])
            assert p.returncode == 0, p.stderr
            times.append(dt)
        out["results"]["render-article"] = {"ms_min": round(min(times), 1),
            "ms_p50": round(statistics.median(times), 1),
            "png_kb": png.stat().st_size // 1024}
        print(f"{'render-article':28s} p50={statistics.median(times):8.1f} ms png={png.stat().st_size//1024} KB")

        with tui_protocol.server(work) as base:
            cold_prof = work / "cold"
            # Cold dump (fresh profile): startup + fetch + layout.
            p, dt_cold = run_wall(binary, ["browse", "--dump", "--profile", str(cold_prof),
                                           "--width", "900", base + "/relayout"])
            assert p.returncode == 0, p.stderr
            assert "Reflow Test" in p.stdout.decode()
            # Warm dump (same profile): cache hit path.
            times = []
            for _ in range(4):
                p, dt = run_wall(binary, ["browse", "--dump", "--profile", str(cold_prof),
                                          "--width", "900", base + "/relayout"])
                assert p.returncode == 0
                times.append(dt)
            out["results"]["dump-cold"] = {"ms": round(dt_cold, 1)}
            out["results"]["dump-warm"] = {"ms_min": round(min(times), 1),
                                           "ms_p50": round(statistics.median(times), 1)}
            print(f"{'dump-cold':28s} {dt_cold:8.1f} ms")
            print(f"{'dump-warm-p50':28s} {statistics.median(times):8.1f} ms")

            # PTY time-to-first-frame (transfer completion, not presentation).
            first_frames = []
            for i in range(3):
                prof = work / f"pty{i}"
                t0 = time.monotonic()
                with tui_protocol.browser(binary, prof, base + "/relayout",
                                          cols=80, rows=24) as b:
                    b.until(lambda: b.screen.frames > 0)
                    first_frames.append((time.monotonic() - t0) * 1000)
                    b.quit()
            out["results"]["pty-first-frame"] = {"ms_min": round(min(first_frames), 1),
                "ms_p50": round(statistics.median(first_frames), 1),
                "ms_max": round(max(first_frames), 1)}
            print(f"{'pty-first-frame':28s} min={min(first_frames):8.1f} p50={statistics.median(first_frames):8.1f} ms")

    (ROOT / ".tmp" / "bench.json").write_text(json.dumps(out, indent=2))
    print(f"\nversion: {out['version']}")
    print("wrote .tmp/bench.json")

if __name__ == "__main__":
    main()
