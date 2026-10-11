#!/usr/bin/env python3
"""Generate comp-shader bisection probes from a real preset.

  scripts/probes/gen_comp_probes.py BASE.milk TAG VARIANTS.txt [--warp WARP.txt]

BASE.milk keeps everything (per-frame code, waves, textures) except its `comp_N=` lines, which are replaced by each
variant of VARIANTS.txt. VARIANTS.txt: blocks introduced by a line `### name`, followed by the full comp shader text
(`shader_body { ... }` with any globals before it). Writes scripts/probes/cs_<TAG>_<name>.milk and
scripts/probes/cs_<TAG>_list.txt. Render the references and compare with

  oracle/run_corpus.sh cs-probes scripts/probes/cs_<TAG>_list.txt -n 40 -j 4
  scripts/fdq.py cs-probes cs_<TAG>_ --modes stepc --stages final

`stepc` (fdrender --teacher --comp-from-ref) feeds the oracle's `main[n]` to the composite shader, so a variant
that matches isolates the pieces of the shader that are right; the first one that does not match localises the
difference. Typical probes: output an intermediate (`ret = dz.xyx * 20 + 0.5;`), pin an uninitialised global,
replace a texture lookup by a constant.
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def read_variants(path):
    out, name, buf = {}, None, []
    for line in open(path, errors="replace"):
        m = re.match(r"###\s*(\S+)", line)
        if m:
            if name:
                out[name] = "".join(buf).strip("\n")
            name, buf = m.group(1), []
        elif name:
            buf.append(line)
    if name:
        out[name] = "".join(buf).strip("\n")
    return out


def with_comp(base_lines, comp_text, key="comp_"):
    kept = [l for l in base_lines if not re.match(r"%s\d+=" % key, l)]
    # insert after the last per_pixel_/per_frame_/warp_ line, i.e. at the end
    body = [("%s%d=`%s\n" % (key, i + 1, ln)) for i, ln in enumerate(comp_text.split("\n"))]
    return kept + body


def main():
    args = sys.argv[1:]
    if len(args) < 3:
        sys.exit(__doc__)
    base, tag, variants = args[:3]
    base_lines = open(base, errors="replace").read().splitlines(keepends=True)
    names = []
    for name, text in read_variants(variants).items():
        out = os.path.join(HERE, "cs_%s_%s.milk" % (tag, name))
        open(out, "w").write("".join(with_comp(base_lines, text)))
        names.append(out)
    lst = os.path.join(HERE, "cs_%s_list.txt" % tag)
    open(lst, "w").write("".join(os.path.relpath(n, os.path.join(HERE, "..", "..")) + "\n" for n in names))
    print("wrote %d probes, list %s" % (len(names), lst))


if __name__ == "__main__":
    main()
