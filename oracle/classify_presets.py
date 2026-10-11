#!/usr/bin/env python3
"""Classify .milk presets by the pipeline features they exercise (used to pick oracle/starter_set.txt).

usage: classify_presets.py [DIR ...]          -> TSV: path, features...
       classify_presets.py --find FEATURE...  -> presets having all FEATUREs (features listed below)
"""
import glob
import os
import re
import sys

ROOT = "/usr/share/projectM/presets"


def parse(path):
    kv = {}
    code = {}
    with open(path, "r", errors="replace") as f:
        for line in f:
            line = line.rstrip("\r\n")
            if "=" in line:
                k, _, v = line.partition("=")
                kv[k.strip()] = v.strip()
    return kv


def num(kv, k, d=0.0):
    try:
        return float(kv.get(k, d))
    except ValueError:
        return d


def shader_text(kv, prefix):
    keys = sorted((int(k[len(prefix):]), k) for k in kv if re.fullmatch(prefix + r"\d+", k))
    return "\n".join(kv[k].lstrip("`") for _, k in keys)


def features(path):
    kv = parse(path)
    ver = int(num(kv, "MILKDROP_PRESET_VERSION", 0))
    warp = shader_text(kv, "warp_")
    comp = shader_text(kv, "comp_")
    f = set()
    if ver >= 200 and warp:
        f.add("warp_shader")
    if ver >= 200 and comp:
        f.add("comp_shader")
    if not ({"warp_shader", "comp_shader"} & f):
        f.add("v1")
    f.add("wave_mode_%d" % int(num(kv, "nWaveMode", 0)))
    if num(kv, "fVideoEchoAlpha") > 0.001:
        f.add("echo")
        f.add("echo_orient_%d" % int(num(kv, "nVideoEchoOrientation", 0)))
    if num(kv, "fGammaAdj", 1) > 1.001:
        f.add("gamma")
    for k, n in (("bBrighten", "brighten"), ("bDarken", "darken"), ("bSolarize", "solarize"), ("bInvert", "invert"),
                 ("bDarkenCenter", "darken_center")):
        if num(kv, k) > 0:
            f.add(n)
    if num(kv, "mv_a") > 0 and num(kv, "nMotionVectorsX", num(kv, "mv_x")) > 0:
        f.add("motion_vectors")
    if num(kv, "ob_size") > 0 and num(kv, "ob_a") > 0:
        f.add("outer_border")
    if num(kv, "ib_size") > 0 and num(kv, "ib_a") > 0:
        f.add("inner_border")
    if num(kv, "bWaveDots") > 0:
        f.add("wave_dots")
    if num(kv, "bWaveThick") > 0:
        f.add("wave_thick")
    if num(kv, "bAdditiveWaves") > 0:
        f.add("wave_additive")
    if num(kv, "bMaximizeWaveColor") > 0:
        f.add("wave_brighten")
    if num(kv, "fWaveAlpha") > 0.01:
        f.add("main_wave")
    if any(k.startswith("per_pixel_") for k in kv):
        f.add("per_pixel")
    if any(k.startswith("per_frame_") for k in kv):
        f.add("per_frame")
    waves = [i for i in range(8) if num(kv, "wavecode_%d_enabled" % i) > 0]
    shapes = [i for i in range(4) if num(kv, "shapecode_%d_enabled" % i) > 0]
    if waves:
        f.add("custom_wave")
    if shapes:
        f.add("custom_shape")
    for i in waves:
        p = "wavecode_%d_" % i
        if num(kv, p + "bSpectrum") > 0:
            f.add("cw_spectrum")
        if num(kv, p + "bUseDots") > 0:
            f.add("cw_dots")
        if num(kv, p + "bDrawThick") > 0:
            f.add("cw_thick")
        if num(kv, p + "bAdditive") > 0:
            f.add("cw_additive")
    for i in shapes:
        p = "shapecode_%d_" % i
        if num(kv, p + "textured") > 0:
            f.add("cs_textured")
        if num(kv, p + "num_inst", 1) > 1:
            f.add("cs_instanced")
        if num(kv, p + "additive") > 0:
            f.add("cs_additive")
        if num(kv, p + "thickOutline") > 0:
            f.add("cs_thick_outline")
        if num(kv, p + "sides", 4) != 4:
            f.add("cs_sides_%d" % int(num(kv, p + "sides", 4)))
    sh = warp + "\n" + comp
    shl = sh.lower()
    for lvl in (1, 2, 3):
        if re.search(r"getblur%d|sampler_blur%d" % (lvl, lvl), shl):
            f.add("blur%d" % lvl)
    for name in ("noise_lq_lite", "noise_lq", "noise_mq", "noise_hq", "noisevol_lq", "noisevol_hq"):
        if re.search(r"sampler_(?:fw_|fc_|pw_|pc_)?" + name + r"\b", shl):
            f.add(name)
    # user textures: any sampler_* that is not main/blur/noise
    for m in re.finditer(r"sampler_(?:(?:fw|fc|pw|pc)_)?([a-z0-9_]+)", shl):
        n = m.group(1)
        if n.startswith(("main", "blur", "noise")):
            continue
        f.add("rand_texture" if re.match(r"rand\d\d", n) else "user_texture")
        f.add("tex:" + n)
    if re.search(r"\bq([1-9]|[12]\d|3[0-2])\b", shl):
        f.add("shader_q")
    if "rot_" in shl:
        f.add("shader_rot")
    if re.search(r"\brand_(frame|preset)\b", shl):
        f.add("shader_rand")
    if re.search(r"\b(bass|mid|treb)(_att)?\b", shl):
        f.add("shader_audio")
    if "texsize" in shl:
        f.add("shader_texsize")
    if re.search(r"\b(hue_shader|roam_(cos|sin)|slow_roam)", shl):
        f.add("shader_roam")
    f.add("size:%d" % (len(warp) + len(comp)))
    return sorted(f)


def main():
    args = sys.argv[1:]
    if args and args[0] == "--find":
        want = set(args[1:])
        for p in sorted(glob.glob(ROOT + "/**/*.milk", recursive=True)):
            f = features(p)
            if want <= set(f):
                size = [x for x in f if x.startswith("size:")][0]
                print("%s\t%s" % (os.path.relpath(p, ROOT), size))
        return
    dirs = args or [ROOT]
    for d in dirs:
        for p in sorted(glob.glob(d + "/**/*.milk", recursive=True)):
            print(p + "\t" + ",".join(features(p)))


if __name__ == "__main__":
    main()
