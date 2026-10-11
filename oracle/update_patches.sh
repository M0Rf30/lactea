#!/usr/bin/env bash
# Developer helper: regenerate oracle/patches/*.patch from the working tree in third_party/projectm
# (i.e. after editing the clone directly). Not needed for normal builds.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SRC=${PROJECTM_SRC:-$HERE/../third_party/projectm}
mkdir -p "$HERE/patches"
git -C "$SRC" add -N src >/dev/null
git -C "$SRC" diff --no-color -- src > "$HERE/patches/0001-fdoracle-determinism-dumps-shader-errors.patch"
git -C "$SRC/vendor/projectm-eval" diff --no-color > "$HERE/patches/0002-eval-seed-and-variable-enumeration.patch"
wc -l "$HERE"/patches/*.patch
