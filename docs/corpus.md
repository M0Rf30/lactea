# Whole-corpus runs

How to run Lactea against every preset of the projectM distribution (`$LACTEA_CORPUS`, default
`/usr/share/projectM/presets`, 4194 presets) and read the result. Method, gate and current numbers:
[`fidelity.md`](fidelity.md). The full corpus needs about 25 GB of disk under `out/` (references
22 GB, reports 5 GB) and roughly 35 min of reference rendering plus 35 min of Lactea runs on 8-10 jobs.

## Reproduce

```sh
CORPUS=${LACTEA_CORPUS:-/usr/share/projectM/presets}

# 1. reference dumps (320x180, 30 frames, final+main+warped + vars)
python3 - "$CORPUS" <<'E'      # list = every .milk; run_corpus.sh makes stems unique with a path hash
import glob, sys
open('out/corpus-all.list', 'w').write('\n'.join(sorted(glob.glob(sys.argv[1] + '/**/*.milk', recursive=True))) + '\n')
E
oracle/run_corpus.sh corpus-all out/corpus-all.list -j 8 -n 30 -t 120 --resume --share-textures -- \
    --size 320x180 --stages final,main,warped --vars --textures --texture-dir oracle/build/texpack/textures

# 2. fdrender free + teacher (step) for every preset, fdcompare per chunk of 300
scripts/corpus_run.py corpus-all --jobs 10       # FD_TARGET selects the cargo target dir; --no-build skips cargo

# 3. clustering + index.html + buckets.md + combined corpus.json
scripts/corpus_report.py corpus-all --md

# 4. robustness sweeps (no references needed): any exit code other than 0/3/2 is a bug
scripts/corpus_robust.py --tag odd-size --jobs 16 -- --size 257x145 --frames 8 --audio fixtures/audio/sweep.wav --mesh 20x16
scripts/corpus_robust.py --tag silence-md2 --jobs 16 -- --frames 6 --audio fixtures/audio/silence.wav --compat md2
scripts/corpus_robust.py --tag hd-720p --jobs 6 -- --size 1280x720 --frames 4 --mesh 64x48
scripts/corpus_robust.py --tag tiny --jobs 16 -- --size 33x17 --frames 5 --mesh 2x2
```

Regression check against an earlier run: keep its `step/corpus.json` and `free/corpus.json` and pass them as
baselines. `fdcompare corpus --baseline` exits 1 on any pass→fail or a stage minimum that drops by more than
0.5 dB PSNR / 0.005 SSIM.

```sh
cp out/report/corpus-all/step/corpus.json out/baseline/corpus-all/step.corpus.json
cp out/report/corpus-all/free/corpus.json out/baseline/corpus-all/free.corpus.json
scripts/corpus_run.py corpus-all --fresh --baseline step=out/baseline/corpus-all/step.corpus.json \
                                                    free=out/baseline/corpus-all/free.corpus.json
```

Outputs (all under `out/`, gitignored):

- `report/corpus-all/index.html`: ranked buckets with worst-frame triptychs, links to the per-mode reports;
- `report/corpus-all/{step,free}/index.html`: every preset, thumbnails, per-stage metrics;
- `report/corpus-all/corpus.json`: combined (features, reference/render status, per-stage metrics and bucket of every preset);
- `report/corpus-all/{step,free,frame0}/corpus.json`: `fdcompare` format, usable as regression baselines;
- `report/corpus-all/render/<mode>/<stem>.json`: exit code, time, stderr tail, shader fallbacks and repro command of every `fdrender` run;
- `report/corpus-all/buckets.md`, `report/robust/<tag>.json`.

Cost: reference 1.4 s/preset median, `fdrender` 0.36 s (free) / 1.0 s (step), worst case 15.8 s; a reference dump is 5.3 MB
per preset (the six noise textures are hard-linked into `out/ref/corpus-all/_textures` by `--share-textures`).
`scripts/corpus_run.py` deletes candidate dumps after each chunk; `--keep-cand` keeps them (about 4 GB per mode).

## Method

- Parameters: `synth128.wav`, 320x180, mesh 48x36, 60 fps, 30 frames, seed 1, stages `final,main,warped`. Smaller frames
  and shorter runs than `starter`: a failure that needs 100 frames to show is invisible here.
- **step** = `fdrender --teacher` (frame n rendered from the reference's `main[n-1]` with forced per-frame variables and noise),
  **free** = `--inject-noise`. Gate per frame and stage: PSNR ≥ 40 dB and SSIM ≥ 0.98, frames 1.. (`--frames 1..`);
  frame 0 is compared separately (`report/corpus-all/frame0`).
- "comparable" = the reference exits 0 (4169 presets). The 22 presets projectM fails to load have no reference; the 3
  whose shaders projectM cannot parse (it renders its default shader) are excluded, Lactea translates them.
- Buckets (`scripts/corpus_report.py`): first failing stage in step mode (warped → main → final) × first matching preset
  feature (priority list in the script; features from `oracle/classify_presets.py`) × severity (median PSNR of that stage
  ≥ / < 30 dB). Buckets are per preset, not per pixel: a preset with textured shapes and per-pixel code is counted under
  `main: custom shapes` even if the shapes are not the culprit; the per-feature failure rate is the unbiased view.

## corpus-s400: the cheap regression loop

A seeded random sample of 400 of the 4194 presets (`scripts/corpus_sample.py --n 400 --seed 1` →
`oracle/corpus_s400_set.txt`) at the corpus-all settings: about 90 s of `fdrender` per engine, 70 s of reference
rendering, 1.9 GB of references. 399 of the 400 are comparable.

```sh
oracle/run_corpus.sh corpus-s400 oracle/corpus_s400_set.txt -j 6 -n 30 -t 120 --resume --share-textures -- \
    --size 320x180 --stages final,main,warped --vars --textures --texture-dir oracle/build/texpack/textures
FD_TARGET=target-engine scripts/corpus_run.py corpus-s400 --jobs 10 --no-build --fresh
scripts/corpus_report.py corpus-s400 --md                    # buckets.md / index.html
scripts/fdfails.py out/report/corpus-s400 --other out/report/corpus-s400-before    # pass counts, status changes
scripts/fdregress.py out/report/corpus-s400-before out/report/corpus-s400          # per-stage PSNR regressions > 0.5 dB
```

## Robustness

`scripts/corpus_robust.py` runs every preset through `fdrender` with no reference: a panic, signal, hang, wgpu validation
error (exit 4) or any exit code other than 0 (ok), 3 (shader fallback) or 2 (preset load error) is a bug. The current tree
is clean on all four sweeps above (exit 0 for 4171 presets, 3 for one, 2 for the 22 that projectM also rejects with eel
syntax errors; slowest run 15.8 s). Tiny and odd sizes (1x1, 2x2, 3x200, 200x3, 1x64, 33x17, 257x145) and 1280x720 render
without errors; an odd `--mesh` cell count is a usage error in the CLI and `RenderError::Invalid` in `lactea-render`.

## Shader translation fallbacks

`fdrender.json` `shader_fallbacks` lists presets whose shader fell back to the default one. Exactly one corpus preset does:
`m0rf30/M0Rf30 - Nebula Forge.milk` uses `sampler_pw_noise_lq` without declaring it, which projectM accepts; the
`pw_`/`fw_`/`fc_`/`pc_` built-in samplers should resolve without a declaration (open, listed in `fidelity.md`). No
wgpu/naga validation error occurs on any translated shader of the corpus (716 comp and 695 warp shader presets).

## Tools

- `oracle/run_corpus.sh`: `-t TIMEOUT`, `--share-textures` (hard-links the byte-identical noise textures, `cmp`-checked),
  exit-code summary instead of a 4000-row table.
- `scripts/corpus_run.py`, `scripts/corpus_report.py`, `scripts/corpus_robust.py`, `scripts/corpus_sample.py` (above).
- CI does not run any of this (it needs the reference build and a GPU); see `.github/workflows/ci.yml` for what it does run.
