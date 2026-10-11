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
* `snapshots` — 10 insta snapshots (`tests/snapshots/`).

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
* `sampler_pw_noise_lq` etc. must be declared by the user (`sampler sampler_pw_noise_lq;`); MD2
  rejects an undeclared one ("undefined identifier") and falls back — `Nebula Forge` (comp) is such a shader.
* ABI note: `TexSource` has no `Fft`/`Wave` (BeatDrop extensions); they are reported as
  `User("fft")`/`User("wave")`. `include.fx` here has no `_c14.._c16` (mouse/system time); ABI has no
  place for them and no corpus shader uses them. No ABI change is required.
* Undefined variables (`Options::projectm_flavor`, `emit/undef.rs`): a variable read before it is written is undefined in
  projectM's GLSL; Mesa's `nir_opt_undef` turns an undef whose every use is a float ALU operand into NaN (so
  `max(x, f(undef))` is `x`). `PS` locals / private globals with that property start as NaN, every other undef read keeps
  the WGSL default 0 (the oracle reads a register there; unreproducible). `martin - organic light`: comp final 33 -> 99 dB.
* Mesa's backward inter-shader code motion (`nir_opt_varyings`) is not emulated: GL hoists single-input linear chains of
  the fragment shader (`uv.y + (h - 1)`) into the vertex shader and interpolates the result, ANV interpolates the input
  and adds in the fragment shader (`Flexi + Martin - dive`, horizon row of `3 / c.y`; see `docs/fidelity.md`).
