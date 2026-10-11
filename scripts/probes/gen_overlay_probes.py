#!/usr/bin/env python3
"""Generate the overlay probe presets (stroke rasterisation, motion vectors, custom shapes / waves).

  scripts/probes/gen_overlay_probes.py            # writes scripts/probes/ov_*.milk and ov_list.txt

Every probe is a v1 (no shader) preset with decay 0 (`fDecay=0`: the warp pass writes black), no main wave
(`fWaveAlpha=0`), no borders, so the `main` stage of frame n >= 1 contains ONLY the probed primitive
(frame 0 only shows overlays for presets that do not sample blur, see the `_blur` probes below). Render the references with

  oracle/run_corpus.sh overlays-probes scripts/probes/ov_list.txt -n 6 -- --stages all,warpsrc --vars --textures

and compare with `scripts/fdq.py overlays-probes ov_ --modes step --stages main`. The generated `ov_*.milk` and
`ov_list.txt` are written into scripts/probes/; rerun this script to regenerate them.
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))

HEADER = """MILKDROP_PRESET_VERSION=100
PSVERSION=0
[preset00]
fRating=3.000000
fGammaAdj=1.000000
fDecay={decay}
fVideoEchoZoom=1.000000
fVideoEchoAlpha=0.000000
nVideoEchoOrientation=0
nWaveMode=6
bAdditiveWaves=0
bWaveDots=0
bWaveThick=0
bModWaveAlphaByVolume=0
bMaximizeWaveColor=0
bTexWrap={texwrap}
bDarkenCenter=0
bRedBlueStereo=0
bBrighten=0
bDarken=0
bSolarize=0
bInvert=0
fWaveAlpha=0.000000
fWaveScale=0.500000
fWaveSmoothing=0.500000
fWaveParam=0.000000
fModWaveAlphaStart=0.750000
fModWaveAlphaEnd=0.950000
fWarpAnimSpeed=1.000000
fWarpScale=1.000000
fZoomExponent=1.000000
fShader=0.000000
zoom={zoom}
rot={rot}
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
nMotionVectorsX={mvx}
nMotionVectorsY={mvy}
mv_dx={mvdx}
mv_dy={mvdy}
mv_l={mvl}
mv_r=1.000000
mv_g=1.000000
mv_b=1.000000
mv_a={mva}
"""


def header(decay=0.0, zoom=1.0, rot=0.0, mv=(0, 0, 0, 0, 0.9, 0.0), texwrap=1):
    return HEADER.format(decay="%f" % decay, zoom="%f" % zoom, rot="%f" % rot, texwrap=texwrap,
                         mvx="%f" % mv[0], mvy="%f" % mv[1], mvdx="%f" % mv[2], mvdy="%f" % mv[3],
                         mvl="%f" % mv[4], mva="%f" % mv[5])


def wave(i, samples=512, dots=0, thick=0, additive=0, spectrum=0, sep=0, scaling=1.0, smoothing=0.5,
         rgba=(1, 1, 1, 1), per_point=(), per_frame=(), enabled=1):
    s = f"""wavecode_{i}_enabled={enabled}
wavecode_{i}_samples={samples}
wavecode_{i}_sep={sep}
wavecode_{i}_bSpectrum={spectrum}
wavecode_{i}_bUseDots={dots}
wavecode_{i}_bDrawThick={thick}
wavecode_{i}_bAdditive={additive}
wavecode_{i}_scaling={scaling:f}
wavecode_{i}_smoothing={smoothing:f}
wavecode_{i}_r={rgba[0]:f}
wavecode_{i}_g={rgba[1]:f}
wavecode_{i}_b={rgba[2]:f}
wavecode_{i}_a={rgba[3]:f}
"""
    for k, l in enumerate(per_frame, 1):
        s += f"wave_{i}_per_frame{k}={l}\n"
    for k, l in enumerate(per_point, 1):
        s += f"wave_{i}_per_point{k}={l}\n"
    return s


def shape(i, sides=4, additive=0, thick=0, textured=0, num_inst=1, x=0.5, y=0.5, rad=0.2, ang=0.0, tex_ang=0.0,
          tex_zoom=1.0, rgba=(1, 1, 1, 1), rgba2=(1, 1, 1, 1), border=(1, 1, 1, 0), per_frame=(), init=(), enabled=1):
    s = f"""shapecode_{i}_enabled={enabled}
shapecode_{i}_sides={sides}
shapecode_{i}_additive={additive}
shapecode_{i}_thickOutline={thick}
shapecode_{i}_textured={textured}
shapecode_{i}_num_inst={num_inst}
shapecode_{i}_x={x:f}
shapecode_{i}_y={y:f}
shapecode_{i}_rad={rad:f}
shapecode_{i}_ang={ang:f}
shapecode_{i}_tex_ang={tex_ang:f}
shapecode_{i}_tex_zoom={tex_zoom:f}
shapecode_{i}_r={rgba[0]:f}
shapecode_{i}_g={rgba[1]:f}
shapecode_{i}_b={rgba[2]:f}
shapecode_{i}_a={rgba[3]:f}
shapecode_{i}_r2={rgba2[0]:f}
shapecode_{i}_g2={rgba2[1]:f}
shapecode_{i}_b2={rgba2[2]:f}
shapecode_{i}_a2={rgba2[3]:f}
shapecode_{i}_border_r={border[0]:f}
shapecode_{i}_border_g={border[1]:f}
shapecode_{i}_border_b={border[2]:f}
shapecode_{i}_border_a={border[3]:f}
"""
    for k, l in enumerate(init, 1):
        s += f"shape_{i}_init{k}={l}\n"
    for k, l in enumerate(per_frame, 1):
        s += f"shape_{i}_per_frame{k}={l}\n"
    return s


# --------------------------------------------------------------------------------------------------------------
# a zig-zag "star": even samples on an inner radius, odd samples on an outer radius, angle advancing per pair; the
# radii wobble so that the segment angles / lengths / sub-pixel phases are all different
STAR = [
    "n=floor(sample*{nm1}+0.5);",
    "k=floor(n*0.5);",
    "odd=n-2*k;",
    "t=k*{dt};",
    "ri={ri0}+{ri1}*sin(k*0.31);",
    "ro={ro0}+{ro1}*sin(k*0.173+1);",
    "rw=ri+odd*(ro-ri);",
    "x=0.5+rw*cos(t)*{ax};",
    "y=0.5+rw*sin(t)*{ay};",
]


def star(samples=512, dt=0.2371, ri=(0.1, 0.09), ro=(0.3, 0.17), ax=0.5625, ay=1.0):
    return [l.format(nm1=samples - 1, dt=dt, ri0=ri[0], ri1=ri[1], ro0=ro[0], ro1=ro[1], ax=ax, ay=ay) for l in STAR]


PROBES = {}

# ---- non-AA lines / dots (custom waves: GL_LINE_STRIP / GL_POINTS, smoothing off, width 1) -------------------------
PROBES["ov_line"] = header() + wave(0, per_point=star())
PROBES["ov_line_short"] = header() + wave(0, per_point=star(dt=0.05, ri=(0.02, 0.01), ro=(0.06, 0.03)))
PROBES["ov_line_thick"] = header() + wave(0, thick=1, per_point=star())
PROBES["ov_line_add"] = header() + wave(0, additive=1, rgba=(1, 1, 1, 0.5), per_point=star())
PROBES["ov_line_alpha"] = header() + wave(0, rgba=(1, 1, 1, 0.5), per_point=star())
PROBES["ov_dots"] = header() + wave(0, dots=1, per_point=star())
PROBES["ov_dots_thick"] = header() + wave(0, dots=1, thick=1, per_point=star())
# boustrophedon of (nearly) horizontal rows, 3 + 9/16 px apart (16 different sub-pixel phases), ends drifting in 3/8 px
# steps; SmoothWave bends the vertical connectors, which adds more angles
SERP = [
    "n=min(floor(sample*511+0.5),199);",
    "rw=floor(n*0.5);",
    "odd=n-2*rw;",
    "rr=rw-2*floor(rw*0.5);",
    "flag=abs(odd-rr);",
    "xl=(20+0.375*rw)/640;",
    "xr=(620-0.4375*rw)/640;",
    "x=xl+flag*(xr-xl);",
    "y=0.5+(172-3.5625*rw)/640;",
]
PROBES["ov_serp"] = header() + wave(0, per_point=SERP)
# vertical y-major strips whose x sits on / near a pixel boundary. Groups of 4 points A(X+5, mid) B(X, top) C(X, bottom)
# D(X-5+eps, mid): SmoothWave's midpoint of B-C is X - 0.075*eps (A and D cancel for eps = 0), so the vertical strip
# B-mid-C is exactly vertical at X for eps = 0 (as the stick-man of Mstress & Juppy - Dancer's spine: x = 0.5 on a
# 320 px target), a hair off for eps = +-1/4 px, clearly off for +-2 px. X = 20 + 6 g + (g mod 8)/8 px: eight sub-pixel
# phases, phase 0 on the pixel boundary; the eps pattern advances every 8 groups.
SERPV = [
    "n=min(floor(sample*511+0.5),199);",
    "g=floor(n*0.25);",
    "m=n-4*g;",
    "ph=g-8*floor(g*0.125);",
    "ev=floor(g*0.125);",
    "ev=ev-5*floor(ev*0.2);",
    "eps=if(equal(ev,1),0.25,if(equal(ev,2),-0.25,if(equal(ev,3),2,if(equal(ev,4),-2,0))));",
    "xx=20+6*g+ph*0.125;",
    "x=if(equal(m,0),xx+5,if(equal(m,3),xx-5+eps,xx))/320;",
    "y=if(equal(m,1),0.5+80/320,if(equal(m,2),0.5-80/320,0.5));",
]
PROBES["ov_serp_v"] = header() + wave(0, per_point=SERPV)

# the pseudo-random hash of Flexi + Martin - astral projection (decimal literals beyond int32 wrap through `atoi`, `%` on
# int64): x/y of 512 dots straight from the chain, so a mismatch is the expression semantics and not the geometry
HASH = [
    "xx=((sample*0983624912364)%10000000+100)/10000000;",
    "yy=((xx*1896575575)%10000000+100)/10000000;",
    "x=xx;",
    "y=yy;",
]
PROBES["ov_hash"] = header() + wave(0, dots=1, per_point=HASH)

# ---- AA lines: shape outlines (GL_LINE_LOOP + GL_LINE_SMOOTH) ----------------------------------------------------
AA_FRAME = ["rad=0.12+0.03*instance;", "ang=instance*0.37;", "x=0.5+0.08*sin(instance*1.7);", "y=0.5+0.08*cos(instance*2.3);"]
PROBES["ov_aa"] = (header()
                   + shape(0, sides=3, num_inst=8, rad=0.4, rgba=(0, 0, 0, 0), rgba2=(0, 0, 0, 0), border=(1, 1, 1, 1),
                           per_frame=AA_FRAME)
                   + shape(1, sides=5, num_inst=8, rad=0.4, rgba=(0, 0, 0, 0), rgba2=(0, 0, 0, 0), border=(1, 1, 1, 1),
                           per_frame=AA_FRAME)
                   + shape(2, sides=7, num_inst=8, rad=0.4, rgba=(0, 0, 0, 0), rgba2=(0, 0, 0, 0), border=(1, 1, 1, 1),
                           per_frame=AA_FRAME)
                   + shape(3, sides=100, num_inst=8, rad=0.4, rgba=(0, 0, 0, 0), rgba2=(0, 0, 0, 0),
                           border=(1, 1, 1, 1), per_frame=AA_FRAME))
PROBES["ov_aa_thick"] = PROBES["ov_aa"].replace("thickOutline=0", "thickOutline=1")
PROBES["ov_aa_alpha"] = PROBES["ov_aa"].replace("border_a=1.000000", "border_a=0.500000")
# one big triangle: for fitting the coverage function
PROBES["ov_aa_one"] = header() + shape(0, sides=3, num_inst=1, rad=0.45, ang=0.3, rgba=(0, 0, 0, 0), rgba2=(0, 0, 0, 0),
                                       border=(1, 1, 1, 1))

# ---- custom shapes: orientation, fill, texture ---------------------------------------------------------------------
# y orientation: small square at (0.25, 0.15) (x right, y expected to be UP in milkdrop => lower-left part of the image)
PROBES["ov_shape_orient"] = header() + shape(0, sides=4, num_inst=1, x=0.25, y=0.15, rad=0.1, ang=0.0,
                                             rgba=(1, 0, 0, 1), rgba2=(0, 1, 0, 1))
PROBES["ov_shape_fill"] = header() + shape(0, sides=7, num_inst=1, x=0.5, y=0.5, rad=0.3, ang=0.3,
                                           rgba=(1, 0.5, 0.2, 0.8), rgba2=(0.2, 0.5, 1, 0.3), border=(0, 1, 0, 0.5))
PROBES["ov_shape_add"] = header() + shape(0, sides=5, num_inst=1, additive=1, rad=0.3, rgba=(1, 0.5, 0.2, 0.8),
                                          rgba2=(0.2, 0.5, 1, 0.3), border=(0, 1, 0, 0.5))
PROBES["ov_shape_inst"] = header() + shape(0, sides=6, num_inst=12, rad=0.1, rgba=(1, 1, 0, 0.7), rgba2=(0, 1, 1, 0.7),
                                           border=(1, 0, 1, 0.8),
                                           per_frame=["x=0.5+0.3*cos(instance*0.5236);", "y=0.5+0.3*sin(instance*0.5236);",
                                                      "ang=instance*0.2;", "r=instance/11;", "rad=0.06+0.01*instance;"])
PROBES["ov_shape_thick"] = header() + shape(0, sides=9, thick=1, rad=0.3, rgba=(0, 0, 0, 0), rgba2=(0, 0, 0, 0),
                                            border=(1, 1, 1, 1))
# textured: needs an image in the previous frame; draw a gradient with a first shape then a textured one that samples
# the previous frame.  decay 1 (the feedback keeps the picture), warp identity.
TEX_BASE = header(decay=1.0) + shape(0, sides=4, num_inst=1, x=0.3, y=0.4, rad=0.18, ang=0.0, rgba=(1, 0, 0, 1),
                                     rgba2=(1, 1, 0, 1), border=(0, 0, 1, 1)) \
    + shape(1, sides=3, num_inst=1, x=0.7, y=0.65, rad=0.2, ang=0.5, rgba=(0, 1, 0, 1), rgba2=(0, 0, 1, 1),
            border=(1, 1, 1, 1))
PROBES["ov_shape_tex"] = TEX_BASE + shape(2, sides=6, num_inst=1, textured=1, x=0.5, y=0.5, rad=0.25, ang=0.2,
                                          tex_ang=0.4, tex_zoom=1.7, rgba=(1, 1, 1, 1), rgba2=(1, 1, 1, 1),
                                          per_frame=[])
PROBES["ov_shape_tex2"] = TEX_BASE + shape(2, sides=5, num_inst=1, textured=1, additive=1, x=0.5, y=0.45, rad=0.3,
                                           ang=0.0, tex_ang=1.1, tex_zoom=0.8, rgba=(1, 0.8, 0.6, 0.6),
                                           rgba2=(0.6, 0.8, 1, 0.9), per_frame=[])

# ---- blur interaction (projectM `BlurTexture::Update`): only presets whose shaders sample blur are affected ----------
# (1) frame 0: the update that allocates the blur textures leaves FBO 0 bound -> every overlay of that frame is lost;
# (2) the update leaves its linear/clamp sampler bound on unit 0 -> the *first* shape draw, if textured, uses it.
# `_blur` twins add a composite shader that samples blur1; they are otherwise identical to the plain probe.
TEXFIRST = header(decay=1.0) + shape(0, sides=6, num_inst=1, textured=1, x=0.5, y=0.5, rad=0.25, ang=0.2, tex_ang=0.4,
                                     tex_zoom=0.6, rgba=(1, 1, 1, 1), rgba2=(1, 1, 1, 1), per_frame=[]) \
    + shape(1, sides=4, num_inst=1, x=0.3, y=0.4, rad=0.18, ang=0.0, rgba=(1, 0, 0, 1), rgba2=(1, 1, 0, 1),
            border=(0, 0, 1, 1)) \
    + shape(2, sides=3, num_inst=1, x=0.7, y=0.65, rad=0.2, ang=0.5, rgba=(0, 1, 0, 1), rgba2=(0, 0, 1, 1),
            border=(1, 1, 1, 1))
BLUR_COMP = "\n".join(["comp_1=`shader_body {", "comp_2=`  ret = tex2D(sampler_main, uv).xyz * 0.5 + GetBlur1(uv) * 0.5;",
                       "comp_3=`}", ""])


def with_blur(text):
    text = text.replace("MILKDROP_PRESET_VERSION=100", "MILKDROP_PRESET_VERSION=201", 1)
    return text.replace("PSVERSION=0", "PSVERSION=2\nPSVERSION_WARP=2\nPSVERSION_COMP=2", 1) + BLUR_COMP


PROBES["ov_texfirst"] = TEXFIRST
PROBES["ov_texfirst_blur"] = with_blur(TEXFIRST)
PROBES["ov_shapes_blur"] = with_blur(header(decay=1.0) + shape(0, sides=4, x=0.3, y=0.4, rad=0.18, rgba=(1, 0, 0, 1),
                                                                rgba2=(1, 1, 0, 1), border=(0, 0, 1, 1)))
PROBES["ov_shapes_plain"] = header(decay=1.0) + shape(0, sides=4, x=0.3, y=0.4, rad=0.18, rgba=(1, 0, 0, 1),
                                                       rgba2=(1, 1, 0, 1), border=(0, 0, 1, 1))

# ---- custom waves ------------------------------------------------------------------------------------------------
PROBES["ov_wave_val"] = header() + wave(0, scaling=1.0, smoothing=0.0,
                                        per_point=["x=0.5+value1*2;", "y=0.5+value2*2+0.0*sample;"])
PROBES["ov_wave_val_sm"] = header() + wave(0, scaling=2.0, smoothing=0.7,
                                           per_point=["x=sample;", "y=0.5+value1;"])
PROBES["ov_wave_spec"] = header() + wave(0, spectrum=1, scaling=1.0, smoothing=0.3,
                                         per_point=["x=sample;", "y=0.3+value1*0.6;"])
PROBES["ov_wave_spec_sep"] = header() + wave(0, spectrum=1, sep=64, samples=200, scaling=1.0, smoothing=0.0,
                                             per_point=["x=sample;", "y=0.3+value1*0.6;"])
PROBES["ov_wave_n64"] = header() + wave(0, samples=64, scaling=1.5, smoothing=0.2,
                                        per_point=["x=sample;", "y=0.5+value1*1.0;", "r=sample;", "g=1-sample;", "b=0.5;"])
PROBES["ov_wave_big"] = header() + wave(0, samples=512, scaling=1.5, smoothing=0.2, per_point=["x=sample;", "y=0.5+value1*1.0;"])

# ---- motion vectors ------------------------------------------------------------------------------------------------
# warp: zoom + rot so that the lookup of the previous UV grid is non trivial
PROBES["ov_mv"] = header(decay=1.0, zoom=1.04, rot=0.03, mv=(24, 18, 0, 0, 1.0, 1.0))
PROBES["ov_mv_len"] = header(decay=1.0, zoom=1.04, rot=0.03, mv=(12.5, 9.3, 0.02, -0.01, 3.0, 0.8))
PROBES["ov_mv_max"] = header(decay=1.0, zoom=1.0, rot=0.0, mv=(64, 48, 0, 0, 0.0, 1.0))
PROBES["ov_mv_bw"] = header(decay=1.0, zoom=1.02, rot=-0.02, mv=(30, 20, 0, 0, 5.0, 0.6), texwrap=0) + \
    shape(0, sides=4, num_inst=1, x=0.5, y=0.5, rad=0.3, rgba=(0.2, 0.2, 0.9, 1), rgba2=(0.9, 0.2, 0.2, 1))
# "per_frame" variants: moving centre so that dx/dy of the warp vary (static here)
PROBES["ov_mv"] += "per_frame_1=warp = 0;\n"
PROBES["ov_mv_len"] += "per_frame_1=warp = 0;\n"

if __name__ == "__main__":
    names = []
    for name, text in PROBES.items():
        with open(os.path.join(HERE, name + ".milk"), "w") as f:
            f.write(text)
        names.append(name)
    with open(os.path.join(HERE, "ov_list.txt"), "w") as f:
        for n in names:
            f.write(f"scripts/probes/{n}.milk\n")
    print(f"wrote {len(names)} probes to {HERE}")
