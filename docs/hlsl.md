# lactea-hlsl: MD2 HLSL -> WGSL

Translator for MilkDrop 2 warp / composite pixel shaders. `translate(kind, src, &Options)`
returns `Translated { wgsl, interface: ShaderInterface, texsize_params, warnings }`.
`src` is the preset's `warp_N=` / `comp_N=` lines (numeric order, stop at the first missing
number, ONE leading backtick stripped per line — MD2 `ReadCode`) joined with `\n`.

```
translate(kind, src)
 └ wrap::md2_wrapped_hlsl      CPlugin::LoadShaderFromMemory text assembly (plugin.cpp 4019+)
 └ pp  (C preprocessor)        #define (object/function-like, ##), #undef, #if/#ifdef/#ifndef/#elif/#else/#endif
 └ lex / parse                 HLSL SM2/SM3 subset -> AST
 └ emit (type check + WGSL)    only code reachable from `PS` is translated
 └ iface                       CShaderParams::CacheParams sampler rules -> ShaderInterface
```

Output = `diagnostic(off, derivative_uniformity);` + `abi::WGSL_COMMON` + group(1) texture/sampler
bindings (`texture_binding(i)` / `sampler_binding(i)`, `texture_3d` for `noisevol`) + private
globals + functions + `@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32>`.

## What MD2 does (and we reproduce)

* Text: `strip_comments(include.fx)` + per-pass `#define`s + comment-stripped user text, where the
  first `shader_body` becomes `void PS( float4 _vDiffuse : COLOR, float4|float2 _uv : TEXCOORD0,
  float2 _rad_ang : TEXCOORD1, out float4 _return_value : COLOR0 )`, `float3 ret = 0;` is inserted
  after the first following `{`, and everything from the *last* `}` on becomes
  `_return_value = float4(ret.xyz, _vDiffuse.w); }`. A shader without `shader_body`/`{`/`}` is an
  error (`TranslateError::Wrap`; MD2 falls back to its built-in shader).
* Warp defines: `rad=_rad_ang.x ang=_rad_ang.y uv=_uv.xy uv_orig=_uv.zw`. Comp defines: same, but
  **`uv_orig = _uv.xy`** ([sic] in MD2; `_uv` is a float2 there) and `hue_shader = _vDiffuse.xyz`.
  `fs_main` calls `PS(in.diffuse, in.uv | in.uv.xy, in.rad_ang, &rv)`.
* Default texts when a preset has a PS version but no text: `default_warp_shader(decay, wrap)`
  (`GenWarpPShaderText`) and `default_comp_shader(...)` (`GenCompPShaderText`).
* `include.fx` is parsed as HLSL (copy in `src/include.fx`); its globals map onto the frozen
  `MilkUniforms` (`time -> mu.c[2].x`, `q5 -> mu.q[1].x`, `rot_s1 -> mu.rot[0]`,
  `texsize -> mu.c[7]`, `texsize_noise_lq -> mu.texsize_noise[0]`, ...). A shader that writes to a
  uniform (`q25 = ...`, 27 corpus shaders) gets a private per-invocation copy (HLSL semantics).
* Samplers (`CacheParams`): only samplers the shader actually uses are bound, in declaration order
  (include.fx first, then user declarations). `sampler_` prefix removed, a
  3-char `XX_` prefix is peeled (also when not recognised): `FW/WF` bilinear+wrap, `FC/CF`
  bilinear+clamp, `PW/WP` point+wrap, `PC/CP` point+clamp. Defaults: bilinear + wrap, except
  `blur1..3` (bilinear + clamp), `fft`/`wave` (bilinear + clamp; exposed as `TexSource::User("fft"|"wave")`).
  `main`, `blur1..3`, the six noise names, everything else `TexSource::User(lowercase root)`
  (`randNN[_prefix]` is kept as-is; resolution is the engine's job). `sampler3D`, `noisevol_*`
  or a `tex3D` use make the binding `TexDim::D3`.
* `float4 texsize_<name>;` (and `texsize_noise_*` from include.fx): user ones go to
  `pu.texsize[slot]`, slots in order of first use, `texsize_slots[slot]` = the source of the named
  texture (`randNN` matched by slot number as MD2 does).
* `max_blur_level` = highest `blurN` sampled.

## Semantics decisions

| Topic | Decision |
|---|---|
| `%` on floats | truncated remainder (HLSL/D3D `fmod`, sign of dividend) = WGSL `%`. glslang lowers it to `OpFMod` (floor); the oracle SPIR-V is patched to `OpFRem` in `tests/common/oracle.rs` |
| `frac` | `fract` (= `x - floor(x)`) |
| `pow(x,y)` | `pow(abs(x), y)`, and `pow(x, 1)` with a literal one is `x`: projectM's GLSL generator (hlslparser) does exactly this (D3D9 takes \|x\|); likewise `sqrt/log/log2/log10/rsqrt` take `abs(arg)`. Measured on the oracle with the `Infusion 2` warp shader: `pow(1-rad, 18)` is finite for `rad > 1` |
| `round` | `floor(x+0.5)` (what D3DX9 emits), not WGSL's ties-to-even |
| `atan2(y,x)` | WGSL `atan2(y,x)` (same argument order) |
| `lerp/saturate/clamp/step/smoothstep` | `mix/saturate/clamp/step/smoothstep`, all operands splatted/truncated to one type first (WGSL requires identical types) |
| `rsqrt`, `log10`, `sincos`, `modf` | `inverseSqrt`, `log(x)/log(10)`, two stores, `trunc` + remainder |
| `any/all` on floats | compared `!= 0` first |
| ints (ps_2/ps_3: ints are floats) | `int/uint` are `f32` in WGSL. `float->int` conversion = `trunc`; `int/int` = `trunc(a/b)` (integer semantics); `+ - * %` are float ops on integral values; array indices use `i32()`. `bool` is a real WGSL `bool`; `bool -> float` is `f32(b)` |
| implicit conversions | scalar splat, vector truncation (`float4 -> float3`, `float3 -> float`, `float2 -> float` takes `.x`), int/bool promotion; expanding conversions are errors. `float4x3 -> float3x3` matrix truncation inside `mul` (`mul(float3, float4x3)` is valid, like glslang) |
| matrices | `floatRxC` -> `matCxR<f32>` (ABI layout); `*` between matrices is component-wise (HLSL), `mul()` is the matrix product; constructors are row-major (`transpose(matRxC(...))`) |
| `tex2D/3D` | `textureSample`; `*lod` -> `textureSampleLevel`, `*bias` -> `textureSampleBias`, `*proj` divides by `.w`, `*grad` -> `textureSampleGrad`; `ddx/ddy` -> `dpdx/dpdy` |
| uniformity | WGSL rejects `textureSample`/derivatives in non-uniform control flow, but MD2's ps_2_x evaluates both branch sides and **MD2 user/noise textures are mip-mapped** (D3DX loads with all levels, noise textures auto-generate them, `MIPFILTER` = filter), so `textureSampleLevel(.., 0)` would be wrong. Strategy: keep `textureSample` and emit `diagnostic(off, derivative_uniformity)` (Options). Single-level textures (main `m_lpVS` and blur textures: `CreateTexture(..., levels=1, RENDERTARGET)`) are unaffected. The render backend must therefore give user and noise/noisevol textures (MD2: `AddNoiseTex/AddNoiseVol` request the full chain with AUTOGENMIPMAP, sizes lq 256 / lq_lite 32 / mq 256 / hq 256 / vol 32) full mip chains with trilinear filtering |
| uninitialised vars/globals | zero. Non-`static` globals are per-invocation private copies, initialised in `fs_main` (in dependency order) — initialisers are evaluated once, before `PS` |
| division by literal zero | routed through a `var` (WGSL const-eval would reject it) |

Unsupported (corpus needs none): `struct`, `switch`, bitwise/shift operators (no integer ops on
SM2/3), sampler-typed function parameters, `texCUBE/tex1D`, matrix `_m00` swizzles, writing to
matrix rows.

## MilkDrop 3 prelude

`Options { md3: true, .. }` translates for `Compat::MilkDrop3` (see `md3.md`). With the flag off nothing changes:
`md2_wrapped_hlsl` is untouched and every translation is byte-identical to a build without MD3 support (the shared
`abi::WGSL_COMMON` only grew the `mx` block, which the snapshots strip with the rest of the prelude).

Text assembly follows MD3 3.37: comment-stripped `include.fx`, then the **MD3 prelude**, then the per-pass `#define`s, then
the wrapped user text. `md3_wrapped_hlsl(kind, user)` builds it (`translate` uses it when `md3` is set;
`translate_wrapped` needs the same flag for text that came from `md3_wrapped_hlsl`). The prelude (written from the
documented behaviour, nothing copied from MD3) declares:

| Name | Meaning |
|---|---|
| `float4 _c14;` and the macros `mouse` (= `_c14`), `mouse_x/_y/_z/_w`, `mouse_pos` (= `_c14.xy`) | `(px/W, py/H, held, released)`, `(-1, -1, ..)` outside the window; emitted as `mx.mouse` |
| `float4 _qi .. _qp;` and the macros `q33 .. q64` | `qN` = component `(N-1)%4` of register `(N-1)/4` (`_qa` = q1-4 ... `_qi` = q33-36 ... `_qp` = q61-64); `_qi.._qp` are `mx.q_hi[0..7]`, `_qa.._qh` stay `mu.q[0..7]` |
| `sampler2D sampler_fft;` | the shader FFT, `TexSource::Fft` (below) |
| `get_fft(pos)` | macro: `tex2D(sampler_fft, float2(clamp(pos, 0, 1), 0.5)).r`, **no `sqrt`** (BeatDrop's has one) |
| `get_fft_hz(freq)` | macro: `get_fft(clamp((freq) / 24000.0, 0, 1))` (MD3 assumes a 48 kHz capture; BeatDrop divides by 22050) |

`get_fft*` are macros like in MD3, so the argument expression is pasted textually (once) into the `clamp` and nested
macros (`get_fft(mouse_x)`, `get_fft(bass > 0.5 ? .25 : .75)`) work. Not declared, on purpose: `get_fft_peak*`,
`get_wave*`, `HAS_FFT_PEAK`, `sysTime*`, `mouse_clicked`, `aspect_sd` (BeatDrop only), `_c15..` (MD3 never uploads
them), `_mxd3qx*` / `_mxd3qs*` / `MXD3Q*` (engine-private accumulators of locked presets, not implemented). The
prelude's `sampler_fft` counts as declared for the implicit `sampler_<name>` declarations.

**Decisions**

* One dialect for every PSVERSION (0..5): the translator does not know about shader models, so MD3's retry ladder
  (`ps_2_0` -> `ps_3_0` -> `ps_4_0`) has nothing to do.
* `sampler_wave` stays what it is without MD3: an undeclared-unless-used user texture (`TexSource::User("wave")`,
  bilinear + clamp, the `fw_/fc_/pw_/pc_` prefixes apply). MD3 creates a 480 x 2 `R32F` texture for it and never
  writes it, so the engine's 1 x 1 black dummy for unresolved user textures is the faithful content. No `get_wave`.
* `fft` -> `TexSource::Fft` only with `md3` (otherwise `User("fft")`, as before). Default clamp + bilinear; the prefixes
  select filter and wrap (`sampler_pw_fft`: point, `sampler_fw_fft`: wrap). No anisotropy special case: MD3 never sets
  it. `texsize_fft` also reports `TexSource::Fft` (MD3 leaves it unresolved; the host answers 512 x 1).
* `_c14` and `_qi.._qp` only exist (and map to `mx`) under `md3`; a shader that writes to them (`q40 = 1;`,
  `mouse.x = 0.5;`) gets the same private per-invocation copy as for `_qa` (seeded from the uniform).

**FFT lowering.** The texture is `FFT_BINS` (512) x 1 `R32Float`, which wgpu cannot filter without
`FLOAT32_FILTERABLE`, so no `textureSample` is emitted for a sampler whose root is `fft`. Every `tex2D`, `tex2Dlod`,
`tex2Dbias`, `tex2Dproj` (`x / w`) and `tex2Dgrad` becomes `fft_fetch(tex_<sampler>, u, linear, wrapped)`, with the
filter and address of the sampler's binding baked in as literals. The WGSL helper (`fft_fetch` / `fft_texel`,
appended once after the group(1) bindings whenever the interface has a `TexSource::Fft` binding) reproduces D3D9:

* result `(r, 1, 1, 1)` (D3D9 `R32F`), the `v` coordinate is ignored (MD3's texture had two identical rows);
* linear: `x = u * 512 - 0.5`, `i0 = floor(x)`, `mix(S[i0], S[i0 + 1], x - i0)`, the indices clamped to `0..511` (clamp
  address) or wrapped modulo 512 (wrap address); point: `S[floor(u * 512)]`, clamped or wrapped the same way.

So `get_fft(0.5)` is the mean of `S[255]` and `S[256]`, `get_fft(0) = S[0]`, `get_fft(1) = S[511]`,
`get_fft(-3) = S[0]`. The group(1) bindings keep the usual `texture_2d<f32>` + `sampler` pair; the renderer binds an
`R32Float` view declared as non-filterable and never samples it.

**Legacy comp generator.** `default_comp_shader(.., md3_effects)`: with `true` the `brighten` line is
`ret = sqrt(ret);` and `solarize` is `ret = ret*(1-ret)*4;` (MD2: `1 - (1-ret)*(1-ret)` and `*2`).

**Scoping regression.** `float aspect = aspect.x / aspect.y;` (the macro makes it `float _c0 = _c0.x / _c0.y;`)
is valid HLSL and MD2/MD3 compile it: the initialiser sees the outer `_c0`, the rest of the block the new scalar. The
emitter evaluates an initialiser before it declares the local, so it translates with and without `md3` (also when the
shadow is assigned to, which turns the outer uniform into a seeded private copy). Only whole-register macros (`aspect`,
`mouse`, `texsize`) can be shadowed: component macros (`q40`, `mouse_x`) expand to a swizzle, so `float q40` is a syntax
error in D3DX too.

**Visual check against MD3 3.37 under Wine** (`oracle/md3shot.sh`, 800x600, no audio, versus `fdrender --compat md3` with
`fixtures/audio/silence.wav`; the oracle can only run on silence and its time and random state are not reproducible, so
only static scenes compare pixel for pixel):

* Generated composite shader: hand-written presets without `warp_N` / `comp_N` text (`PSVERSION=2`, `fDecay=1`, gamma 1,
  no echo, a solid outer border `ob = (0.25, 0.5, 0.75)`, `ob_size=0.5`, one of `bBrighten` / `bSolarize` / `bDarken` /
  `bInvert`) give the **same 8-bit pixel** in MD3 and in `--compat md3`: plain `(63, 127, 191)`, brighten `(127, 180, 221)`
  (`sqrt`), solarize `(190, 255, 192)` (`* 4`), darken `(16, 63, 143)`, invert `(192, 128, 64)`. `--compat md2` gives
  `(110, 191, 239)` for brighten and `(95, 127, 96)` for solarize, so the check does distinguish the two formulas.
* `MilkDrop3 - Equalizer2 red.milk` (`get_fft` returns 0 on silence): the cyan glow along the bottom edge is identical in
  both, down to the gradient. `Equalizer1`, `Equalizer2 purple`, `Equalizer4` and `Equalizer4 MidSoft` are black in both
  (all bars at height 0); `Equalizer1b` shows the same sparse violet particles (positions differ, random).
* With audio (`synth128.wav`; also `sweep.wav` for `Equalizer1`) `Equalizer1`, `Equalizer2 red` / `purple`, `Equalizer4` and
  `Equalizer4 MidSoft` draw bar spectra (LED blocks, solid bars, centred bars) with a jagged, falling profile. MD3 cannot be fed
  audio in this setup, so the bar heights themselves are only checked against the D3D9 model by the GPU tests, not
  against MD3.
* `lara - Flexi Romanesco Uncompressed [MOUSE].milk` (PSVERSION 4/3, `mouse.x/.y/.z`, `get_fft` in the composite): the same
  faceted fractal with the same cyan-to-deep-blue palette and framing, morphing differently with time (and `mouse`, which is
  `(-1, -1, 0, 0)` in `fdrender`, an unknown real cursor in the oracle); the exact shape was not compared.
* A locked preset (`MD31` / `MD32`, e.g. `A story of lasers - Over Water.milk`) is black under Lactea (the placeholder shader
  runs) while MD3 draws its licensed laser scene: expected, see `md3.md`.

## Verification

All commands: `CARGO_TARGET_DIR=target-hlsl cargo test -p lactea-hlsl [--test X] -- --nocapture`.

* `corpus` — all 1423 warp/comp shaders of `/usr/share/projectM/presets` (701 warp + 722 comp): translate + naga validate; glslang baseline of "is this HLSL
  valid". `target-*/tmp/corpus-failures.txt` lists failures.
* `diff` — semantic differential (fidelity ladder rung 2): our WGSL vs glslang SPIR-V of the same
  MD2-wrapped HLSL (`harness::glslang_source`, behind the internal `testing` feature: include.fx declarations replaced by byte-compatible
  `mu_f[]/pu_f[]` uniform blocks, user globals made `static`, `double -> float`, `modf` rewritten;
  `glslangValidator -D -V -S frag --hlsl-dx9-compatible -e PS`, `spirv-opt --split-combined-image-sampler`,
  naga spv-in, `OpFMod -> OpFRem` patch, ABI re-binding). Both rendered by wgpu on the same GPU at 64x64 from fixed inputs
  (RGBA8 pattern main, derived blur patterns, hash noise, 16^3 volume, checker user texture, fixed
  MilkUniforms incl. q vars and D3DX-style rotation matrices, 32x32 mesh with per-vertex uv/uv_orig/rad/ang/diffuse).
  `semantic_diff_sample` runs every 20th shader; `semantic_diff_full -- --ignored` all of them.
  `HLSL_SABOTAGE="from=>to"` mutates our WGSL to prove the harness is sensitive
  (`fract(`->`floor(` fails 252 shaders, `0.5f`->`0.51f` fails 715).
* `micro` — 127 hand-written snippets (operators, swizzle write masks, ints, matrices, loops,
  texture lookups, ...) vs glslang: all agree except the documented `matrix*matrix` deviation.
* `unit` — wrapper, default shaders, sampler rules, interface, preprocessor, error paths, and a
  mutation/truncation robustness test (translator never panics on malformed input).
* `snapshots` — 11 insta snapshots (`tests/snapshots/`), one of them (`md3_fft_mouse_and_q`) with `md3`.
* `md3` — the prelude text and ordering, `md2_wrapped_hlsl` untouched, `get_fft` / `mouse` / `q33..q64` lowering, sampler
  prefixes, every `tex2D*` flavour on `sampler_fft`, the legacy comp texts, the shadowing regression, every corpus shader
  translating byte-identically with and without `md3`, and three GPU tests (skipped without an adapter) that render
  `get_fft` / `sampler_{pw,fw,pc}_fft` / `mouse` / `q_hi` through `tests/common/gpu.rs` (a jagged `R32Float` 512 x 1
  texture and a fixed `MilkExtUniforms`) and compare every column with the D3D9 model above.

## Known divergences / limits

* glslang itself deviates from D3D HLSL in a few places (we follow D3D): `%` is `OpFMod`
  (patched in `tests/common/oracle.rs`), `matrix * matrix` is a matrix product (component-wise in HLSL),
  `normalize(scalar)` and a few expressions (`a < sin(..)` after a parenthesised dot()) are rejected
  by its parser, and a generic `sampler` declared 2D but used with `tex3D` cannot be bound
  (3 shaders; reported as "baseline reject", not counted against us).
* D3D9 `0 * inf = 0` (and `0/0` style rules) are not emulated: `LuxXx - Done For the Night ii` (comp)
  samples with a NaN coordinate and renders differently from glslang (the only >2/255 shader in the
  differential).
* MD2 compiles with `D3DXSHADER_USE_LEGACY_D3DX9_31_DLL` (legacy compiler); constant folding of
  int literals (`7/2`) follows the integer rule above.
* `sampler_pw_noise_lq` etc. need no declaration: `translate` declares the `sampler_*` / `texsize_*` names a shader uses
  without declaring them (projectM does; MD2 rejects such a shader as "undefined identifier" and falls back).
* ABI note: `TexSource::Fft` is only produced with `Options::md3`; otherwise `fft` / `wave` are reported as
  `User("fft")` / `User("wave")` (BeatDrop extensions). `include.fx` here has no `_c14.._c16` (mouse / BeatDrop system
  time and date); with `md3` the prelude adds `_c14` (mouse, `MilkExtUniforms`), `_c15..` stay absent (MD3 never uploads them).
* Undefined variables (`Options::projectm_flavor`, `emit/undef.rs`): a variable read before it is written is undefined in
  projectM's GLSL; Mesa's `nir_opt_undef` turns an undef whose every use is a float ALU operand into NaN (so
  `max(x, f(undef))` is `x`). `PS` locals / private globals with that property start as NaN, every other undef read keeps
  the WGSL default 0 (the oracle reads a register there; unreproducible). `martin - organic light`: comp final 33 -> 99 dB.
* Mesa's backward inter-shader code motion (`nir_opt_varyings`, `Options::gl_varying_fold`, `emit/fold.rs`, projectM
  flavour only): the GL driver moves arithmetic on interpolated inputs into the vertex stage and interpolates the result
  (`uv.y + (h - 1)` becomes `interp(uv.y + (h - 1))`), ANV interpolates the input and adds in the fragment stage, about
  1 ulp apart (`Flexi + Martin - dive`: horizon row of `3 / c.y`; `Geiss - Cauldron - Tendrils`: drift through the
  feedback loop). The translator mirrors Mesa's decision on the AST of `PS`: a scalar SSA use graph (value numbering as
  NIR's CSE does, dead code removed), and for every input load the farthest *movable post-dominator* (all use paths of the
  load end in it). Movable: constants / uniforms and any ALU op over them, `+` / `-` of interpolated values and
  convergent ones, `/` by a non-literal uniform, plain products (`*=`, `mul()`) with a convergent factor, `lerp` with a
  convergent weight. Not movable (measured on the oracle with `max(x, -10)` twins of 80 probe shaders): HLSL `a * b`
  (`mult()`, a `select`), a lone `-x` (`fneg`), `x / literal`, anything non-linear; a load with such a use, or with uses
  that meet only in an unmovable instruction, starts no motion. A warp shader's `uv.xy` also feeds the motion-vector
  output, so only `uv_orig` and `rad` / `ang` start motions there (and bring `uv` along as a co-input). Each moved
  value becomes a `fold_k` vertex function in `Translated::folds` and the fragment stage reads it as an extra varying
  (`in.fold{k/4}`); `lactea-render` runs the functions in the vertex stage (`CompiledShader::folds`). Not modelled (left
  as is): user functions, `?:`, vector `mul()`, `x / literal` fused into a multiply-add with a convergent addend.
