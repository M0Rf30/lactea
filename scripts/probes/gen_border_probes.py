#!/usr/bin/env python3
"""Generate the border / darken-centre probe presets (`bd_*.milk`, list `bd_list.txt`).

  scripts/probes/gen_border_probes.py

Every probe is a v1 (no shader) preset with decay 0 (the warp pass writes black), no main wave, no shapes and the
border / darken-centre state under test, so frame n >= 1 of `main` contains ONLY the probed primitive (the `bd_dc*` probes
keep a white full-screen shape instead, darken-centre needs something to darken). Reference sets, one per frame size
(pixel-centre ties need sizes where `size * W / 2` is a half integer):

  for s in 320x180 330x190 321x181 640x360 1000x200 360x640; do
    oracle/run_corpus.sh borders-probes-$s scripts/probes/bd_list.txt -n 6 -- --size $s --stages final,main,warped --vars
  done
  scripts/fdq.py borders-probes-330x190 bd_ --modes step --stages main
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from gen_overlay_probes import header, shape  # noqa: E402


def borders(text, ob=None, ib=None, darken_center=False):
    """Set `ob = (size, r, g, b, a)` / `ib = ...` and bDarkenCenter in a probe header."""
    for pre, v in (("ob", ob), ("ib", ib)):
        if v is None:
            continue
        for k, x in zip(("size", "r", "g", "b", "a"), v):
            text = re.sub(rf"^{pre}_{k}=.*$", f"{pre}_{k}={x:f}", text, flags=re.M)
    if darken_center:
        text = text.replace("bDarkenCenter=0", "bDarkenCenter=1")
    return text


def pf(*lines):
    return "".join(f"per_frame_{i + 1}={l}\n" for i, l in enumerate(lines))


PROBES = {}
# plain bands, opaque: edge positions only
PROBES["bd_ob"] = borders(header(), ob=(0.1, 1.0, 0.5, 0.2, 1.0))
PROBES["bd_ib"] = borders(header(), ib=(0.05, 0.2, 0.6, 1.0, 1.0))
PROBES["bd_both"] = borders(header(), ob=(0.1, 1.0, 0.3, 0.2, 0.5), ib=(0.05, 0.2, 0.9, 0.4, 0.5))
# sizes whose edges fall on pixel centres at 320x180 (0.05 -> 4.5 px) / 330x190 (0.1 -> 16.5 x 9.5 px)
PROBES["bd_tie_a"] = borders(header(), ob=(0.05, 1.0, 1.0, 0.0, 1.0))
PROBES["bd_tie_b"] = borders(header(), ob=(0.1, 0.0, 1.0, 1.0, 1.0), ib=(0.1, 1.0, 0.0, 1.0, 1.0))
PROBES["bd_tie_c"] = borders(header(), ob=(0.1, 1.0, 0.5, 0.2, 0.5), ib=(0.05, 0.2, 0.6, 1.0, 0.5))
# degenerate geometry: inner radius negative / negative sizes / sizes beyond the screen
PROBES["bd_huge"] = borders(header(), ob=(0.6, 1.0, 0.5, 0.2, 0.5), ib=(0.6, 0.2, 0.9, 0.4, 0.5))
PROBES["bd_neg"] = borders(header(), ob=(-0.2, 0.8, 0.2, 0.2, 0.7), ib=(0.1, 0.2, 0.2, 0.8, 0.7))
PROBES["bd_neg_ib"] = borders(header(), ob=(0.2, 0.8, 0.2, 0.2, 0.7), ib=(-0.1, 0.2, 0.2, 0.8, 0.7))
PROBES["bd_big"] = borders(header(), ob=(2.5, 0.8, 0.8, 0.2, 0.5), ib=(0.3, 0.2, 0.2, 0.8, 0.5))
PROBES["bd_ob_over"] = borders(header(), ob=(1.2, 0.8, 0.8, 0.2, 0.5))
# colours outside 0..1 (raw glVertexAttrib4f, clamped by the output stage), alpha around the 0.001 threshold
PROBES["bd_color"] = borders(header(), ob=(0.1, 2.0, -1.0, 0.5, 0.8), ib=(0.08, 1.5, 0.5, -0.5, 0.8))
PROBES["bd_a_thr"] = borders(header(), ob=(0.1, 1.0, 1.0, 1.0, 0.001), ib=(0.1, 1.0, 1.0, 1.0, 0.0011))
PROBES["bd_a_hi"] = borders(header(), ob=(0.1, 1.0, 1.0, 1.0, 1.5), ib=(0.1, 0.0, 1.0, 0.0, 3.0))
# per-frame animated sizes / colours (the vars come from the per-frame code, not the preset header)
PROBES["bd_dyn"] = borders(header(), ob=(0.05, 1.0, 0.5, 0.2, 0.6), ib=(0.05, 0.2, 0.6, 1.0, 0.6)) + pf(
    "ob_size = 0.03 + 0.04*sin(time*3);", "ib_size = 0.02 + 0.03*cos(time*5);",
    "ob_r = 0.5 + 0.5*sin(time*7); ib_g = 0.5 + 0.5*cos(time*4);")
# darken centre over a white full-screen shape (decay 1 so it accumulates)
BG = shape(0, sides=4, num_inst=1, x=0.5, y=0.5, rad=3.0, rgba=(1, 1, 1, 1), rgba2=(1, 1, 1, 1), border=(1, 1, 1, 0))
PROBES["bd_dc"] = borders(header(decay=1.0), darken_center=True) + BG
PROBES["bd_dc_ob"] = borders(header(decay=1.0), ob=(0.1, 0.2, 0.2, 0.8, 0.5), darken_center=True) + BG
PROBES["bd_dc_off"] = header(decay=1.0) + BG

if __name__ == "__main__":
    for name, text in PROBES.items():
        with open(os.path.join(HERE, name + ".milk"), "w") as f:
            f.write(text)
    with open(os.path.join(HERE, "bd_list.txt"), "w") as f:
        for n in PROBES:
            f.write(f"scripts/probes/{n}.milk\n")
    print(f"wrote {len(PROBES)} probes to {HERE}")
