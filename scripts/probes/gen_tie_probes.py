#!/usr/bin/env python3
"""Exact-tie probes for the GL line-strip rule (`StrokeRaster::Emulated`, `crates/lactea-render/src/stroke.rs`).

Each probe preset draws four custom waves of four collinear points on the 1/256-pixel grid (the grid the GPU snaps
vertices to), arranged so that the line crosses a pixel boundary *exactly* at a pixel centre of the major axis. The
`SmoothWave` mid vertices keep the collinearity, the tie lies either on a vertex (frac 1/2) or inside a sub-segment
(frac 1/4, 3/4). One pixel per column (x-major) or row (y-major) is lit, so the oracle's choice between the two
candidate pixels is read off the image.

  scripts/probes/gen_tie_probes.py gen  OUTDIR [--random N] [--seed S]    # write OUTDIR/tieNNN.milk + tests.json
  scripts/probes/gen_tie_probes.py run  OUTDIR                            # oracle + fdrender at 320x180, compare

`run` needs `oracle/build/refrender` (patched projectM; `LD_LIBRARY_PATH` is set to `oracle/build/install/lib`) and
`FD_TARGET/release/fdrender` (default `target-engine`). Without `--random` the dyadic grid `q x slope x direction`
(260 tests) is generated; with it random non-dyadic steps and slopes. Result of the measurement (860 tests, both
modes): every tie of

  * an x-major segment (`|dx| >= |dy|`) that rises to the right (image `dy` and `dx` of opposite sign) goes to the
    upper row (`floor(y) - 1`); falling or flat x-major segments to `floor(y)`;
  * a y-major segment goes to the left column (`floor(x) - 1`), whatever its slope,

independent of the drawing direction, step length and tie position on the segment.
"""
import argparse, json, os, random, subprocess, sys
from fractions import Fraction

W, H = 320, 180
ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
TEMPLATE = os.path.join(ROOT, "scripts", "probes", "ov_line.milk")


def fmt(v):
    return repr(float(v))


def wave_block(i, xa, xb, ya, yb):
    return [f"wavecode_{i}_enabled=1", f"wavecode_{i}_samples=4", f"wavecode_{i}_sep=0", f"wavecode_{i}_bSpectrum=0",
            f"wavecode_{i}_bUseDots=0", f"wavecode_{i}_bDrawThick=0", f"wavecode_{i}_bAdditive=0",
            f"wavecode_{i}_scaling=1.000000", f"wavecode_{i}_smoothing=0.500000",
            f"wavecode_{i}_r=1.000000", f"wavecode_{i}_g=1.000000", f"wavecode_{i}_b=1.000000", f"wavecode_{i}_a=1.000000",
            f"wave_{i}_per_point1=n=floor(sample*3+0.5);",
            f"wave_{i}_per_point2=x={fmt(xa)}+n*({fmt(xb)});",
            f"wave_{i}_per_point3=y={fmt(ya)}+n*({fmt(yb)});"]


def px_to_x(xpx):
    return Fraction(xpx) / W


def row_to_y(rowpx):  # projectM custom wave: ndc_y = (-2y + 1) * invAspectY, invAspectY = W/H on a landscape target
    return Fraction(1, 2) + (Fraction(rowpx) - Fraction(H, 2)) / W


def make_tests(random_n, seed):
    tests = []
    rnd = random.Random(seed)
    if random_n:
        for _ in range(random_n):
            q = 2 * rnd.randint(10, 350)  # step 2q, a multiple of 4
            minor = 4 * rnd.randint(-(q // 2 - 1), q // 2 - 1) if rnd.random() < 0.9 else 0
            tests.append(dict(axis=rnd.choice("XY"), q=q, minor=minor, rev=rnd.randint(0, 1),
                              frac=str(rnd.choice((Fraction(1, 4), Fraction(3, 4), Fraction(1, 2))))))
        return tests
    slopes = [Fraction(0)] + [Fraction(s, d) * sg for s, d in ((1, 64), (1, 16), (1, 4), (1, 2), (3, 4), (1, 1)) for sg in (1, -1)]
    for axis in "XY":
        for q in (16, 48, 128, 320, 768):
            for s in slopes:
                for rev in (0, 1):
                    minor = int(s * 2 * q)
                    minor += (minor % 2) * (1 if minor > 0 else -1)
                    tests.append(dict(axis=axis, q=q, minor=minor, rev=rev, frac="1/2"))
    return tests


def gen(out, random_n, seed):
    os.makedirs(out, exist_ok=True)
    header = open(TEMPLATE).read().splitlines()[:65]
    tests = make_tests(random_n, seed)
    presets = []
    for k in range(0, len(tests), 4):
        lines, meta = list(header), []
        for w, t in enumerate(tests[k:k + 4]):
            ic, j = 40 + 80 * w, 60 + 15 * (k // 4 % 6)  # tie column (or integer column boundary) / tie row
            q, minor, frac = t["q"], t["minor"], Fraction(t["frac"])
            if t["axis"] == "X":
                dx, dy = 2 * q, minor
                xs, ys = ic * 256 + 128 - int(frac * dx), 256 * j - int(frac * dy)
            else:
                dx, dy = minor, 2 * q
                xs, ys = 256 * ic - int(frac * dx), j * 256 + 128 - int(frac * dy)
            if t["rev"]:
                xa, xb, ya, yb = xs + 2 * dx, -dx, ys + 2 * dy, -dy
            else:
                xa, xb, ya, yb = xs - dx, dx, ys - dy, dy
            lines += wave_block(w, px_to_x(Fraction(xa, 256)), px_to_x(Fraction(xb, 256)),
                                row_to_y(Fraction(ya, 256)), Fraction(yb, 256) / W)
            meta.append(dict(t, dx=dx, dy=dy, wave=w, Ic=ic, J=j))
        name = f"tie{k // 4:03d}"
        open(f"{out}/{name}.milk", "w").write("\n".join(lines) + "\n")
        presets.append(dict(name=name, tests=meta))
    json.dump(presets, open(f"{out}/tests.json", "w"))
    print(len(tests), "tests in", len(presets), "presets ->", out)


def run(out):
    import numpy as np
    from PIL import Image
    fd = os.path.join(ROOT, os.environ.get("FD_TARGET", "target-engine"), "release", "fdrender")
    ref = os.path.join(ROOT, "oracle", "build", "refrender")
    env = dict(os.environ, LD_LIBRARY_PATH=os.path.join(ROOT, "oracle", "build", "install", "lib"))
    audio = os.path.join(ROOT, "fixtures", "audio", "synth128.wav")
    presets = json.load(open(f"{out}/tests.json"))
    bad = total = 0
    for p in presets:
        imgs = {}
        for tag, exe in (("ref", ref), ("our", fd)):
            d = f"{out}/{tag}_{p['name']}"
            subprocess.run([exe, "--preset", f"{out}/{p['name']}.milk", "--audio", audio, "--out", d, "--size", f"{W}x{H}",
                            "--mesh", "48x36", "--frames", "2", "--stages", "main"], capture_output=True, env=env, check=True)
            imgs[tag] = np.array(Image.open(f"{d}/main/00001.png").convert("L"))
        for t in p["tests"]:
            x0 = t["Ic"] - 40
            a, b = imgs["ref"][:, x0:x0 + 80], imgs["our"][:, x0:x0 + 80]
            total += 1
            if (a != b).any():
                bad += 1
                print(p["name"], t, "differing pixels:", int((a != b).sum()))
    print(f"{total - bad}/{total} tie probes identical")
    return bad


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=("gen", "run"))
    ap.add_argument("out")
    ap.add_argument("--random", type=int, default=0)
    ap.add_argument("--seed", type=int, default=1)
    a = ap.parse_args()
    if a.mode == "gen":
        gen(a.out, a.random, a.seed)
    else:
        sys.exit(1 if run(a.out) else 0)
