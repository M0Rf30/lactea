#!/usr/bin/env python3
"""Per-preset, per-stage PSNR comparison of two `fdcompare corpus` reports (before/after).

  scripts/fdregress.py out/report-before/<SET> out/report/<SET> [--modes step,free] [--thr 0.5] [--key psnr_min|psnr_median]

Lists every (mode, preset, stage) whose PSNR dropped by more than --thr dB (inf/99 = identical are capped at 100 dB so a
bit-exact stage that becomes merely very good is not reported unless it falls below 80 dB) and the improvements, plus counts.
"""
import argparse
import json
import os

CAP = 100.0


def num(x):
    if isinstance(x, str):
        return CAP if x.lower() in ("inf", "+inf") else float("nan")
    return min(float(x), CAP)


def load(root, mode):
    p = os.path.join(root, mode, "corpus.json")
    if not os.path.exists(p):
        return {}
    return {e["preset"]: {s["stage"]: s for s in e["stages"]} for e in json.load(open(p))["entries"]}


ap = argparse.ArgumentParser()
ap.add_argument("before")
ap.add_argument("after")
ap.add_argument("--modes", default="step,free")
ap.add_argument("--thr", type=float, default=0.5)
ap.add_argument("--key", default="psnr_min")
ap.add_argument("--improvements", action="store_true")
a = ap.parse_args()
for mode in a.modes.split(","):
    b, c = load(a.before, mode), load(a.after, mode)
    worse = better = same = 0
    for k in sorted(set(b) & set(c)):
        for st in sorted(set(b[k]) & set(c[k])):
            x, y = num(b[k][st][a.key]), num(c[k][st][a.key])
            if x != x or y != y:
                continue
            d = y - x
            if d < -a.thr and not (x >= 80 and y >= 80):
                worse += 1
                print("  %-4s WORSE %-58s %-6s %6.1f -> %6.1f (%+.1f)" % (mode, k[:58], st, x, y, d))
            elif d > a.thr:
                better += 1
                if a.improvements:
                    print("  %-4s better %-58s %-6s %6.1f -> %6.1f (%+.1f)" % (mode, k[:58], st, x, y, d))
            else:
                same += 1
    print("%s: %d stage-metrics worse by > %.1f dB, %d better, %d within tolerance (%s)" % (mode, worse, a.thr, better, same, a.key))
