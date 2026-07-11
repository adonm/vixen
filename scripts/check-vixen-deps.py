#!/usr/bin/env python3
"""Enforce stable leaf-crate and frontend dependency boundaries."""

import json
import subprocess
import sys


ALLOWED = {
    "vixen-api": set(),
    "vixen-net": set(),
    "vixen-store": set(),
    "vixen-wpt": {"vixen-api"},
    "vixen-shell": {"vixen-api", "vixen-engine"},
    "vixen-headless": {"vixen-api", "vixen-engine"},
    "vixen-ffi": {"vixen-api", "vixen-engine"},
}


def main() -> int:
    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            text=True,
        )
    )
    packages = {package["name"]: package for package in metadata["packages"]}
    failures: list[str] = []
    for crate, expected in ALLOWED.items():
        package = packages.get(crate)
        if package is None:
            failures.append(f"missing workspace package {crate}")
            continue
        actual = {
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency["name"].startswith("vixen-")
            and dependency.get("kind") in (None, "normal")
        }
        if actual != expected:
            failures.append(
                f"{crate}: expected Vixen deps {sorted(expected)}, got {sorted(actual)}"
            )
    if failures:
        print("architecture dependency gate failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print("architecture dependency gate ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
