#!/usr/bin/env python3
"""Create a deterministic official Linux release archive from a Flutter bundle."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import os
import re
import subprocess
import tarfile
from pathlib import Path, PurePosixPath

REQUIRED = (
    "vixen_shell",
    "data/icudtl.dat",
    "data/flutter_assets/AssetManifest.bin",
    "lib/libapp.so",
    "lib/libflutter_linux_gtk4.so",
    "lib/libvixen_ffi.so",
)
PREFIX = PurePosixPath("vixen")


def validate_bundle(bundle: Path) -> None:
    if not bundle.is_dir():
        raise SystemExit(f"release bundle does not exist: {bundle}")
    for relative in REQUIRED:
        path = bundle / relative
        if not path.is_file():
            raise SystemExit(f"release bundle is missing {relative}")
    if not os.access(bundle / "vixen_shell", os.X_OK):
        raise SystemExit("release runner is not executable")

    saw_gtk4 = False
    for path in bundle.rglob("*"):
        if not path.is_file():
            continue
        with path.open("rb") as file:
            magic = file.read(4)
        if magic == b"\x7fELF":
            sections = subprocess.run(
                ["readelf", "--sections", "--wide", path],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            if re.search(r"\.debug_(?:info|line)\b", sections):
                raise SystemExit(f"release ELF contains debug sections: {path.relative_to(bundle)}")
            dynamic = subprocess.run(
                ["readelf", "--dynamic", "--wide", path],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            needed = set(re.findall(r"\(NEEDED\).*Shared library: \[([^]]+)]", dynamic))
            if "libgtk-3.so.0" in needed:
                raise SystemExit(
                    f"GTK4 release ELF links GTK3: {path.relative_to(bundle)}"
                )
            saw_gtk4 = saw_gtk4 or "libgtk-4.so.1" in needed

    if not saw_gtk4:
        raise SystemExit("GTK4 release bundle does not link libgtk-4.so.1")

    forbidden = [
        path.relative_to(bundle)
        for path in bundle.rglob("*")
        if path.name.endswith((".debug", ".dSYM"))
        or path.name in {"kernel_blob.bin", "vm_snapshot_data", "isolate_snapshot_data"}
    ]
    if forbidden:
        raise SystemExit(f"release bundle contains debug/JIT artifacts: {forbidden}")


def normalized(info: tarfile.TarInfo) -> tarfile.TarInfo:
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = 0
    if info.issym() or info.islnk():
        link = PurePosixPath(info.linkname)
        if link.is_absolute() or ".." in link.parts:
            raise SystemExit(f"unsafe release symlink: {info.name} -> {info.linkname}")
    return info


def add_path(archive: tarfile.TarFile, source: Path, name: PurePosixPath) -> None:
    info = normalized(archive.gettarinfo(str(source), arcname=str(name)))
    if info.isfile():
        with source.open("rb") as file:
            archive.addfile(info, file)
    else:
        archive.addfile(info)


def create_archive(bundle: Path, output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    temporary.unlink(missing_ok=True)

    with temporary.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", compresslevel=9, mtime=0, fileobj=raw) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                add_path(archive, bundle, PREFIX)
                for path in sorted(bundle.rglob("*"), key=lambda item: item.relative_to(bundle).as_posix()):
                    add_path(archive, path, PREFIX / path.relative_to(bundle).as_posix())
    temporary.replace(output)


def validate_archive(output: Path) -> None:
    with tarfile.open(output, "r:gz") as archive:
        members = archive.getmembers()
        names = {member.name for member in members}
        for member in members:
            path = PurePosixPath(member.name)
            if path.is_absolute() or not path.parts or path.parts[0] != PREFIX.name or ".." in path.parts:
                raise SystemExit(f"unsafe release archive path: {member.name}")
        for relative in REQUIRED:
            if str(PREFIX / relative) not in names:
                raise SystemExit(f"release archive is missing {relative}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("bundle", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()

    bundle = args.bundle.resolve()
    output = args.output.resolve()
    if output == bundle or bundle in output.parents:
        raise SystemExit("release archive must be outside the Flutter bundle")

    validate_bundle(bundle)
    create_archive(bundle, output)
    validate_archive(output)
    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    print(f"{digest}  {output}")


if __name__ == "__main__":
    main()
