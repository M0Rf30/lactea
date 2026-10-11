#!/usr/bin/env python3
"""Generate main-waveform probe presets (decay 0, nothing but the main wave) into scripts/probes/mw_*.milk and
scripts/probes/mw_list.txt.   Render:  oracle/run_corpus.sh mainwave-probes scripts/probes/mw_list.txt -n 40
Compare:  FD_TARGET=target-mainwave scripts/fdq.py mainwave-probes mw_ --modes step --stages main"""
import os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = dict(
    fDecay=0.0, nWaveMode=6, bAdditiveWaves=0, bWaveDots=0, bWaveThick=0, bModWaveAlphaByVolume=0,
    bMaximizeWaveColor=0, fWaveAlpha=1.0, fWaveScale=1.0, fWaveSmoothing=0.75, fWaveParam=0.0,
    fModWaveAlphaStart=0.75, fModWaveAlphaEnd=0.95, wave_r=1.0, wave_g=0.6, wave_b=0.2, wave_x=0.5, wave_y=0.5,
)

TEMPLATE = """MILKDROP_PRESET_VERSION=100
PSVERSION=0
[preset00]
fRating=3.000000
fGammaAdj=1.000000
fDecay={fDecay:.6f}
fVideoEchoZoom=1.000000
fVideoEchoAlpha=0.000000
nVideoEchoOrientation=0
nWaveMode={nWaveMode}
bAdditiveWaves={bAdditiveWaves}
bWaveDots={bWaveDots}
bWaveThick={bWaveThick}
bModWaveAlphaByVolume={bModWaveAlphaByVolume}
bMaximizeWaveColor={bMaximizeWaveColor}
bTexWrap=1
bDarkenCenter=0
bRedBlueStereo=0
bBrighten=0
bDarken=0
bSolarize=0
bInvert=0
fWaveAlpha={fWaveAlpha:.6f}
fWaveScale={fWaveScale:.6f}
fWaveSmoothing={fWaveSmoothing:.6f}
fWaveParam={fWaveParam:.6f}
fModWaveAlphaStart={fModWaveAlphaStart:.6f}
fModWaveAlphaEnd={fModWaveAlphaEnd:.6f}
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
wave_r={wave_r:.6f}
wave_g={wave_g:.6f}
wave_b={wave_b:.6f}
wave_x={wave_x:.6f}
wave_y={wave_y:.6f}
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
nMotionVectorsX=12.000000
nMotionVectorsY=9.000000
mv_dx=0.000000
mv_dy=0.000000
mv_l=0.900000
mv_r=1.000000
mv_g=1.000000
mv_b=1.000000
mv_a=0.000000
"""

probes = {}
for m in range(16):
    probes[f"m{m}"] = dict(nWaveMode=m)
# parameter variants (on a representative subset)
for m in range(16):
    probes[f"m{m}_xy"] = dict(nWaveMode=m, wave_x=0.3, wave_y=0.7)
for m in (0, 1, 4, 6, 7, 9, 10, 11, 12, 13, 14):
    probes[f"m{m}_myst"] = dict(nWaveMode=m, fWaveParam=0.4)
    probes[f"m{m}_myst2"] = dict(nWaveMode=m, fWaveParam=-1.7)
for m in (0, 4, 6, 7):
    probes[f"m{m}_scale"] = dict(nWaveMode=m, fWaveScale=2.5, fWaveSmoothing=0.3)
for m in (0, 3, 6):
    probes[f"m{m}_alpha"] = dict(nWaveMode=m, fWaveAlpha=0.5)
    probes[f"m{m}_vol"] = dict(nWaveMode=m, bModWaveAlphaByVolume=1, fModWaveAlphaStart=0.2, fModWaveAlphaEnd=0.9)
    probes[f"m{m}_dots"] = dict(nWaveMode=m, bWaveDots=1)
    probes[f"m{m}_thick"] = dict(nWaveMode=m, bWaveThick=1)
    probes[f"m{m}_add"] = dict(nWaveMode=m, bAdditiveWaves=1, fWaveAlpha=0.6)
    probes[f"m{m}_max"] = dict(nWaveMode=m, bMaximizeWaveColor=1, wave_r=0.4, wave_g=0.2, wave_b=0.1)

lst = []
for name, kw in probes.items():
    d = dict(BASE)
    d.update(kw)
    p = os.path.join(HERE, f"mw_{name}.milk")
    open(p, "w").write(TEMPLATE.format(**d))
    lst.append(f"scripts/probes/mw_{name}.milk")
open(os.path.join(HERE, "mw_list.txt"), "w").write("\n".join(lst) + "\n")
print(len(lst), "probes")
