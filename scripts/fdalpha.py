#!/usr/bin/env python3
"""Alpha-channel agreement of a stage (default `main`) between a run and the oracle reference.

  scripts/fdalpha.py SET STEM... [--mode step|free] [--stage main] [--first 1]

Prints per preset: % of pixels with identical alpha, max |diff| and median over frames (>= first).
"""
import argparse, os
import numpy as np
from PIL import Image

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
ap = argparse.ArgumentParser()
ap.add_argument("set")
ap.add_argument("stems", nargs="+")
ap.add_argument("--mode", default="step")
ap.add_argument("--stage", default="main")
ap.add_argument("--first", type=int, default=1)
a = ap.parse_args()
ref = os.path.join(ROOT, "out", "ref", a.set)
run = os.path.join(ROOT, "out", "fd" if a.mode == "free" else "fd-step", a.set)
names = sorted(d for d in os.listdir(ref) if os.path.isfile(os.path.join(ref, d, "meta.json")))
for s in a.stems:
    for d in [n for n in names if s in n]:
        rd, od = os.path.join(ref, d, a.stage), os.path.join(run, d, a.stage)
        eq, mx = [], []
        for f in sorted(os.listdir(rd)):
            n = int(f.split(".")[0])
            if n < a.first or not os.path.exists(os.path.join(od, f)):
                continue
            x = np.array(Image.open(os.path.join(rd, f)).convert("RGBA"))[..., 3].astype(int)
            y = np.array(Image.open(os.path.join(od, f)).convert("RGBA"))[..., 3].astype(int)
            df = np.abs(x - y)
            eq.append((df == 0).mean() * 100)
            mx.append(df.max())
        print(f"{d[:46]:46s} {a.mode} {a.stage} alpha identical {np.median(eq):6.2f}% (min {np.min(eq):6.2f}%)  max|d| median {np.median(mx):.0f} worst {np.max(mx)}")
