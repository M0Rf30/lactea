#!/usr/bin/env python3
"""Generate the large-translation warp probes (`wl_*`): v1 presets without per-pixel code whose per-frame `dx`/`dy`/`zoom`
are huge, so the warp vertex stage produces UVs of magnitude 1e3..1e7 where one f32 ulp of a UV is several texels
(`docs/render.md`, "Huge warp UVs").

  scripts/probes/gen_warp_probes.py        # writes scripts/probes/wl_*.milk and wl_list.txt
  oracle/run_corpus.sh warp-large-probes scripts/probes/wl_list.txt -n 8 -- --size 320x180 --stages final,main,warped,warpsrc --vars --textures
  scripts/fdq.py warp-large-probes wl_ --modes step --stages main,warped --first 1

The main wave (thick dots, full alpha) puts sharp high-frequency content on every frame, so a sub-texel UV
difference shows as a pixel difference in `warped` (teacher forced: the source is the oracle's `main[n-1]`).
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))

HEADER = """MILKDROP_PRESET_VERSION=100
PSVERSION=0
[preset00]
fRating=3.000000
fGammaAdj=1.000000
fDecay=1.000000
fVideoEchoZoom=1.000000
fVideoEchoAlpha=0.000000
nVideoEchoOrientation=0
nWaveMode=5
bAdditiveWaves=0
bWaveDots=1
bWaveThick=1
bModWaveAlphaByVolume=0
bMaximizeWaveColor=0
bTexWrap={texwrap}
bDarkenCenter=0
bRedBlueStereo=0
bBrighten=0
bDarken=0
bSolarize=0
bInvert=0
fWaveAlpha=1.000000
fWaveScale=1.000000
fWaveSmoothing=0.500000
fWaveParam=0.000000
fModWaveAlphaStart=0.750000
fModWaveAlphaEnd=0.950000
fWarpAnimSpeed=1.000000
fWarpScale=1.000000
fZoomExponent=1.000000
fShader=0.000000
zoom=1.000000
rot=0.000000
cx=0.500000
cy=0.500000
dx=0.000000
dy=0.000000
warp=0.000000
sx=1.000000
sy=1.000000
wave_r=1.000000
wave_g=1.000000
wave_b=1.000000
wave_x=0.500000
wave_y=0.500000
ob_size=0.000000
ob_r=0.000000
ob_g=0.000000
ob_b=0.000000
ob_a=0.000000
ib_size=0.000000
ib_r=0.000000
ib_g=0.000000
ib_b=0.000000
ib_a=0.000000
nMotionVectorsX=0.000000
nMotionVectorsY=0.000000
mv_dx=0.000000
mv_dy=0.000000
mv_l=0.900000
mv_r=1.000000
mv_g=1.000000
mv_b=1.000000
mv_a=0.000000
"""

PROBES = {}


def probe(name, per_frame=(), per_pixel=(), texwrap=1):
    s = HEADER.format(texwrap=texwrap)
    for k, l in enumerate(per_frame, 1):
        s += f"per_frame_{k}={l}\n"
    for k, l in enumerate(per_pixel, 1):
        s += f"per_pixel_{k}={l}\n"
    PROBES[name] = s


for mag in (1e2, 1e3, 1e4, 1e5, 1e6):
    e = f"{int(mag):d}"
    probe(f"wl_dx_{e}", [f"dx = {mag:g};"])
    probe(f"wl_dy_{e}", [f"dy = {mag:g};"])
    probe(f"wl_dxy_{e}", [f"dx = {mag:g}; dy = {mag * 0.7:g};"])
# the same through the per-pixel path (per-vertex attributes that vary with the mesh)
for mag in (1e3, 1e4, 1e5):
    e = f"{int(mag):d}"
    probe(f"wl_pp_dx_{e}", [], [f"dx = {mag:g}*rad;"])
    probe(f"wl_pp_dy_{e}", [], [f"dy = {mag:g}*rad;"])
# clamped texture (`bTexWrap` 0 and per-frame `wrap = 0`)
probe("wl_clamp_dx_1e4", ["wrap = 0; dx = 1e4;"], texwrap=0)

if __name__ == "__main__":
    for name, text in PROBES.items():
        with open(os.path.join(HERE, name + ".milk"), "w") as f:
            f.write(text)
    with open(os.path.join(HERE, "wl_list.txt"), "w") as f:
        for n in PROBES:
            f.write(f"scripts/probes/{n}.milk\n")
    print(f"wrote {len(PROBES)} probes to {HERE}")
