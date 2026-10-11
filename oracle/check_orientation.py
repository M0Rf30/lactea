#!/usr/bin/env python3
"""Empirical orientation / consistency proof for the oracle dumps (needs numpy + Pillow).

Renders three tiny probe presets (oracle/tests/*.milk) and one real preset with refrender and checks:

 1. comp-shader probe (`ret = float3(uv.x, uv.y, 0)`): in `final`, green (uv.y) is ~0 at the top row and
    ~255 at the bottom row  => row 0 of every PNG is the top of the displayed image, uv.y runs downward
    exactly like in MD2/D3D.
 2. shape probe (red square, one custom shape, decay 0): the square's centroid is at the SAME vertical
    position in final, main, warpsrc, blur1, blur2 => all stages share one orientation (warped is black
    because decay is 0, so the square is only drawn later).
 3. real preset: main[n] vs final[n] error is small for the stored orientation and large when one of
    them is flipped; warpsrc[n] == main[n-1] bit-exactly; blur1[n] is closer to a box-downsample of
    warpsrc[n] than of warped[n] (documents that blur reads the PRE-warp image, as in MD2 BlurPasses).

usage: check_orientation.py [REFRENDER] [OUTDIR]
"""
import os
import subprocess
import sys

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "build", "refrender")
OUT = sys.argv[2] if len(sys.argv) > 2 else os.path.join(ROOT, "out", "orientation-check")
AUDIO = os.path.join(ROOT, "fixtures", "audio", "synth128.wav")
SILENCE = os.path.join(ROOT, "fixtures", "audio", "silence.wav")
ok = True


def render(name, preset, frames, audio=AUDIO, stages="all,warpsrc"):
    d = os.path.join(OUT, name)
    subprocess.run([BIN, "--preset", preset, "--audio", audio, "--out", d, "--frames", str(frames), "--stages", stages],
                   check=True)
    return d


def load(d, stage, n):
    return np.asarray(Image.open(os.path.join(d, stage, "%05d.png" % n)).convert("RGBA")).astype(int)


def check(cond, msg):
    global ok
    print(("PASS  " if cond else "FAIL  ") + msg)
    ok &= bool(cond)


# 1. uv gradient
d = render("uv", os.path.join(HERE, "tests", "orientation_uv.milk"), 3, SILENCE, "final")
a = load(d, "final", 2)
top, bot = a[3, :, 1].mean(), a[-4, :, 1].mean()
left, right = a[:, 3, 0].mean(), a[:, -4, 0].mean()
check(top < 20 and bot > 235, "comp uv.y: green top row %.0f, bottom row %.0f (expect ~0 -> ~255)" % (top, bot))
check(left < 20 and right > 235, "comp uv.x: red left col %.0f, right col %.0f (expect ~0 -> ~255)" % (left, right))

# 2. shape probe
d = render("shape", os.path.join(HERE, "tests", "orientation.milk"), 3, SILENCE)
ys = {}
for st in ("final", "main", "warpsrc", "blur1", "blur2"):
    a = load(d, st, 2)
    m = (a[..., 0] > 60) & (a[..., 1] < 80)
    h = a.shape[0]
    ys[st] = np.where(m)[0].mean() / h if m.any() else float("nan")
print("      shape centroid y (fraction from top): " + ", ".join("%s=%.3f" % kv for kv in ys.items()))
check(max(ys.values()) - min(ys.values()) < 0.02, "shape at the same height in final/main/warpsrc/blur1/blur2")
check(load(d, "warped", 2)[..., :3].max() == 0, "warped is black for decay=0 (shape is drawn after the warp pass)")

# 3. real preset
preset = "/usr/share/projectM/presets/presets_stock/Aderrasi - Antidote.milk"
d = render("real", preset, 30)
n = 20
fin, main, ws, wp = (load(d, s, n) for s in ("final", "main", "warpsrc", "warped"))
prev = load(d, "main", n - 1)


def err(x, y):
    return np.abs(x[..., :3] - y[..., :3]).mean()


check(err(main, fin) < 1.5 and err(main[::-1], fin) > 3 * err(main, fin),
      "main~final same orientation: %.3f (flipped: %.3f)" % (err(main, fin), err(main[::-1], fin)))
check(np.array_equal(ws, prev), "warpsrc[n] == main[n-1] bit-exact")
check(err(wp, main) < 1.5 and err(wp[::-1], main) > 3 * err(wp, main),
      "warped~main same orientation: %.3f (flipped: %.3f)" % (err(wp, main), err(wp[::-1], main)))
b1 = np.asarray(Image.open(os.path.join(d, "blur1", "%05d.png" % n)).convert("RGB")).astype(float)
bh, bw = b1.shape[:2]


def down(x):
    return np.asarray(Image.fromarray(x[..., :3].astype(np.uint8)).resize((bw, bh), Image.BOX)).astype(float)


e_src, e_warp = np.abs(b1 - down(ws)).mean(), np.abs(b1 - down(wp)).mean()
e_flip = np.abs(b1[::-1] - down(ws)).mean()
check(e_src < e_flip, "blur1 orientation: err %.2f vs %.2f when flipped" % (e_src, e_flip))
check(e_src < e_warp, "blur1 source: err vs pre-warp image %.2f < vs warped image %.2f" % (e_src, e_warp))
sys.exit(0 if ok else 1)
