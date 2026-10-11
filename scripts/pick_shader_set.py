#!/usr/bin/env python3
"""Pick a diverse set of v2 (shader) presets by greedy feature cover (used for oracle set `compshader-v2`).

  scripts/pick_shader_set.py [N=40] [--exclude oracle/starter_set.txt] > oracle/compshader_v2_set.txt

Features are the ones of oracle/classify_presets.py plus shader-token features (tex2Dlod, tex2Dbias, sampler prefixes,
GetPixel, rot_*, mip registers, hue_shader, ...). Presets with huge shaders or per-frame code that is likely to render
black are not filtered here: check `summary.tsv` after rendering.
"""
import glob, os, re, sys, random
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)) + "/../oracle")
import classify_presets as cp

N = 40
excl = set()
args = sys.argv[1:]
i = 0
while i < len(args):
    if args[i] == "--exclude":
        for line in open(args[i + 1]):
            if line.strip() and not line.startswith("#"):
                excl.add(os.path.basename(line.split("\t")[0]))
        i += 2
    else:
        N = int(args[i]); i += 1

TOK = {
    "tex2dlod": r"tex2dlod", "tex2dbias": r"tex2dbias", "tex2dproj": r"tex2dproj", "tex3d": r"tex3d",
    "pc_": r"sampler_pc_", "pw_": r"sampler_pw_", "fc_": r"sampler_fc_", "fw_": r"sampler_fw_",
    "getpixel": r"getpixel", "lum": r"\blum\(", "uv_orig": r"uv_orig", "ang": r"\bang\b", "rad": r"\brad\b",
    "hue_shader": r"hue_shader", "mip": r"mip_", "fmod": r"\bfmod\b", "matrix": r"float3x3|float4x4|mul\(",
    "ddx": r"\bddx\b|\bddy\b", "frac": r"\bfrac\b", "pow": r"\bpow\b", "atan2": r"atan2", "dot": r"\bdot\(",
    "texsize_main": r"texsize\b", "texsize_noise": r"texsize_noise", "texsize_blur": r"texsize_blur",
    "blurmin": r"blur\d_(min|max)", "if": r"\bif\b", "for": r"\bfor\b", "static": r"\bstatic\b",
    "sampler_main_warp": r"sampler_main", "gamma": r"\bgamma\b", "mul3": r"mul\(.*rot_",
}

cands = []
seen = set()
for p in sorted(glob.glob(cp.ROOT + "/**/*.milk", recursive=True)):
    if os.path.basename(p) in excl:
        continue
    kv = cp.parse(p)
    warp = cp.shader_text(kv, "warp_")
    comp = cp.shader_text(kv, "comp_")
    ver = int(cp.num(kv, "MILKDROP_PRESET_VERSION", 0))
    if ver < 200 or not (warp.strip() or comp.strip()):
        continue
    f = set(x for x in cp.features(p) if not x.startswith("size:") and not x.startswith("wave_mode"))
    low = (warp + "\n" + comp).lower()
    for k, rx in TOK.items():
        if re.search(rx, low):
            f.add("tok:" + k)
    f.add("shape:%s" % ("warp+comp" if warp.strip() and comp.strip() else "warp" if warp.strip() else "comp"))
    size = len(warp) + len(comp)
    if size > 6000:
        continue
    # overlays (custom shapes/waves, motion vectors) are covered by other sets and would pollute `main`
    if f & {"custom_shape", "custom_wave", "motion_vectors"}:
        continue
    if os.path.basename(p) in seen:
        continue
    seen.add(os.path.basename(p))
    cands.append((p, f, size))

random.seed(7)
random.shuffle(cands)
freq = {}
for _, f, _ in cands:
    for x in f:
        freq[x] = freq.get(x, 0) + 1
covered = {}
chosen = []
for _ in range(N):
    best = None
    for c in cands:
        if c in chosen:
            continue
        # rare features are worth more; features already covered twice are worth nothing
        score = sum(1.0 / freq[x] ** 0.5 for x in c[1] if covered.get(x, 0) < 2) - 1e-6 * c[2]
        if best is None or score > best[0]:
            best = (score, c)
    if best is None:
        break
    c = best[1]
    chosen.append(c)
    for x in c[1]:
        covered[x] = covered.get(x, 0) + 1
for p, f, size in chosen:
    print("%s\t%s" % (p, ",".join(sorted(f))))
