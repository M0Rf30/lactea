#!/usr/bin/env python3
"""Soft-cut (preset transition) fidelity loop: oracle pair renders vs fdrender, per frame of the blend window.

  scripts/fdtransition.py [PAIRS_FILE] [--filter SUBSTR] [--modes free,step,blend] [--no-ref] [--no-render]

For every line of oracle/transition_pairs.txt (A<TAB>B<TAB>options, see that file):
  ref    oracle/build/refrender --preset A --preset2 B --switch-frame N --soft-cut S ... --stages all --vars
         -> out/ref/transitions/<pair>/          (final, old_out, new_out, main/warped/blur*, new_*, vars, new_vars)
  free   fdrender --inject-noise ref (same flags)               -> out/fd/transitions/<pair>
  step   fdrender --teacher ref: both presets teacher-forced    -> out/fd-step/transitions/<pair>
  blend  step + --blend-teacher ref: the transition shader gets the oracle's old_out/new_out  -> out/fd-blend/transitions/<pair>
Reports PSNR (RGB) of `final` over the transition window (frames where the oracle dumped old_out), plus `old_out`/`new_out`
(the two presets' images before the blend), `new_warpsrc` at the first frame (initial image), and the first normal frame
after the window. SSIM of `final` comes from `fdcompare diff` (min over the window).

Env: FD_TARGET (default target-engine) -> $FD_TARGET/release/{fdrender,fdcompare}; FDCOMPARE (override).
"""
import argparse, json, os, shlex, subprocess, sys
import numpy as np
from PIL import Image
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
FD = os.environ.get("FD_TARGET", "target-engine")
FDRENDER = os.path.join(ROOT, FD, "release", "fdrender")
FDCOMPARE = os.environ.get("FDCOMPARE", os.path.join(ROOT, FD, "release", "fdcompare"))
REFRENDER = os.path.join(ROOT, "oracle", "build", "refrender")
AUDIO = os.path.join(ROOT, "fixtures", "audio", "synth128.wav")
TEXDIR = os.path.join(ROOT, "oracle", "build", "texpack", "textures")

ap = argparse.ArgumentParser()
ap.add_argument("pairs", nargs="?", default=os.path.join(ROOT, "oracle", "transition_pairs.txt"))
ap.add_argument("--filter", default="")
ap.add_argument("--modes", default="free,step,blend")
ap.add_argument("--no-ref", action="store_true", help="reuse existing oracle renders")
ap.add_argument("--no-render", action="store_true", help="reuse existing fdrender outputs (only score)")
ap.add_argument("--jobs", type=int, default=2)
ap.add_argument("--size", default="640x360")
ap.add_argument("--compat", default="projectm")
ap.add_argument("--set", default="transitions", help="output set name under out/{ref,fd,fd-step,fd-blend}/")
ap.add_argument("--no-ssim", action="store_true")
a = ap.parse_args()


def parse_pairs(path):
    out = []
    for line in open(path):
        if not line.strip() or line.startswith("#"):
            continue
        f = line.rstrip("\n").split("\t")
        opts = dict(kv.split("=", 1) for kv in (f[2].split() if len(f) > 2 else []) if "=" in kv)
        out.append((f[0], f[1], opts))
    return out


def stem(p):
    return os.path.splitext(os.path.basename(p))[0]


def pair_name(A, B, o):
    n = f"{stem(A)} -- {stem(B)}"
    if "shader" in o:
        n += f" [s{o['shader']}]"
    if "seed" in o:
        n += f" [seed{o['seed']}]"
    return n


def base_args(A, B, o, out, tool):
    seed = o.get("seed", "1")
    args = ["--preset", A, "--preset2", B, "--audio", AUDIO, "--size", a.size, "--fps", "60",
            "--frames", o.get("frames", "100"), "--seed", seed, "--switch-frame", o.get("switch", "20"),
            "--soft-cut", o.get("soft", "1"), "--start-clean", o.get("start_clean", "0"),
            "--stages", "all,warpsrc", "--vars", "--out", out]
    if "shader" in o:
        args += ["--transition-shader", o["shader"]]
    if tool == "ref":
        if os.path.isdir(TEXDIR):
            args += ["--texture-dir", TEXDIR]
        args += ["--textures"]
    return args


def run(cmd):
    r = subprocess.run(cmd, capture_output=True, text=True)
    return r.returncode, r.stderr.strip().splitlines()[-1:] if r.returncode not in (0, 3) else []


def ldrgb(p):
    return np.asarray(Image.open(p).convert("RGB"), dtype=np.float64)


def psnr(x, y):
    mse = ((x - y) ** 2).mean()
    return 99.0 if mse == 0 else 10 * np.log10(255.0 ** 2 / mse)


def frames(d, stage):
    p = os.path.join(d, stage)
    return sorted(int(f[:-4]) for f in os.listdir(p) if f.endswith(".png")) if os.path.isdir(p) else []


def series(ref, cand, stage, fr, shift=0):
    out = {}
    for n in fr:
        pr, pc = os.path.join(ref, stage, f"{n:05d}.png"), os.path.join(cand, stage, f"{n + shift:05d}.png")
        if os.path.exists(pr) and os.path.exists(pc):
            out[n] = psnr(ldrgb(pr), ldrgb(pc))
    return out


def stat(v):
    if not v:
        return "-"
    x = np.array(list(v.values()))
    return f"{np.median(x):.1f}/{x.min():.1f}"


def ssim_min(ref, cand, lo, hi):
    if a.no_ssim or not os.path.exists(FDCOMPARE):
        return None
    tmp = os.path.join(ROOT, "out", "tmp-fdcompare", os.path.basename(cand) + str(os.getpid()))
    r = subprocess.run([FDCOMPARE, "diff", ref, cand, "--out", tmp, "--stages", "final", "--frames", f"{lo}..={hi}",
                        "--images", "none", "--no-vars", "--gate", "psnr=0,ssim=0"], capture_output=True, text=True)
    try:
        rep = json.load(open(os.path.join(tmp, "report.json")))
    except Exception:
        return None
    best = None
    def walk(o):
        nonlocal best
        if isinstance(o, dict):
            for k, v in o.items():
                if k in ("ssim_min", "min_ssim") and isinstance(v, (int, float)):
                    best = v if best is None else min(best, v)
                else:
                    walk(v)
        elif isinstance(o, list):
            for v in o:
                walk(v)
    walk(rep)
    return best


pairs = [p for p in parse_pairs(a.pairs) if a.filter in pair_name(*p)]
modes = [m for m in a.modes.split(",") if m]
dirs = {"free": "fd", "step": "fd-step", "blend": "fd-blend"}
os.makedirs(os.path.join(ROOT, "out"), exist_ok=True)


def do_ref(p):
    A, B, o = p
    out = os.path.join(ROOT, "out", "ref", a.set, pair_name(*p))
    if os.path.isdir(out):
        subprocess.run(["rm", "-rf", out])
    os.makedirs(os.path.dirname(out), exist_ok=True)
    return run([REFRENDER] + base_args(A, B, o, out, "ref"))


def do_fd(job):
    p, mode = job
    A, B, o = p
    name = pair_name(*p)
    ref = os.path.join(ROOT, "out", "ref", a.set, name)
    out = os.path.join(ROOT, "out", dirs[mode], a.set, name)
    if os.path.isdir(out):
        subprocess.run(["rm", "-rf", out])
    os.makedirs(os.path.dirname(out), exist_ok=True)
    cmd = [FDRENDER] + base_args(A, B, o, out, "fd") + ["--compat", a.compat]
    if mode == "free":
        cmd += ["--inject-noise", ref]
    else:
        cmd += ["--teacher", ref]
        if mode == "blend":
            cmd += ["--blend-teacher", ref]
    return run(cmd)


if not a.no_ref:
    print("oracle renders...", flush=True)
    for p in pairs:  # one GL render at a time
        rc, err = do_ref(p)
        print(f"  ref {pair_name(*p)}: rc={rc} {err}", flush=True)
if not a.no_render:
    print("fdrender...", flush=True)
    jobs = [(p, m) for p in pairs for m in modes]
    with ThreadPoolExecutor(a.jobs) as ex:
        for (p, m), (rc, err) in zip(jobs, ex.map(do_fd, jobs)):
            if rc not in (0, 3):
                print(f"  FAIL {m} {pair_name(*p)}: rc={rc} {err}", flush=True)

hdr = f"{'pair':64s} {'shader':>11s} {'win':>9s} | " + " | ".join(f"{m} final" for m in modes) + " | step old_out | step new_out | free init | free post"
print("\nPSNR dB (median/min over the blend window)\n" + hdr)
rows = []
for p in pairs:
    A, B, o = p
    name = pair_name(*p)
    ref = os.path.join(ROOT, "out", "ref", a.set, name)
    if not os.path.isdir(ref):
        continue
    win = frames(ref, "old_out")
    info = json.load(open(os.path.join(ref, "oracle.json"))).get("transition", {})
    if not win:
        print(f"{name}: no transition frames in the oracle dump")
        continue
    lo, hi = win[0], win[-1]
    cells = []
    summary = {"pair": name, "shader": info.get("name"), "static": info.get("static"), "window": [lo, hi]}
    for m in modes:
        d = os.path.join(ROOT, "out", dirs[m], a.set, name)
        s = series(ref, d, "final", win)
        ss = ssim_min(ref, d, lo, hi)
        cells.append(stat(s) + (f" ssim {ss:.4f}" if ss is not None else ""))
        summary[m] = {"psnr": s, "ssim_min": ss}
    stepd = os.path.join(ROOT, "out", dirs["step"], a.set, name)
    freed = os.path.join(ROOT, "out", dirs["free"], a.set, name)
    oo = series(ref, stepd, "old_out", win) if "step" in modes else {}
    no = series(ref, stepd, "new_out", win) if "step" in modes else {}
    first = {lo: psnr(ldrgb(os.path.join(ref, "new_warpsrc", f"{lo:05d}.png")), ldrgb(os.path.join(freed, "new_warpsrc", f"{lo:05d}.png")))} \
        if "free" in modes and os.path.exists(os.path.join(freed, "new_warpsrc", f"{lo:05d}.png")) else {}
    after = series(ref, freed, "final", [hi + 1, hi + 2, hi + 10]) if "free" in modes else {}
    print(f"{name[:64]:64s} {str(info.get('name')):>11s} {lo:>3d}-{hi:<3d}   | " + " | ".join(cells)
          + f" | {stat(oo)} | {stat(no)} | {stat(first)} | {stat(after)}", flush=True)
    summary.update({"old_out": oo, "new_out": no, "initial_image": first, "after": after})
    rows.append(summary)
os.makedirs(os.path.join(ROOT, "out", "report"), exist_ok=True)
json.dump(rows, open(os.path.join(ROOT, "out", "report", f"{a.set}.json"), "w"), indent=1)
