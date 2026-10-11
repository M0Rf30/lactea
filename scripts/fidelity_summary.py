#!/usr/bin/env python3
"""Compact summary of out/report/<SET>/{free,step}: per preset, step-mode PSNR median per stage (warped -> blur1 -> main -> final)
and free-run first frame below the gate. Medians include frame 0 (see docs/fidelity.md).

  scripts/fidelity_summary.py out/report/<SET> [--md]
"""
import json, os, sys

root = sys.argv[1]
md = "--md" in sys.argv
STAGES = ["warped", "blur1", "main", "final"]


def load(mode):
    out = {}
    cj = os.path.join(root, mode, "corpus.json")
    if not os.path.exists(cj):
        return out
    for e in json.load(open(cj))["entries"]:
        row = {}
        rp = e.get("report")
        if rp:
            try:
                r = json.load(open(os.path.join(root, mode, rp, "report.json")))
                for st in r["stages"]:
                    s = st["summary"]
                    row[st["stage"]] = (s["psnr_median"], s["ssim_median"], s.get("first_frame_below_gate"))
            except Exception:
                pass
        out[e["preset"]] = (e["status"], row, e.get("vars_diverging"), e.get("vars_first_diverging_frame"))
    return out


step, free = load("step"), load("free")
names = sorted(set(step) | set(free))
f = lambda v: "inf" if v == "inf" or v is None or (isinstance(v, float) and v > 99) else f"{v:.1f}"
hdr = ["preset"] + [f"step {s} dB/SSIM" for s in STAGES] + ["free final dB", "free 1st bad (final)", "vars div"]
rows = []
for n in names:
    s = step.get(n, (None, {}, None, None))
    fr = free.get(n, (None, {}, None, None))
    cells = [n]
    for st in STAGES:
        v = s[1].get(st)
        cells.append("-" if not v else f"{f(v[0])}/{v[1]:.3f}")
    ff = fr[1].get("final")
    cells.append("-" if not ff else f(ff[0]))
    cells.append("-" if not ff or ff[2] is None else str(ff[2]))
    cells.append(str(s[2]))
    rows.append(cells)
if md:
    print("| " + " | ".join(hdr) + " |")
    print("|" + "---|" * len(hdr))
    for r in rows:
        print("| " + " | ".join(r) + " |")
else:
    w = [max(len(r[i]) for r in rows + [hdr]) for i in range(len(hdr))]
    print("  ".join(h.ljust(w[i]) for i, h in enumerate(hdr)))
    for r in rows:
        print("  ".join(c.ljust(w[i]) for i, c in enumerate(r)))
for mode, d in (("step", step), ("free", free)):
    t = {}
    for v in d.values():
        t[v[0]] = t.get(v[0], 0) + 1
    print(f"{mode}: {len(d)} presets, status {t}")
