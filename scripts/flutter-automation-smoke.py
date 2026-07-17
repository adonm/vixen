#!/usr/bin/env python3
"""Capture two exact presented Flutter scenes from the release automation host."""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import zlib


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
CAPTURE_RE = re.compile(
    r"Vixen automation captured context=(\d+) document=(\d+) commit=(\d+) "
    r"viewport=(\d+)x(\d+) output=(.+)"
)
VIEWPORTS = ((320, 240), (480, 300))
EXPECTED_SHA256 = {
    (320, 240): "500ddde8c74f57dcd04d6d29388246a5ce82fc12cc20232ac024fb21a41f564d",
    (480, 300): "e548d131b11f5c0e4532d02aab77ab3a5593095ca24ae6819a9dbc5ca2c87c10",
}


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", required=True)
    parser.add_argument("--library", required=True)
    parser.add_argument("--url", required=True)
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--timeout", type=float, default=90.0)
    return parser.parse_args()


def decode_rgba(png: bytes, width: int, height: int) -> bytes:
    if png[:8] != PNG_SIGNATURE:
        raise ValueError("capture is not a PNG")
    offset = 8
    compressed = bytearray()
    color_type = None
    bit_depth = None
    saw_iend = False
    while offset + 12 <= len(png):
        length = struct.unpack(">I", png[offset : offset + 4])[0]
        kind = png[offset + 4 : offset + 8]
        chunk_end = offset + 12 + length
        if chunk_end > len(png):
            raise ValueError("PNG chunk exceeds the encoded output")
        data = png[offset + 8 : offset + 8 + length]
        expected_crc = struct.unpack(">I", png[offset + 8 + length : chunk_end])[0]
        actual_crc = zlib.crc32(kind + data) & 0xFFFFFFFF
        if actual_crc != expected_crc:
            raise ValueError(f"PNG {kind!r} chunk has an invalid CRC")
        offset = chunk_end
        if kind == b"IHDR":
            actual_width, actual_height, bit_depth, color_type = struct.unpack(
                ">IIBB", data[:10]
            )
            if (actual_width, actual_height) != (width, height):
                raise ValueError(
                    f"PNG is {actual_width}x{actual_height}, expected {width}x{height}"
                )
        elif kind == b"IDAT":
            compressed.extend(data)
        elif kind == b"IEND":
            saw_iend = True
            break
    if not saw_iend or offset != len(png):
        raise ValueError("PNG is missing its final IEND chunk")
    if bit_depth != 8 or color_type != 6:
        raise ValueError(
            f"expected an 8-bit RGBA PNG, got depth={bit_depth} type={color_type}"
        )
    filtered = zlib.decompress(compressed)
    stride = width * 4
    expected = height * (stride + 1)
    if len(filtered) != expected:
        raise ValueError(f"decoded PNG has {len(filtered)} bytes, expected {expected}")
    rgba = bytearray(height * stride)
    source = 0
    for y in range(height):
        filter_kind = filtered[source]
        source += 1
        row = bytearray(filtered[source : source + stride])
        source += stride
        prior = rgba[(y - 1) * stride : y * stride] if y else bytes(stride)
        for x in range(stride):
            left = row[x - 4] if x >= 4 else 0
            above = prior[x]
            upper_left = prior[x - 4] if x >= 4 else 0
            if filter_kind == 1:
                row[x] = (row[x] + left) & 0xFF
            elif filter_kind == 2:
                row[x] = (row[x] + above) & 0xFF
            elif filter_kind == 3:
                row[x] = (row[x] + ((left + above) // 2)) & 0xFF
            elif filter_kind == 4:
                estimate = left + above - upper_left
                distances = (
                    abs(estimate - left),
                    abs(estimate - above),
                    abs(estimate - upper_left),
                )
                predictor = (left, above, upper_left)[distances.index(min(distances))]
                row[x] = (row[x] + predictor) & 0xFF
            elif filter_kind != 0:
                raise ValueError(f"unsupported PNG filter {filter_kind}")
        rgba[y * stride : (y + 1) * stride] = row
    return bytes(rgba)


def capture(args: argparse.Namespace, width: int, height: int) -> tuple[Path, str, int]:
    output_dir = Path(args.output_dir).resolve()
    output = output_dir / f"scene-{width}x{height}.png"
    profile_dir = output_dir / f"profile-{width}x{height}"
    shutil.rmtree(profile_dir, ignore_errors=True)
    profile_dir.mkdir(parents=True)
    output.unlink(missing_ok=True)
    env = os.environ.copy()
    env.update(
        {
            "GDK_BACKEND": "wayland",
            "LIBGL_ALWAYS_SOFTWARE": "1",
            "VIXEN_FFI_LIBRARY": str(Path(args.library).resolve()),
            "VIXEN_PROFILE_PATH": str(profile_dir / "profile.redb"),
        }
    )
    command = [
        str(Path(args.app).resolve()),
        "--vixen-automation",
        f"--vixen-url={args.url}",
        f"--vixen-viewport={width}x{height}",
        f"--vixen-output={output}",
    ]
    result = subprocess.run(
        command,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=args.timeout,
        check=False,
    )
    print(result.stdout, end="")
    if result.returncode != 0:
        raise SystemExit(
            f"automation host exited with {result.returncode} for {width}x{height}"
        )
    if "Using the Impeller rendering backend" not in result.stdout:
        raise SystemExit("automation host did not report an Impeller backend")
    matches = list(CAPTURE_RE.finditer(result.stdout))
    if len(matches) != 1:
        raise SystemExit(f"expected one exact capture diagnostic, got {len(matches)}")
    match = matches[0]
    if (int(match.group(4)), int(match.group(5))) != (width, height):
        raise SystemExit(f"capture diagnostic named the wrong viewport: {match.group(0)}")
    if Path(match.group(6)) != output or int(match.group(3)) <= 0:
        raise SystemExit(f"capture diagnostic named invalid output/commit: {match.group(0)}")
    png = output.read_bytes()
    rgba = decode_rgba(png, width, height)
    if rgba[:4] != bytes((255, 255, 255, 255)):
        raise SystemExit(
            "top-left scene pixel was not the document canvas background; "
            "capture may contain host/compositor chrome"
        )
    if not any(pixel != 255 for offset, pixel in enumerate(rgba) if offset % 4 != 3):
        raise SystemExit("scene did not contain any painted document content")
    digest = hashlib.sha256(png).hexdigest()
    expected_digest = EXPECTED_SHA256[(width, height)]
    if digest != expected_digest:
        raise SystemExit(
            f"scene hash was {digest}, expected controlled fixture hash {expected_digest}"
        )
    return output, digest, int(match.group(3))


def main() -> int:
    args = arguments()
    app = Path(args.app).resolve()
    library = Path(args.library).resolve()
    if not app.is_file() or not os.access(app, os.X_OK):
        raise SystemExit(f"automation app is not executable: {app}")
    if not library.is_file():
        raise SystemExit(f"automation native library is missing: {library}")
    output_dir = Path(args.output_dir).resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    captures = [capture(args, width, height) for width, height in VIEWPORTS]
    if captures[0][1] == captures[1][1]:
        raise SystemExit("different automation viewports produced identical PNG hashes")
    print(
        "Flutter automation ok: "
        + ", ".join(
            f"{path.name} sha256={digest} commit={commit}"
            for path, digest, commit in captures
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
