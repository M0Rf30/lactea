#!/usr/bin/env python3
"""Look at one stage/frame of an oracle vs lactea pair.

  scripts/fdlook.py SET STEM [--mode step|free] [--stage final] [--frame 10] [--crop x0,y0,x1,y1] [--scale 2] [--out f.png]

Writes ref | ours | |diff|x4 side by side (default out/fdlook.png) and prints PSNR, max/mean abs diff and the
bounding box of pixels differing by more than 8. STEM may be a substring of the preset directory name.
"""
import argparse, os, sys
import numpy as np
from PIL import Image

ap = argparse.ArgumentParser()
ap.add_argument("set")
ap.add_argument("stem")
ap.add_argument("--mode", default="step", choices=["step", "free"])
ap.add_argument("--stage", default="final")
ap.add_argument("--frame", type=int, default=10)
ap.add_argument("--crop")
ap.add_argument("--scale", type=int, default=1)
ap.add_argument("--out", default="out/fdlook.png")
ap.add_argument("--cand")
a = ap.parse_args()

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "out")
refdir = os.path.join(root, "ref", a.set)
names = [d for d in os.listdir(refdir) if os.path.isdir(os.path.join(refdir, d)) and a.stem in d]
if not names:
    sys.exit(f"no preset matching {a.stem!r}")
stem = sorted(names)[0]
candroot = os.path.join(root, "fd" if a.mode == "free" else "fd-step", a.set)
cand = a.cand or os.path.join(candroot, stem)


def ld(d):
    return np.array(Image.open(os.path.join(d, a.stage, f"{a.frame:05d}.png")).convert("RGB")).astype(np.int32)


r, c = ld(os.path.join(refdir, stem)), ld(cand)
if a.crop:
    x0, y0, x1, y1 = map(int, a.crop.split(","))
    r, c = r[y0:y1, x0:x1], c[y0:y1, x0:x1]
d = np.abs(r - c)
mse = (d.astype(np.float64) ** 2).mean()
psnr = 99.0 if mse == 0 else 10 * np.log10(255.0**2 / mse)
mask = d.max(-1) > 8
bbox = None
if mask.any():
    ys, xs = np.where(mask)
    bbox = (xs.min(), ys.min(), xs.max(), ys.max())
print(f"{stem} [{a.mode}] {a.stage}#{a.frame}: psnr {psnr:.2f} dB  max {d.max()}  mean {d.mean():.3f}  >8: {mask.sum()} px bbox {bbox}")
dv = np.clip(d * 4, 0, 255).astype(np.uint8)
sep = np.full((r.shape[0], 4, 3), 255, np.uint8)
img = np.concatenate([r.astype(np.uint8), sep, c.astype(np.uint8), sep, dv], axis=1)
im = Image.fromarray(img)
if a.scale != 1:
    im = im.resize((im.width * a.scale, im.height * a.scale), Image.NEAREST)
im.save(a.out)
print("wrote", a.out)
