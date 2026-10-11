#!/usr/bin/env python3
"""Quick fidelity probe (no SSIM): render presets matching STEM in teacher (step) and/or free mode and print
per-stage PSNR (median / min over frames >= `--first`, default 1; use `--first 0` to include frame 0).

  scripts/fdq.py SET STEM... [--modes step,free,stepc] [--frames 120] [--no-render] [--stages final,main,warped] [--texture-dir DIR]

mode `stepc` = step + the oracle's main[n] fed to the composite shader (isolates the composite pass).
"""
import argparse, json, os, subprocess, sys
import numpy as np
from PIL import Image
from concurrent.futures import ThreadPoolExecutor as ProcessPoolExecutor

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
FD = os.environ.get("FD_TARGET", "target-engine")
BIN = os.path.join(ROOT, FD, "release", "fdrender")

ap = argparse.ArgumentParser()
ap.add_argument("set")
ap.add_argument("stems", nargs="*")
ap.add_argument("--list", help="file with exact preset directory names, one per line (added to STEM matches)")
ap.add_argument("--modes", default="step,free")
ap.add_argument("--frames", type=int, default=0)
ap.add_argument("--no-render", action="store_true")
ap.add_argument("--stages", default="final,main,warped,blur1")
ap.add_argument("--first", type=int, default=1)
ap.add_argument("--texture-dir", help="passed to fdrender (default: its oracle/build/texpack)")
a = ap.parse_args()

refroot = os.path.join(ROOT, "out", "ref", a.set)
allp = sorted(d for d in os.listdir(refroot) if os.path.isfile(os.path.join(refroot, d, "meta.json")))
sel = []
if a.list:
    sel += [l.rstrip("\n") for l in open(a.list) if l.strip() and l.rstrip("\n") in allp]
for s in a.stems:
    sel += [d for d in allp if s in d and d not in sel]


def render(args):
    stem, mode = args
    ref = os.path.join(refroot, stem)
    m = json.load(open(os.path.join(ref, "meta.json")))
    c = m["config"]
    out = os.path.join(ROOT, "out", {"free": "fd", "step": "fd-step", "stepc": "fd-stepc"}[mode], a.set, stem)
    audio = m["audio"] if os.path.exists(m["audio"]) else os.path.join(ROOT, "fixtures", "audio", os.path.basename(m["audio"]))
    cmd = [BIN, "--preset", m["preset"], "--audio", audio, "--size", f'{c["width"]}x{c["height"]}',
           "--mesh", f'{c["mesh"][0]}x{c["mesh"][1]}', "--fps", str(c["fps"]),
           "--frames", str(a.frames or c["frames"]), "--seed", str(c["seed"]),
           "--texel-offset", f'{c["texel_offset"][0]},{c["texel_offset"][1]}',
           "--stages", ",".join(dict.fromkeys(m["stages"] + a.stages.split(","))), "--vars", "--out", out]
    if a.texture_dir:
        cmd += ["--texture-dir", os.path.abspath(a.texture_dir)]
    cmd += ["--inject-noise", ref] if mode == "free" else ["--teacher", ref]
    if mode == "stepc":
        cmd += ["--comp-from-ref"]
    os.makedirs(os.path.dirname(out), exist_ok=True)
    r = subprocess.run(cmd, capture_output=True, text=True)
    return stem, mode, r.returncode, r.stderr.strip().splitlines()[-1:] if r.returncode not in (0, 3) else []


def psnr(a_, b_):
    mse = ((a_.astype(np.float64) - b_.astype(np.float64)) ** 2).mean()
    return 99.0 if mse == 0 else 10 * np.log10(255.0 ** 2 / mse)


def ld(p):
    return np.array(Image.open(p).convert("RGB"))


def score(args):
    stem, mode = args
    out = os.path.join(ROOT, "out", {"free": "fd", "step": "fd-step", "stepc": "fd-stepc"}[mode], a.set, stem)
    ref = os.path.join(refroot, stem)
    res = {}
    for st in a.stages.split(","):
        vals = []
        n = 0
        while True:
            pr, pc = os.path.join(ref, st, f"{n:05d}.png"), os.path.join(out, st, f"{n:05d}.png")
            if not (os.path.exists(pr) and os.path.exists(pc)):
                break
            if n >= a.first:
                vals.append(psnr(ld(pr), ld(pc)))
            n += 1
        res[st] = (float(np.median(vals)), float(min(vals)), next((i + a.first for i, v in enumerate(vals) if v < 40), None)) if vals else None
    return stem, mode, res


modes = a.modes.split(",")
jobs = [(s, m) for s in sel for m in modes]
with ProcessPoolExecutor(max_workers=int(os.environ.get("JOBS", min(6, os.cpu_count() or 2)))) as ex:
    if not a.no_render:
        for stem, mode, rc, err in ex.map(render, jobs):
            if rc not in (0, 3):
                print(f"RENDER FAIL {stem} {mode}: {rc} {err}")
    for stem, mode, res in ex.map(score, jobs):
        cells = []
        for st, v in res.items():
            cells.append(f"{st}:{'-' if v is None else f'{v[0]:.1f}/{v[1]:.1f}@{v[2]}'}")
        print(f"{stem[:44]:44s} {mode:4s} " + "  ".join(cells))
