#!/usr/bin/env python3
"""Geometry-oriented comparison of main-wave probe renders (teacher/step mode), independent of 1-px stroke
rasterization differences: for every frame >= 1 reports the PSNR, the symmetric mean nearest-pixel distance of the
lit pixel sets (px; ~0.5 = same curve, different rasterization) and the ratio of summed intensity (alpha/colour/coverage).

  scripts/mwcmp.py SET STEM... [--stage main] [--out out/fd-step]
"""
import argparse, os
import numpy as np
from PIL import Image
from scipy import ndimage

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
ap = argparse.ArgumentParser()
ap.add_argument("set"); ap.add_argument("stems", nargs="+")
ap.add_argument("--stage", default="main"); ap.add_argument("--out", default="out/fd-step")
ap.add_argument("--thr", type=int, default=12)
a = ap.parse_args()
ref = os.path.join(ROOT, "out/ref", a.set)
for stem in sorted(d for d in os.listdir(ref) if any(s in d for s in a.stems) and os.path.isdir(os.path.join(ref, d))):
    ps, ds, rs = [], [], []
    n = 1
    while True:
        pr = os.path.join(ref, stem, a.stage, f"{n:05d}.png"); pc = os.path.join(ROOT, a.out, a.set, stem, a.stage, f"{n:05d}.png")
        if not (os.path.exists(pr) and os.path.exists(pc)): break
        A = np.array(Image.open(pr).convert("RGB")).astype(np.float64); B = np.array(Image.open(pc).convert("RGB")).astype(np.float64)
        mse = ((A - B) ** 2).mean(); ps.append(99 if mse == 0 else 10 * np.log10(255 ** 2 / mse))
        ma = A.max(2) > a.thr; mb = B.max(2) > a.thr
        if ma.any() and mb.any():
            da = ndimage.distance_transform_edt(~ma); db = ndimage.distance_transform_edt(~mb)
            ds.append((db[ma].mean() + da[mb].mean()) / 2)
        elif ma.any() != mb.any():
            ds.append(999.0)
        rs.append((B.sum() + 1) / (A.sum() + 1))
        n += 1
    f = lambda x: f"{np.median(x):.2f}" if x else "-"
    print(f"{stem:16s} psnr_med {np.median(ps):5.1f}  dist_med {f(ds)} dist_max {max(ds) if ds else 0:7.2f}  int_ratio_med {np.median(rs):.3f} (min {min(rs):.2f} max {max(rs):.2f})")
