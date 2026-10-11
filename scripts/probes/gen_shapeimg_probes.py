#!/usr/bin/env python3
"""Generate the custom-shape `image=` probes (projectM only: `shapecode_N_image`, `CustomShape.cpp:154-175`).

  scripts/probes/gen_shapeimg_probes.py [TEXDIR]  # writes scripts/probes/si_*.milk, si_list.txt and TEXDIR

Every probe is a v1 (no shader) preset with decay 0, no main wave and no borders, so the `main` stage of frame n
contains only the shapes (the textured fans sample a pack texture, never the previous frame). The probes that need
an alpha texture use `si_alpha.png`, which is written into TEXDIR next to symlinks to every file of the projectM
texture pack (the oracle and fdrender both take a single `--texture-dir`).

Reference + comparison (320x180, 10 frames; FD_TARGET = your cargo target dir):

  TEX=out/si_tex; scripts/probes/gen_shapeimg_probes.py $TEX
  oracle/run_corpus.sh shapeimg scripts/probes/si_list.txt -n 10 -j 2 -- --size 320x180 \\
      --stages final,main,warped --vars --textures --texture-dir $TEX
  FD_TARGET=target-p2 scripts/fdq.py shapeimg si_ --modes step --first 0 --stages final,main --texture-dir $TEX

What each probe pins down (the pack textures are JPEGs, i.e. alpha 255; `si_alpha.png` has a 4-band alpha ramp):

  si_filter  fw_/fc_/pw_/pc_ prefixes, wrap vs clamp visible (tex_zoom < 1), nearest vs linear (tex_zoom > 1)
  si_prefix  cf_/wp_/cp_ aliases, unknown `xx_` prefix (stripped, linear + repeat), plain name
  si_name    placeholder paths: `clouds.jpg` (extension is part of the name -> not found), a missing name, a
             mixed-case `FW_Lichen` (found); a 1x1 black opaque placeholder on a grey backdrop
  si_blend   alpha / additive blending of a textured fan with vertex alpha, a texture with alpha, nearest + clamp
  si_rot     tex_ang / tex_zoom, 3 / 7 / 100 sides (the aspect of the texture coordinates is 1), per-frame code
             and instances, a plain upper-case name
  si_nearest 4 slots x 8 instances of nearest-filtered fans near 1:1 texel scale (texture-coordinate rounding, ties)
  si_misc    `image=` on an untextured shape (ignored), the built-in noise textures by name, `textured` toggled by
             per-frame code
  si_mix     two image shapes, then a textured shape without `image=` (it samples the main image with the texture's own
             nearest/repeat parameters once the image shape's sampler is unbound), then another image shape
  si_blur    si_mix as a v2 preset whose composite shader samples blur (the blur sampler leak must not reach a shape
             that binds its own sampler, but does reach the first shape without one)
"""
import os
import sys

from gen_overlay_probes import HERE, header, shape

ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
PACK = os.path.join(ROOT, "oracle", "build", "texpack", "textures")

# a big grey square (the backdrop that makes a black placeholder / an additive fan visible): 4 sides = axis aligned
BACKDROP = dict(sides=4, num_inst=1, x=0.5, y=0.5, rad=2.6, rgba=(0.35, 0.5, 0.35, 1), rgba2=(0.35, 0.5, 0.35, 1),
                border=(0, 0, 0, 0))


def ishape(i, image=None, **kw):
    """`shape()` plus the projectM `image=` key (every shape of these probes is textured unless told otherwise)."""
    kw.setdefault("textured", 1)
    s = shape(i, **kw)
    if image is not None:
        s += f"shapecode_{i}_image={image}\n"
    return s


# 2x2 layout on a 16:9 target: centres 0.28 / 0.72, radius 0.35 (63 px across at 320x180)
POS = [dict(x=0.28, y=0.28), dict(x=0.72, y=0.28), dict(x=0.28, y=0.72), dict(x=0.72, y=0.72)]
PROBES = {}

PROBES["si_filter"] = header() + "".join([
    ishape(0, "fw_clouds", sides=4, rad=0.35, tex_zoom=0.45, **POS[0]),
    ishape(1, "fc_clouds", sides=4, rad=0.35, tex_zoom=0.45, **POS[1]),
    ishape(2, "pw_lichen", sides=6, rad=0.35, tex_zoom=4.0, tex_ang=0.3, **POS[2]),
    ishape(3, "pc_lichen", sides=6, rad=0.35, tex_zoom=0.45, tex_ang=0.3, **POS[3]),
])
PROBES["si_prefix"] = header() + "".join([
    ishape(0, "cf_clouds", sides=5, rad=0.35, tex_zoom=0.45, **POS[0]),
    ishape(1, "wp_lichen", sides=5, rad=0.35, tex_zoom=3.0, **POS[1]),
    ishape(2, "xx_clouds", sides=5, rad=0.35, tex_zoom=0.45, **POS[2]),
    ishape(3, "cp_wrenches", sides=5, rad=0.35, tex_zoom=0.45, **POS[3]),
])

PROBES["si_name"] = header() + "".join([
    ishape(0, None, textured=0, **BACKDROP),
    ishape(1, "clouds.jpg", sides=4, rad=0.5, tex_zoom=1.0, x=0.2, y=0.5),
    ishape(2, "nosuchtex_zz", sides=4, rad=0.5, tex_zoom=1.0, x=0.5, y=0.5),
    ishape(3, "FW_Lichen", sides=4, rad=0.5, tex_zoom=1.0, x=0.8, y=0.5),
])

PROBES["si_blend"] = header() + "".join([
    ishape(0, None, textured=0, **BACKDROP),
    ishape(1, "si_alpha", sides=6, rad=0.5, tex_zoom=1.0, rgba=(1, 1, 1, 0.8), rgba2=(1, 0.5, 0.5, 0.4), x=0.25, y=0.5),
    ishape(2, "lichen", sides=6, rad=0.5, tex_zoom=1.3, additive=1, rgba=(1, 0.8, 0.6, 0.6), rgba2=(0.6, 0.8, 1, 0.9),
           x=0.5, y=0.5, ang=0.4),
    ishape(3, "pc_si_alpha", sides=6, rad=0.5, tex_zoom=0.6, rgba=(0.3, 1, 0.3, 1), rgba2=(1, 1, 0.3, 0.7), x=0.75,
           y=0.5, tex_ang=0.5),
])

PROBES["si_rot"] = header() + "".join([
    ishape(0, "clouds", sides=3, rad=0.5, tex_zoom=0.8, tex_ang=0.7, ang=0.3, x=0.2, y=0.3),
    ishape(1, "sunrise", sides=7, rad=0.4, tex_zoom=1.7, tex_ang=-1.3, x=0.55, y=0.3),
    ishape(2, "CLOUDS", sides=100, rad=0.35, tex_zoom=1.0, tex_ang=2.5, x=0.8, y=0.75),
    ishape(3, "wrenches", sides=5, num_inst=3, rad=0.22, tex_zoom=1.0, x=0.2, y=0.75,
           per_frame=["x=0.15+instance*0.12;", "tex_ang=time*0.7+instance;", "tex_zoom=1+0.5*sin(time*2+instance);",
                      "y=0.7+0.1*instance;"]),
])

# nearest-filtered fans, 8 instances per slot with different positions / angles / zooms near 1:1 texel scale: many
# pixels sample close to a texel boundary, so the f32 evaluation order of the texture coordinates and the
# rasteriser's interpolation show up (every pixel is either the right texel or its neighbour)


def nearest_slot(i, image, sides):
    return ishape(i, image, sides=sides, num_inst=8, rad=0.17, tex_zoom=1.0, x=0.5, y=0.5,
                  per_frame=[f"x=0.12+instance*0.108+{0.01 * i};", f"y=0.12+{0.25 * i}+0.05*sin(instance*1.9);",
                             f"tex_ang=instance*0.41+{0.3 * i};", "tex_zoom=0.55+instance*0.19;", "ang=instance*0.5;"])


PROBES["si_nearest"] = header() + "".join([nearest_slot(0, "pw_lichen", 6), nearest_slot(1, "pc_clouds", 4),
                                           nearest_slot(2, "pw_sunrise", 5), nearest_slot(3, "pc_wrenches", 3)])

PROBES["si_misc"] = header() + "".join([
    ishape(0, "clouds", textured=0, sides=5, rad=0.35, rgba=(1, 0.3, 0.3, 1), rgba2=(0.3, 0.3, 1, 1), **POS[0]),
    ishape(1, "noise_lq", sides=4, rad=0.35, tex_zoom=1.0, **POS[1]),
    ishape(2, "fc_noise_mq", sides=4, rad=0.35, tex_zoom=0.5, **POS[2]),
    ishape(3, "clouds", textured=0, sides=4, rad=0.35, tex_zoom=1.0, per_frame=["textured=frame%3;"], **POS[3]),
])

# v2 preset whose composite shader samples blur (BlurTexture::Update leaves its linear/clamp sampler on unit 0 for the first
# shape draw; a shape with an image binds its own sampler, so nothing may leak)
BLUR_COMP = "\n".join(["comp_1=`shader_body {", "comp_2=`  ret = tex2D(sampler_main, uv).xyz * 0.5 + GetBlur1(uv) * 0.5;",
                       "comp_3=`}", ""])


def with_blur(text):
    text = text.replace("MILKDROP_PRESET_VERSION=100", "MILKDROP_PRESET_VERSION=201", 1)
    return text.replace("PSVERSION=0", "PSVERSION=2\nPSVERSION_WARP=2\nPSVERSION_COMP=2", 1) + BLUR_COMP


SI_MIX = header() + "".join([
    ishape(0, "pw_lichen", sides=6, rad=0.35, tex_zoom=4.0, tex_ang=0.3, **POS[0]),
    ishape(1, "fw_clouds", sides=6, rad=0.35, tex_zoom=0.45, **POS[1]),
    ishape(2, None, sides=6, rad=0.35, tex_zoom=1.0, **POS[2]),   # samples the main image after two image shapes
    ishape(3, "pc_lichen", sides=6, rad=0.35, tex_zoom=0.45, tex_ang=0.3, **POS[3]),
])
PROBES["si_mix"] = SI_MIX
PROBES["si_blur"] = with_blur(SI_MIX)


def alpha_png(path):
    """64x64 RGBA: red grows to the right, green downwards, blue = 8 px checker, alpha = 4 horizontal bands
    (255, 170, 85, 30); a white top-left corner marker makes the vertical orientation unambiguous."""
    from PIL import Image
    im = Image.new("RGBA", (64, 64))
    px = im.load()
    for y in range(64):
        for x in range(64):
            a = (255, 170, 85, 30)[x // 16]
            px[x, y] = (x * 4, y * 4, 255 if ((x // 8 + y // 8) & 1) else 0, a)
    for y in range(8):
        for x in range(8):
            px[x, y] = (255, 255, 255, 255)
    im.save(path)


def make_texdir(texdir):
    os.makedirs(texdir, exist_ok=True)
    for f in sorted(os.listdir(PACK)):
        dst = os.path.join(texdir, f)
        if not os.path.lexists(dst):
            os.symlink(os.path.join(PACK, f), dst)
    alpha_png(os.path.join(texdir, "si_alpha.png"))


if __name__ == "__main__":
    for name, text in PROBES.items():
        with open(os.path.join(HERE, name + ".milk"), "w") as f:
            f.write(text)
    with open(os.path.join(HERE, "si_list.txt"), "w") as f:
        for n in PROBES:
            f.write(f"scripts/probes/{n}.milk\n")
    if len(sys.argv) > 1:
        make_texdir(sys.argv[1])
    print(f"wrote {len(PROBES)} probes to {HERE}")
