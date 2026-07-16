#!/usr/bin/env python3
"""Require BrowserCore content semantics through Flutter's native AT-SPI tree."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402


MAX_NODES = 4096


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", required=True)
    parser.add_argument("--library", required=True)
    parser.add_argument("--url", required=True)
    parser.add_argument("--expect", action="append", required=True)
    parser.add_argument("--timeout", type=float, default=30.0)
    return parser.parse_args()


def accessible_names(process_id: int) -> set[str]:
    names: set[str] = set()
    pending = [Atspi.get_desktop(index) for index in range(Atspi.get_desktop_count())]
    visited = 0
    while pending and visited < MAX_NODES:
        node = pending.pop()
        visited += 1
        try:
            node_process_id = node.get_process_id()
            name = node.get_name()
            if node_process_id == process_id and name:
                names.add(name)
            count = min(node.get_child_count(), MAX_NODES - visited)
            pending.extend(node.get_child_at_index(index) for index in range(count))
        except Exception:
            # Applications can disappear while the desktop tree is traversed.
            continue
    return names


def main() -> int:
    args = arguments()
    app = Path(args.app).resolve()
    library = Path(args.library).resolve()
    if not app.is_file() or not os.access(app, os.X_OK):
        raise SystemExit(f"AT-SPI app is not executable: {app}")
    if not library.is_file():
        raise SystemExit(f"AT-SPI native library is missing: {library}")

    env = os.environ.copy()
    env.update(
        {
            "GDK_BACKEND": "wayland",
            "GTK_A11Y": "atspi",
            "NO_AT_BRIDGE": "0",
            "LIBGL_ALWAYS_SOFTWARE": "1",
            "VIXEN_FFI_LIBRARY": str(library),
            "VIXEN_PROFILE_PATH": str(
                Path.cwd() / ".tmp" / "at-spi-profile" / "profile.redb"
            ),
            "VIXEN_START_URL": args.url,
        }
    )
    Path(env["VIXEN_PROFILE_PATH"]).parent.mkdir(parents=True, exist_ok=True)
    Path(env["VIXEN_PROFILE_PATH"]).unlink(missing_ok=True)
    process = subprocess.Popen(
        [str(app)],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    seen: set[str] = set()
    try:
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            if process.poll() is not None:
                output = process.stdout.read() if process.stdout else ""
                raise SystemExit(
                    f"Flutter shell exited before AT-SPI evidence ({process.returncode})\n{output}"
                )
            seen.update(accessible_names(process.pid))
            if all(expected in seen for expected in args.expect):
                print("AT-SPI names:", ", ".join(sorted(args.expect)))
                return 0
            time.sleep(0.2)
        missing = sorted(set(args.expect) - seen)
        sample = sorted(seen)[:80]
        raise SystemExit(
            f"AT-SPI names missing after {args.timeout:.0f}s: {missing}; observed: {sample}"
        )
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


if __name__ == "__main__":
    sys.exit(main())
