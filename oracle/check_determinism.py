#!/usr/bin/env python3
"""Determinism proof: render each preset twice with identical arguments and compare sha256 of EVERY output file.

usage: check_determinism.py [--frames N] [--bin REFRENDER] [--seed S] PRESET [PRESET ...]
Output dirs: out/determinism/<n>/{run1,run2}. Exit code 0 iff all trees are byte-identical.
Prints the per-tree digest (sha256 over the sorted "relpath sha256" lines) and the file count.
"""
import argparse
import hashlib
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def tree(d):
    out = {}
    for base, _, files in os.walk(d):
        for fn in files:
            p = os.path.join(base, fn)
            out[os.path.relpath(p, d)] = sha(p)
    return out


def digest(t):
    return hashlib.sha256("".join("%s %s\n" % (k, t[k]) for k in sorted(t)).encode()).hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--frames", type=int, default=120)
    ap.add_argument("--bin", default=os.path.join(HERE, "build", "refrender"))
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--audio", default=os.path.join(ROOT, "fixtures", "audio", "synth128.wav"))
    ap.add_argument("presets", nargs="+")
    a = ap.parse_args()
    tex = os.path.join(HERE, "build", "texpack", "textures")
    ok = True
    for i, preset in enumerate(a.presets):
        base = os.path.join(ROOT, "out", "determinism", str(i))
        shutil.rmtree(base, ignore_errors=True)
        trees = []
        for run in ("run1", "run2"):
            cmd = [a.bin, "--preset", preset, "--audio", a.audio, "--out", os.path.join(base, run), "--frames",
                   str(a.frames), "--seed", str(a.seed), "--stages", "all,warpsrc", "--vars", "--textures"]
            if os.path.isdir(tex):
                cmd += ["--texture-dir", tex]
            rc = subprocess.run(cmd).returncode
            if rc not in (0, 3):
                print("render failed rc=%d: %s" % (rc, preset))
                sys.exit(1)
            trees.append(tree(os.path.join(base, run)))
        t1, t2 = trees
        same = t1 == t2
        ok &= same
        print("%s %s" % ("IDENTICAL" if same else "DIFFERENT", os.path.basename(preset)))
        print("  run1 digest %s  (%d files)" % (digest(t1), len(t1)))
        print("  run2 digest %s  (%d files)" % (digest(t2), len(t2)))
        if not same:
            bad = sorted(k for k in set(t1) | set(t2) if t1.get(k) != t2.get(k))
            print("  %d differing files, first: %s" % (len(bad), bad[:5]))
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
