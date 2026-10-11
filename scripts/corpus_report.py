#!/usr/bin/env python3
"""Whole-corpus report: pass counts, robustness list, oracle failures, shader fallbacks and a ranked
clustering of the failures (first failing stage x preset features).

    scripts/corpus_report.py [SET=corpus-all] [--examples 4] [--md]

Reads  out/ref/<SET>/summary.tsv + <stem>/oracle.json     (oracle/run_corpus.sh)
       out/report/<SET>/render/{free,step}/*.json          (scripts/corpus_run.py: fdrender exit/log/fdrender.json)
       out/report/<SET>/{free,step}/corpus.json            (fdcompare corpus / corpus-merge)
       oracle/classify_presets.py                          (preset features)
Writes out/report/<SET>/corpus.json (combined, per preset: features, oracle/render status, per-stage metrics,
       bucket), buckets.md, index.html. Regression checks against a previous run use the per-mode
       {free,step}/corpus.json: `scripts/corpus_run.py --baseline step=OLD/step/corpus.json free=...`.

Stage order of the pipeline: warped -> (blur, not dumped by the corpus set) -> main -> final. In step
(teacher-forced) mode every stage is rendered from the oracle's previous `main`, so the first failing
stage is where the error is *introduced*; free mode only adds accumulation.
"""
import argparse
import collections
import html
import importlib.util
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ORDER = ["warped", "blur1", "blur2", "blur3", "main", "final"]


def load_classifier():
    spec = importlib.util.spec_from_file_location("classify_presets", os.path.join(ROOT, "oracle", "classify_presets.py"))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def jf(v):
    """corpus.json encodes non-finite floats as strings."""
    if isinstance(v, str):
        return {"inf": float("inf"), "-inf": float("-inf"), "nan": float("nan")}.get(v, float("nan"))
    return float("nan") if v is None else float(v)


def first_fail(stages):
    by = {s["stage"]: s for s in stages}
    for st in ORDER:
        if st in by and by[st]["status"] != "pass":
            return st
    return None


# Bucket labels: first matching feature (priority order) per first-failing stage. The goal is a
# handful of actionable buckets, not a taxonomy: the data-driven feature table (features_table)
# backs the choice of the priority lists.
PRIORITY = {
    "warped": [("motion_vectors", "motion vectors drawn on the warp source"),
               ("user_texture", "warp/comp shader with user texture"),
               ("blur1", "shader sampling blur"), ("blur2", "shader sampling blur"), ("blur3", "shader sampling blur"),
               ("warp_shader", "warp shader"),
               ("per_pixel", "per-pixel equations, no warp shader"),
               ("v1", "v1 preset (fixed-function warp), no per-pixel code")],
    "main": [("custom_shape", "custom shapes"), ("custom_wave", "custom waves"),
             ("motion_vectors", "motion vectors"),
             ("outer_border", "borders"), ("inner_border", "borders"),
             ("main_wave", "main waveform"), ("wave_dots", "main waveform"),
             ("darken_center", "darken center"),
             ("comp_shader", "shader preset, no overlays"), ("warp_shader", "shader preset, no overlays"),
             ("v1", "v1 preset, no overlays")],
    "final": [("comp_shader", "composite shader"),
              ("echo", "no-shader composite: echo"), ("gamma", "no-shader composite: gamma"),
              ("brighten", "no-shader composite: filters"), ("darken", "no-shader composite: filters"),
              ("solarize", "no-shader composite: filters"), ("invert", "no-shader composite: filters"),
              ("v1", "no-shader composite")],
}
# comp-shader sub-buckets (what the composite shader samples)
COMP_SUB = [("user_texture", "user texture"), ("noisevol_hq", "3D noise"), ("noisevol_lq", "3D noise"),
            ("noise_hq", "noise"), ("noise_mq", "noise"), ("noise_lq", "noise"), ("noise_lq_lite", "noise"),
            ("blur1", "blur"), ("blur2", "blur"), ("blur3", "blur"), ("shader_rot", "rot_*"), ("shader_rand", "rand_*"),
            ("shader_audio", "audio")]


def bucket(stage, feats, psnr):
    if stage is None:
        return None
    label = None
    for f, name in PRIORITY[stage]:
        if f in feats:
            label = name
            break
    label = label or "other"
    sub = ""
    if stage in ("final", "warped") and ("comp_shader" in feats or "warp_shader" in feats):
        for f, name in COMP_SUB:
            if f in feats:
                sub = f" [{name}]"
                break
    sev = "marginal (>=30 dB)" if psnr >= 30 else "bad (<30 dB)"
    return f"{stage}: {label}{sub} | {sev}"


def norm_reason(r):
    r = re.sub(r"0x[0-9a-fA-F]+", "0x?", r)
    r = re.sub(r"\d+(\.\d+)?", "N", r)
    return r[:160]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("set", nargs="?", default="corpus-all")
    ap.add_argument("--examples", type=int, default=4)
    ap.add_argument("--md", action="store_true", help="print buckets.md to stdout")
    a = ap.parse_args()
    ref_dir = os.path.join(ROOT, "out", "ref", a.set)
    rep = os.path.join(ROOT, "out", "report", a.set)
    cls = load_classifier()

    # ---- oracle
    oracle = {}
    with open(os.path.join(ref_dir, "summary.tsv")) as f:
        next(f)
        for line in f:
            p = line.rstrip("\n").split("\t")
            if len(p) < 6:
                continue
            oracle[p[0]] = {"rc": p[1], "shader_errors": p[2], "frames": p[3], "seconds": p[4], "preset": p[5]}
    for stem, o in oracle.items():
        oj = os.path.join(ref_dir, stem, "oracle.json")
        if os.path.isfile(oj):
            try:
                j = json.load(open(oj))
                o["preset_loaded"] = j.get("preset_loaded", True)
                o["shader_error_messages"] = j.get("shader_error_messages", [])[:3]
            except Exception:
                pass

    # ---- renders and compare
    cmp_ = {}
    for mode in ("free", "step"):
        cj = os.path.join(rep, mode, "corpus.json")
        c = json.load(open(cj))
        cmp_[mode] = {e["preset"]: e for e in c["entries"]}
    rend = {"free": {}, "step": {}}
    for mode in rend:
        d = os.path.join(rep, "render", mode)
        for fn in os.listdir(d) if os.path.isdir(d) else []:
            r = json.load(open(os.path.join(d, fn)))
            rend[mode][r["stem"]] = r

    f0 = {}
    f0p = os.path.join(rep, "frame0", "corpus.json")
    if os.path.isfile(f0p):
        for e in json.load(open(f0p))["entries"]:
            f0[e["preset"]] = {st["stage"]: jf(st["psnr_min"]) for st in e["stages"]}
    presets = {}
    for stem, o in sorted(oracle.items()):
        feats = []
        try:
            feats = cls.features(o["preset"])
        except Exception:
            pass
        fs = set(feats)
        rec = {"preset": o["preset"], "features": [f for f in feats if not f.startswith(("size:", "tex:"))],
               "oracle": {"rc": o["rc"], "shader_errors": o["shader_errors"], "seconds": o["seconds"]}}
        rec["frame0_psnr"] = f0.get(stem)
        for mode in ("free", "step"):
            e = cmp_[mode].get(stem)
            r = rend[mode].get(stem)
            m = {"status": e["status"] if e else "missing"}
            if r:
                m["render_rc"] = r["rc"]
                m["render_seconds"] = r["seconds"]
            if e and e["stages"]:
                m["stages"] = {s["stage"]: {"psnr_min": jf(s["psnr_min"]), "psnr_median": jf(s["psnr_median"]),
                                            "ssim_min": jf(s["ssim_min"]), "first_bad": s["first_diverging_frame"],
                                            "status": s["status"]} for s in e["stages"]}
                m["first_fail_stage"] = first_fail(e["stages"])
                ff = m["first_fail_stage"]
                m["bucket"] = bucket(ff, fs, m["stages"][ff]["psnr_median"]) if ff else None
                m["thumb"] = e.get("thumb")
                m["worst_image"] = e.get("worst_image")
                m["report"] = e.get("report")
            rec[mode] = m
        presets[stem] = rec

    stems = sorted(oracle)
    out = {s: presets[s] for s in stems}

    # ---- numbers
    def cnt(pred):
        return sum(1 for s in stems if pred(out[s]))

    n = len(stems)
    orc_ok = [s for s in stems if oracle[s]["rc"] in ("0", "3") and os.path.isdir(os.path.join(ref_dir, s, "final"))]
    orc_fail = [s for s in stems if s not in orc_ok]
    orc_shader_err = [s for s in orc_ok if oracle[s]["rc"] == "3" or oracle[s]["shader_errors"] not in ("0", "NA", "")]
    comparable = [s for s in orc_ok if s not in orc_shader_err]
    T = {}
    T["presets"] = n
    T["oracle_ok"] = len(orc_ok)
    T["oracle_exit"] = dict(collections.Counter(oracle[s]["rc"] for s in stems))
    T["oracle_shader_errors"] = len(orc_shader_err)
    for mode in ("free", "step"):
        T[mode] = dict(collections.Counter(out[s][mode]["status"] for s in orc_ok))
        T[mode + "_comparable"] = dict(collections.Counter(out[s][mode]["status"] for s in comparable))
        T[mode + "_render_exit"] = dict(collections.Counter(str(r["rc"]) for r in rend[mode].values()))
    T["step_pass_free_fail"] = len([s for s in comparable if out[s]["step"]["status"] == "pass" and out[s]["free"]["status"] == "fail"])
    T["both_pass"] = len([s for s in comparable if out[s]["step"]["status"] == "pass" and out[s]["free"]["status"] == "pass"])

    # per-stage pass counts (step) over comparable presets
    stage_pass = {}
    for mode in ("step", "free"):
        for st in ("warped", "main", "final"):
            ok = tot = 0
            for s in comparable:
                sg = out[s][mode].get("stages", {}).get(st)
                if sg:
                    tot += 1
                    ok += sg["status"] == "pass"
            stage_pass[f"{mode}.{st}"] = [ok, tot]
    T["stage_pass"] = stage_pass

    # ---- robustness / fallbacks
    robust = []
    fallbacks = collections.defaultdict(list)
    warnings = collections.defaultdict(list)
    unresolved = collections.Counter()
    slow = []
    for mode in ("free", "step"):
        for stem, r in rend[mode].items():
            if r["rc"] not in (0, 3):
                robust.append({"stem": stem, "mode": mode, "rc": r["rc"], "seconds": r["seconds"], "panic": r.get("panic"),
                               "tail": r.get("log_tail", [])[-3:], "repro": r.get("repro"),
                               "oracle_rc": oracle.get(stem, {}).get("rc")})
            info = r.get("info") or {}
            if mode == "step":
                for fb in info.get("shader_fallbacks") or []:
                    fallbacks[norm_reason(f"{fb['pass']}: {fb['reason']}")].append(stem)
                for w in info.get("warnings") or []:
                    warnings[norm_reason(w)].append(stem)
                for t in info.get("unresolved_textures") or []:
                    unresolved[t] += 1
            if r["seconds"] > 60:
                slow.append((stem, mode, r["seconds"]))

    # ---- clusters (step mode, comparable presets)
    clusters = collections.defaultdict(list)
    for s in comparable:
        m = out[s]["step"]
        if m["status"] == "fail" and m.get("bucket"):
            clusters[m["bucket"]].append(s)
        elif m["status"] not in ("pass", "fail"):
            clusters[f"no step result ({m['status']})"].append(s)
    ranked = sorted(clusters.items(), key=lambda kv: -len(kv[1]))

    # first failing stage (step) overall
    stage_hist = collections.Counter(out[s]["step"].get("first_fail_stage") for s in comparable if out[s]["step"]["status"] == "fail")

    # feature table: failing rate among presets that carry the feature (step)
    ftab = {}
    base_fail = sum(1 for s in comparable if out[s]["step"]["status"] == "fail")
    for s in comparable:
        for f in out[s]["features"]:
            if f.startswith(("wave_mode_", "echo_orient_")) or f in ("per_frame",):
                continue
            t = ftab.setdefault(f, [0, 0])
            t[0] += 1
            t[1] += out[s]["step"]["status"] == "fail"
    ftable = sorted(((f, t[0], t[1], t[1] / t[0]) for f, t in ftab.items() if t[0] >= 15), key=lambda x: (-x[2]))
    # wave modes separately
    wm = collections.defaultdict(lambda: [0, 0])
    for s in comparable:
        for f in out[s]["features"]:
            if f.startswith("wave_mode_"):
                wm[f][0] += 1
                wm[f][1] += out[s]["step"]["status"] == "fail"

    # free-run drift: step pass, free fail -> first bad frame histogram
    drift = [s for s in comparable if out[s]["step"]["status"] == "pass" and out[s]["free"]["status"] == "fail"]
    drift_first = collections.Counter()
    for s in drift:
        fb = [sg["first_bad"] for sg in out[s]["free"].get("stages", {}).values() if sg["first_bad"] is not None]
        drift_first[min(fb) if fb else -1] += 1

    result = {"set": a.set, "totals": T, "clusters": {k: v for k, v in ranked}, "first_fail_stage_step": dict(stage_hist),
              "robustness": robust, "slow": slow,
              "shader_fallbacks": {k: len(v) for k, v in fallbacks.items()},
              "oracle_failures": {s: oracle[s] for s in orc_fail},
              "oracle_shader_error_presets": orc_shader_err}
    with open(os.path.join(rep, "corpus.json"), "w") as f:
        json.dump({"schema": 1, "summary": result, "presets": out}, f, indent=1, default=lambda x: None)
    # ---- markdown
    L = []
    P = L.append
    P(f"# Corpus report: {a.set}\n")
    P(f"- presets: {n}; oracle OK: {len(orc_ok)}; oracle exit codes: {T['oracle_exit']}; oracle shader errors (projectM fell back, rendered anyway): {len(orc_shader_err)}")
    for mode in ("step", "free"):
        P(f"- {mode}: status over oracle-OK presets {T[mode]}; fdrender exit codes {T[mode + '_render_exit']}")
    P(f"- comparable (oracle OK, no oracle shader error): {len(comparable)}; step {T['step_comparable']}; free {T['free_comparable']}")
    P(f"- step pass & free pass: {T['both_pass']}; step pass, free fail (drift only): {T['step_pass_free_fail']}")
    P("\n| mode.stage | pass | of |\n|---|---|---|")
    for k, (ok, tot) in stage_pass.items():
        P(f"| {k} | {ok} | {tot} |")
    head_end = len(L)
    if f0:
        P("\n## Frame 0 (compared separately; the gate above excludes it)\n")
        P("| stage | presets >= 40 dB | of | median PSNR |\n|---|---|---|---|")
        for st in ("warped", "main", "final"):
            v = [out[x]["frame0_psnr"][st] for x in comparable if out[x]["frame0_psnr"] and st in out[x]["frame0_psnr"]]
            if v:
                v.sort()
                P(f"| {st} | {sum(1 for y in v if y >= 40)} | {len(v)} | {v[len(v) // 2]:.1f} |")
    P("\n## First failing stage (step, comparable presets)\n")
    P("| stage | presets |\n|---|---|")
    for st, c in stage_hist.most_common():
        P(f"| {st} | {c} |")
    P("\n## Ranked buckets (step: first failing stage | preset feature | severity of that stage's median PSNR)\n")
    P("| # | bucket | presets | examples |\n|---|---|---|---|")
    for i, (k, v) in enumerate(ranked, 1):
        ex = "; ".join(sorted(v, key=lambda s: out[s]["step"].get("stages", {}).get(k.split(":")[0], {}).get("psnr_median", 0) if k.split(":")[0] in ("warped", "main", "final") else 0)[:a.examples])
        P(f"| {i} | {k} | {len(v)} | {ex} |")
    P("\n## Feature table (step failure rate among comparable presets with the feature, >= 15 presets)\n")
    P("| feature | presets | failing | rate |\n|---|---|---|---|")
    for f, t, b, r in ftable[:40]:
        P(f"| {f} | {t} | {b} | {r:.0%} |")
    P("\n## Wave modes\n")
    P("| mode | presets | failing |\n|---|---|---|")
    for f, (t, b) in sorted(wm.items(), key=lambda kv: int(kv[0].split("_")[-1])):
        P(f"| {f} | {t} | {b} |")
    P("\n## Drift only (step passes, free fails): first bad frame\n")
    P(", ".join(f"frame {k}: {v}" for k, v in sorted(drift_first.items())) or "none")
    P("\n## Robustness (fdrender exit not 0/3)\n")
    for r in robust:
        P(f"- {r['mode']} {r['stem']}: rc={r['rc']} panic={r['panic']} {r['tail'][-1:] }")
    if not robust:
        P("none")
    P("\n## Shader fallbacks (ours, step run; distinct normalised reasons)\n")
    for k, v in sorted(fallbacks.items(), key=lambda kv: -len(kv[1])):
        P(f"- {len(v)}x {k} (e.g. {v[0]})")
    if not fallbacks:
        P("none")
    P("\n## Oracle failures\n")
    for s in orc_fail:
        P(f"- rc={oracle[s]['rc']} {s}")
    if not orc_fail:
        P("none")
    P("\n## Unresolved textures (ours)\n")
    P(", ".join(f"{t}: {c}" for t, c in unresolved.most_common(20)) or "none")
    md = "\n".join(L) + "\n"
    open(os.path.join(rep, "buckets.md"), "w").write(md)
    if a.md:
        sys.stdout.write(md)

    # ---- html
    H = ["<!doctype html><html><head><meta charset=utf-8><title>lactea corpus</title><style>body{font-family:sans-serif;background:#111;color:#ddd;margin:20px}"
         "table{border-collapse:collapse}td,th{border:1px solid #444;padding:3px 8px;font-size:13px;vertical-align:top}a{color:#8cf}img{max-width:320px;display:block}"
         ".ex{display:inline-block;margin:4px;font-size:11px;max-width:320px}</style></head><body>"]
    H.append(f"<h1>lactea corpus: {html.escape(a.set)}</h1>")
    H.append("<p>per-mode reports: <a href='step/index.html'>step (teacher-forced)</a> &middot; <a href='free/index.html'>free run</a> "
             "&middot; <a href='buckets.md'>buckets.md</a> &middot; <a href='corpus.json'>corpus.json</a></p><pre>")
    H.append(html.escape("\n".join(L[1:head_end])))
    H.append("</pre><h2>Ranked buckets</h2><table><tr><th>#</th><th>bucket</th><th>presets</th><th>examples (step worst frame: ref | ours | diff)</th></tr>")
    for i, (k, v) in enumerate(ranked, 1):
        st = k.split(":")[0]
        exs = sorted(v, key=lambda s: out[s]["step"].get("stages", {}).get(st, {}).get("psnr_median", 0))[:a.examples]
        cells = []
        for s in exs:
            m = out[s]["step"]
            img = ""
            if m.get("thumb"):
                img = f"<a href=\"step/{html.escape(m['worst_image'] or '')}\"><img loading=lazy src=\"step/{html.escape(m['thumb'])}\"></a>"
            med = m.get("stages", {}).get(st, {}).get("psnr_median")
            cells.append(f"<div class=ex>{img}<a href=\"step/{html.escape(m.get('report') or '')}/summary.md\">{html.escape(s)}</a> ({med:.1f} dB)</div>" if med is not None else html.escape(s))
        H.append(f"<tr><td>{i}</td><td>{html.escape(k)}</td><td>{len(v)}</td><td>{''.join(cells)}</td></tr>")
    H.append("</table></body></html>")
    open(os.path.join(rep, "index.html"), "w").write("\n".join(H))
    print(f"[corpus_report] {rep}/index.html, buckets.md, corpus.json", file=sys.stderr)


if __name__ == "__main__":
    main()
