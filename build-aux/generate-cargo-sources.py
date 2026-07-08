#!/usr/bin/env python3
"""Generate Flatpak cargo source archives from Cargo.lock.

The output is the format consumed from a Flatpak manifest `sources` array. It
keeps Cargo builds offline inside the flatpak-builder sandbox: flatpak-builder
downloads and verifies each crate as a source before the build starts, then
Cargo reads from `cargo/vendor` with `--offline`.
"""

from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path
from urllib.parse import quote


def crate_url(name: str, version: str) -> str:
    crate = quote(name, safe="")
    archive = quote(f"{name}-{version}.crate", safe="")
    return f"https://static.crates.io/crates/{crate}/{archive}"


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    lock_path = root / "Cargo.lock"
    out_path = root / "build-aux" / "cargo-sources.json"

    lock = tomllib.loads(lock_path.read_text(encoding="utf-8"))
    sources = []
    for package in lock.get("package", []):
        if not str(package.get("source", "")).startswith("registry+"):
            continue
        name = package["name"]
        version = package["version"]
        checksum = package.get("checksum")
        if not checksum:
            raise SystemExit(f"registry package {name} {version} has no checksum")
        sources.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": crate_url(name, version),
                "sha256": checksum,
                "dest": f"cargo/vendor/{name}-{version}",
                "strip-components": 1,
            }
        )

    out_path.write_text(json.dumps(sources, indent=4, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {out_path.relative_to(root)} ({len(sources)} crates)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
