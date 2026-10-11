#!/usr/bin/env python3
"""Whole-corpus fidelity run: fdrender free + teacher (step) over an oracle set, `fdcompare corpus`
per chunk, candidate PNGs pruned after the metrics (disk), merged reports.

    scripts/corpus_run.py [SET=corpus-all] [--chunk 300] [--jobs 6] [--timeout 180] [--filter SUBSTR]
                          [--no-build] [--keep-cand] [--fresh] [--baseline DIR/corpus.json ...]

Inputs : out/ref/<SET>/<stem>/        (oracle/run_corpus.sh <SET> ...; meta.json carries the run parameters)
Outputs: out/report/<SET>/{free,step}/{corpus.json,index.html,summary.md,presets/<stem>/,thumbs/}
         out/report/<SET>/render/{free,step}/<stem>.json   one record per fdrender run: exit code, seconds,
                                                           fdrender.json essentials, last log lines
         out/report/<SET>/parts/<mode>.<chunk>.json        per-chunk corpus.json (resume unit)
         out/fd/<SET>/, out/fd-step/<SET>/                 candidate dumps, deleted after each chunk
                                                           unless --keep-cand

A crash (any exit code but 0/3), a timeout (rc 124) or a wgpu validation error (rc 4) in fdrender is
recorded, never fatal: the run continues with the next preset.
Env: FD_TARGET (cargo target dir, default target-corpus), COMPAT (projectm|md2), GATE (psnr=40,ssim=0.98),
     TEXTURE_DIR.
"""
import argparse
import concurrent.futures as cf
import json
import os
import shlex
import shutil
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def sh(cmd, **kw):
    return subprocess.run(cmd, **kw)


def audio_path(m):
    """meta.json of the shared references still points at the pre-rename checkout: fall back to the repo fixture."""
    p = m["audio"]
    return p if os.path.exists(p) else os.path.join(ROOT, "fixtures", "audio", os.path.basename(p))


def render_args(ref):
    m = json.load(open(os.path.join(ref, "meta.json")))
    c = m["config"]
    a = ["--preset", m["preset"], "--audio", audio_path(m),
         "--size", f'{c["width"]}x{c["height"]}', "--mesh", f'{c["mesh"][0]}x{c["mesh"][1]}',
         "--fps", str(c["fps"]), "--frames", str(c["frames"]), "--seed", str(c["seed"]),
         "--texel-offset", f'{c["texel_offset"][0]},{c["texel_offset"][1]}',
         "--stages", ",".join(m["stages"]) or "none"]
    if m.get("has_vars"):
        a.append("--vars")
    return a


def render_one(env, stem, mode):
    ref = os.path.join(env["REF"], stem)
    out = os.path.join(env["FD"] if mode == "free" else env["STEP"], stem)
    rec_path = os.path.join(env["REPORT"], "render", mode, stem + ".json")
    os.makedirs(os.path.dirname(rec_path), exist_ok=True)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    rec = {"stem": stem, "mode": mode}
    try:
        args = render_args(ref)
    except Exception as e:  # unreadable reference
        rec.update(rc=-1, seconds=0, error=f"meta.json: {e}")
        json.dump(rec, open(rec_path, "w"))
        return rec
    args += ["--inject-noise" if mode == "free" else "--teacher", ref]
    if env.get("TEXTURE_DIR"):
        args += ["--texture-dir", env["TEXTURE_DIR"]]
    args += ["--compat", env["COMPAT"], "--out", out]
    t0 = time.time()
    try:
        p = sh([env["FDRENDER"]] + args, capture_output=True, timeout=env["TIMEOUT"])
        rc, err = p.returncode, p.stderr.decode("utf-8", "replace")
    except subprocess.TimeoutExpired as e:
        rc, err = 124, (e.stderr or b"").decode("utf-8", "replace") + "\n[corpus_run] TIMEOUT"
    rec["rc"] = rc
    rec["seconds"] = round(time.time() - t0, 2)
    if rc < 0:  # killed by a signal (SIGSEGV, SIGABRT, OOM)
        rec["signal"] = -rc
    lines = err.splitlines()
    rec["log_tail"] = lines[-12:]
    rec["panic"] = any("panicked at" in l for l in lines)
    rec["repro"] = shlex.join(["fdrender"] + args)
    info_p = os.path.join(out, "fdrender.json")
    if os.path.isfile(info_p):
        try:
            info = json.load(open(info_p))
            rec["info"] = {k: info.get(k) for k in (
                "preset_loaded", "preset_error", "warp_shader", "comp_shader", "shader_fallbacks",
                "unresolved_textures", "warnings", "validation_errors", "frames_rendered", "seconds")}
        except Exception as e:
            rec["info_error"] = str(e)
    json.dump(rec, open(rec_path, "w"))
    return rec


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("set", nargs="?", default="corpus-all")
    ap.add_argument("--chunk", type=int, default=300)
    ap.add_argument("--jobs", type=int, default=6)
    ap.add_argument("--timeout", type=int, default=180, help="per fdrender run, seconds")
    ap.add_argument("--filter", default="")
    ap.add_argument("--frames", default="1..", help="fdcompare frame range for the gate (default: all but frame 0)")
    ap.add_argument("--no-build", action="store_true")
    ap.add_argument("--keep-cand", action="store_true")
    ap.add_argument("--fresh", action="store_true", help="ignore finished chunks")
    ap.add_argument("--baseline", nargs="*", default=[], metavar="MODE=corpus.json",
                    help="e.g. free=old/free/corpus.json step=old/step/corpus.json")
    a = ap.parse_args()

    os.chdir(ROOT)
    tgt = os.environ.get("FD_TARGET", "target-corpus")
    if not a.no_build:
        sh(["cargo", "build", "--release", "-q", "-p", "lactea-cli", "-p", "lactea-compare"],
           env=dict(os.environ, CARGO_TARGET_DIR=tgt), check=True)
    env = {
        "REF": f"out/ref/{a.set}", "FD": os.path.abspath(f"out/fd/{a.set}"),
        "STEP": os.path.abspath(f"out/fd-step/{a.set}"), "REPORT": os.path.abspath(f"out/report/{a.set}"),
        "FDRENDER": os.path.abspath(f"{tgt}/release/fdrender"),
        "FDCOMPARE": os.path.abspath(f"{tgt}/release/fdcompare"),
        "COMPAT": os.environ.get("COMPAT", "projectm"), "TIMEOUT": a.timeout,
        "TEXTURE_DIR": os.environ.get("TEXTURE_DIR", ""),
    }
    env["REF"] = os.path.abspath(env["REF"])
    gate = os.environ.get("GATE", "psnr=40,ssim=0.98")
    stems = sorted(d for d in os.listdir(env["REF"])
                   if os.path.isfile(os.path.join(env["REF"], d, "meta.json")) and a.filter in d)
    if not stems:
        sys.exit(f"no dumps in {env['REF']}")
    os.makedirs(os.path.join(env["REPORT"], "parts"), exist_ok=True)
    chunks = [stems[i:i + a.chunk] for i in range(0, len(stems), a.chunk)]
    print(f"[corpus_run] {a.set}: {len(stems)} presets, {len(chunks)} chunks of {a.chunk}, jobs={a.jobs}", flush=True)
    t_all = time.time()
    for k, ch in enumerate(chunks):
        parts = {m: os.path.join(env["REPORT"], "parts", f"{m}.{k:03d}.json") for m in ("free", "step")}
        ids_file = os.path.join(env["REPORT"], "parts", f"ids.{k:03d}.txt")
        want = "\n".join(ch) + "\n"
        if (not a.fresh and all(os.path.isfile(p) for p in parts.values())
                and os.path.isfile(ids_file) and open(ids_file).read() == want):
            print(f"[corpus_run] chunk {k}: done (resume)", flush=True)
            continue
        open(ids_file, "w").write(want)
        t0 = time.time()
        for mode in ("free", "step"):
            with cf.ThreadPoolExecutor(a.jobs) as ex:
                recs = list(ex.map(lambda s: render_one(env, s, mode), ch))
            bad = [r for r in recs if r["rc"] not in (0, 3)]
            print(f"[corpus_run] chunk {k} {mode}: {len(recs)} runs, "
                  f"{sum(r['rc'] == 0 for r in recs)} ok, {sum(r['rc'] == 3 for r in recs)} shader-fallback, "
                  f"{len(bad)} FAILED", flush=True)
            for r in bad[:5]:
                print(f"    rc={r['rc']} {r['stem']}: {' | '.join(r.get('log_tail', [])[-2:])}", flush=True)
            cand = env["FD"] if mode == "free" else env["STEP"]
            out = os.path.join(env["REPORT"], mode)
            # Frame 0 is excluded from the gate: oracle frame 0 depends on the oracle's blur forcing
            # (docs/corpus.md, "Frame 0"); it is compared separately below (step only).
            cmd = [env["FDCOMPARE"], "corpus", env["REF"], cand, "--out", out, "--gate", gate,
                   "--ids-file", ids_file, "--frames", a.frames]
            if os.environ.get("NO_SSIM"):
                cmd.append("--no-ssim")
            # cand roots may hold dumps of other chunks only if --keep-cand; --ids-file selects ours
            p = sh(cmd, capture_output=True)
            if not os.path.isfile(os.path.join(out, "corpus.json")):
                sys.exit(f"fdcompare failed: {p.stderr.decode()[-2000:]}")
            shutil.copy(os.path.join(out, "corpus.json"), parts[mode])
            if mode == "step":
                f0 = os.path.join(env["REPORT"], "frame0")
                p0 = sh([env["FDCOMPARE"], "corpus", env["REF"], cand, "--out", f0, "--gate", gate, "--ids-file", ids_file,
                         "--frames", "0", "--images", "none", "--no-vars"], capture_output=True)
                if os.path.isfile(os.path.join(f0, "corpus.json")):
                    shutil.copy(os.path.join(f0, "corpus.json"), os.path.join(env["REPORT"], "parts", f"frame0.{k:03d}.json"))
            if not a.keep_cand:
                for s in ch:
                    shutil.rmtree(os.path.join(cand, s), ignore_errors=True)
        print(f"[corpus_run] chunk {k}/{len(chunks)} {time.time() - t0:.0f}s (total {time.time() - t_all:.0f}s)", flush=True)

    for mode in ("free", "step", "frame0"):
        out = os.path.join(env["REPORT"], mode)
        pj = [os.path.join(env["REPORT"], "parts", f"{mode}.{k:03d}.json") for k in range(len(chunks))]
        pj = [p for p in pj if os.path.isfile(p)]
        if not pj:
            continue
        cmd = [env["FDCOMPARE"], "corpus-merge", "--out", out]
        base = dict(b.split("=", 1) for b in a.baseline)
        if mode in base:  # frame0 has no baseline
            cmd += ["--baseline", base[mode]]
        p = sh(cmd + pj, capture_output=True)
        print(p.stdout.decode().strip(), flush=True)
    print(f"[corpus_run] finished in {time.time() - t_all:.0f}s -> {env['REPORT']}")


if __name__ == "__main__":
    main()
