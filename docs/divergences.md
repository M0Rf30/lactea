# projectM vs MilkDrop 2 divergences

lactea defaults to projectM behaviour (the measurement oracle) and implements the MilkDrop 2
behaviour behind `Compat::MilkDrop2` (render) / `AnalysisMode::BeatDrop` (audio). MD2
references are to `third_party/md2/`. `Compat::MilkDrop3` is MilkDrop 2 plus the MilkDrop 3 deltas:
every column below headed "MilkDrop 2" applies to it too, and its own differences are listed in
[`md3.md`](md3.md).

## Render (`lactea-render`, `Compat`)

| Item | projectM (default) | MilkDrop 2 |
|---|---|---|
| `vol`, `vol_att` (`_c3.w`, `_c4.w`) | `(bass+mid+treb)*0.333` | `0.3333*treb` (comma-operator bug in `ApplyShaderParams`) |
| `mip_y` | from height | from width |
| `_c1` | zeros | aspect "sd" |
| No-shader decay | `min(decay,1)` as float | `(int)(decay*255)&0xFF` |
| Vertex colours | float, modulo-wrapped | 8-bit truncated |
| No-shader composite hue tint | always applied | only if `fShader > 0.001` |
| Thick/dots main-wave passes | whenever `thick \|\| dots` | also requires texsize ≥ 512 |
| Shape thick outline offset | half a pixel | one pixel |
| Motion vectors | skipped on frame 0; min length 1.25 px; geometric lookup | drawn on frame 0; 1 px; mirrored-row lookup (`ReversePropagatePoint`) |
| Comp-shader `sampler_main` | current frame | previous frame |
| Warp mesh `ang` seam | no fix | patched to -π / +π |
| Warp mesh cell diagonal | mirrored (projectM mesh row 0 = top) | MD2 orientation |
| Warp animation terms' `y` | opposite sign fed to the four sin/cos terms | MD2 sign |
| Blur pass 0 source | sampled with `uv.y` flipped (+1 texel bias goes the other way) | MD2 bias |
| No-shader composite hue corners | `shade[0..1]` are the top corners | MD2 order |
| Border / MV / main-wave / shape-outline colours | raw values (clamped by GL) | `Color::Modulo`-style wrap |
| First frame after preset load (the frame that allocates the blur textures) | presets whose shaders sample `GetBlur1..3`: no shapes/waves/main wave/darken-centre/borders (`BlurTexture::Update` restores FBO 0, projectM bug); all other presets draw them normally | overlays always drawn |
| First shape draw after a blur update | presets that sample blur: a textured first shape reads the main image with the leaked linear/clamp `BlurTexture` sampler (`Kind::TexLeak`); later shapes nearest/repeat | bilinear everywhere (no sampler objects) |
| `gamma`, `echo_*`, brighten/darken/solarize/invert, main-wave mode/flags | preset's initial values | per-frame variable values |
| Shader random streams | separate warp/comp streams, `20*9` constructor draws after `rand_preset`, `rot_*` translation row dropped | one shared stream per preset |
| Lines / points | GL rules emulated exactly (`StrokeRaster::Emulated`): diamond-exit for smoothed lines, half-open strip rule for waves, 1-px floor points | D3D9 thin quads, point sprites |
| Custom-wave dots / thick | the 2n−1 `SmoothWave` vertices as dots; thick offset `1/W` ndc in x and y | n source points; one-pixel offsets |
| Custom-wave point position | `static_cast<float>((x*2-1) * invAspectX)`: the product in double, one rounding (`wave_point_f64_aspect`) | `(float)(x*2-1) * invAspectX`: rounded before the float product |
| Motion vectors (geometry) | mirrored, half-float prev-UV lookup, stale vertex slots draw from bottom-left | MD2 `ReversePropagatePoint` |
| No-shader composite quad diagonal | TR–BL (strip TL,TR,BL,BR) | triangles (0,1,2),(2,1,3) |
| Main waveform | `Waveforms/*.cpp` (240/÷3 samples, per-mode channels, 512-slot smoothing, modes 0–15) | `DrawWave` port (modes 0–8) |
| Per-pixel `ang` on the centre row | `-π` (`-atan2f`) | `+π` (seam-patched) |
| Comp mesh UV | `+0.5/size` | exact |
| Warp-shader `VsOut.diffuse` | `(decay, decay, decay, 1)` | `(1, 1, 1, 1)` |
| Texel offset | 0 (modern pixel centres) | 0.5 texel (cancels D3D9 pixel centres; net-equivalent to 0) |
| Preset change (`Transition::Soft`, rule `md2_preset_blend`) | two independent presets (own renderer, feedback image, shaders), each rendered to its composite image every frame; one of six built-in full-screen transition shaders (circle, plasma, simple blend, sweep, warp, zoom blur; chosen at random per soft cut) blends the two finished images with the cosine progress | one pipeline, two *states* (`m_pOldState`) and one renderer (old shaders / textures kept as `m_OldShaders`): per-frame variables blended by `CosineInterp` (and the base values by `CBlendableFloat` ramps), per-vertex blended warp UVs (`mix2` from one of 19 random blend patterns), old/new warp and comp shaders drawn as alpha-tiled passes (`WarpedBlit_*` / `ShowToUser_*` tables), old/new custom shapes / waves with `alpha_mult`, two-wave main-wave blend, video-echo flip; see `docs/engine.md` |
| `progress` while a soft cut runs | `1.0` for both presets (`PresetProgressA()` while smoothing), then `elapsed / preset_duration` from the start of the soft cut | `(time - m_fPresetStartTime) / (m_fNextPresetTime - m_fPresetStartTime)` for both states (`LoadPerFrameEvallibVars`) |
| Start of a new preset (`EngineConfig.start_clean = false`) | first frame warps from the old active preset's composite image (`DrawInitialImage`); `true`: black | the feedback buffers (`m_lpVS`) simply continue |
| Expression `rand()` across presets | one process-wide Mersenne Twister shared by every preset, also the two of a blend (`Engine::new` seeds it once) | both states' per-frame equations run every blend frame, the new state first (`RunPerFrameEquations`) |
| Blend pattern randomness (MD2 soft cut) | n/a (shader choice: `transition_select` / `transition_static` streams) | `FRAND` / `rand()` of `RandomizeBlendPattern` are one seeded stream, `md2_blend_pattern` (`std::mt19937_64 gen` seeded from `random_device` and the C `rand()` in MD2); `Engine::set_md2_blend_pattern` forces `m_nTransitionBlendPattern` |
| Preset loaded while a soft cut runs | the running transition completes at once (incoming becomes active), then the new one starts | the half-blended `m_pState` becomes `m_pOldState`, the previous old state and shaders are dropped, the new state blends from the current `CBlendableFloat` values |
| Hard cut / `Soft { seconds <= 0 }` while a blend runs | n/a | ends the blend at once (`Default()` clears `m_bBlending`), usual cut path |
| `progress` / `m_fSnapPoint` / blend-frame `wrap` | n/a | blend frames: `progress` as above; sampler address `wrap > m_fSnapPoint` (snap point 0.5 / -0.01 / 1.01 by the shader flags); unblended MD2 frames keep `wrap > 0.0001` |
| Shader translation failure during a blend | n/a | counts as "no shader" for the pass table and snap point (MD2 would draw its fallback shader) |
| No-shader composite sampler address (`noshader_comp_wrap`) | clamp | wrap |
| `shapecode_N_image` (`custom_shape_image`) | read (projectM addition) | not a key; the line stays in `Preset::extra` |

Shared quirk kept without a switch (present in both): `GetSafeBlurMinMax` typo (`max = avg - 0.05`).
Deliberately not copied: MD2's one-frame lag of comp-shader blur usage (`m_nHighestBlurTexUsedThisFrame`).

## Audio (`lactea-audio`, `AnalysisMode`)

| Item | projectM (default) | BeatDrop MD2 (`third_party/md2`) |
|---|---|---|
| Bands | bins `[0,85)`, `[85,170)`, `[170,256)` of the mono 512 spectrum | Hz bands 0–250, 250–4000, 4000–20000 |
| FFT input | 480 damped samples (`0.5·(x[i]+x[i-1])`) | zero-padded 1024 window |
| Silence | all `*_rel` = 1.0 | 0 |

Input scaling: float PCM ×128; ring of 576 frames; mono duplicated; >2 channels ignored. See
`docs/audio.md` for the full table.

## Preset parsing and expressions

`lactea-preset` reproduces MD2's import (`state.cpp`) byte-for-byte (`docs/preset.md` lists the quirks);
`Flavor::Md2` serves the whole MD2 family, `Flavor::ProjectM` is projectM's `PresetFileParser`.

| Item | projectM (default) | MilkDrop 2 / MilkDrop 3 |
|---|---|---|
| Joining of code lines (`md2_code_join`; init, per-frame, per-pixel, wave init / per-frame / per-point, shape init / per-frame) | lines joined with `\n`: an identifier split over two lines (`per_frame_22=k1 = is_` / `per_frame_23=beat*...;`) is two tokens and fails to compile | `CState::StripLinefeedCharsAndComments` (`state.cpp:1593`): the `\x01` separators are deleted without a replacement and `//` / `\\` comments are dropped up to the next separator, so the halves glue together (`k1 = is_beat*...;`). A block that is only blanks or strips to nothing compiles no code (`buf[0] == 0`). Shader text is not touched: it keeps its lines (`code_to_source`), `#define`s need them |
