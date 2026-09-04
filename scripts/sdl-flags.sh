#!/usr/bin/env bash
# Linker flags for the pinned static SDL3, sanitized for odin's
# space-splitting flag parser: -Bstatic fences libSDL3 into the archive
# (otherwise the system shared lib wins), backend deps stay shared.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
libdir="$root/thirdparty/sdl3/lib"
pc="$libdir/pkgconfig/sdl3.pc"
test -f "$pc" || { echo "sdl-flags: missing $pc; run 'just setup-sdl' first" >&2; exit 1; }
export PKG_CONFIG_PATH="$(dirname "$pc")"

out="-L$libdir -Wl,-Bstatic -lSDL3 -Wl,-Bdynamic"
# shellcheck disable=SC2046
for tok in $(pkg-config --static --libs sdl3); do
  case "$tok" in
    -L*|-Wl,*|-lSDL3) continue ;;
    *) out="$out $tok" ;;
  esac
done
printf '%s' "$out"
