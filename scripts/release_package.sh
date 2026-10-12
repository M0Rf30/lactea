#!/usr/bin/env bash
# Package the release archive of one platform from an existing `cargo build --release`.
#
#   scripts/release_package.sh linux|macos|windows [version]
#
# Writes dist/lactea-<version>-<platform>.tar.gz (.zip on windows) containing the app, the C library
# (shared + static + import libs), include/lactea.h, the C examples, the desktop entry (linux) and the
# README. `version` defaults to $VERSION, then `dev`. Used by .github/workflows/release.yml; running it
# by hand after `cargo build --release -p lactea-app -p lactea-capi` is the local dry run of the
# packaging step. CARGO_TARGET_DIR is honoured.
set -euo pipefail
cd "$(dirname "$0")/.."

platform=${1:?usage: release_package.sh linux|macos|windows [version]}
version=${2:-${VERSION:-dev}}
rel=${CARGO_TARGET_DIR:-target}/release
header=crates/lactea-capi/include/lactea.h
examples=crates/lactea-capi/examples/c

case "$platform" in
  linux) name="lactea-$version-linux-x86_64" ;;
  macos) name="lactea-$version-macos-arm64" ;;
  windows) name="lactea-$version-windows-x86_64" ;;
  *) echo "unknown platform '$platform' (linux|macos|windows)" >&2; exit 2 ;;
esac

need() {
  local f
  for f in "$@"; do
    [ -e "$f" ] || { echo "missing $f: build first (cargo build --release -p lactea-app -p lactea-capi)" >&2; exit 1; }
  done
}

dir="dist/$name"
rm -rf "$dir" "dist/$name.tar.gz" "dist/$name.zip"
mkdir -p "$dir/include" "$dir/examples"
cp "$header" "$dir/include/"
cp "$examples"/*.c "$dir/examples/"
cp README.md "$dir/"

case "$platform" in
  linux)
    need "$rel/lactea" "$rel/liblactea_capi.so" "$rel/liblactea_capi.a"
    mkdir -p "$dir/lib"
    install -m755 "$rel/lactea" "$dir/"
    install -m644 "$rel/liblactea_capi.so" "$rel/liblactea_capi.a" "$dir/lib/"
    install -m644 packaging/lactea.desktop packaging/lactea.svg "$dir/"
    tar -C dist -czf "dist/$name.tar.gz" "$name"
    ;;
  macos)
    need "$rel/lactea" "$rel/liblactea_capi.dylib" "$rel/liblactea_capi.a"
    mkdir -p "$dir/lib"
    cp "$rel/lactea" "$dir/"
    cp "$rel/liblactea_capi.dylib" "$rel/liblactea_capi.a" "$dir/lib/"
    tar -C dist -czf "dist/$name.tar.gz" "$name"
    ;;
  windows)
    need "$rel/lactea.exe" "$rel/lactea_capi.dll"
    cp "$rel/lactea.exe" "$rel/lactea_capi.dll" "$dir/"
    # import library: `lactea_capi.dll.lib` (MSVC) or `liblactea_capi.dll.a` (GNU), plus the static lib
    for f in "$rel/lactea_capi.dll.lib" "$rel/lactea_capi.lib" "$rel/liblactea_capi.dll.a"; do
      if [ -e "$f" ]; then cp "$f" "$dir/"; fi
    done
    # Git Bash on the Windows runners has no `zip`; 7-Zip ships with them.
    (cd dist && 7z a -tzip -bd "$name.zip" "$name" >/dev/null)
    ;;
esac
ls -l dist/"$name".*
echo "packaged dist/$name"
