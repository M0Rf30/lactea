#!/usr/bin/env python3
"""Stage-selection invariance proof: dumping more stages (notably blur1..3, which force the blur chain on) must not change
what the other stages contain.

usage: check_stage_selection.py [--frames N] [--jobs J] [--size WxH] [--bin REFRENDER] [--out DIR] LIST [--limit K]
LIST: preset list (first TAB-separated column; e.g. oracle/starter_set.txt, a corpus-s400 list).

Every preset is rendered once per stage selection (default: `final,main,warped`  |  `all`  |  `all,warpsrc`  |  `blur1,final`)
with `--vars`, then the sha256 of every `final/ main/ warped/ vars/` file is compared across the selections that carry that
stage. Exit code 0 iff identical for all presets. (Before the GlStateGuard in the oracle's BlurTexture patch, `all` turned
frame 0 of `main`/`final` black for every preset that does not use blur itself.)
"""
import argparse
import concurrent.futures as cf
import hashlib
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

SELECTIONS = ["final,main,warped", "all", "all,warpsrc", "blur1,final"]
CHECK = ["final", "main", "warped", "vars"]


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def tree(d, sub):
    out = {}
    base = os.path.join(d, sub)
    if not os.path.isdir(base):
        return None
    for fn in sorted(os.listdir(base)):
        out[fn] = sha(os.path.join(base, fn))
    return out


def one(args):
    preset, idx, a = args
    base = os.path.join(a.out, str(idx))
    shutil.rmtree(base, ignore_errors=True)
    trees = {}
    for si, sel in enumerate(SELECTIONS):
        d = os.path.join(base, str(si))
        cmd = [a.bin, "--preset", preset, "--audio", a.audio, "--out", d, "--frames", str(a.frames), "--size", a.size,
               "--stages", sel, "--vars"]
        tex = os.path.join(HERE, "build", "texpack", "textures")
        if os.path.isdir(tex):
            cmd += ["--texture-dir", tex]
        r = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=a.timeout)
        if r.returncode not in (0, 3):
            return preset, "render rc=%d (%s)" % (r.returncode, sel), 0
        trees[sel] = {s: tree(d, s) for s in CHECK}
        if not a.keep:
            pass
    bad = []
    nfiles = 0
    for s in CHECK:
        ref = None
        for sel in SELECTIONS:
            t = trees[sel][s]
            if t is None:
                continue
            if ref is None:
                ref = (sel, t)
                nfiles += len(t)
            elif t != ref[1]:
                diff = sorted(k for k in set(t) | set(ref[1]) if t.get(k) != ref[1].get(k))
                bad.append("%s: '%s' vs '%s' differ in %d files, first %s" % (s, sel, ref[0], len(diff), diff[:3]))
    if not a.keep:
        shutil.rmtree(base, ignore_errors=True)
    return preset, bad, nfiles


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("list")
    ap.add_argument("--frames", type=int, default=30)
    ap.add_argument("--jobs", type=int, default=8)
    ap.add_argument("--size", default="640x360")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--bin", default=os.path.join(HERE, "build", "refrender"))
    ap.add_argument("--audio", default=os.path.join(ROOT, "fixtures", "audio", "synth128.wav"))
    ap.add_argument("--out", default=os.path.join(ROOT, "out", "stage-selection"))
    a = ap.parse_args()
    presets = []
    for line in open(a.list):
        line = line.rstrip("\n")
        if line.strip() and not line.lstrip().startswith("#"):
            presets.append(line.split("\t")[0])
    if a.limit:
        presets = presets[: a.limit]
    os.makedirs(a.out, exist_ok=True)
    fails = 0
    total_files = 0
    with cf.ThreadPoolExecutor(a.jobs) as ex:
        for preset, bad, nfiles in ex.map(one, [(p, i, a) for i, p in enumerate(presets)]):
            total_files += nfiles
            if bad:
                fails += 1
                print("DIFFERENT %s" % os.path.basename(preset))
                for b in ([bad] if isinstance(bad, str) else bad):
                    print("   ", b)
            else:
                print("IDENTICAL %s" % os.path.basename(preset), flush=True)
    print("%d presets, %d differ (%d compared files per selection family, %d frames, %s, selections %s)" %
          (len(presets), fails, total_files, a.frames, a.size, SELECTIONS))
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
