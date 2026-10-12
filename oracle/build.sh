#!/usr/bin/env bash
# Build the instrumented projectM oracle + the refrender harness.
#
#   oracle/build.sh            # idempotent; incremental when nothing changed
#
# Env overrides (all optional):
#   PROJECTM_SRC   projectM checkout   (default <repo>/third_party/projectm; cloned if missing)
#   ORACLE_BUILD   output dir          (default <repo>/oracle/build)
#   JOBS           parallel jobs       (default nproc)
#
# Steps: (1) clone projectM master @ oracle/PROJECTM_COMMIT incl. submodules if absent,
# (2) reset the tree and apply oracle/patches/*.patch (files named *-eval-*.patch apply inside
# vendor/projectm-eval), (3) CMake/Ninja RelWithDebInfo build + install into $ORACLE_BUILD/install,
# (4) compile oracle/refrender.c into $ORACLE_BUILD/refrender.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
SRC=${PROJECTM_SRC:-$ROOT/third_party/projectm}
BUILD=${ORACLE_BUILD:-$HERE/build}
JOBS=${JOBS:-$(nproc)}
SHA=$(tr -d '[:space:]' < "$HERE/PROJECTM_COMMIT")
URL=${PROJECTM_URL:-https://github.com/projectM-visualizer/projectm.git}

# ---- 1. source checkout --------------------------------------------------------------------
if [ ! -f "$SRC/CMakeLists.txt" ]; then
  echo "[oracle] cloning projectM @ $SHA into $SRC"
  mkdir -p "$SRC"
  git -C "$SRC" init -q
  git -C "$SRC" remote add origin "$URL" 2>/dev/null || true
  git -C "$SRC" fetch -q --depth 1 origin "$SHA"
  git -C "$SRC" checkout -q FETCH_HEAD
  git -C "$SRC" submodule update --init --depth 1 --recursive
fi
HEAD_SHA=$(git -C "$SRC" rev-parse HEAD)
if [ "$HEAD_SHA" != "$SHA" ]; then
  echo "[oracle] WARNING: $SRC is at $HEAD_SHA, oracle/PROJECTM_COMMIT says $SHA" >&2
fi

# ---- 2. patches (skipped when the stamp matches) -------------------------------------------
mkdir -p "$BUILD"
STAMP_NEW=$( (echo "$HEAD_SHA"; sha256sum "$HERE"/patches/*.patch) | sha256sum | cut -d' ' -f1)
STAMP_OLD=$(cat "$BUILD/.patch-stamp" 2>/dev/null || true)
if [ "$STAMP_NEW" != "$STAMP_OLD" ]; then
  echo "[oracle] (re)applying patches"
  git -C "$SRC" reset -q
  git -C "$SRC" checkout -q -- .
  git -C "$SRC" clean -fdq -- src
  git -C "$SRC/vendor/projectm-eval" checkout -q -- .
  git -C "$SRC/vendor/projectm-eval" clean -fdq
  for p in "$HERE"/patches/*.patch; do
    case "$(basename "$p")" in
      *-eval-*) git -C "$SRC/vendor/projectm-eval" apply --whitespace=nowarn "$p" ;;
      *)        git -C "$SRC" apply --whitespace=nowarn "$p" ;;
    esac
    echo "[oracle]   applied $(basename "$p")"
  done
  echo "$STAMP_NEW" > "$BUILD/.patch-stamp"
fi

# ---- 3. projectM ---------------------------------------------------------------------------
# -ffp-contract=off / no -march: bit-reproducible float math. projectm-eval ENABLE_FAST_MATH=OFF
# keeps NaN/Inf semantics (-ffinite-math-only would optimise isnan() away).
cmake -S "$SRC" -B "$BUILD/projectm" -G Ninja \
  -DCMAKE_BUILD_TYPE=RelWithDebInfo \
  -DCMAKE_INSTALL_PREFIX="$BUILD/install" \
  -DCMAKE_C_FLAGS="-ffp-contract=off" -DCMAKE_CXX_FLAGS="-ffp-contract=off" \
  -DBUILD_SHARED_LIBS=ON -DENABLE_INSTALL=ON \
  -DENABLE_PLAYLIST=OFF -DENABLE_SDL_UI=OFF -DBUILD_TESTING=OFF -DBUILD_DOCS=OFF \
  -DENABLE_SYSTEM_PROJECTM_EVAL=OFF -DENABLE_FAST_MATH=OFF \
  -DENABLE_GLES=OFF -DENABLE_DEBUG_POSTFIX=OFF >/dev/null
cmake --build "$BUILD/projectm" -j "$JOBS"
cmake --install "$BUILD/projectm" >/dev/null

# ---- 4. harness ----------------------------------------------------------------------------
# The RUNPATH is $ORIGIN-relative: a logical $BUILD (e.g. a symlink into a worktree) must not be
# baked into the binary, or it breaks as soon as that worktree is removed.
LIBDIR=$(dirname "$(ls "$BUILD"/install/lib*/libprojectM-4.so | head -n1)")
RELLIB=${LIBDIR#"$BUILD"/}
cc -O2 -g -std=gnu11 -Wall -Wextra -Wno-unused-parameter \
  -DPROJECTM_SHA="\"$SHA\"" -I"$BUILD/install/include" \
  "$HERE/refrender.c" -o "$BUILD/refrender" \
  -L"$LIBDIR" -lprojectM-4 -Wl,-rpath,"\$ORIGIN/$RELLIB" \
  $(pkg-config --cflags --libs libpng) -lEGL -lGL -lm
# ---- 5. optional texture pack (user textures such as clouds/worms); best effort -------------------
if [ ! -d "$BUILD/texpack/textures" ]; then
  git clone -q --depth 1 https://github.com/projectM-visualizer/presets-milkdrop-texture-pack.git "$BUILD/texpack" \
    || echo "[oracle] WARNING: texture pack not cloned; presets using user textures will see a placeholder" >&2
fi
echo "[oracle] OK: $BUILD/refrender  (projectM $HEAD_SHA)"
