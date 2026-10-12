#!/usr/bin/env bash
# Render a list of presets with oracle/build/refrender into out/ref/<set>/<preset-stem>/.
#
#   oracle/run_corpus.sh <set> <list> [-j JOBS] [-n FRAMES] [-a AUDIO.wav] [-t TIMEOUT_S] [--resume] [--share-textures]
#                        [-- extra refrender args]
#
# <list>: text file, one preset per line; only the first TAB-separated column is used; blank lines and
#         lines starting with '#' are ignored. Relative paths are resolved against the repo root.
# Defaults: JOBS=nproc/2, FRAMES=120, AUDIO=fixtures/audio/synth128.wav,
#           refrender args "--stages all --vars --textures" (override by passing your own after --).
# <preset-stem> = basename without .milk; if two list entries share a stem, a short path hash is appended
#           ("name~1a2b3c") so every output directory is unique and stable.
# Output: out/ref/<set>/<stem>/{meta.json,oracle.json,final/,main/,...}, <stem>.log next to it, and
#         out/ref/<set>/summary.tsv (stem, exit code, shader_errors, frames, seconds, preset).
# -t: per-preset wall-clock limit in seconds (default 900; a hang is recorded as exit 124).
# --share-textures: the noise textures a dump carries (`--textures`) depend only on the seed, so they are
#         byte-identical for every preset; after the run all copies are replaced by hard links to
#         out/ref/<set>/_textures (~0.9 MB per preset at the default sizes; `cmp` is checked first).
# Exit codes of refrender: 0 ok, 2 preset load failure, 3 shader errors (frames are still rendered).
set -uo pipefail
export LC_ALL=C

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
BIN=${REFRENDER:-$HERE/build/refrender}

[ $# -ge 2 ] || { sed -n '2,15p' "${BASH_SOURCE[0]}"; exit 1; }
SET=$1; LIST=$2; shift 2
JOBS=$(( $(nproc) / 2 )); [ "$JOBS" -ge 1 ] || JOBS=1
FRAMES=120
AUDIO=$ROOT/fixtures/audio/synth128.wav
RESUME=0
SHARE=0
TMO=900
EXTRA=()
while [ $# -gt 0 ]; do
  case "$1" in
    -j) JOBS=$2; shift 2 ;;
    -n) FRAMES=$2; shift 2 ;;
    -a) AUDIO=$2; shift 2 ;;
    -t) TMO=$2; shift 2 ;;
    --resume) RESUME=1; shift ;;
    --share-textures) SHARE=1; shift ;;
    --) shift; EXTRA=("$@"); break ;;
    *) echo "unknown option $1" >&2; exit 1 ;;
  esac
done
if [ ${#EXTRA[@]} -eq 0 ]; then
  EXTRA=(--stages all --vars --textures)
  # user textures (clouds, worms, ...) from the optional projectM texture pack cloned by build.sh
  [ -d "$HERE/build/texpack/textures" ] && EXTRA+=(--texture-dir "$HERE/build/texpack/textures")
fi
[ -x "$BIN" ] || { echo "missing $BIN (run oracle/build.sh)" >&2; exit 1; }
[ -f "$AUDIO" ] || { echo "missing audio $AUDIO (run fixtures/audio/gen_audio.py)" >&2; exit 1; }

OUT=$ROOT/out/ref/$SET
mkdir -p "$OUT"

# Build "preset<TAB>stem" job list.
JOBLIST=$(mktemp)
trap 'rm -f "$JOBLIST"' EXIT
awk -F'\t' '!/^[[:space:]]*(#|$)/ {print $1}' "$LIST" | while IFS= read -r p; do
  case "$p" in /*) ;; *) p=$ROOT/$p ;; esac
  stem=$(basename "$p" .milk)
  printf '%s\t%s\n' "$p" "$stem"
done | awk -F'\t' '{c[$2]++; p[NR]=$1; s[NR]=$2} END {
  for (i = 1; i <= NR; i++) {
    stem = s[i]
    if (c[stem] > 1) { cmd = "printf %s \"" p[i] "\" | sha1sum | cut -c1-6"; cmd | getline h; close(cmd); stem = stem "~" h }
    printf "%s\t%s\n", p[i], stem
  }}' > "$JOBLIST"

render_one() {
  local preset=$1 stem=$2 dir=$OUT/$2
  if [ "$RESUME" = 1 ] && [ -f "$dir/oracle.json" ] && [ -f "$dir/.complete" ]; then
    return 0
  fi
  rm -rf "$dir"
  local t0 t1 rc
  t0=$(date +%s.%N)
  timeout "$TMO" "$BIN" --preset "$preset" --audio "$AUDIO" --out "$dir" --frames "$FRAMES" "${EXTRA[@]}" > "$OUT/$stem.log" 2>&1
  rc=$?
  t1=$(date +%s.%N)
  [ $rc -eq 0 ] || [ $rc -eq 3 ] && touch "$dir/.complete"
  printf '%s\t%s\t%s\t%s\n' "$stem" "$rc" "$(echo "$t1 - $t0" | bc -l | xargs printf '%.1f')" "$preset" > "$OUT/.result.$stem"
  # 127 = the harness did not start (missing binary or shared library): every other preset would
  # fail the same way. xargs stops launching jobs when a worker exits 255.
  if [ "$rc" -eq 127 ]; then
    echo "[run_corpus] refrender exited 127 (loader error?), aborting; see $OUT/$stem.log" >&2
    return 255
  fi
}
export -f render_one
export OUT BIN AUDIO FRAMES RESUME TMO
export EXTRA_STR="${EXTRA[*]}"

# xargs cannot export arrays; re-split EXTRA inside the worker.
render_wrapper() {
  IFS=' ' read -r -a EXTRA <<< "$EXTRA_STR"
  render_one "$@"
}
export -f render_wrapper

tr '\t' '\n' < "$JOBLIST" | xargs -d '\n' -n 2 -P "$JOBS" bash -c 'render_wrapper "$0" "$1"'
if [ "${PIPESTATUS[1]}" -ne 0 ]; then
  echo "[run_corpus] aborted: a render job failed to start" >&2
  exit 1
fi

printf 'stem\texit\tshader_errors\tframes\tseconds\tpreset\n' > "$OUT/summary.tsv"
while IFS=$'\t' read -r p stem; do
  res=$(cat "$OUT/.result.$stem" 2>/dev/null || true)
  rc=$(echo "$res" | cut -f2); sec=$(echo "$res" | cut -f3)
  errs=NA; frames=NA
  if [ -f "$OUT/$stem/oracle.json" ]; then
    errs=$(grep -o '"shader_errors": [0-9]*' "$OUT/$stem/oracle.json" | grep -o '[0-9]*$')
    frames=$(grep -o '"frames_rendered": [0-9]*' "$OUT/$stem/oracle.json" | grep -o '[0-9]*$')
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$stem" "${rc:-?}" "$errs" "$frames" "${sec:-?}" "$p" >> "$OUT/summary.tsv"
done < "$JOBLIST"

if [ "$SHARE" = 1 ]; then
  # Byte-identical noise textures -> hard links into $OUT/_textures (cmp-checked; anything that differs stays).
  SH=$OUT/_textures
  linked=0; kept=0
  for d in "$OUT"/*/textures; do
    [ -d "$d" ] || continue
    for f in "$d"/*; do
      [ -f "$f" ] || continue
      b=$(basename "$f")
      if [ ! -f "$SH/$b" ]; then mkdir -p "$SH"; cp -p "$f" "$SH/$b"; fi
      if [ "$(stat -c %i "$f")" = "$(stat -c %i "$SH/$b")" ]; then continue; fi
      if cmp -s "$f" "$SH/$b"; then ln -f "$SH/$b" "$f"; linked=$((linked + 1)); else kept=$((kept + 1)); fi
    done
  done
  echo "[run_corpus] textures: $linked hard-linked to $SH, $kept differing kept"
fi
echo "[run_corpus] $OUT/summary.tsv"
awk -F'\t' 'NR > 1 {c[$2]++} END {printf "[run_corpus] exit codes:"; for (k in c) printf " %s=%d", k, c[k]; printf "\n"}' "$OUT/summary.tsv"
if [ "$(wc -l < "$OUT/summary.tsv")" -le 60 ]; then column -t -s $'\t' "$OUT/summary.tsv"; fi
