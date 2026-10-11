#!/usr/bin/env bash
# Proof that projectm_touch / _touch_drag / _touch_destroy / _touch_destroy_all draw nothing in the
# pinned projectM: the same preset rendered with and without `--touch` is byte-identical (every PNG,
# vars and oracle.json). Lactea mirrors this (Engine::touch*, lactea_touch*; test
# `touch_is_projectms_no_op_and_debug_images_cover_every_stage`).
#   oracle/check_touch.sh [PRESET...]   (default: two starter presets)
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(dirname "$HERE")
BIN=${REFRENDER:-$HERE/build/refrender}
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
TOUCH='5:touch,0.5,0.5,1,1;6:drag,0.6,0.4,1;7:touch,0.2,0.8,1,8;20:destroy,0.2,0.8;30:destroy_all'
if [ $# -eq 0 ]; then
  set -- "$(sed -n '2p' "$HERE/starter_set.txt" | cut -f1)" "$(sed -n '3p' "$HERE/starter_set.txt" | cut -f1)"
fi
for p in "$@"; do
  for v in plain touched; do
    extra=()
    [ "$v" = touched ] && extra=(--touch "$TOUCH")
    "$BIN" --preset "$p" --audio "$ROOT/fixtures/audio/synth128.wav" --out "$TMP/$v" --size 320x180 \
      --frames 40 --stages final,main,warped --vars --seed 1 "${extra[@]}"
  done
  diff -r "$TMP/plain" "$TMP/touched" && echo "ok: $(basename "$p"): touch calls change nothing"
  rm -rf "$TMP/plain" "$TMP/touched"
done
