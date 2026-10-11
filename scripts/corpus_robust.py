#!/usr/bin/env python3
"""fdrender robustness sweep: every preset of a list, no oracle needed. A panic, signal, hang, wgpu
validation error (rc 4) or any exit code other than 0 (ok) / 3 (shader fallback) / 2 (preset load error)
is a bug; load errors are listed too (they should coincide with the oracle's `rc=2` presets).

    scripts/corpus_robust.py [LIST=out/corpus-all.list] --tag NAME [--jobs 8] [--timeout 120]
                             [-- fdrender args, e.g. --size 257x145 --frames 8 --audio fixtures/audio/sweep.wav]

LIST: one preset path per line (first TAB column) or a directory (all **/*.milk).
Writes out/report/robust/<tag>.json (one record per non-clean run, plus counts) and prints a summary.
"""
import concurrent.futures as cf
import glob
import json
import os
import shlex
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def main():
    argv = sys.argv[1:]
    extra = []
    if "--" in argv:
        i = argv.index("--")
        argv, extra = argv[:i], argv[i + 1:]
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("list", nargs="?", default="out/corpus-all.list")
    ap.add_argument("--tag", required=True)
    ap.add_argument("--jobs", type=int, default=8)
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--limit", type=int, default=0)
    a = ap.parse_args(argv)
    os.chdir(ROOT)
    tgt = os.environ.get("FD_TARGET", "target-corpus")
    exe = os.path.abspath(f"{tgt}/release/fdrender")
    if os.path.isdir(a.list):
        presets = sorted(glob.glob(a.list + "/**/*.milk", recursive=True))
    else:
        presets = [l.split("\t")[0].rstrip("\n") for l in open(a.list) if l.strip() and not l.startswith("#")]
    if a.limit:
        presets = presets[:a.limit]
    defaults = ["--audio", "fixtures/audio/synth128.wav", "--frames", "10", "--stages", "final", "--size", "320x180"]
    given = {x for x in extra if x.startswith("--")}
    base = []
    for k, v in zip(defaults[::2], defaults[1::2]):
        if k not in given:
            base += [k, v]
    out_root = f"out/fd-robust-{a.tag}"

    def run(i_p):
        i, p = i_p
        out = f"{out_root}/{i}"
        cmd = [exe, "--preset", p, "--out", out] + base + extra
        t0 = time.time()
        try:
            r = subprocess.run(cmd, capture_output=True, timeout=a.timeout)
            rc, err = r.returncode, r.stderr.decode("utf-8", "replace")
        except subprocess.TimeoutExpired as e:
            rc, err = 124, (e.stderr or b"").decode("utf-8", "replace")
        subprocess.run(["rm", "-rf", out])
        lines = err.splitlines()
        return {"preset": p, "rc": rc, "seconds": round(time.time() - t0, 2),
                "panic": any("panicked at" in l for l in lines),
                "tail": [l for l in lines if any(k in l for k in ("panicked", "PRESET LOAD", "validation", "FALLBACK", "Error", "error"))][:3]
                        or lines[-2:],
                "repro": shlex.join(["fdrender", "--preset", p, "--out", "out/x"] + base + extra)}

    with cf.ThreadPoolExecutor(a.jobs) as ex:
        recs = list(ex.map(run, enumerate(presets)))
    counts = {}
    for r in recs:
        counts[str(r["rc"])] = counts.get(str(r["rc"]), 0) + 1
    bad = [r for r in recs if r["rc"] not in (0, 3, 2)]
    loads = [r for r in recs if r["rc"] == 2]
    slow = sorted((r for r in recs if r["seconds"] > 30), key=lambda r: -r["seconds"])
    os.makedirs("out/report/robust", exist_ok=True)
    json.dump({"tag": a.tag, "args": base + extra, "presets": len(recs), "exit_codes": counts, "bugs": bad,
               "load_errors": loads, "slow_over_30s": slow,
               "shader_fallbacks": [r for r in recs if r["rc"] == 3]},
              open(f"out/report/robust/{a.tag}.json", "w"), indent=1)
    print(f"[robust:{a.tag}] {len(recs)} presets, exit codes {counts}, bugs {len(bad)}, load errors {len(loads)}, "
          f"fallbacks {counts.get('3', 0)}, slow {len(slow)}")
    for r in bad[:20]:
        print(f"  BUG rc={r['rc']} {r['preset']}\n      {r['repro']}\n      {r['tail'][:2]}")


if __name__ == "__main__":
    main()
