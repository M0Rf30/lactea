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

**Gate** per frame and stage: PSNR ≥ 40 dB and SSIM ≥ 0.98 (SSIM on Rec.601 luma). The corpus sets (`corpus-s400`,
`s400-f120`, the full collection) gate frames 1.. and compare frame 0 separately (the oracle's frame 0 depends on its
blur forcing, `docs/corpus.md`); `starter` gates frame 0 too. A preset passes a mode if every gated stage of every
frame passes. Numbers below always carry their frame count: a free run needs time to trip the gate.

## Current numbers

Whole collection: the 4194 presets of the projectM preset distribution; 4169 are *comparable*
(the reference loads them and uses their own shaders). The other 25 are excluded: 22 fail to load in
projectM and 3 have HLSL that projectM cannot parse (it renders its default shader instead).

| set | step | free |
|---|---|---|
| starter (36 presets, 640x360, 120 frames, frame 0 included) | 36/36 | 36/36 |
| corpus-s400 (seeded random 400, 320x180, 30 frames) | 399/400 | 399/400 |
| s400-f120 (the same 400, 120 frames) | 399/400 | 397/400 |
| full collection (4169 comparable, 320x180, 30 frames; last run before the GL varying fold and shape fill ties) | 4162/4169 (99.8 %) | 4148/4169 (99.5 %) |

The 30-frame gate hides drift: before the GL varying fold and shape fill ties `s400-f120` gave free 394/400 against
398/400 at 30 frames (four presets tripped between frames 76 and 102: `Geiss - Cauldron - Tendrils`, `martin - baby one
more time`, Fractopia, `had to cum that - jaben`). About 1 % of comparable presets fail free only after 30 frames, so the
full-collection free figure (30 frames) overstates the long-run pass rate; the full collection has not been rerun at 120
frames. Last change (`fix/fidelity`), corpus-s400 before -> after: step 120 frames 398 -> 399, free 120 frames 394 -> 397,
step 30 frames 398 -> 399, free 30 frames 398 -> 399; presets with every stage minimum >= 90 dB (step / free, 120 frames)
299 -> 303 / 283 -> 287; starter 22 -> 24 / 21 -> 23. No stage minimum fell by more than 0.5 dB in any set.

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
| `flexi + cope - i blew you a soap bubble now what...` | two to three isolated `main` pixels per frame differ by up to 226 (additive textured 100-gon fans); not root-caused (nearest texel boundaries; the vertex nudge of flat fills is not applied to textured fills, it makes `Zylot - Star Ornament` worse) |
| `Rovastar - Fractopia (Upspoken Mix)_Phat_Speak_When_Spoken_2` (free fails at frame 91 of 120) | one `main` pixel off at step frame 90 from shape 2, a flat hexagon without a horizontal edge: slanted-edge or vertex-snap tie, mechanism open |
| `had to cum that - jaben` (free fails at frame 102 of 120) | warp-pass one-level ties amplified by the feedback loop; not isolated |
| the other drift-only presets of the last full run | the 30-frame full run listed 7 step and 21 free failures; only dive ×2, the soap bubble and the three above are attributed, the names of the rest were not recorded: keep `buckets.md` of the next full run |
| `mult(x, y) + c` and `x / literal + c` | the oracle's compiler contracts them into a multiply-add (`ffma`, the latter also moved into the vertex stage); not emulated (probes `hp_m04`, `hp_e22` differ with and without the fold) |
| reads of undefined shader variables that are not float-ALU-only | the reference uses whatever the GPU register held |
| three presets whose shaders projectM fails to parse (`mindblob`, `[dylan] cube in a room` ×2) | the reference renders its default shader; Lactea translates the real one |
| slanted lines or dots on a 1/256-px rounding tie | validated against axis-aligned probes and random wobbling-wave probes; single-pixel `main` differences remain possible |
| `rot_*` shader matrices | projectM computes the angle in f32 with `glm::rotate` (diagonal `c + (1 - c)`), Lactea in f64 with D3DX matrices: 68 of 400 free runs have `rot_*` variables one ulp off (<= 1.2e-7), no gate failure attributed |

## Findings the implementation encodes

Root causes found against the reference, kept here because each one is a contract the tests pin:

- projectM's custom-wave point and shape-centre coordinates are `f64` until the final rounding
  (`CompatRules::wave_point_f64_aspect`); one ulp flips the 1/256-px vertex snap.
- The GPU's clip unit truncates `fma(c, size/2, size/2)` toward zero before the snap
  (`stroke.rs::viewport_axis`).
- Mesa's backward inter-shader code motion (`nir_opt_varyings`) moves `input ± uniform/const` and a few other linear
  ops into the vertex stage; `lactea-hlsl/src/emit/fold.rs` mirrors the use-dominance decision (`docs/hlsl.md`), measured
  with twin probes (`max(x, -10)` around the inputs) rendered by the oracle. `Flexi + Martin - dive` ×2 final 29 -> 83 dB.
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
