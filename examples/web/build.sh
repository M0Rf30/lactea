#!/bin/sh
# Build the browser host into ./dist (the deployable site) and optionally serve it.
#
#   examples/web/build.sh                  # build
#   examples/web/build.sh serve            # build, then http://localhost:8000/ (WebGPU needs a secure context)
#   PRESETS=dir examples/web/build.sh      # also deploy dir's .milk/.milk2 files and images (dist/presets)
#
# Without PRESETS the page plays its two built-in presets; visitors can drop their own files either way.
# Only deploy presets you may redistribute.
#
# Needs: rustup target add wasm32-unknown-unknown, and a wasm-bindgen CLI matching the crate
# version in Cargo.toml (cargo install wasm-bindgen-cli --version 0.2.128, or wasm-pack).
# wasm-opt (binaryen) is used when installed. Resource limits (AGENTS.md): CARGO_BUILD_JOBS defaults to 2.
set -eu
cd "$(dirname "$0")"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
cargo build --release --target wasm32-unknown-unknown
wasm_in="${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release/lactea_web_demo.wasm"
rm -rf dist
mkdir -p dist
wasm-bindgen --target web --no-typescript --out-dir dist/pkg "$wasm_in"
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -O3 --all-features -o dist/pkg/lactea_web_demo_bg.wasm dist/pkg/lactea_web_demo_bg.wasm
fi
cp index.html main.js dist/

if [ -n "${PRESETS:-}" ]; then
  mkdir -p dist/presets
  find "$PRESETS" -maxdepth 1 -type f \( -iname '*.milk' -o -iname '*.milk2' -o -iname '*.png' \
    -o -iname '*.jpg' -o -iname '*.jpeg' -o -iname '*.tga' -o -iname '*.bmp' \) -exec cp {} dist/presets/ \;
  # dist/presets/index.json: {"presets": [...], "textures": [...]}, file names sorted
  python3 - dist/presets <<'EOF'
import json, os, sys
d = sys.argv[1]
names = sorted(n for n in os.listdir(d) if n != "index.json")
milk = [n for n in names if n.lower().endswith((".milk", ".milk2"))]
tex = [n for n in names if n not in milk]
with open(os.path.join(d, "index.json"), "w") as f:
    json.dump({"presets": milk, "textures": tex}, f, indent=1)
print(f"{len(milk)} presets, {len(tex)} textures")
EOF
fi
du -h dist/pkg/lactea_web_demo_bg.wasm

if [ "${1:-}" = serve ]; then
  exec python3 -m http.server 8000 --directory dist
fi
