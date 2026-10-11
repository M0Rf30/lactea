#!/usr/bin/env python3
"""Exactness statistics of oracle vs lactea stage images (PSNR hides single-code-value errors).

  scripts/fdexact.py SET STEM [--mode step|free|stepc] [--stages warped,main,final] [--frames 0-119] [--table]
                     [--cand-root DIR]

For every matching preset and stage prints, over the selected frames: the number of bit-identical frames, the median and
max of the fraction of differing pixels (any channel), the max-abs difference, the number of frames with max-abs > 1 and
the first frame whose max-abs exceeds 1 / 8.  `--table` prints one row per frame (differing %, max, #px |d|>1, #px |d|>8,
signed mean per channel).
"""
import argparse, os, sys
import numpy as np
from PIL import Image

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
ap = argparse.ArgumentParser()
ap.add_argument("set")
ap.add_argument("stems", nargs="+")
ap.add_argument("--mode", default="step", choices=["step", "free", "stepc"])
ap.add_argument("--stages", default="warped,main,final")
ap.add_argument("--frames", default="0-")
ap.add_argument("--table", action="store_true")
ap.add_argument("--cand-root")
a = ap.parse_args()

refroot = os.path.join(ROOT, "out", "ref", a.set)
sub = {"free": "fd", "step": "fd-step", "stepc": "fd-stepc"}[a.mode]
candroot = a.cand_root or os.path.join(ROOT, "out", sub, a.set)
names = sorted(d for d in os.listdir(refroot) if os.path.isfile(os.path.join(refroot, d, "meta.json")))
sel = []
for s in a.stems:
    sel += [d for d in names if s in d and d not in sel]
lo, _, hi = a.frames.partition("-")
lo = int(lo or 0)
hi = int(hi) if hi else 10**9


def ld(p):
    return np.array(Image.open(p).convert("RGB")).astype(np.int16)


for stem in sel:
    for st in a.stages.split(","):
        rows = []
        n = lo
        while n <= hi:
            pr = os.path.join(refroot, stem, st, f"{n:05d}.png")
            pc = os.path.join(candroot, stem, st, f"{n:05d}.png")
            if not (os.path.exists(pr) and os.path.exists(pc)):
                break
            d = ld(pc) - ld(pr)
            ad = np.abs(d)
            m = ad.max(-1)
            rows.append((n, 100.0 * (m > 0).mean(), int(m.max()), int((m > 1).sum()), int((m > 8).sum()),
                         d.reshape(-1, 3).mean(0)))
            n += 1
        if not rows:
            print(f"{stem[:50]}  {st}: no frames")
            continue
        if a.table:
            print(f"== {stem[:60]} [{a.mode}] {st}")
            for r in rows:
                print(f"  f{r[0]:3d}  diff {r[1]:7.3f}%  max {r[2]:3d}  >1:{r[3]:6d}  >8:{r[4]:6d}  mean {r[5][0]:+.3f} {r[5][1]:+.3f} {r[5][2]:+.3f}")
        pct = np.array([r[1] for r in rows])
        mx = np.array([r[2] for r in rows])
        ident = int((mx == 0).sum())
        f1 = next((r[0] for r in rows if r[2] > 1), None)
        f8 = next((r[0] for r in rows if r[2] > 8), None)
        print(f"{stem[:46]:46s} {a.mode:4s} {st:6s} frames {len(rows):3d} identical {ident:3d}  diff% med {np.median(pct):6.3f} max {pct.max():6.3f}"
              f"  maxabs med {int(np.median(mx)):3d} max {int(mx.max()):3d}  frames>1: {int((mx > 1).sum()):3d} first>1 {f1} first>8 {f8}")
