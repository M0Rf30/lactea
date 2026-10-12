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

## MilkDrop 3 (`Compat::MilkDrop3`)

`Compat::MilkDrop3` = MilkDrop 2 plus the rows below; everything not listed is as in MilkDrop 2. Every row is a
`CompatRules` field (`crates/lactea-render/src/config.rs`) that is `false` / unchanged for `ProjectM` and `MilkDrop2`.
Details and verification in [`md3.md`](md3.md).

### Rendering

| Item | projectM (default) | MilkDrop 2 | MilkDrop 3 |
|---|---|---|---|
| Main waveform modes (`md3_wave_modes`) | projectM's 16 `WaveformMode`s (`% 16`, preset's initial value) | `wavegen::main_wave_geometry` port: modes 0..8 (`% 18`), left+right averaged in most modes; 9..17 draw nothing | 17 modes `0..=16` selected by the C remainder `(int)wave_mode % 17`; one channel per formula (`wavegen_md3`); mode 9 has a zero second strip, 13 reads the spectrum past the right channel, 14 halves `rad` / `ang`, 16 is the triangle spinning with `-0.2 t`; BeatDrop's mode 17 is mode 0; a negative remainder draws 480 zero vertices |
| Custom-shape `sides` (`max_shape_sides`) | `3..=100` | `3..=100` | `3..=500` (`sides + 2` fan vertices) |
| Warp `sin` / `cos` terms (`md3_warp_skip_tiny`) | always added (GPU vertex shader) | always added | added only if `\|warp\| > 0.0001` |
| Borders during a preset blend (`md3_border_gate`, `EngineConfig::md3_disable_box`) | drawn | drawn | not drawn while blending when `DisableBox` is on (default), and never while a `.milk2` is on screen (also with `DisableBox` off) |

### Preset switching

| Item | projectM (default) | MilkDrop 2 | MilkDrop 3 |
|---|---|---|---|
| Auto hard cut (`md3_auto_hard_cut`; `EngineConfig::auto_hard_cut_mode`, `Engine::set_auto_hard_cut_mode`, F8 in the app, `--auto-hard-cut`) | none (projectM's own `hard_cut_*` volume-jump rule is a separate setting and stays) | none | modes 1..7 watch the previous frame's `bass` / `treb`: modes 1-6 request a blend-time-0 preset change (mode 4 can ask twice in a frame), mode 7 injects a random effect into the composite shader instead; independent of both hard-cut rules above |
| Display time of a preset (`md3_preset_time_rand`; `PresetControl::set_md3_time_rand`) | gaussian (`easter_egg` sigma) around the preset duration | BeatDrop draws `uniform(0,1) * TimeBetweenPresetsRand`; `Compat::MilkDrop2` does not model it and keeps projectM's gaussian | `BlendTimeAuto + TimeBetweenPresets + (rand() % 1000) * TimeBetweenPresetsRand * 0.001` |

### Audio and shader inputs

| Item | MilkDrop 2 / BeatDrop | MilkDrop 3 |
|---|---|---|
| Audio analysis (`md3_audio_analysis`, `AnalysisMode::Md3`) | BeatDrop: mono `(L+R)/2`, Hz bands, silence 0 | left channel only, 576-sample Hann FFT zero-padded to 1024 on the aligned 8-bit-quantised waveform times `VolumeSensitivity`, bins `[0,85)`, `[85,170)`, `[170,256)` summed, silence 1.0 |
| `get_fft` / `sampler_fft` data (`md3_shader_fft`) | BeatDrop: mono 1024-bin RMS-normalised FFT, FPS-independent attack / decay, `sqrt`, peak-hold row | left equalised spectrum, per-frame max-normalised, 0.2 gate, squared, 3-bin smear, attack `0.5a+0.5` / decay `0.1d+0.9` once per rendered frame; the state survives preset changes |
| `FFTAttack` / `FFTDecay` (`md3_shader_fft`) | per state, blended in soft cuts | process-global, replaced by every preset import (file value or 0.5 / 0.7), never blended; a `.milk2` imports PRESET1 then PRESET2, so PRESET2's keys (or 0.5 / 0.7) win |
| Shader prelude (`md3_shader_prelude`) | `get_fft` = `sqrt(tex2D(sampler_fft, (x, .25)))`, `get_fft_hz` `/22050`, `get_fft_peak*`, `get_wave*`, `sampler_wave`, `_c15` / `_c16` time / date | `get_fft` without `sqrt` at v = 0.5, `get_fft_hz` `/24000`, none of the other helpers; `_c14` mouse, `mouse*` macros, `q33..q64` (`_qi.._qp`) |
| `q` / `t` variables (`md3_extended_vars`) | `q1..q32`, `t1..t8` | `q1..q64`, `t1..t16` in every VM that has them, `monitor2` / `monitor3` persisted like `monitor`, no q blending |
| Mouse (`md3_extended_vars`) | BeatDrop `mousex` ... | shader-only: `_c14` / `mouse*` (top-origin y, `-1, -1` outside the window, `z` held, `w` one frame after release). Preset expressions cannot read it: `mouse.x` does not compile in MD3 3.37 (measured), so no EEL variable is registered |
| Shader `time` (`md3_shader_time`, `_c2.x`) | absolute frame time | seconds since this state's preset started, modulo 10000 (the outgoing state of a blend uses its own start) |
| Generated composite shader of presets without `comp_N` (`md3_md2_comp_effects`) | brighten `1-(1-ret)^2`, solarize `*2` | brighten `sqrt(ret)`, solarize `*4` |

### Expressions (ns-eel2)

| Item | projectM / MilkDrop 2 | MilkDrop 3 |
|---|---|---|
| Expression language (`md3_ns_eel2`) | projectm-eval (`Dialect::ProjectMEval`) | ns-eel2 (`Dialect::NsEel2`) |
| `/` | `\|divisor\| < 1e-5` gives 0 | unguarded: ±Inf / NaN (`1/0.000001 = 1e6`) |
| `=` and compound operators | all store the raw result | `=` stores 0 for NaN / Inf / denormal / `-0`; `+= -= *= /= %= ^= \|= &=` store the raw result |
| `%`, `%=` | signed C remainder of the truncated 64-bit operands (`-7 % 3 = -1` through a variable) | `\|a\| mod \|b\|` of the rounded 32-bit operands, zero divisor 0 (`b=-7; b%3 = 1`) |
| Conversions (`&`, `\|`, `%`, `loop`, memory index) | truncation | round to nearest even |
| Operator levels | `%` below `^`; a unary `-` binds tighter than `^` and `%` | `%` and `^` one left-associative level above `*`; a unary `-` covers the whole `%` / `^` chain (`-7%3 = -1`, `-2^2 = -4`); `&` / `\|` below `+` as before |
| Number literals | exponent notation (`1e-5`) | none: `1e-5` does not compile (`.5`, `5.` do) |
| Identifiers | letters, digits, `_` | the same: `mouse.x`, `a.b` do not compile (the spec's `mouse.x` EEL access does not exist in MD3 3.37) |
| `c ? x` without `:` | syntax error | `c ? x : 0` |
| Truthiness (`if`, `&&`, `\|\|`, `!`, `while`) | `if`: `!= 0`; `&&` / `\|\|` / `while`: `\|x\| > 1e-5` | `\|x\| >= 1e-5` everywhere (NaN false); `==` / `!=` as `\|a-b\| < 1e-5` / `>= 1e-5` |
| `pow`, `log`, `log10`, `asin`, `acos`, `invsqrt` | guarded to 0 / double magic number | raw IEEE results; `invsqrt` float32 magic number |
| `sign`, `sqrt` | `sign(±0) = 0` | `sign(-0) = -0` until stored; `sqrt(\|x\|)` in both |
| `$PI`, `$E`, `$PHI` | float32 values | `3.141592653589793`, `2.71828183`, `1.61803399` |
| `megabuf` index | `trunc(i + 0.0001)` | `round(i + 1e-5)` |
| `gmegabuf`, `reg00..reg99` | per preset, 8M entries, out of range reads 0 | one 1,048,576-entry array that wraps, shared by all presets and never cleared; registers shared by all presets |
| `memcpy`, `memset` | `trunc(x + 0.0001)` | `trunc(x)` |
| Callable functions | table plus `_add _sub _mul _div _neg`, bare `gmem` is a keyword | the 53-entry table only; a bare `gmem` is a variable |
| Code that fails to compile (`md3_drop_failed_code`) | the preset is rejected (`EngineError::Compile`; BeatDrop itself drops the part) | the part is dropped with a warning and the preset loads; a failed init leaves `q` at 0. Sprite code and both halves of a `.milk2` follow the same rule (and the same dialect) |

### Double presets (`.milk2`) and blend patterns

| Item | MilkDrop 2 / BeatDrop | MilkDrop 3 |
|---|---|---|
| `.milk2` double presets (`md3_double_preset`) | not a format: loading one is an error (`EngineError::Unsupported`) | two presets in one file played as a frozen MD2 soft cut: PRESET1 = `m_pOldState`, PRESET2 = `m_pState`, per-vertex mix `clamp(a·P + c)` with `P = blending_progress` held indefinitely, both presets run all their equations, `CosineInterp(P)` for per-frame variables, `P` / `1-P` for shapes and waves; outer / inner borders are never drawn. Details and deviations: [`md3.md`](md3.md) "Double presets" |
| Blend patterns of soft cuts (`md3_blend_patterns`) | BeatDrop's 19 kinds (`TRANSITION_LIMIT`), `FRAND` inside each generator | MD3's 30 patterns; every random value is drawn up front (`pattern, direction, r1..r5`) and the generators are pure; the pattern is a uniform element of `EngineConfig::md3_transition_patterns` (default: the 13 MD3 enables), `Engine::set_md2_blend_pattern` forces any of the 30 |
| Soft-cut random source | `md2_blend_pattern` stream (`FRAND` = 24-bit fraction) | the stream `md3_blend_pattern` with MSVC `rand()` semantics: `rand() = next32 >> 17`, `FRAND = (rand() % 7381) / 7380`, direction `2 * (rand() & 1) - 1`; MD3 reseeds the C `rand()` from `time(0)` before every draw, which has no equivalent here (seeded streams, determinism contract) |
| Header keys missing from a `.milk2` | n/a | MD3 keeps the process-global values of the previous load or soft cut; lactea keeps them in `Engine` state (`blending_pattern`, `blending_progress`, `blending_direction`, `random_1..5`), initial values as at process start (`""`, 1.0, 0, 0.0) |
| `.milk2` marker scan | n/a | `fgets(255)` chunks are counted, not lines (a line of 254+ bytes shifts every later marker): reproduced |
| `.milk2` header number that does not parse | n/a | MD3's `std::stof` / `std::stoi` throw (no handler, process dies): `EngineError::Milk2`, the load fails and the previous preset stays |
| Hard load of a `.milk2` | n/a | MD3 shows PRESET1 alone for about seven frames (staged load) before the mix appears; lactea builds both halves at once and the first frame already shows the mix |
| Request for another preset while a `.milk2` is on screen | n/a | MD3 defers playlist-driven requests (`progressMode` 5: the ramp runs on to `P = 1`, then the request loads) but loads menu / drag&drop requests directly; the engine cannot tell the two apart, so every `Transition::Soft` request is deferred and a second request while leaving is ignored. The deferred request is parsed at once (errors of the preset text) but built when the ramp ends: build failures then reach the switch-failed callback |
| PRESET2 failing to build at the end of the first soft-in leg | n/a | MD3 reports the failed load; here the switch-failed callback runs and PRESET1 stays alone on screen |
| `DoublePresetMode` (F9 random pairs), `AutoHardCutMode`'s first-six-patterns rule, the `p` / `P` / `o` / `U` / `u` live edits | n/a | not played / exposed; `md3_random_instance` implements the draws (including both) for hosts that build pairs |
| Sprite of a `.milk2` | n/a | the block belongs to PRESET2 and is read only with the header's `sprite=1` (a block inside either half is ignored); a hard load builds it with PRESET2, a soft transition when the first leg completes |

### Sprites and animated textures

| Item | projectM / MilkDrop 2 | MilkDrop 3 |
|---|---|---|
| Preset sprite (`md3_preset_sprites`): `[SPRITE1_BEGIN]` block, one image, 11 blend modes, layers 0..4 | the block is ignored (projectM, MD2: no preset sprites) | the sprite is loaded with the preset and drawn every frame ([`md3.md`](md3.md#sprites)) |
| Sprite burn in layers 1..4 (`md3_sprite_burn_twice`) | n/a | a burning sprite is drawn twice into the feedback image: `SpriteBurn=1` (every pack preset) makes additive / alpha sprites twice as strong as `SpriteBurn=0`. Measured against MD3 under Wine (spec open question 1) |
| Sprite `progress` variable | n/a | the blend's progress (`m_pState->m_fBlendProgress`: `blending_progress` in a steady `.milk2`), 1 when no blend runs (inferred: MD3 leaves the value just above 1); not the preset progress |
| Sprite code that does not compile | n/a | the code is not executed (MD3 is silent), the sprite is still drawn: the same rule as `md3_drop_failed_code`; a warning is reported |
| Shader textures from GIF / `sprites` / `gifs` (`md3_gif_textures`) | `sampler_<name>` searched in the texture folders with `jpg jpeg dds png tga bmp dib`; a still image | also `.jfif` / `.gif`, folders `textures`, `sprites`, `gifs`; an animated GIF shows frame `fmod(time, total)` of its delays |
| Animated image provider API | `TextureProvider::load` returns one still image | `load_md3_texture` / `load_sprite` may return `Media::Animated` (frames + delays); hosts decode GIFs with `lactea::decode_gif` (GIF89a disposal) |
