#!/usr/bin/env bash
set -euo pipefail

readonly IMAGE=ghcr.io/flathub-infra/flatpak-github-actions@sha256:a2b78890f165cd5b5c6a8629c5f6cb293e64d1bf523ca6662fac8ca8e247f8b0
readonly FLUTTER_URL=https://storage.googleapis.com/flutter_infra_release/releases/stable/linux/flutter_linux_3.47.1-stable.tar.xz
readonly FLUTTER_SHA256=a1d8166c0309267cb7dc99f1424eecf08b86946ad3b50723c6f59945964aea45
readonly RUSTUP_URL=https://static.rust-lang.org/rustup/archive/1.29.0/x86_64-unknown-linux-gnu/rustup-init
readonly RUSTUP_SHA256=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10
readonly RUSTY_V8_URL=https://github.com/denoland/rusty_v8/releases/download/v149.4.0/librusty_v8_simdutf_release_x86_64-unknown-linux-gnu.a.gz
readonly RUSTY_V8_ARCHIVE=.tmp/linux-release/librusty_v8_simdutf_release_x86_64-unknown-linux-gnu.a.gz
readonly RUSTY_V8_SHA256=aa30f198b6e7be2188df6498f95053c4c052f212037a01f2c31414d7aca84b53

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
mode=${1:-release}
docker=${DOCKER:-docker}

command -v "$docker" >/dev/null 2>&1 || {
  echo "Docker CLI missing from PATH" >&2
  exit 1
}
if [[ $(uname -m) != x86_64 ]]; then
  echo "Vixen Flutter builds require an x86_64 Docker host" >&2
  exit 1
fi

case "$mode" in
  pull)
    exec "$docker" pull --platform linux/amd64 "$IMAGE"
    ;;
  check)
    "$docker" image inspect "$IMAGE" >/dev/null 2>&1 || {
      echo "GNOME SDK builder image missing; run 'just docker-builder-pull'" >&2
      exit 1
    }
    exit 0
    ;;
  prefetch | release | hello | wayland-driver | shell)
    "$docker" image inspect "$IMAGE" >/dev/null 2>&1 || {
      echo "GNOME SDK builder image missing; run 'just docker-builder-pull'" >&2
      exit 1
    }
    ;;
  *)
    echo "usage: docker-flutter.sh <pull|check|prefetch|release|hello|wayland-driver|shell>" >&2
    exit 2
    ;;
esac

cache="$root/.tmp/docker-flutter"
workspace="$cache/workspace"
rm -rf "$workspace"
mkdir -p \
  "$workspace/.tmp" \
  "$workspace/flutter/vixen_shell" \
  "$workspace/fixtures/artifact-size/flutter_hello" \
  "$cache/cargo" \
  "$cache/downloads" \
  "$cache/flutter" \
  "$cache/home" \
  "$cache/pub" \
  "$cache/rustup" \
  "$cache/target" \
  "$root/.tmp/linux-release" \
  "$root/.tmp/wayland-virtual-pointer" \
  "$root/flutter/vixen_shell/build" \
  "$root/fixtures/artifact-size/flutter_hello/build"
ln -s /outputs/linux-release "$workspace/.tmp/linux-release"
ln -s /outputs/wayland-virtual-pointer "$workspace/.tmp/wayland-virtual-pointer"
ln -s /outputs/vixen-build "$workspace/flutter/vixen_shell/build"
ln -s /outputs/hello-build "$workspace/fixtures/artifact-size/flutter_hello/build"

network_args=()
if [[ $mode == release || $mode == hello || $mode == wayland-driver ]]; then
  network_args=(--network=none)
fi

# Root in a rootless daemon maps to the invoking host user. A rootful Docker
# daemon instead runs with the invoking numeric identity so bind-mounted build
# outputs never become root-owned.
user_args=()
if ! "$docker" info --format '{{json .SecurityOptions}}' | grep -q 'rootless'; then
  user_args=(--user "$(id -u):$(id -g)")
fi

run_args=(
  --rm
  --platform linux/amd64
  --pull=never
  --security-opt label=disable
  "${network_args[@]}"
  "${user_args[@]}"
  --env HOME=/cache/home
  --env CARGO_HOME=/cache/cargo
  --env RUSTUP_HOME=/cache/rustup
  --env RUSTUP_TOOLCHAIN=1.96.1
  --env VIXEN_CARGO_HOME=/cache/cargo
  --env VIXEN_CARGO_TARGET_DIR=/cache/target
  --env "RUSTY_V8_ARCHIVE=/workspace/$RUSTY_V8_ARCHIVE"
  --env PUB_CACHE=/cache/pub
  --env CI=true
  --env FLUTTER_ALREADY_LOCKED=true
  --env FLUTTER_SUPPRESS_ANALYTICS=true
  --env GIT_OPTIONAL_LOCKS=0
  --env SOURCE_DATE_EPOCH="$(git -C "$root" show -s --format=%ct HEAD)"
  --env TZ=UTC
  --volume "$root:/source:ro"
  --volume "$cache:/cache"
  --volume "$workspace:/workspace"
  --volume "$root/.tmp/linux-release:/outputs/linux-release"
  --volume "$root/.tmp/wayland-virtual-pointer:/outputs/wayland-virtual-pointer"
  --volume "$root/flutter/vixen_shell/build:/outputs/vixen-build"
  --volume "$root/fixtures/artifact-size/flutter_hello/build:/outputs/hello-build"
  --workdir /workspace
)
if [[ $mode != prefetch && $mode != shell ]]; then
  run_args+=(--env CARGO_NET_OFFLINE=true)
fi
if [[ $mode == shell ]]; then
  run_args+=(-it)
fi

read -r -d '' environment <<'EOF' || true
set -euo pipefail
sdk_root="$(realpath /var/lib/flatpak/runtime/org.gnome.Sdk/x86_64/50/active)/files"
sysroot=/workspace/.tmp/gnome-sysroot
mkdir -p /workspace/.tmp/compiler-bin "$sysroot"
ln -sfn "$sdk_root" "$sysroot/usr"
ln -sfn usr/lib "$sysroot/lib"
ln -sfn usr/lib "$sysroot/lib64"
ln -sf "$sdk_root/bin/gcc" /workspace/.tmp/compiler-bin/clang
ln -sf "$sdk_root/bin/g++" /workspace/.tmp/compiler-bin/clang++
export PATH="/workspace/.tmp/compiler-bin:/cache/flutter/bin:/cache/cargo/bin:$sdk_root/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export PKG_CONFIG_LIBDIR="$sdk_root/lib/x86_64-linux-gnu/pkgconfig:$sdk_root/share/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export CFLAGS="--sysroot=$sysroot"
export CXXFLAGS="--sysroot=$sysroot"
export LDFLAGS="--sysroot=$sysroot -Wl,-rpath-link,$sysroot/usr/lib/x86_64-linux-gnu"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$sdk_root/bin/gcc"
export RUSTFLAGS="-C link-arg=--sysroot=$sysroot -C link-arg=-Wl,-rpath-link,$sysroot/usr/lib/x86_64-linux-gnu"
pkg-config --exists gtk+-3.0
EOF

read -r -d '' copy_source <<'EOF' || true
tar -C /source \
  --exclude=./.cargo/bin \
  --exclude=./.cargo/git \
  --exclude=./.cargo/registry \
  --exclude=./.git \
  --exclude=./.oy \
  --exclude=./.tmp \
  --exclude=./flutter/vixen_shell/.dart_tool \
  --exclude=./flutter/vixen_shell/.flutter-plugins-dependencies \
  --exclude=./flutter/vixen_shell/build \
  --exclude=./fixtures/artifact-size/flutter_hello/.dart_tool \
  --exclude=./fixtures/artifact-size/flutter_hello/.flutter-plugins-dependencies \
  --exclude=./fixtures/artifact-size/flutter_hello/build \
  --exclude=./target \
  -cf - . | tar -C /workspace -xf -
EOF

case "$mode" in
  prefetch)
    read -r -d '' command <<EOF || true
flutter_archive=/cache/downloads/flutter-linux-x64-3.47.1.tar.xz
if ! test -x /cache/flutter/bin/flutter || \
    ! /cache/flutter/bin/flutter --version --machine | grep -q '"frameworkRevision": "6655482ec06e547f90abf8ae7590466f4415978d"'; then
  curl --fail --location --retry 3 '$FLUTTER_URL' --output "\$flutter_archive.part"
  mv "\$flutter_archive.part" "\$flutter_archive"
  rm -rf /cache/flutter
  mkdir -p /cache/flutter
  tar --extract --xz --file "\$flutter_archive" --strip-components=1 --directory /cache/flutter
fi
printf '%s  %s\n' '$FLUTTER_SHA256' "\$flutter_archive" | sha256sum --check

rustup_init=/cache/downloads/rustup-init
if ! test -x "\$rustup_init"; then
  curl --fail --location --retry 3 '$RUSTUP_URL' --output "\$rustup_init"
  chmod 0755 "\$rustup_init"
fi
printf '%s  %s\n' '$RUSTUP_SHA256' "\$rustup_init" | sha256sum --check
"\$rustup_init" -y --profile minimal --default-toolchain 1.96.1

test "\$(rustc --version)" = 'rustc 1.96.1 (31fca3adb 2026-06-26)'
flutter --version --machine | python3 -c '
import json, sys
value = json.load(sys.stdin)
assert value["frameworkVersion"] == "3.47.1"
assert value["frameworkRevision"] == "6655482ec06e547f90abf8ae7590466f4415978d"
assert value["engineRevision"] == "5d531788691ec3404cac0cee66ead4007b177363"
assert value["engineContentHash"] == "11d79658c444477b06513d32b52c8c4ccb7276b0"
assert value["dartSdkVersion"].startswith("3.13.1")
'

mkdir -p "\$(dirname '$RUSTY_V8_ARCHIVE')"
test -f '$RUSTY_V8_ARCHIVE' || curl --fail --location --retry 3 \
  '$RUSTY_V8_URL' --output '$RUSTY_V8_ARCHIVE'
printf '%s  %s\n' '$RUSTY_V8_SHA256' '$RUSTY_V8_ARCHIVE' | sha256sum --check
cargo fetch --locked
cd flutter/vixen_shell
flutter pub get --enforce-lockfile
cd ../../fixtures/artifact-size/flutter_hello
flutter pub get --enforce-lockfile
EOF
    ;;
  release)
    read -r -d '' command <<EOF || true
printf '%s  %s\n' '$RUSTY_V8_SHA256' '$RUSTY_V8_ARCHIVE' | sha256sum --check
test "\$(rustc --version)" = 'rustc 1.96.1 (31fca3adb 2026-06-26)'
cd flutter/vixen_shell
rm -rf build/linux
flutter pub get --offline --enforce-lockfile
flutter build linux --release --no-pub
strip --strip-unneeded build/linux/x64/release/bundle/vixen_shell
find build/linux/x64/release/bundle/lib -maxdepth 1 -type f \
  -name '*_plugin.so' -exec strip --strip-unneeded {} +
EOF
    ;;
  hello)
    read -r -d '' command <<'EOF' || true
cd fixtures/artifact-size/flutter_hello
rm -rf build/linux
flutter pub get --offline --enforce-lockfile
flutter build linux --release --no-pub
strip --strip-unneeded build/linux/x64/release/bundle/vixen_hello
find build/linux/x64/release/bundle/lib -maxdepth 1 -type f \
  -name '*_plugin.so' -exec strip --strip-unneeded {} +
EOF
    ;;
  wayland-driver)
    read -r -d '' command <<'EOF' || true
rm -rf .tmp/wayland-virtual-pointer/*
wayland-scanner client-header scripts/protocols/wlr-virtual-pointer-unstable-v1.xml \
  .tmp/wayland-virtual-pointer/wlr-virtual-pointer-unstable-v1-client-protocol.h
wayland-scanner private-code scripts/protocols/wlr-virtual-pointer-unstable-v1.xml \
  .tmp/wayland-virtual-pointer/wlr-virtual-pointer-unstable-v1-protocol.c
gcc -std=c11 -D_POSIX_C_SOURCE=200809L -Wall -Wextra -Werror \
  -I.tmp/wayland-virtual-pointer scripts/wayland-virtual-pointer.c \
  .tmp/wayland-virtual-pointer/wlr-virtual-pointer-unstable-v1-protocol.c \
  $(pkg-config --cflags --libs wayland-client) -lm \
  -o .tmp/wayland-virtual-pointer/wayland-virtual-pointer
EOF
    ;;
  shell)
    command='exec bash'
    ;;
esac

exec "$docker" run "${run_args[@]}" "$IMAGE" bash -lc \
  "$environment
$copy_source
$command"
