#!/usr/bin/env python3
"""Gate counts and failing presets of an `fdcompare corpus` report pair.

  scripts/fdfails.py out/report/<SET> [--modes step,free] [--list] [--other out/report-before/<SET>]

Prints `pass/total` per mode (gate = whatever the report was made with, `psnr>=40 ssim>=0.98` by default, frame 0 included
unless the report was made with `--frames 1..`) and, with --list, each failing preset with the stages that fail
(`stage:min PSNR@first diverging frame`). --other prints the pass/total of a second report (e.g. a saved "before") and the
presets whose status changed.
"""
import argparse
import json
import os


def load(root, mode):
    p = os.path.join(root, mode, "corpus.json")
    if not os.path.exists(p):
        return None
    return {e["preset"]: e for e in json.load(open(p))["entries"]}


def fails(e):
    return " ".join("%s:%.1f@%s" % (s["stage"], s["psnr_min"], s["first_diverging_frame"])
                    for s in e["stages"] if s["status"] != "pass") or (e.get("error") or e["status"])


ap = argparse.ArgumentParser()
ap.add_argument("report")
ap.add_argument("--modes", default="step,free")
ap.add_argument("--list", action="store_true")
ap.add_argument("--other")
a = ap.parse_args()
for mode in a.modes.split(","):
    cur = load(a.report, mode)
    if cur is None:
        print("%s: no %s report" % (a.report, mode))
        continue
    ok = sum(1 for e in cur.values() if e["status"] == "pass")
    print("%s %-4s %d/%d pass" % (os.path.basename(a.report.rstrip("/")), mode, ok, len(cur)))
    if a.list:
        for k in sorted(cur):
            if cur[k]["status"] != "pass":
                print("   FAIL %-60s %s" % (k[:60], fails(cur[k])))
    if a.other:
        old = load(a.other, mode)
        if old:
            print("   before: %d/%d pass" % (sum(1 for e in old.values() if e["status"] == "pass"), len(old)))
            for k in sorted(set(cur) & set(old)):
                if cur[k]["status"] != old[k]["status"]:
                    print("   %s -> %s  %s" % (old[k]["status"], cur[k]["status"], k[:70]))
