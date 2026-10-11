# Fidelity

Lactea's rendering is checked numerically against projectM, the reference implementation. This is
the single place for the method, the current numbers and the open items. Differences between
projectM and MilkDrop 2 and which one Lactea follows: [`divergences.md`](divergences.md). How to
reproduce a whole-corpus run: [`corpus.md`](corpus.md). The reference build: [`oracle/README.md`](../oracle/README.md).

## Method

A pinned, patched projectM build (`oracle/`, commit in `oracle/PROJECTM_COMMIT`) renders presets
deterministically and dumps every pipeline stage (`warped`, `blur1..3`, `main`, `final`) plus the
expression variables per frame. `lactea-cli`'s `fdrender` produces the same dump layout from Lactea
and `fdcompare` (`lactea-compare`) diffs the two. Runs use fixed audio (`fixtures/audio`, generated
by `gen_audio.py`), a fixed seed and frame time `n / fps`.

| mode | what it isolates |
|---|---|
| **step** (teacher-forced) | frame n is rendered from the reference's `main[n-1]` with its per-frame variables and noise, so errors cannot accumulate: each stage is checked on its own (`warped` → `blur*` → `main` → `final`) |
| **stepc** | as step, but the composite only is fed the reference's `main` |
| **free** | Lactea runs on its own (only the reference's noise textures are injected); shows accumulated drift |

**Gate** per frame and stage: PSNR ≥ 40 dB and SSIM ≥ 0.98 (SSIM on Rec.601 luma). Frame 0 is
included. A preset passes a mode if every stage of every frame passes.

## Current numbers

Whole collection: the 4194 presets of the projectM preset distribution; 4169 are *comparable*
(the reference loads them and uses their own shaders). The other 25 are excluded: 22 fail to load in
projectM and 3 have HLSL that projectM cannot parse (it renders its default shader instead).

| set | step | free |
|---|---|---|
| starter (36 presets, 640x360, 120 frames) | 36/36 | 36/36 |
| corpus-s400 (seeded random 400, 320x180, 30 frames) | 398/400 | 398/400 |
| full collection (4169 comparable, 320x180, 30 frames) | 4162/4169 (99.8 %) | 4148/4169 (99.5 %) |

Component checks:

| component | check |
|---|---|
| `lactea-preset` | the whole corpus parses and round-trips |
| `lactea-eel` | bitwise equal to projectm-eval on 17,411 corpus blocks and 600k fuzz cases |
| `lactea-audio` | max difference 1.8e-15 vs projectM |
| `lactea-hlsl` | 1422/1423 corpus shaders translate and validate; 1412/1413 within 2/255 of glslang |
| `lactea-jpeg` | byte-identical to `stb_image` on the reference images |
| transitions | free runs of ten preset pairs over the whole blend window ≥ 40 dB / SSIM ≥ 0.98 on every frame |
| shape `image=` | nine probe presets: bit-exact or ≥ 82 dB (residual: three nearest-sampled pixels at a texel boundary) |

## Open items

Known and not reproduced, with the reason:

| item | cause |
|---|---|
| `Flexi + Martin - dive` (two presets, step `final` ~29 dB at frame 7) | Mesa's `nir_opt_varyings` moves `uv.y + (h - 1)` into the vertex shader for projectM's GL pipeline; wgpu/ANV interpolates `uv.y` and adds in the fragment shader. The ~1e-7 difference is amplified by a division by ~0 on the horizon row |
| `flexi + cope - i blew you a soap bubble now what...` | two to three isolated `main` pixels per frame differ by up to 226 (additive textured 100-gon fans); not root-caused (fan fill ties at snapped vertices or nearest texel boundaries) |
| nine drift-only presets (step passes, free trips the gate after 10-60 dB for 10-20 frames) | one-ulp warp-shader differences growing in the feedback loop; likely the same Mesa compile difference as `dive` |
| reads of undefined shader variables that are not float-ALU-only | the reference uses whatever the GPU register held |
| three presets whose shaders projectM fails to parse (`mindblob`, `[dylan] cube in a room` ×2) | the reference renders its default shader; Lactea translates the real one |
| slanted lines or dots on a 1/256-px rounding tie | validated against axis-aligned probes and random wobbling-wave probes; single-pixel `main` differences remain possible |
| shader fallback `sampler_pw_noise_lq` on `m0rf30/M0Rf30 - Nebula Forge.milk` | known engine gap, the preset renders with the fallback shader |

## Findings the implementation encodes

Root causes found against the reference, kept here because each one is a contract the tests pin:

- projectM's custom-wave point and shape-centre coordinates are `f64` until the final rounding
  (`CompatRules::wave_point_f64_aspect`); one ulp flips the 1/256-px vertex snap.
- The GPU's clip unit truncates `fma(c, size/2, size/2)` toward zero before the snap
  (`stroke.rs::viewport_axis`).
- Mesa replaces an undefined variable used only by float ALU ops with NaN, so `max(x, undef)` is `x`
  (`lactea-hlsl/src/emit/undef.rs`, projectM flavour only).
- GL line tie rules: x-major rising lines go to the upper row, y-major to the left column;
  guard-band clipping of huge lines; diamond-exit reversal.
- Built-in noise textures are resolved by name before any directory scan (shape `image=`).
- projectM's persistent 512-slot wave buffer tail, VideoEcho aspect quad and gamma epsilon, the
  8-vertex border mesh with GL fill ties, four custom wave/shape slots, warp-shader sampler wrap from
  `bTexWrap`, hlslparser quirks (`abs()` around pow/sqrt/log, post-constructor priority reset, DX9
  `asin`/`acos` polynomial, literal `0/0` = NaN, 6-digit float literals), first duplicate preset key
  wins, eel integer literals via `atof`, implicit sampler declarations, raw VideoEcho colours, RG16F
  saturation.

## Reproducing

```sh
oracle/build.sh                                   # pinned, patched projectM (headless)
python3 fixtures/audio/gen_audio.py
oracle/run_corpus.sh starter oracle/starter_set.txt
scripts/fidelity.sh starter                       # free + teacher-forced runs, fdcompare reports
scripts/fidelity_summary.py out/report/starter
scripts/fdq.py starter "<preset>" --modes step,stepc,free        # quick single-preset probe
scripts/fdregress.py out/report-before/<SET> out/report/<SET>    # before/after of two runs
```

One-feature probe presets (`scripts/probes/`) isolate a cause; the generators are the source, the
generated `.milk` files are rebuilt on demand. A rendering change should not lower any stage's
median PSNR by more than 0.5 dB on `starter` and `corpus-s400`. The loop is optional for code that
does not change rendering (see `CONTRIBUTING.md`).
