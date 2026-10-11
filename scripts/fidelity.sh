#!/usr/bin/env bash
# Fidelity loop over an oracle set (docs/fidelity.md).
#
#   scripts/fidelity.sh SET [FILTER]
#
# For every preset dump in out/ref/<SET>/ (produced by oracle/run_corpus.sh):
#   1. free run     fdrender --inject-noise <ref>   -> out/fd/<SET>/<stem>        (frames n depend on n-1)
#   2. teacher run  fdrender --teacher <ref>        -> out/fd-step/<SET>/<stem>   (frame n from the oracle's n-1)
# then `fdcompare corpus` of both against out/ref/<SET> -> out/report/<SET>/{free,step}/ and a
# compact summary. Run parameters (preset, audio, size, mesh, fps, frames, seed, texel offset)
# come from each reference dump's meta.json, so both sides are driven identically.
#
# Env: FILTER (2nd arg, substring of the preset stem), JOBS (parallel presets, default 3),
#      FD_TARGET (cargo target dir, default target-engine), SKIP_BUILD=1, COMPAT=projectm|md2,
#      GATE (default psnr=40,ssim=0.98), TEXTURE_DIR (default: oracle/build/texpack).
set -euo pipefail

SET="${1:?usage: scripts/fidelity.sh SET [FILTER]}"
FILTER="${2:-${FILTER:-}}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

JOBS="${JOBS:-3}"
FD_TARGET="${FD_TARGET:-target-engine}"
COMPAT="${COMPAT:-projectm}"
GATE="${GATE:-psnr=40,ssim=0.98}"
REF="out/ref/$SET"
FD="out/fd/$SET"
STEP="out/fd-step/$SET"
REPORT="out/report/$SET"

[ -d "$REF" ] || { echo "no oracle set at $REF (run oracle/run_corpus.sh $SET ...)" >&2; exit 2; }

if [ "${SKIP_BUILD:-0}" != 1 ]; then
  CARGO_TARGET_DIR="$FD_TARGET" cargo build --release -q -p lactea-cli -p lactea-compare
fi
FDRENDER="$ROOT/$FD_TARGET/release/fdrender"
FDCOMPARE="$ROOT/$FD_TARGET/release/fdcompare"
[ -x "$FDRENDER" ] && [ -x "$FDCOMPARE" ] || { echo "missing binaries in $FD_TARGET/release" >&2; exit 2; }

# one render job: <stem> <mode: free|step>
render_one() {
  local stem="$1" mode="$2" ref="$REF/$1" out args
  # meta.json -> argument list (python3 is only used for JSON parsing)
  args=$(python3 - "$ref/meta.json" "$ROOT" <<'PY'
import json, os, sys, shlex
m = json.load(open(sys.argv[1]))
c = m["config"]
aud = m["audio"] if os.path.exists(m["audio"]) else os.path.join(sys.argv[2], "fixtures", "audio", os.path.basename(m["audio"]))
a = ["--preset", m["preset"], "--audio", aud,
     "--size", f'{c["width"]}x{c["height"]}', "--mesh", f'{c["mesh"][0]}x{c["mesh"][1]}',
     "--fps", str(c["fps"]), "--frames", str(c["frames"]), "--seed", str(c["seed"]),
     "--texel-offset", f'{c["texel_offset"][0]},{c["texel_offset"][1]}',
     "--stages", ",".join(m["stages"]) or "none"]
if m.get("has_vars"): a.append("--vars")
print(" ".join(shlex.quote(x) for x in a))
PY
)
  if [ "$mode" = free ]; then
    out="$FD/$stem"; args="$args --inject-noise $(printf %q "$ref")"
  else
    out="$STEP/$stem"; args="$args --teacher $(printf %q "$ref")"
  fi
  [ -n "${TEXTURE_DIR:-}" ] && args="$args --texture-dir $(printf %q "$TEXTURE_DIR")"
  rm -rf "$out"; mkdir -p "$(dirname "$out")"
  set +e
  eval "\"$FDRENDER\" $args --compat $COMPAT --out $(printf %q "$out")" >"$out.log" 2>&1
  local rc=$?
  set -e
  # 0 ok, 3 shader fallback (still rendered); anything else is a failure
  if [ $rc -ne 0 ] && [ $rc -ne 3 ]; then
    echo "FAIL($rc) $mode $stem: $(tail -n 2 "$out.log" | tr '\n' ' ')"
  fi
  return 0
}
export -f render_one
export ROOT REF FD STEP FDRENDER COMPAT

STEMS=()
for d in "$REF"/*/; do
  [ -f "${d}meta.json" ] && STEMS+=("$(basename "$d")")
done
mapfile -t STEMS < <(printf '%s\n' "${STEMS[@]}" | sort)
if [ -n "$FILTER" ]; then
  mapfile -t STEMS < <(printf '%s\n' "${STEMS[@]}" | grep -F -- "$FILTER" || true)
fi
[ ${#STEMS[@]} -gt 0 ] || { echo "no presets in $REF${FILTER:+ matching '$FILTER'}" >&2; exit 2; }
echo "fidelity: set=$SET presets=${#STEMS[@]} jobs=$JOBS compat=$COMPAT"

mkdir -p "$FD" "$STEP" "$REPORT"
t0=$(date +%s)
for mode in free step; do
  printf '%s\n' "${STEMS[@]}" | xargs -d '\n' -P "$JOBS" -I{} bash -c 'render_one "$1" "$2"' _ {} "$mode"
done
echo "rendered in $(( $(date +%s) - t0 )) s"

# fdcompare corpus: compare only the presets we rendered (--filter takes one substring, so with
# a FILTER pass it through; otherwise the whole set).
for mode in free step; do
  cand="$FD"; [ $mode = step ] && cand="$STEP"
  mkdir -p "$REPORT/$mode"
  args=(corpus "$REF" "$cand" --out "$REPORT/$mode" --gate "$GATE")
  [ -n "$FILTER" ] && args+=(--filter "$FILTER")
  [ -f "$REPORT/$mode/corpus.json" ] && cp "$REPORT/$mode/corpus.json" "$REPORT/$mode/corpus.prev.json" 2>/dev/null || true
  set +e
  "$FDCOMPARE" "${args[@]}" >"$REPORT/$mode/stdout.txt" 2>"$REPORT/$mode/stderr.txt"
  rc=$?
  set -e
  echo "fdcompare $mode: exit $rc -> $REPORT/$mode/index.html"
done

python3 "$ROOT/scripts/fidelity_summary.py" "$REPORT" || true
