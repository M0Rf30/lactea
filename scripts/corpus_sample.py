#!/usr/bin/env python3
"""Seeded random sample of the preset corpus (a cheap stand-in for the 4194-preset `corpus-all`).

    scripts/corpus_sample.py [--n 400] [--seed 1] [--root /usr/share/projectM/presets] [--out oracle/corpus_s400_set.txt]

The universe is `sorted(glob(ROOT/**/*.milk))` (what docs/corpus.md feeds to `oracle/run_corpus.sh corpus-all`), the sample is
`random.Random(seed).sample(universe, n)` written in sorted order, one absolute path per line, so the list is reproducible
for a given preset install and committed under `oracle/`.
"""
import argparse
import glob
import os
import random

ap = argparse.ArgumentParser()
ap.add_argument("--n", type=int, default=400)
ap.add_argument("--seed", type=int, default=1)
ap.add_argument("--root", default="/usr/share/projectM/presets")
ap.add_argument("--out", default=os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "oracle",
                                              "corpus_s400_set.txt"))
a = ap.parse_args()
universe = sorted(glob.glob(os.path.join(a.root, "**", "*.milk"), recursive=True))
pick = sorted(random.Random(a.seed).sample(universe, min(a.n, len(universe))))
with open(a.out, "w") as f:
    f.write("\n".join(pick) + "\n")
print("universe %d presets, sampled %d (seed %d) -> %s" % (len(universe), len(pick), a.seed, a.out))
