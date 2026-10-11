#!/usr/bin/env python3
"""Generate the "random slanted segment" probes (custom-wave polylines and shape outlines at irrational positions).

  scripts/probes/gen_slant_probes.py          # writes scripts/probes/sl_*.milk and sl_list.txt

Every probe is a v1 preset with decay 0 and nothing but the probed primitive in `main` (like `gen_overlay_probes.py`).
The point positions come from per-point / per-frame code with irrational constants, so the f64 value of every `x` / `y`
has no short f32 form: the segments are slanted at all angles, start at all sub-pixel phases and the position rounding
of the engine (f64 VM value -> clip space) decides the 1/256-px snap of every end point.

  oracle/run_corpus.sh slant scripts/probes/sl_list.txt -n 12 -j 2 -- --size 320x180 --stages final,main,warped --vars
  FD_TARGET=target-p2 scripts/fdq.py slant sl_ --modes step --first 0 --stages final,main

Root cause this probe pinned down (ORB - Liquid Fire, corpus-s400 main min 56.7 dB, one pixel lit white): the engine handed
custom-wave points / shape centres to the renderer as `f32` and computed `(x*2-1)*invAspect` in `f32`, projectM computes it in
double (`static_cast<float>((x*2.0-1.0) * invAspectX)`). The rasteriser itself (stroke.rs) agrees with the GPU on the
offending segment (checked with `oracle/probes/line_probe.c`); a 1-ulp difference of an end point flips the 1/256 snap of a
vertex that sits on a rounding tie, and with it a pixel of a segment that passes 0.0008 px from a row boundary.
"""
import os

from gen_overlay_probes import HERE, header, shape, wave

PROBES = {}

# a wobbling closed-ish curve: every sample moves by a different amount in a different direction
WOBBLE = [
    "ph = sample * {k1} + {p} * sin(sample * {k2} + {p2} + time * 1.3) + time * 0.9;",
    "x = 0.5 + 0.46 * sin(ph) * cos(sample * {k3} + time * 0.37);",
    "y = 0.5 + 0.44 * cos(ph * 1.0731) * sin(sample * {k4} + {p} + time * 0.53);",
]


def wobble(k1, k2, k3, k4, p, p2):
    return [l.format(k1=k1, k2=k2, k3=k3, k4=k4, p=p, p2=p2) for l in WOBBLE]


def four_waves(thick, additive=0, alpha=1.0):
    cfg = [(37.1, 11.7, 5.3, 23.9, 2.3, 0.4), (41.3, 7.9, 3.1, 29.7, 1.1, 1.9), (29.9, 13.3, 7.7, 17.3, 0.7, 2.7),
           (53.7, 5.1, 9.1, 31.1, 1.7, 0.9)]
    return "".join(wave(i, samples=512, thick=thick, additive=additive, smoothing=0.0, rgba=(1, 1, 1, alpha),
                        per_point=wobble(*cfg[i])) for i in range(4))


PROBES["sl_wave"] = header() + four_waves(0)
PROBES["sl_wave_thick"] = header() + four_waves(1)
PROBES["sl_wave_add"] = header() + four_waves(1, additive=1, alpha=0.5)
PROBES["sl_wave_dots"] = header() + "".join(
    wave(i, samples=512, dots=1, smoothing=0.0, rgba=(1, 1, 1, 1), per_point=wobble(37.1 + i, 11.7, 5.3, 23.9, 2.3, 0.4))
    for i in range(4))

# shape outlines (GL_LINE_LOOP + GL_LINE_SMOOTH = diamond exit): 4 slots x 12 instances, centres / angles / radii from
# irrational per-instance code, thin and thick outline
SHAPE_FRAME = ["x = 0.5 + 0.38 * sin(instance * 2.399963 + {p} + time * 0.61);",
               "y = 0.5 + 0.38 * cos(instance * 1.618034 + {p} + time * 0.47);",
               "rad = 0.05 + 0.03 * sin(instance * 0.7311 + time);", "ang = instance * 0.6180339 + {p} + time * 0.3;"]


def outline_slot(i, thick):
    return shape(i, sides=3 + 2 * i, thick=thick, num_inst=12, rgba=(0, 0, 0, 0), rgba2=(0, 0, 0, 0),
                 border=(1, 1, 1, 1), per_frame=[l.format(p=0.37 * i) for l in SHAPE_FRAME])


PROBES["sl_outline"] = header() + "".join(outline_slot(i, 0) for i in range(4))
PROBES["sl_outline_thick"] = header() + "".join(outline_slot(i, 1) for i in range(4))

if __name__ == "__main__":
    for name, text in PROBES.items():
        with open(os.path.join(HERE, name + ".milk"), "w") as f:
            f.write(text)
    with open(os.path.join(HERE, "sl_list.txt"), "w") as f:
        for n in PROBES:
            f.write(f"scripts/probes/{n}.milk\n")
    print(f"wrote {len(PROBES)} probes to {HERE}")
