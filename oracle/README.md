# lactea oracle (instrumented projectM)

A deterministic, instrumented projectM (master, 4.2 API) that dumps every pipeline stage, the per-frame
variables and the static textures in the `lactea_abi::dump` layout. It is an **external executable
only** (LGPL-2.1): never linked into lactea crates.

```
oracle/build.sh                       # clone @ PROJECTM_COMMIT, apply patches, build, build refrender (+ texture pack)
oracle/build/refrender --preset P.milk --audio fixtures/audio/synth128.wav --out out/ref/x \
        --frames 120 --stages all --vars --textures
oracle/run_corpus.sh starter oracle/starter_set.txt        # -> out/ref/starter/<preset>/ , summary.tsv
python3 oracle/check_orientation.py                         # orientation / consistency proof
python3 oracle/check_determinism.py PRESET...               # byte-identical re-render proof
```

## Files

| path | what |
|---|---|
| `PROJECTM_COMMIT` | projectM sha the patches apply to (vendors projectm-eval; the file is the authoritative pin) |
| `build.sh` | idempotent from-scratch build (shallow clone incl. submodules into `third_party/projectm` if absent; patches re-applied only when they change; RelWithDebInfo, Ninja, `-ffp-contract=off`, projectm-eval `ENABLE_FAST_MATH=OFF`, install prefix `oracle/build/install`). Env: `PROJECTM_SRC`, `ORACLE_BUILD`, `JOBS`. |
| `patches/0001-fdoracle-…patch` | all projectM changes (new `src/libprojectM/FdOracle.{hpp,cpp}` + small hooks) |
| `patches/0002-eval-…patch` | projectm-eval: `FD_SEED` seeds the global MT; `projectm_eval_context_enum_variables()` |
| `update_patches.sh` | dev helper: regenerate the patches from the working clone |
| `refrender.c` | the harness (EGL pbuffer, GL 3.3 core, FBO target, libpng) |
| `run_corpus.sh` | parallel batch render into `out/ref/<set>/<preset-stem>/` (`-j`, `-n`, `-a`, `-t` per-preset timeout, `--resume`, `--share-textures` = hard-link the byte-identical noise textures, `-- <refrender args>` for cheaper sets such as `--size 320x180 --stages final,main,warped`); whole-corpus workflow in `docs/corpus.md` |
| `starter_set.txt` | 36 presets covering distinct pipeline features (`path<TAB>features`) |
| `classify_presets.py` | feature classifier used to pick the set (`--find FEATURE...`) |
| `tests/*.milk` | probe presets: `orientation*.milk`, `bad_shader.milk` (must exit 3) |
| `check_orientation.py`, `check_determinism.py` | the proofs described below |
| `check_stage_selection.py` | stage-selection invariance proof: `final`/`main`/`warped`/`vars` are byte-identical whatever other stages (blur1..3, warpsrc, all) are requested; `check_stage_selection.py oracle/starter_set.txt` (36 presets) and `oracle/compshader_v2_set.txt` (44), 30 frames |
| `corpus_s400_set.txt` | seeded random 400-preset sample of the corpus (`scripts/corpus_sample.py --n 400 --seed 1`); see `docs/corpus.md` |
| `shapes_extra_set.txt` | 40 shape-heavy presets (`path<TAB>features`: textured, instanced, additive, thick outline, border, shape slot >= 4), disjoint from `corpus-s400`; rendered with `oracle/run_corpus.sh shapes-extra oracle/shapes_extra_set.txt -n 60 --share-textures -- --size 320x180 --stages final,main,warped --vars --textures` into `out/ref/shapes-extra` |

## refrender

```
refrender --preset P --audio WAV --out DIR [--size 640x360] [--mesh 48x36] [--fps 60] [--frames 300]
          [--seed 1] [--texel-offset 0,0] [--stages final,main,warped,blur1,blur2,blur3 | all | none]
          [--vars] [--textures] [--texture-dir DIR] [--default-fb]
          [--preset2 P --switch-frame N --soft-cut S [--start-clean 0|1] [--transition-shader K] [--transition-static a,b,c,d]]
```
Defaults = `RunConfig::default()`. `--stages` may additionally contain `warpsrc` (non-ABI extra, see below).
Driving: preset locked, hard cuts off, soft cut 0, `projectm_set_preset_start_clean(true)`, frame `n` is rendered
after `projectm_set_frame_time(n/fps)` and after pushing PCM frames `pcm_range(n, sr, fps)` (WAV float32 / int16,
mono is duplicated, a short file is padded with silence). Mesh/texel offset/fps are set through the 4.2 API.

Exit codes: `0` ok, `1` usage/IO/GL error, `2` preset failed to load (stderr message, `oracle.json`
has `preset_loaded:false`), `3` shader errors (frames are still rendered with projectM's fallback shaders;
stderr message). `oracle.json`: `shader_errors` (number of `ShaderException`s: HLSL parse/translate, GLSL
compile, link — every one of them is swallowed by projectM, so the patch counts them where they are
constructed), `shader_error_messages`, `blur{1,2,3}_size`, `warp_shader`/`comp_shader` (with `--vars`),
`frames_rendered`, `projectm` (commit).

`meta.json` is `lactea_abi::dump::DumpMeta` (`producer = "projectm@<12 hex>"`; `stages` lists only ABI stages).

## Output layout

```
<dir>/meta.json  <dir>/oracle.json
<dir>/<stage>/<frame:05>.png         RGBA8, row 0 = top of the displayed image
<dir>/vars/<frame:05>.json           {"name": number, ...}
<dir>/textures/noise_{lq,lq_lite,mq,hq}.png   noisevol_{lq,hq}.bin
```
Stages (all RGBA8 PNG; blur textures are RGB8 in projectM, alpha reads 255):

| stage | size | what / hook |
|---|---|---|
| `final` | WxH | the image in the target FBO after `projectm_opengl_render_frame_fbo` (read with `glReadPixels`, rows flipped) |
| `main` | WxH | current frame buffer after shapes, waves, darken-center, borders (`MilkdropPreset::RenderFrame`, before the flip for the composite). This is what the next frame warps — **plus** the next frame's motion vectors, which projectM/MD2 draw onto it *before* warping |
| `warped` | WxH | right after the warp mesh draw (before blur/shapes/waves) |
| `blur1..3` | native, e.g. 160x92 / 80x48 / 32x24 for 640x360 | the textures shaders sample (`BlurTexture` slots 1,3,5). Requesting any of them (or `all`) computes all three levels on every preset, even if no shader samples blur (`FdOracle::ForceBlur`). **That forced update runs inside a `FdOracle::GlStateGuard` and cannot change any other stage** (see "Forced blur is perturbation-free" below; proven by `check_stage_selection.py`) |
| `warpsrc` (extra) | WxH | previous `main` + this frame's motion vectors = the image the warp samples **and the source of the blur passes** |

3D noise `.bin`: little-endian `u32` magic `0x33564446` ("FDV3"), `u32 version=1`, `u32 width,height,depth`
(32,32,32), `u32 channels=4`, `u32 format=0` (RGBA8), then `w*h*d*4` bytes, x fastest, then y, then z, byte order R,G,B,A.
2D noise PNGs are written in GL memory order (row 0 = t=0), i.e. as the shader samples them (`uv.y=0` = row 0).

### vars/NNNNN.json
Flat object of f64 values. **Non-finite values are the JSON strings `"nan"`, `"inf"`, `"-inf"`** (same as lactea-compare).
Contents (after the per-frame equations and projectM's `gamma`/`echo_zoom` clamps): every variable registered in the
per-frame eval context (builtins `zoom, rot, q1..q32, bass, … blur3_max`, **and every user variable the preset's code
created**, lower-cased), `reg00..reg99`, `audio.{bass,mid,treb,vol}[_att]`, `render.time`, `render.frame`,
`hue_offset.0..3` (the v1 "shade" randoms), and, when the preset has shaders, per shader (`warp.` / `comp.` prefix):
`rand_frame.x..w`, `rand_preset.x..w`, `time` (wrapped, `_c2.x`), `progress`, and all 24 `rot_*` matrices as the 12 floats uploaded
(`<prefix>rot_s1.0 … .11`, column-major mat3x4). Unprefixed `rand_frame.x..w`/`rand_preset.x..w` = the composite shader's
values (warp's if there is no composite shader). **projectM gives the warp and the composite shader independent random
state** (MD2 shares one `rand_preset`/rotation set per preset) — see divergences.

## Determinism (patch 0001/0002, everything gated by env vars set by `refrender`)

`FD_SEED` (set from `--seed`) replaces every RNG:
* one documented stream family: `Rng(name)`: `state = splitmix64(seed + γ) ^ fnv1a64(name)`; `next64(): state += γ; return mix(state)`
  (γ = 0x9E3779B97F4A7C15, mix = splitmix64 finaliser), `next32 = next64 >> 32`.
* `shader_warp` / `shader_comp` streams: `rand_preset`, rotation centres/speeds/translations (constructor) and
  `rand_frame` + the 4 per-frame random rotations (`ShaderFloat = next32 % 7381 / 7380`, same order as upstream's `rand()` calls).
* `hue_offsets` (`next32>>1` moduli 64841/53751/42661/31571, ×0.01), `noise_{2d,3d}_<size>_<zoom>` (texture generation),
  `randtex_<name>` (random user texture pick; pool sorted by name first — directory order is filesystem dependent),
  TimeKeeper's generator.
* projectm-eval `rand()`: its process-global Mersenne Twister is seeded `0x4141f00d + (seed-1)*0x9E3779B9`, i.e.
  **seed 1 = MD2's original seed**, so `lactea-eel`'s `Context::new()` state matches. It is one shared stream across
  per-frame/per-vertex/wave/shape code, so call order matters.
* undefined GL storage is zero-filled (framebuffer/texture allocation), and time is exactly `n/fps`: frame 0 has `time=0`,
  `dt=0` (TimeKeeper no longer reads the wall clock; verified: `vars` frame 0: `time=0, frame=0, fps=60`; frame 1: `time=0.016666668`).
* user texture search path = `--texture-dir` (build.sh clones the projectM texture pack into `oracle/build/texpack`).

Two runs with identical arguments are byte-identical (`check_determinism.py`; see the report for digests).
Noise textures use `std`-independent integer streams but the evaluation order of the four channel draws inside one
expression is compiler-defined — **load the dumped `textures/` into lactea rather than regenerating them**.

## Orientation (verified by `check_orientation.py`)

* PNG row 0 = top of the screen as projectM displays it in a GL window (`final`: `glReadPixels` flipped; `main`, `warped`,
  `warpsrc`: projectM's internal textures are stored top-down for D3D parity, memory row 0 = top → `glGetTexImage` flipped to
  match the readback convention of `final`; `blur*`: stored vertically flipped relative to `main` (pass 0 flips) → dumped as-is).
* Evidence: (1) a comp shader `ret=float3(uv.x,uv.y,0)` gives green≈0 at the top row and ≈255 at the bottom of `final`
  (uv.y runs downward as in MD2); (2) a probe shape has the same height in final/main/warpsrc/blur1/blur2; (3) real preset:
  `main[n]` vs `final[n]` mean error 0.59 vs 6.16 when flipped, `warpsrc[n] == main[n-1]` bit-exact,
  `warped` ≈ `main`, `blur1` matches a box-downsample of `warpsrc` better than of the flipped image.


## Transitions (soft cuts; `--preset2`)

```
refrender --preset A.milk --preset2 B.milk --switch-frame 10 --soft-cut 1 --audio fixtures/audio/synth128.wav \
          --out out/ref/x --size 640x360 --frames 90 --seed 1 --stages all --vars
          [--start-clean 0|1]  [--transition-shader 0..5]  [--transition-static a,b,c,d]
```
Preset 1 loads exactly as without `--preset2` (hard cut, start_clean, soft cut 0). At the top of loop iteration `n == N`
(before PCM / `projectm_set_frame_time`) the harness calls `set_soft_cut_duration(S)`, `set_preset_start_clean(start_clean)`
(default 0 = projectM's default; applies to this switch only) and `load_preset_file(B, smooth=true)`. Preset 2 failing to load =>
`oracle.json` `preset2_loaded:false`, exit 2. Without `--preset2` outputs are byte-identical to before (the extra flags are
rejected without it). `oracle.json` additionally holds `preset2`, `preset2_loaded`, `switch_frame`, `soft_cut`, `start_clean`
and `transition` (`meta.json` is unchanged).

**Timing** (projectM `PresetTransition`): start time = `GetFrameTime()` at load = time of the previously rendered frame, i.e.
`(N-1)/fps`; the transition is drawn while `t_frame - start < S` and the frame where `IsDone` is true renders only the new preset
as the active one (a normal frame). Both presets render every transition frame (incoming first, then outgoing); a full-screen
shader blends their `OutputTexture()`s into the target FBO.

**Determinism** (only with `FD_SEED`, i.e. always in refrender): two persistent process-wide `FdOracle::Rng` streams
(function-local statics; consecutive transitions in one process continue them):
* `"transition_select"`: shader index `= Next32() % 6`, advanced exactly once per `RandomTransition()` call.
* `"transition_static"`: the four `iRandStatic` ints `= static_cast<int32_t>(Next32())` x4 per transition (`PresetTransition` ctor).
* `iRandFrame` (unused by the built-in shaders) stays unseeded.
* Overrides (streams still advance): `--transition-shader K` = env `FD_TRANSITION_SHADER`, `--transition-static a,b,c,d` = env `FD_TRANSITION_STATIC`.

Shader order (`TransitionShaderManager`): 0 Circle, 1 Plasma, 2 SimpleBlend, 3 Sweep, 4 Warp, 5 ZoomBlur.
`oracle.json` `transition` = `{"shader": k, "name": "...", "static": [x,y,z,w], "start_time": t, "duration": s, "shader_ok": bool}`
(recorded when the transition is created; `shader_ok:false` = the shader failed to compile => nothing is drawn by `Draw`).

**Dumps** during frames where a transition is drawn (`final/NNNNN.png` is the blended image as always):

| dir | what |
|---|---|
| `old_out/`, `new_out/` | the outgoing / incoming preset's `OutputTexture()` right before the blend (tokens `old_out`, `new_out`; `all` adds both when `--preset2` is given; non-ABI extras, not listed in `meta.json`). Same orientation as `final` (row 0 = top, GL rows flipped) |
| `new_main/ new_warped/ new_warpsrc/ new_blur1..3/` | the incoming preset's per-preset dumps (requested by the plain tokens `main`, `warped`, ... / `all`) |
| `new_vars/NNNNN.json` | the incoming preset's variables (`--vars`) |
| `main/ warped/ ... vars/` | the outgoing (active) preset, plain names; it renders after the incoming one so nothing is overwritten |

Implementation: `FdOracle::SetStagePrefix("new_")` around `m_transitioningPreset->RenderFrame` in `ProjectM::RenderFrame`.

Measured (640x360, `--switch-frame 10 --soft-cut 1`, fps 60, Mesa Iris Xe): `start_time = 0.15` (= 9/60); transition frames are
**10..69** (60 frames; at frame 69 the double `69/60 - 0.15` is `0.9999999999999999 < 1`, so `IsDone` first holds at frame **70**,
the first normal frame). `old_out`/`new_out`/`new_main`/`new_vars` therefore have exactly 60 files. All six built-in shaders compile
(`shader_ok:true`) and render. Orientation: on a normal frame `final` == flipped `OutputTexture()` bit-exactly (every channel incl.
alpha); during a SimpleBlend transition `final` is a per-pixel RGB blend of the **unflipped-relative** `old_out`/`new_out` dumps
(least-squares fit error 0.17/255 at frame 30; the vertically mirrored pair fits with error 10). **Alpha**: `OutputTexture()` alpha is
~0 (0..3) while the transition shaders write alpha 1, so `final` alpha is 255 on transition frames; compare RGB only.

## Forced blur is perturbation-free

Requesting a `blur*` stage (and therefore `--stages all`, the default of `run_corpus.sh`) forces projectM's blur update, which
upstream runs only for presets whose shaders sample `GetBlur1..3`. Left alone, that update has side effects: on the call that
allocates the blur textures `Framebuffer::SetSize()` ends with `glBindFramebuffer(GL_FRAMEBUFFER, 0)` before `BlurTexture::Update`
snapshots the bound framebuffer, so everything drawn afterwards in `MilkdropPreset::RenderFrame` (custom shapes and waves, main
waveform, darken centre, borders) lands in FBO 0 and frame 0 of `main`/`final` is black; it also leaves the linear/clamp
`BlurTexture` sampler bound on texture unit 0 for the first shape draw. Both effects are real projectM behaviour for presets
that sample blur (probes `scripts/probes/ov_shapes_blur.milk`, `ov_texfirst_blur.milk` against their plain twins) and are kept
for those.

When the chain runs only because blur dumps were requested (`m_blurLevel == None`), the patch snapshots the GL state before
`AllocateTextures` (`FdOracle::GlStateGuard`: FBO read/draw, viewport, blend, active texture, unit-0 texture and sampler,
program, VAO, array buffer) and restores it afterwards, so no stage depends on which stages were requested.
`oracle/check_stage_selection.py` checks it: `final`, `main`, `warped` and `vars` are byte-identical across
`--stages final,main,warped`, `all`, `all,warpsrc` and `blur1,final` for the starter and compshader-v2 sets.

## Gotchas / projectM ≠ MD2 divergences found

1. **Blur source** — in MD2 `BlurPasses()` reads `m_lpVS[0]` = the *previous* frame (the warp's source, motion vectors
   already drawn onto it), not the warped image; projectM does the same. `docs/ARCHITECTURE.md` ("blur levels are taken from
   the warped image") is wrong for both. Measured: blur1 is closer to the downsampled `warpsrc` (2.3) than to `warped` (3.4).
2. **Blur texture sizes**: `((w/2 → 16-multiple width, 4-multiple height)` as MD2: 640x360 → blur1 160x92, blur2 80x48,
   blur3 32x24 (width rounded *down* to /16, height *up* to /4). Blur textures are RGB8; the quad covers the padded size.
   projectM's `GetSafeBlurMinMaxValues` has `blurMax = avg - 0.05` where MD2 has `+` (typo) — affects `blurN_min/max` close together.
3. **v1 (no-shader) composite tint**: projectM's `VideoEcho::Draw` always multiplies by the 4-corner "shade" gradient
   (0.5 + 0.5·shade → up to −35 % brightness at some corners; observed (255,0,0) → (166,0,0)); MD2 `ShowToUser_NoShaders` applies it
   only when `fShader > 0.001` and weights it by `fShader`. projectM parses `fShader` but never uses it.
4. **Custom shape y**: a shape with `y=0.15` shows in the *lower* part of `final` (centroid 0.85 from the top) in projectM master
   **and** in system projectM 4.1.6; MD2's `v.y = y*-2+1` in D3D clip space would put it near the top. Not understood yet. (`tests/orientation.milk`.)
5. **Random state**: warp and composite shaders each have their own `rand_preset`, rotation set and `rand_frame` stream
   (MD2: one per preset). The `rot_rand*` matrices and `rand_frame` are drawn per `LoadVariables` call.
6. **Frame 0 audio**: `bass/mid/treb` = 1 at frame 0 and then spike (e.g. 19.5 at frame 1 with `synth128`) — projectM's
   running-average normalisation starts from 0. `progress` = `time / 1e9` because the oracle sets the preset duration to 1e9 s
   (preset locked); MD2's `progress` is the fraction of the preset time elapsed.
7. **Texel offset**: projectM default is 0,0 and is what is used (`RunConfig.texel_offset`).
8. `aspectx/aspecty` per-frame vars carry `invAspectX/Y` (MD2 quirk preserved: `aspecty = 1.7778` at 640x360).
9. Presets that render (almost) black with this audio/oracle: `Geiss - Explosion`, `Vovan - Bass With Flover - smooth kaleidoscope`
   (all-zero for 120 frames, no shader error), `ORB - Starfish` — not used in the starter set.
10. Shaders that fail translate/compile silently fall back (comp → plain `tex2D(sampler_main, uv)`); that is what `shader_errors` flags.
    The 36 starter presets all report 0 errors.
11. Without the texture pack, `sampler_<name>` user textures resolve to projectM's 1x1 placeholder (the pack is cloned by `build.sh` into `oracle/build/texpack`).
