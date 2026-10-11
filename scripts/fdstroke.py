#!/usr/bin/env python3
"""Stroke-level comparison of probe presets against the oracle (teacher-forced fdrender).

  scripts/fdstroke.py SET STEM... [--stage main] [--frames 1-11] [--no-render] [--tag NAME] [--tol 0]

For every frame >= 1 and every probe it prints

  union   pixels where either image is not (0,0,0)            (the "stroke" pixels)
  same    fraction of `union` where all RGB channels are within --tol (default 0 = bit identical)
  ref/fd  pixels that are lit only in the oracle / only in lactea
  mae     mean absolute error over `union` (8-bit levels, RGB mean)

Frame 0 is skipped by default (`--frames 1-11`: nothing to compare yet for motion vectors; overlays are drawn on frame 0 unless the preset
samples blur). Output dirs: out/fd-probe[-TAG]/SET/STEM.
Env: FD_TARGET (default target-overlays) selects the cargo target dir with release/fdrender.
"""
import argparse, json, os, subprocess, sys
import numpy as np
from PIL import Image
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
FD = os.environ.get("FD_TARGET", "target-overlays")
BIN = os.path.join(ROOT, FD, "release", "fdrender")

ap = argparse.ArgumentParser()
ap.add_argument("set")
ap.add_argument("stems", nargs="+")
ap.add_argument("--stage", default="main")
ap.add_argument("--frames", default="1-11")
ap.add_argument("--no-render", action="store_true")
ap.add_argument("--tag", default="")
ap.add_argument("--tol", type=int, default=0)
ap.add_argument("--free", action="store_true", help="free run instead of teacher forcing")
ap.add_argument("--exact", action="store_true", help="stems are exact names")
ap.add_argument("--stroke", default="", help="fdrender --stroke native|emulated|quads")
a = ap.parse_args()

refroot = os.path.join(ROOT, "out", "ref", a.set)
allp = sorted(d for d in os.listdir(refroot) if os.path.isfile(os.path.join(refroot, d, "meta.json")))
sel = []
for s in a.stems:
    sel += [d for d in allp if (d == s if a.exact else s in d) and d not in sel]
f0, f1 = (int(x) for x in a.frames.split("-"))
outroot = os.path.join(ROOT, "out", "fd-probe" + ("-" + a.tag if a.tag else ""), a.set)


def render(stem):
    ref = os.path.join(refroot, stem)
    m = json.load(open(os.path.join(ref, "meta.json")))
    c = m["config"]
    out = os.path.join(outroot, stem)
    audio = m["audio"] if os.path.exists(m["audio"]) else os.path.join(ROOT, "fixtures", "audio", os.path.basename(m["audio"]))
    cmd = [BIN, "--preset", m["preset"], "--audio", audio, "--size", f'{c["width"]}x{c["height"]}',
           "--mesh", f'{c["mesh"][0]}x{c["mesh"][1]}', "--fps", str(c["fps"]), "--frames", str(c["frames"]),
           "--seed", str(c["seed"]), "--texel-offset", f'{c["texel_offset"][0]},{c["texel_offset"][1]}',
           "--stages", ",".join(m["stages"]) + ",warpsrc", "--vars", "--out", out]
    cmd += ["--inject-noise" if a.free else "--teacher", ref]
    if a.stroke:
        cmd += ["--stroke", a.stroke]
    os.makedirs(os.path.dirname(out), exist_ok=True)
    r = subprocess.run(cmd, capture_output=True, text=True)
    return stem, r.returncode, r.stderr.strip().splitlines()[-1:] if r.returncode not in (0, 3) else []


def ld(p):
    return np.array(Image.open(p).convert("RGB")).astype(np.int32)


def compare(stem):
    ref = os.path.join(refroot, stem, a.stage)
    out = os.path.join(outroot, stem, a.stage)
    rows = []
    for n in range(f0, f1 + 1):
        pr, pc = os.path.join(ref, f"{n:05d}.png"), os.path.join(out, f"{n:05d}.png")
        if not (os.path.exists(pr) and os.path.exists(pc)):
            break
        r, c = ld(pr), ld(pc)
        lit_r = (r != 0).any(axis=2)
        lit_c = (c != 0).any(axis=2)
        uni = lit_r | lit_c
        d = np.abs(r - c).max(axis=2)
        same = (d <= a.tol) & uni
        nu = int(uni.sum())
        rows.append((n, nu, int(same.sum()), int((lit_r & ~lit_c).sum()), int((lit_c & ~lit_r).sum()),
                     float(np.abs(r - c).mean(axis=2)[uni].mean()) if nu else 0.0))
    return stem, rows


with ThreadPoolExecutor(max_workers=min(6, os.cpu_count() or 2)) as ex:
    if not a.no_render:
        for stem, rc, err in ex.map(render, sel):
            if rc not in (0, 3):
                print(f"RENDER FAIL {stem}: {rc} {err}")
    print(f"{'probe':22s} {'frames':>6s} {'union':>8s} {'same%':>7s} {'ref-only':>9s} {'fd-only':>8s} {'mae':>7s}")
    for stem, rows in ex.map(compare, sel):
        if not rows:
            print(f"{stem:22s} (no frames)")
            continue
        un = np.median([r[1] for r in rows])
        sm = np.median([100.0 * r[2] / r[1] if r[1] else 100.0 for r in rows])
        ro = np.median([r[3] for r in rows])
        fo = np.median([r[4] for r in rows])
        ma = np.median([r[5] for r in rows])
        print(f"{stem:22s} {len(rows):6d} {un:8.0f} {sm:7.2f} {ro:9.0f} {fo:8.0f} {ma:7.2f}")
