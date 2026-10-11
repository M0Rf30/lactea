#!/usr/bin/env python3
"""Exactness dashboard of a whole oracle set (step or free candidates kept with `corpus_run.py --keep-cand`).

  scripts/fdexact_corpus.py SET [--mode step|free] [--jobs 10] [--json out/exact-SET-MODE.json] [--list N]

Per preset and stage (warped, main, final) over all frames: number of bit-identical frames, total differing pixels,
total pixels differing by exactly 1 (code value, any channel), by 2..8, by more than 8, and the worst frame.
Prints the class counts: `exact` (bit-identical on every frame), `ulp` (max-abs <= 1 everywhere), `small` (max-abs <= 8),
`big` per stage, and with --list the N presets with the most pixels of |d| in 1..8 that are not `big`.
"""
import argparse, json, os
import numpy as np
from PIL import Image
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
ap = argparse.ArgumentParser()
ap.add_argument("set")
ap.add_argument("--mode", default="step", choices=["step", "free", "stepc"])
ap.add_argument("--jobs", type=int, default=10)
ap.add_argument("--json")
ap.add_argument("--list", type=int, default=0)
ap.add_argument("--stages", default="warped,main,final")
a = ap.parse_args()
sub = {"free": "fd", "step": "fd-step", "stepc": "fd-stepc"}[a.mode]
REF = os.path.join(ROOT, "out", "ref", a.set)
CAND = os.path.join(ROOT, "out", sub, a.set)
STAGES = a.stages.split(",")


def ld(p):
    return np.array(Image.open(p).convert("RGB")).astype(np.int16)


def one(stem):
    res = {}
    for st in STAGES:
        n = 0
        ident = tot = n1 = n8 = nbig = 0
        worst = (0, 0)
        while True:
            pr = os.path.join(REF, stem, st, f"{n:05d}.png")
            pc = os.path.join(CAND, stem, st, f"{n:05d}.png")
            if not (os.path.exists(pr) and os.path.exists(pc)):
                break
            m = np.abs(ld(pc) - ld(pr)).max(-1)
            k = int((m > 0).sum())
            ident += k == 0
            tot += k
            n1 += int((m == 1).sum())
            n8 += int(((m > 1) & (m <= 8)).sum())
            nbig += int((m > 8).sum())
            if m.max() > worst[0]:
                worst = (int(m.max()), n)
            n += 1
        res[st] = dict(frames=n, ident=ident, diff=tot, d1=n1, d2_8=n8, big=nbig, worst=worst)
    return stem, res


stems = sorted(d for d in os.listdir(CAND) if os.path.isdir(os.path.join(CAND, d)) and os.path.isfile(os.path.join(REF, d, "meta.json")))
with ThreadPoolExecutor(a.jobs) as ex:
    data = dict(ex.map(one, stems))
if a.json:
    json.dump(data, open(a.json, "w"))


def cls(r):
    if r["frames"] == 0:
        return "none"
    if r["diff"] == 0:
        return "exact"
    if r["d2_8"] == 0 and r["big"] == 0:
        return "ulp"
    if r["big"] == 0:
        return "small"
    return "big"


for st in STAGES:
    c = {}
    for s in data.values():
        c[cls(s[st])] = c.get(cls(s[st]), 0) + 1
    print(f"{a.set} {a.mode} {st:7s} presets {len(data)}: " + "  ".join(f"{k} {v}" for k, v in sorted(c.items())))
if a.list:
    for st in STAGES:
        rows = sorted(data.items(), key=lambda kv: -(kv[1][st]["d1"] + kv[1][st]["d2_8"]) if cls(kv[1][st]) in ("ulp", "small") else 0)
        print(f"-- {st}: most ulp/small-error pixels (not `big`)")
        for stem, s in rows[: a.list]:
            r = s[st]
            print(f"   {stem[:60]:60s} {cls(r):5s} frames {r['frames']} ident {r['ident']:3d} diffpx {r['diff']:7d} d1 {r['d1']:7d} d2-8 {r['d2_8']:6d} big {r['big']:5d} worst {r['worst']}")
