#!/bin/sh
# Build the wasm module into ./pkg and (optionally) serve this directory.
#
#   examples/web/build.sh           # build
#   examples/web/build.sh serve     # build, then http://localhost:8000/ (WebGPU needs a secure context)
#
# Needs: rustup target add wasm32-unknown-unknown, and a wasm-bindgen CLI matching the crate
# version in Cargo.toml (cargo install wasm-bindgen-cli --version 0.2.128, or wasm-pack).
# Resource limits (AGENTS.md): CARGO_BUILD_JOBS defaults to 2.
set -eu
cd "$(dirname "$0")"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
cargo build --release --target wasm32-unknown-unknown
wasm_in="${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release/lactea_web_demo.wasm"
wasm-bindgen --target web --no-typescript --out-dir pkg "$wasm_in"
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz --all-features -o pkg/lactea_web_demo_bg.wasm pkg/lactea_web_demo_bg.wasm
fi
if [ "${1:-}" = serve ]; then
  exec python3 -m http.server 8000
fi
