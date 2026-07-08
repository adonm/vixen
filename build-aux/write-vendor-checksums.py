#!/usr/bin/env python3
"""Write Cargo vendor checksum metadata for Flatpak-extracted crates."""

from __future__ import annotations

import hashlib
import json
import sys
import tomllib
from pathlib import Path


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def checksum_files(crate_dir: Path) -> dict[str, str]:
    files: dict[str, str] = {}
    checksum_path = crate_dir / ".cargo-checksum.json"
    for path in sorted(crate_dir.rglob("*")):
        if path == checksum_path or not path.is_file():
            continue
        files[path.relative_to(crate_dir).as_posix()] = file_sha256(path)
    return files


def main() -> int:
    root = Path.cwd()
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    count = 0
    for package in lock.get("package", []):
        if not str(package.get("source", "")).startswith("registry+"):
            continue
        name = package["name"]
        version = package["version"]
        checksum = package.get("checksum")
        crate_dir = root / "cargo" / "vendor" / f"{name}-{version}"
        if not checksum:
            raise SystemExit(f"registry package {name} {version} has no checksum")
        if not crate_dir.is_dir():
            raise SystemExit(f"missing vendored crate directory: {crate_dir}")
        checksum_data = {
            "files": checksum_files(crate_dir),
            "package": checksum,
        }
        (crate_dir / ".cargo-checksum.json").write_text(
            json.dumps(checksum_data, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )
        count += 1
    print(f"wrote Cargo vendor checksums for {count} crates")
    return 0


if __name__ == "__main__":
    sys.exit(main())
