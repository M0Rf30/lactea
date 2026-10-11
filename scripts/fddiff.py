#!/usr/bin/env python3
"""Signed amplified diff of one stage/frame: grey = equal, red = ours brighter, blue = ours darker (x GAIN).

  scripts/fddiff.py SET STEM [--mode step|free|stepc] [--stage warped] [--frame 60] [--gain 60] [--out out/fddiff.png]

Prints the fraction of differing pixels and a histogram of |diff|; writes ref | ours | signed diff side by side.
"""
import argparse, os, sys
import numpy as np
from PIL import Image

ap = argparse.ArgumentParser()
ap.add_argument("set")
ap.add_argument("stem")
ap.add_argument("--mode", default="step", choices=["step", "free", "stepc"])
ap.add_argument("--stage", default="warped")
ap.add_argument("--frame", type=int, default=60)
ap.add_argument("--gain", type=int, default=60)
ap.add_argument("--out", default="out/fddiff.png")
a = ap.parse_args()
root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "out")
refdir = os.path.join(root, "ref", a.set)
names = sorted(d for d in os.listdir(refdir) if os.path.isdir(os.path.join(refdir, d)) and a.stem in d)
if not names:
    sys.exit("no match")
stem = names[0]
sub = {"free": "fd", "step": "fd-step", "stepc": "fd-stepc"}[a.mode]


def ld(d):
    return np.array(Image.open(os.path.join(d, a.stage, f"{a.frame:05d}.png")).convert("RGB")).astype(int)


r, c = ld(os.path.join(refdir, stem)), ld(os.path.join(root, sub, a.set, stem))
d = c - r
v = np.full(r.shape, 128, np.uint8)
v[..., 0] = np.clip(128 + d.max(-1) * a.gain, 0, 255)
v[..., 2] = np.clip(128 + d.min(-1) * a.gain, 0, 255)
sep = np.full((r.shape[0], 4, 3), 255, np.uint8)
Image.fromarray(np.concatenate([r.astype(np.uint8), sep, c.astype(np.uint8), sep, v], axis=1)).save(a.out)
ad = np.abs(d)
print(stem, a.stage, a.frame, "differing px: %.2f%%" % (100 * (ad.max(-1) > 0).mean()),
      "hist", np.bincount(ad.ravel(), minlength=6)[:6].tolist(), "max", ad.max())
