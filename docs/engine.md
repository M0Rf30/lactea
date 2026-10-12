# lactea (engine) and fdrender

`crates/lactea` is the CPU side of MilkDrop 2's `RenderFrame` (`milkdropfs.cpp` + `state.cpp`):
preset → expression contexts → per-frame / per-vertex / waves / shapes → `lactea-render`.
It behaves like **projectM** (the measurement oracle) where projectM and MD2 differ
(`Compat::ProjectM`, default); `Compat::MilkDrop2` keeps MD2's behaviour and `Compat::MilkDrop3` is MD2 plus the MilkDrop 3 deltas (`docs/md3.md`). Source read for
behaviour, not transliterated: projectM `MilkdropPreset/*` (`PerFrameContext`, `PerPixelMesh`,
`CustomWaveform`, `CustomShape`, `MilkdropShader`, `FinalComposite`, ...).

Out of scope: sprites, text, `fft`/`wave` BeatDrop shader textures. Preset changes (hard cut and
soft cut / transitions) are in scope, see "Transitions" below.

## Embedding API

```rust
let mut engine = Engine::new(&device, &queue, EngineConfig { width, height, ..Default::default() });
engine.set_texture_provider(Box::new(MyTextures));          // before load_preset
let report = engine.load_preset(milk_bytes, "name", Transition::Cut)?;   // PresetReport; Transition::Soft { seconds } blends
engine.push_pcm_f32(&interleaved, 2);                       // any rate/channels; EngineConfig.sample_rate
engine.render(&mut encoder, &target_view, FrameClock { time, frame });   // one frame; you submit
engine.resize(w, h);  engine.frame_vars();  engine.renderer() / renderer_mut();
engine.transition_progress();  engine.transition_info();  engine.incoming_renderer();   // soft cut in progress (projectM)
engine.set_md2_blend_pattern(Some(1));  engine.md2_blend_pattern();                      // MilkDrop 2 blend (`Compat::MilkDrop2`)
engine.current_compat();                                                  // mode in use (follows each load with `auto_compat`)
engine.force_next_frame(Some(ForcedFrame { vars, prev_main }));          // teacher forcing (fdrender)
```

* `EngineConfig { width, height, mesh, seed, compat, auto_compat, texel_offset, output_format, sample_rate, fps,
  preset_duration, start_clean }` (`Default`: 640×360, 48×36, seed 1, projectM, `auto_compat: false`, Rgba8Unorm,
  44.1 kHz, 60 fps, `start_clean: false` = projectM's default).
  `fps` is the *target* rate presets read (projectM reports the target, not a measurement);
  `preset_duration` (default 1e9 s = locked preset) is the `progress` denominator.
  `auto_compat: true` makes every `load_preset` pick `Compat::ProjectM` or `Compat::MilkDrop3` from the preset's bytes
  (`lactea::compat_for_preset`, `lactea_preset::detect_compat`) and switch the engine to it when it differs from the
  current mode, with a hard cut; `compat` is then only the mode before the first load. See "Automatic compatibility"
  in `md3.md` (heuristics, what a switch rebuilds, transitions, limits).
* `TextureProvider { fn load(&mut self, name) -> Option<Image{width,height,rgba8}>; fn random_pool(&self) -> Vec<String> }`.
  Names are lowercase, without `sampler_`/`fw_`-style prefixes and file extension. The pool must be
  sorted by name (it is part of the determinism contract). The engine never touches the filesystem.
  `Compat::MilkDrop3` adds two provided methods: `load_md3_texture(name) -> Option<Media>` (shader textures, MD3 folder
  order, GIFs) and `load_sprite(path) -> Option<Media>` (a preset sprite's path relative to the MD3 data root);
  `Media` is a still `Image` or an `AnimatedImage` (full-canvas frames + delays, `lactea::decode_gif`). Hosts index their
  files with `lactea::MediaFiles` (path arithmetic and search order only, no I/O). See `md3.md`, Sprites.
  The engine keeps what the provider returned (found textures, shape images, sprites; up to 128 MiB, least recently
  used evicted) and the translated shaders (16 MiB) across `load_preset` calls, so a reload, a playlist revisit or the
  two halves of a `.milk2` do not decode or translate again; the provider is asked once per name. `set_texture_provider`
  drops the media. A provider whose files change while it is installed is not re-read.
* `PresetReport`: `compat` (the mode the preset was loaded under), `warp`/`comp` `ShaderOutcome {NotUsed, User, Default,
  Fallback}`, `warnings`, `shader_fallbacks`, `unresolved_textures`, `resolved_textures`.
* `EngineError {Parse, Compile{what,error}, Render}`. On error the previous preset stays active.
* Time: `FrameClock.time` is the `time` variable (projectM: global running time, not time since
  preset start); audio smoothing advances by `time - previous time` (0 on the first frame).

## What `load_preset` does (projectM order)

1. `Preset::parse` (MD2 parser semantics, `docs/preset.md`).
2. New `GlobalMemory` per preset (fresh `reg00..99`, `gmegabuf`); its `rand()` Mersenne Twister is the engine's one
   stream, seeded `0x4141f00d + (seed-1)*0x9E3779B9` **once, at `Engine::new`**: the stream is shared by every preset the engine ever
   loads and by both presets of a soft cut (projectm-eval's `rand()` is one process-wide static; registers and
   megabuffers stay per preset), so load #0 is the oracle's and later loads continue the stream.
3. Contexts: per-frame, per-vertex, and for **all** 16 waves / 16 shapes their own (per-frame,
   per-point / per-frame). Code is compiled with `Context::compile` from `code_to_source(..)`.
   Compile errors in per-frame/per-vertex/enabled wave/shape code fail the load (projectM refuses
   such presets); in a *disabled* wave/shape they are warnings (projectM would fail too).
4. Init order (`CompileCodeAndRunInitExpressions`): per-frame **init** code runs once with the builtins loaded
   at load time (time = last clock, audio 0), `q_after_init` snapshot = `q1..q32`; then per-frame/per-vertex
   compile; each wave: load state (q from the preset context), run its init code, snapshot `t1..t8`, compile
   per-frame and per-point; each shape: same with `frameQVariables`.
5. Shaders by PSVERSION (`Preset::{warp,comp}_ps_version`, 0 for version < 200 even if text exists):
   * warp: v≥2 with text → `lactea_hlsl::translate`; **no text → projectM uses the no-shader warp path**
     (MD2 would synthesize `GenWarpPShaderText`: `Compat::MilkDrop2`); translation or wgpu failure → no-shader
     warp, reported in `shader_fallbacks`.
   * comp: v≥2 with text → translated; **no text → projectM's default `ret = tex2D(sampler_main, uv).xyz`**
     (not MD2's gamma/echo shader; `Compat::MilkDrop2` uses `default_comp_shader`); failure → the same default.
   * v1 → fixed-function paths (`VideoEcho`/`Filters` = MD2 `ShowToUser_NoShaders`).
6. Textures: every `TexSource::User(name)` of the installed shaders goes through the provider.
   `randNN[_prefix]`: pick = `StreamRng("randtex_<sampler name as projectM sees it>").next64() % pool.len()`
   over the (prefix-filtered) sorted pool; one pick per slot shared by warp and comp; a `fw_`-style quality
   prefix defeats the prefix filter exactly like in projectM. Unresolved names are reported (the renderer
   binds a 1×1 black placeholder).
7. Random state (`rng.rs`): the oracle's documented stream family (`splitmix64(seed+γ) ^ fnv1a64(name)`):
   `shader_warp` / `shader_comp` (independent in projectM: `rand_preset`, 24 rotation parameters, then
   `rand_frame` + the 4 fully random `rot_rand` matrices per frame), `hue_offsets`, `randtex_*`. MD2
   shares one stream between passes (`Compat::MilkDrop2`). Matrices are built like the renderer's
   `rot_matrix` with the translation row dropped (projectM uploads `glm::mat3x4` of a `mat4`).

## Per-frame pipeline (`Engine::render`)

1. `dt = time - last_time`; `Analyzer::update_dt(dt)` (frame 0: dt = 0). Verified 1e-15 against oracle vars.
2. `PerFrameUpdate`: load state (all builtins reset from the preset's base values every frame, `q` from the
   init snapshot, user variables persist), `LoadStateReadOnlyVariables` for the per-vertex context (**before**
   the per-frame code), run per-frame code, copy `q` to the per-vertex context and to `frameQVariables`,
   clamp `gamma` ∈ [0,8], `echo_zoom` ∈ [0.001,1000], snapshot `reg00..99` (dumps show regs *here*).
3. Warp mesh: without per-vertex code every vertex uses the per-frame values; with it, code runs per vertex in
   projectM's order (its mesh row 0 is the top of the image, ours the bottom) with `x,y,rad,ang` as
   `MeshGeometry::{pv_x,pv_y,rad,ang}` (same values as projectM's `-angle`, derived from the projectM source),
   `aspectx/y` non-inverse in this context; UVs from `warp_mesh::warp_vertex_uv` with `WarpAnim::projectm`.
4. Fixed-function inputs, projectM quirks included: `gamma`, `echo_*`, `brighten/darken/solarize/invert` of the
   no-shader composite and the main wave's `mode`, `dots`, `thick`, `additive`, `scale`, `smoothing` come from the
   **preset's initial values**, not from the per-frame variables (projectM's `VideoEcho`/`Waveform` read
   `PresetState`); `wrap`, `decay`, borders, motion vectors, wave colour/position, `darken_center`, blur ranges
   come from the per-frame context. `Compat::MilkDrop2` reads the variables.
5. Custom shapes (all instances, `num_inst` clamped to 1024; `thick` is the preset flag) **before** custom waves
   (per-frame code, `samples`, `custom_wave_values` smoothing, per-point code with `sample/value1/value2`,
   `x = 0.5+value1`, `y = 0.5+value2`), then the shader pass random state, then `Renderer::render_frame`.
6. First-frame overlay loss (projectM bug, **only for presets whose shaders sample blur**): on the call that allocates the blur
   textures `BlurTexture::Update` restores FBO 0 (`Framebuffer::SetSize` unbinds before its "remember previous framebuffer"
   snapshot), so shapes, waves, main wave, darken-centre and borders of that frame are lost. This is decided by the renderer
   (`CompatRules::blur_alloc_drops_overlays`, `frames_since_resize == 0` and `blur_levels_required() > 0`); the engine always
   passes the overlays and runs their expressions.

## Transitions (soft cuts)

`engine.load_preset(bytes, name, Transition::Soft { seconds })` blends the running preset into the new one while **both keep
running**; `Transition::Cut` replaces it at once. With nothing loaded yet a soft cut is a cut. The behaviour below is
projectM's (`ProjectM::RenderFrame` / `StartPresetTransition`, `PresetTransition`, `TransitionShaderManager`,
`TimeKeeper`), read for behaviour and re-implemented (`lactea-render::transition` + `engine.rs`); `Compat::MilkDrop2`
uses MD2's per-vertex blend instead (below).

* **Two presets, two renderers.** Every load builds into an idle `Renderer` (`Renderer::sibling`: shares the pipelines and the
  immutable noise textures, own images / shaders / user textures; the previous renderer is recycled), so a failed load changes
  nothing and the running preset(s) are never touched. During a blend the incoming preset renders first, then the active one
  (it matters for the shared `rand()` stream), both only to their own composite image (`Renderer::render_frame_to_output`);
  a full-screen shader then blends `old_out` and `new_out` into the host target.
* **Shader choice.** One of six built-ins, in projectM's order: 0 circle, 1 plasma, 2 simple blend, 3 sweep, 4 warp, 5 zoom blur
  (WGSL ports of the GLSL, `lactea_render::TransitionKind`). projectM draws the index from `std::random_device`; both the
  oracle and the engine use two documented streams (`StreamRng("transition_select")`: index `= next32() % 6`, one draw per soft
  cut; `StreamRng("transition_static")`: the four `iRandStatic` integers `= next32() as i32`), seeded from `EngineConfig.seed`
  and persistent for the life of the engine. `Engine::force_next_transition` overrides shader / statics (streams still advance).
  Seed 1 draws plasma, seed 2 zoom blur, seed 3 warp, seed 8 simple blend (regression test = oracle values).
* **Timing** (`PresetClock`, projectM's `TimeKeeper` + `PresetTransition`): the blend starts at the clock time of the *previous*
  rendered frame (`m_timeKeeper->GetFrameTime()` at load); it is drawn while `now - start < seconds` (plain double
  comparison, so a 1 s cut loaded before frame 10 is drawn on frames 10..=69 and frame 70 is the first normal one:
  `69/60 - 9/60 < 1` in doubles). `iProgressLinear = elapsed/duration`, `iProgressCosine = (1 - cos(pi l))/2` (what the circle,
  plasma, simple blend, sweep, warp and zoom-blur shaders use), `iProgressBicubic`. Loading while a blend runs completes it at
  once (the incoming preset becomes active) and starts the new one; `Transition::Soft { seconds <= 0 }` is a cut one frame later.
* **`progress`** (the preset variable and `warp.progress` / `comp.progress`): `PresetProgressA()` is **1.0 for both presets
  while smoothing** and `(now - preset_time_a)/preset_duration` otherwise (`preset_time_a` = the clock time at the last cut, or
  the start of the last soft cut once it ended). `time` and `frame` stay global running values for both presets.
* **`start_clean`** (`EngineConfig.start_clean`, default `false` = projectM's default): the new preset's first frame warps from the
  old active preset's composite image instead of black (`DrawInitialImage`: our images are stored top-down like the dumps, so it is
  a straight copy; verified against the oracle's `new_warpsrc[N]`: 99 dB for the pairs below). The first preset always starts black.
* **Orientation / precision.** The GLSL samples `iChannel0/1` with `uv = gl_FragCoord.xy/iResolution` (y up); our images are
  top-down, so the WGSL uses `fragCoord = (x, H - y)` and samples at `(u, 1 - v)`. Output alpha is 1. `mod` is GLSL's floored
  modulo, `smoothstep` is the plain formula (plasma passes `edge0 > edge1`), the zoom-blur shader's `random()` reads
  `gl_FragCoord.z = 0.5` (the quad's vertex z is 0.5 in wgpu depth). Plasma and zoom blur multiply a `sin` by 1e6 / 43758.5453: they
  are bit-sensitive to the GPU's `sin`; on the oracle's GPU (Mesa Iris) both sides agree exactly.
* **Dumps / API for inspection**: `Engine::renderer()` is the *outgoing* preset's renderer while blending,
  `incoming_renderer()` the incoming one; `frame_vars()` / `incoming_frame_vars()`; `transition_progress()` (linear, `None` when
  no blend), `transition_info()` (shader, statics, start, duration); `force_next_incoming_frame`, `force_next_blend_inputs` (teacher).

Measured against the oracle (`docs/fidelity.md`): free runs of ten pairs over the whole blend window are
>= 40 dB / SSIM >= 0.98 on every frame, all six shaders; the blend pass itself fed the oracle's two images is 77-99 dB.

### MilkDrop 2 blend (`Compat::MilkDrop2`)

MD2 has no image-space cross-fade: `m_bBlending` keeps the old preset's state (`m_pOldState`) next to the new one (`m_pState`) and
blends **inside one frame pipeline**. Selected by `CompatRules::md2_preset_blend` (projectM: off, MD2: on); everything below is
ported from `third_party/md2` (`state.cpp` `StartBlendFrom`, `milkdropfs.cpp` `RunPerFrameEquations` / `RenderFrame` /
`ComputeGridAlphaValues` / `WarpedBlit_*` / `ShowToUser_*` / `DrawCustom*` / `DrawWave`, `plugin.cpp` `LoadPreset*` /
`RandomizeBlendPattern`). Without a blend, and for `Transition::Cut`, the frame path is unchanged (same code, bit-identical).

**What is kept.** One `Renderer` (no second slot, no `TransitionBlender`, `incoming_renderer()` is `None`, `transition_info()` is
`None`): `Engine::active` is the *new* preset (`m_pState`), `Engine::md2` (`BlendState`) holds the old one (`m_pOldState`: its `Loaded`
contexts keep running every frame, `LastFrame`) and the blend bookkeeping; the old preset's shaders and user textures live in the
renderer as `m_OldShaders` (`Renderer::take_shader_state` / `set_old_shader_state`; released when the blend ends, `> 1.0` like
`RenderFrame`). The feedback images simply continue (no `DrawInitialImage` / `start_clean` for a soft cut).

| MD2 | lactea |
|---|---|
| `CBlendableFloat::eval/StartBlendFrom`, the 57 blendable members | `md2_blend.rs` `BlendFloat` / `BlendBase` (`Loaded::floats`), `start_blend_from` (also the `mv_*` transparency rule, ported as written) |
| `m_fBlendStartTime/Duration/Progress`, end of blend | `BlendState::update`, `blend_progress` (all `float`, start = time of the last rendered frame like the projectM path) |
| `RunPerFrameEquations(code)`: both states, snap point, variable blend | `build_blend_frame`, `shader_code`, `snap_point`, `blend_vars` |
| `ComputeGridAlphaValues`: both per-vertex passes, `mix2`, vertex alpha | `eval_vertices` x2, `vertex_mix`, `blend_mesh` |
| `RandomizeBlendPattern`, `GenPlasma`, `CosineInterp` | `randomize_blend_pattern` (19 patterns), `gen_plasma`, `lactea_render::cosine_interp` |
| `WarpedBlit_Shaders/NoShaders`, `ShowToUser_Shaders/NoShaders` pass tables | `blend::{warp_passes, comp_passes}` (internal to `lactea-render`), `Renderer::run_md2_warp` / `run_md2_comp` |
| tile culling `bCullTiles/bFlipCulling`, `bAlphaBlend/bFlipAlpha` | `blend::tile_needed`, `geom::build_warp_vertices_blend`, `comp_indices`, alpha pipelines (`ShaderPass::alpha_pipelines`) |
| comp vertex alpha from `m_verts[].Diffuse` | `blend::comp_vertex_xy01`, `blend::comp_vertex_alpha` |
| `DrawCustomShapes/Waves` `alpha_mult` | `Md2Blend::{shape,wave}_alpha_mult`, `geom::conv_color_alpha_mult` |
| `DrawWave` `its = 2` | `Renderer::main_wave_geometry_blend`, `wavegen::main_wave_geometry_raw`, `blend::blend_waves` |

**Per frame (`build_blend_frame`, MD2 order).** (1) `RunPerFrameEquations`: for the new state, then the old one, the base values are
read the way `LoadPerFrameEvallibVars` does (`m_f*.eval(GetTime())`, i.e. the new state's own `CBlendableFloat` ramps from the old
state's value at the load; the ten motion variables `eval(-1)`: unblended), the per-frame code runs, and afterwards the variables that
do not affect pixel motion are mixed into the new state's context, `new = mix*new + (1-mix)*old` with `mix = CosineInterp(progress)`
(`decay`, `wave_*`, `ob_*`, `ib_*`, `mv_*`, `echo_zoom/alpha`, `gamma`, `blur*`); `echo_orient`, `wave_usedots/thick/additive/brighten`,
`darken_center`, `wrap`, `invert/brighten/darken/solarize` snap at `m_fSnapPoint` (0.5; -0.01 when only the old preset has a composite
shader, 1.01 when only the new one has). `wave_mode` is exempt. Net effect of the double blend on a border colour of two constant
presets: `old + CosineInterp(p) * p * (new - old)` (pinned by a test). (2) `ComputeGridAlphaValues`: per-vertex code of both states
(new first), both UV meshes computed with the **new** state's warp animation terms (MD2 does so), blended per vertex
`uv = uv_new*mix2 + uv_old*(1-mix2)`, `mix2 = clamp(a*progress + c)` from the blend pattern, vertex alpha `(int)(mix2*255)`.
(3) The new state's [`FrameInput`] is built from the blended variables (`frame_head`) and carries `md2_blend`: vertex alphas, the old
state's `q`, blur range and shader random state, the second wave mode (`its == 2` iff `new_wavemode != m_nOldWaveMode`), and
`alpha_mult` of every shape instance / custom wave (`progress` for the new state's, `1 - progress` for the old one's; draw order:
new shapes, old shapes, new waves, old waves, main wave). The video echo flips orientation / fades across the blend as in
`ShowToUser_NoShaders` (`m_fVideoEchoAlphaOld`, snap compared with the *linear* progress). The shared `rand()` stream sees, per
frame: new per-frame, old per-frame, new per-vertex, old per-vertex, new shapes, old shapes, new waves, old waves.

**Passes (renderer).** Warp: `(old uses a warp shader, new does)` selects `warp_passes`: both shaders = old shader opaque on the tiles
with any alpha < 255, then the new shader alpha-blended on the tiles with any alpha > 0; one of each = the fixed-function
`WarpedBlit_NoShaders` takes the missing side (decay of the *blended* state, per-vertex alpha); neither = one un-culled, un-blended
fixed-function pass ("all the blending just happens in the vertex UV's"). Composite: `ShowToUser_NoShaders` first when either side
has no composite shader; old shader first (opaque) then new (alpha) when both have one; "THA FUNKY REVERSAL" = fixed function, then the
*old* shader with flipped alpha (`INVSRCALPHA/SRCALPHA`) and flipped culling. The composite mesh's vertex alpha is the bilinear
lookup of the warp mesh's vertex alphas at the comp vertex (`ShowToUser_Shaders`), truncated to a byte like `D3DCOLOR_RGBA_01`.
Blur levels count the old shaders too (`ApplyShaderParams` of every applied pass), with the blended blur range.

**Preset loaded during a blend / hard cut.** A soft cut while blending makes the half-blended state the old one (its
`CBlendableFloat`s are still ramping: the new state blends from their current value) and drops the previous old state and its
shaders at once (`LoadPresetTick` rotates `m_pState/m_pOldState/m_pNewState`). A failed load puts the running shaders back and
changes nothing. `Transition::Cut` (and `Soft { seconds <= 0 }`: MD2's `fBlendTime == 0`) ends a running blend immediately and
takes the usual cut path (new renderer, `draw_initial_image` unless `start_clean`).

**`progress`, `time`, `frame`.** `time` / `frame` are the global clock for both states. While blending `progress` is MD2's
`(GetTime() - m_fPresetStartTime) / (m_fNextPresetTime - m_fPresetStartTime)` for both states (`m_fPresetStartTime` = clock time of
the load, `m_fNextPresetTime` = time of the first frame after the load + `EngineConfig::preset_duration`; no random extra, no
auto-switch), in `float` and unclamped; outside a blend (also after one) it keeps the engine's usual
`min(1, elapsed / preset_duration)` (measured from the same load time). The blend `progress` and the window follow the same float
arithmetic: a 1 s blend loaded before frame 10 is drawn on frames 10..=69.

**Determinism.** Pattern kind: `StreamRng::new(seed, "md2_blend_pattern").next32() % 19` once per soft cut (unless forced); the
`FRAND` (`uniform_real_distribution<float>(0,1)` of `std::mt19937_64 gen`, seeded from `std::random_device` in MD2) and `rand()` of the
patterns are the next `[0,1)` float (`next32 >> 8` / 2^24) and the next 15-bit integer (`next32 >> 17`) of the same stream; the
animated patterns' `static float` clocks advance by `1/fps` per generation. The extra random draws of the shader passes use the
existing `shader_warp` stream of each preset (the old preset's `rand_frame` is replaced by the new one's: one `m_rand_frame` per
frame). Everything else is deterministic in the engine seed.

**API.** `Engine::transition_progress()` is the linear `m_fBlendProgress` (as of the last rendered frame, `None` when not
blending); `Engine::set_md2_blend_pattern(Some(k))` forces `m_nTransitionBlendPattern` (0 fade, 1 directional wipe, 2 plasma,
3 radial, 4 clock, 5 spiral, 6 rhombus, 7 nuclear clock, 8 square, 9 checkerboard, 10 curtain, 11 bubbles, 12 kaleidoscope,
13 moebius, 14 star, 15 disco floor, 16 fire, 17 whirlpool, 18 Julia; `None` = -1 = random); `Engine::md2_blend_pattern()` is the
kind of the blend in progress. `fdrender --compat md2 --preset2 P --switch-frame N --soft-cut S` runs it.

**Not ported / differences** (unresolved, listed in `docs/divergences.md`): `CosineInterp` is defined in vis_milk2's `utility.h`, which
is not part of `third_party/md2`: `0.5 - 0.5 cos(pi x)` is reconstructed (it is also projectM's cosine progress). `m_fBlendTimeAuto` /
random switch timing, `m_fNextPresetTime`'s random part, the one-frame loading ticks (`m_nLoadingPreset`: lactea builds the
preset synchronously) and the `m_bWarpShaderLock/CompShaderLock` import are not modelled. `fft`/`wave` shader textures, sprites and
BeatDrop wave modes above 8 stay out of scope: a main-wave blend whose *old* mode is one of those draws the new wave alone, a new mode
above 8 draws nothing. Shaders that fail to translate count as "no shader" for the pass tables and the snap point (MD2 would use its
fallback shader). Teacher forcing applies to the new state only. The no-blend `wrap` sampler keeps the engine's `wrap > 0.0001`
(MD2: `> m_fSnapPoint` = 0.5), a blend frame uses `> m_fSnapPoint`. `nVideoEchoOrientation` of the flip case is not reduced `% 4`
before the swap (the renderer reduces it). Composite hue colours stay unquantised, as in the unblended path.

## Teacher forcing

`ForcedFrame { vars, prev_main }` (see `teacher.rs`): after the per-frame equations the engine overwrites
every per-frame variable present in its context (builtins, `q*`, user variables, `reg00..99`) with the oracle's,
overrides `audio.*`, `hue_offset.*`, the per-pass `warp.*`/`comp.*` `rand_frame`/`rand_preset`/`rot_*`, and
injects the previous `main` image. Frames 0..n-1 still run so megabuf and expression state evolve.

## fdrender

```
fdrender --preset P --audio WAV --out DIR [--size 640x360] [--mesh 48x36] [--fps 60] [--frames 300]
         [--seed 1] [--texel-offset 0,0] [--stages final,main,warped,blur1,blur2,blur3|all|none]
         [--vars] [--textures] [--texture-dir DIR] [--teacher REF] [--inject-noise REF] [--compat projectm|md2|md3|auto]
         [--auto-hard-cut 0..7]
```

Same flags and output layout as `oracle/refrender` (`lactea_abi::dump`, producer
`lactea@<version>+<git sha>`), `fdrender.json` instead of `oracle.json`. PCM feeding is
`pcm_range(n, sr, fps)`, frame time exactly `n/fps`. `--texture-dir` default `oracle/build/texpack`
(the pack the oracle used); JPEGs are decoded with the `image` crate (stb_image in projectM: ±1-2 code
values). `--inject-noise REF` loads the oracle's `textures/noise*` (projectM's noise generation order is
compiler-defined); `--teacher REF` implies it. Exit codes: 0 ok, 1 usage/IO, 2 preset load failure,
3 shader fallback (frames still rendered), 4 wgpu validation errors.

Soft cuts (mirror of `refrender`'s transition flags): `--preset2 P --switch-frame N --soft-cut S [--start-clean 0|1]
[--transition-shader K] [--transition-static a,b,c,d]` and, to isolate stages of a transition dump,
`--teacher REF` also forces the incoming preset from the reference's `new_vars/` / `new_main/`, and `--blend-teacher REF`
feeds the reference's `old_out` / `new_out` to the transition pass. Extra dump stages (not ABI, written like `warpsrc`):
`old_out`, `new_out` (the two images before the blend), `new_main`, `new_warped`, `new_blur1..3`, `new_warpsrc`,
`new_vars/` (the incoming preset's, on the frames the blend is drawn). `oracle/transition_pairs.txt` lists the pair set.

`--auto-hard-cut N` sets MilkDrop 3's `AutoHardCutMode` (0 = off, 1..=6 beat-driven instant preset changes, 7 composite-shader
effects; only with `--compat md3`, ignored with a note otherwise; N > 7 is a usage error). The tool has no preset switching, so the
cuts of modes 1..=6 are only counted (`"auto_cuts"` in `fdrender.json`, present when the mode is active); mode 7's effects show in
the rendered frames.

`--compat auto` (default stays `projectm`, the fidelity tool) runs the engine with `auto_compat`: each preset (also
`--preset2`) resolves to projectM or MilkDrop 3 from its bytes, `fdrender: compat auto -> md3 (evidence: ...)` is logged,
`fdrender.json` gets `"compat"` (resolved mode of the first preset) plus `"compat_auto": true` and `"compat_evidence"`,
and the MD3-only flags (`--mouse`, `--volume-sensitivity`, `--auto-hard-cut`) apply when the preset resolved to MD3. A
`--preset2` of another mode is a hard cut (`md3.md`, "Automatic compatibility").

`scripts/fidelity.sh SET [FILTER]` runs free + teacher runs for a whole oracle set and `fdcompare corpus`
(see the script header); `scripts/fdq.py` (quick PSNR probe)
and `scripts/probes/*.milk` (tiny presets rendered through the oracle to isolate one feature) are the
development tools. Results and open issues: `docs/fidelity.md`.

## Tests

`CARGO_TARGET_DIR=target-engine cargo test -p lactea -p lactea-cli` — unit tests (random streams,
texture-pick rules, variable tables), `tests/smoke.rs` (corpus presets render 30 frames each through the
engine at 160×90 with a wgpu validation error scope; compile failure / empty preset / unresolved textures),
CLI end-to-end (`tests/cli.rs`: identical runs are byte-identical; a teacher run fed with a free run's own
dump reproduces it exactly). `tests/transition.rs` (soft cut window / both presets running / `progress`
/ seeded shader choice = oracle values / `start_clean` / failed load during a blend) and
`crates/lactea-render/tests/transition.rs` (blender: cosine mix, orientation, endpoints, sRGB targets).
`tests/transition_md2.rs` (MD2 blend through the engine: window, hand-computed variable blend, first frame = old preset alone,
UV blend `mix2 * dx`, shape `alpha_mult`, `progress`, load / cut / failed load during a blend, seeded pattern, wipe splits for warp /
composite shaders, all 16 shader combinations without validation errors) and the pure-logic unit tests in
`crates/lactea/src/engine/md2_blend.rs` (`CBlendableFloat`, snap points, variable blend, `mix2`, the 19 patterns) and
`crates/lactea-render/src/blend.rs` (tile culling, pass tables, composite alpha lookup, two-wave blend, `CosineInterp`).
`tests/auto_compat.rs` (`EngineConfig::auto_compat`: a projectM / MD3 / `.milk2` playlist through one engine, hard cut on a mode
change, failed load keeps the mode, determinism) and `crates/lactea-preset/src/detect.rs` (detection heuristics and
their false-positive guards).

## projectM API parity additions

Feature-by-feature audit against projectM's headers: `docs/parity.md`. What it added to the engine crate:

* **`PresetControl`** (`control.rs`; standalone, the host drives it): projectM's `TimeKeeper` + the switch
  logic of `ProjectM::RenderFrame`. `start_preset(t)` / `start_smoothing(t)` when the host loads a preset,
  `update(time, vol)` once per rendered frame (`vol` = `engine.audio_frame().vol`) returns
  `Some(SwitchRequest { hard_cut })` once per preset: display time up (`progress >= 1`, no soft cut
  running) or, with hard cuts enabled, `frame > 50 && vol - previous vol > sensitivity && shown longer than
  the hard-cut duration`. Settings and defaults are projectM's (preset 30 s, soft cut 3 s, hard cut off /
  20 s / 2.0, easter egg 1.0, beat sensitivity 1.0 clamped to 0..2, aspect correction on). Beat
  sensitivity and aspect correction are stored only (projectM reads them nowhere). The easter egg samples the
  display time from a seeded gaussian. `progress` (the preset variable) still uses `EngineConfig::preset_duration`
  (`Engine::set_preset_duration`), not the sampled one.
* **`Playlist`** (`playlist.rs`): `projectm_playlist_*` as pure data (filters, shuffle, history, retry,
  sort, events); loading is a closure passed to `play_next` & co. Seeded shuffle.
* **Runtime parameters** (`params.rs`, a child module of `engine`): `set_mesh_size` (projectM's clamp: odd →
  even, 8..=300; `Renderer::set_mesh_size` rebuilds the grid and forgets the previous frame's warp UVs),
  `set_texel_offset`, `set_fps`, `set_preset_duration`, `last_frame_time`, `window_size`, `push_pcm_i16/u8`.
* **Idle preset** (`idle.rs`): `Engine::load_idle_preset()` loads the original built-in `IDLE_PRESET`.
* **Shape images** (`shape_images.rs`): `shapecode_N_image` (projectM flavour only). `load_preset` asks the
  `TextureProvider` for the name with its `fw_/fc_/pw_/pc_` prefix stripped; unresolved names are listed in
  `PresetReport::unresolved_textures` and drawn as a 1x1 black placeholder.
