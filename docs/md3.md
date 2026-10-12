# MilkDrop 3 (`Compat::MilkDrop3`)

`Compat::MilkDrop3` makes lactea follow MilkDrop 3 (MD3, version 3.37) where it differs from MilkDrop 2 /
BeatDrop. It is **MilkDrop 2 plus the MD3 deltas**: `Compat::MilkDrop3.rules()` starts from the
`Compat::MilkDrop2` rules and overrides one `CompatRules` field per MD3 delta, so every difference between
the two is a row in this file and a field in `crates/lactea-render/src/config.rs`. `Compat::md2_family()` is
true for both variants and is used only where a whole MD2 code path is chosen (the preset importer `Flavor`,
HLSL translation, the soft-cut machinery); everything else reads a rule field. The default stays
`Compat::ProjectM`; `Compat::MilkDrop2` is unchanged by anything described here.

Select it with `--compat md3` (`fdrender`), `--compat milkdrop3` (`lactea` app), `Compat::MilkDrop3`
(library) or `LACTEA_COMPAT_MILKDROP3` (C API, value 2).

## Provenance

MD3 is closed source: a fork of BeatDrop (the `vis_milk2` copy in `third_party/md2`) whose source was never
published. Its behaviour was recovered by binary analysis, only to read and render MD3 preset files
(interoperability), and written down as behavioural specs. The work is clean-room:

- the specs (what a file contains, what the program does with it, test vectors) are kept outside this
  repository;
- the implementation is written from those behavioural specs on top of the BeatDrop code that is already
  here;
- no MD3 code, binaries, presets or data are in the repository; tests use small hand-written fixtures.

MD3's paid, locked shaders (the `MD31` / `MD32` keys) are not supported: lactea cannot and does not
decrypt them. A preset that carries one still loads and runs the placeholder shader that stands in for it
in the file, exactly like a build without the license.

## Resources

| Resource | Status |
|---|---|
| `.milk` with MD2 keys and code (what MD2 / BeatDrop read) | Supported: the MD2 importer, expressions and renderer (`Flavor::Md2`); code lines are glued without a separator (`md2_code_join`) |
| MD3 expression engine (ns-eel2 numerics, name / literal / operator rules, RAM model) and compile-failure policy (a failing part is dropped, the preset loads) | Supported: `md3_ns_eel2`, `md3_drop_failed_code`, "Expression engine"; `mouse.x` and exponent literals do not compile in MD3 and do not here |
| `q1..q64`, `t1..t16`, `monitor2` / `monitor3`, `FFTAttack` / `FFTDecay`, shader `time` per state | Supported: "Shader inputs" |
| Shader prelude (`get_fft`, `get_fft_hz`, `sampler_fft`, `mouse*`, `q33..q64`), mouse as shader input | Supported: `lactea-hlsl` / `lactea-render`; `Engine::set_mouse`, `fdrender --mouse`, app and C API |
| Audio analysis of MD3 (left channel, 576-sample Hann FFT, `VolumeSensitivity`) | Supported (`AnalysisMode::Md3`); not exposed in `LacteaConfig` / app options |
| Main waveform modes 0..16, shape sides up to 500, warp-term skip, border gate of blends | Supported: "Main waveform and render deltas" |
| `.milk2` double presets (two presets plus a frozen soft cut), `BlendMilk2`, sprite of PRESET2 | Supported: container, playback, progress modes; geometry of `side` / `cercle` checked against MD3 under Wine ("Double presets"); no `DoublePresetMode` (F9) |
| Blend patterns of MD3 soft cuts (30 patterns) | Supported: `md3_blend.rs`, enabled-pattern list (`EngineConfig::md3_transition_patterns`; no app flags) |
| Sprites (`[SPRITE1_BEGIN]` blocks, layers 0-4, 11 blend modes, burn, GIF sprites and GIF shader textures) | Supported, see [Sprites](#sprites); no `sprites.ini`, user sprite or webcam layers |
| Auto hard cuts (`AutoHardCutMode` 1..7) and the MD3 display time | Supported: "Auto hard cuts"; F8 / `--auto-hard-cut` / C API |
| `.wave` / `.shape` files (single custom wave / shape import and export), MD3 `.milk` writer incl. sprite block | Supported (library / engine API; no app UI) |
| Locked shaders (`MD31` / `MD32`) | Not supported: the placeholder shader in the file runs |

MD3 pack (`~/M0Rf30/md3-re`, local smoke runs only; `fdrender --compat md3 --texture-dir <pack>/textures`, 30 frames):
all 331 `.milk` and all 146 `.milk2` render with exit code 0, no shader fallback and no warning (before the MD3 work:
319 and 39 clean); 97 presets load their sprite, including the 24 `.milk2` with `sprite=1`.

Each feature section below names the `CompatRules` fields it introduced.

## How the features combine

- **One dialect, every VM.** The engine owns a `GlobalMemory` whose dialect follows `md3_ns_eel2`; every context of a
  load is built from it (preset, waves, shapes, sprite, both halves of a `.milk2`), so ns-eel2 and the drop-failed-code
  rule apply everywhere. `megabuf` / `regNN` state is shared as MD3 shares it.
- **A `.milk2` is two states.** Each half is built by the normal load path (own VMs, q / t, start time for the shader
  `time`, shader FFT parameters); PRESET2 is the `m_pState`, PRESET1 the old state of a blend frozen at
  `blending_progress`. Sprite, borders, `FFTAttack` / `FFTDecay` and the soft-in timing follow the rules in the
  respective sections.
- **Auto hard cuts and double presets.** A cut is a hard load, which ends the double preset at once; mode 7's effect text
  is injected into the loaded (PRESET2) composite shader.
- **Hosts.** `Engine::set_mouse`, `take_auto_cuts`, `double_preset_info`, `import_custom_wave` / `_shape` are the engine
  calls a host wires; `docs/app.md`, `docs/engine.md` and `docs/parity.md` list the host side.

## Expression engine (ns-eel2)

`CompatRules::md3_ns_eel2` (MilkDrop 3 only) runs every preset expression on `lactea_eel::Dialect::NsEel2`;
`CompatRules::md3_drop_failed_code` (MilkDrop 3 only) is MD3's compile-failure policy. Under ProjectM and
MilkDrop 2 nothing changes: the default dialect is `Dialect::ProjectMEval`, the unchanged projectm-eval port
that `lactea-eel-oracle` compares against.

### Selection

The dialect rides in the `GlobalMemory` the contexts of one engine share (`GlobalMemory::with_dialect`;
`GlobalMemory::sharing_rng_with` inherits it). `Engine::new` creates its `rng_owner` with
`runtime::eel_dialect(&rules)`, every preset's memory derives from it, and every `Context` built on that memory
(per-frame, per-vertex, waves, shapes, and any context a later slice builds from the same memory) follows. A new
kind of context must be created from the engine's memory, not from `GlobalMemory::new()`.

### What NsEel2 implements (MD3 3.37, spec E Annex A)

| Area | Behaviour |
|---|---|
| `/` | no epsilon guard: `x/0` is ±Inf / NaN, `1/0.000001 = 1e6` |
| `=` | stores `+0.0` when the value's exponent field is 0 (±0, denormal) or all ones (±Inf, NaN); the value of the assignment is the stored value. Compound operators (`+= -= *= /= %= ^= \|= &=`) store the raw result |
| `%`, `%=` | `\|a\|` and `\|b\|` rounded to 32 bit, unsigned remainder, zero divisor gives 0 |
| `&`, `\|`, `&=`, `\|=` | 64-bit integers after rounding |
| `pow`, `^`, `log`, `log10`, `asin`, `acos` | raw libm results (NaN / ±Inf, no zero or domain guards) |
| `sqrt`, `int` | `sqrt(\|x\|)`; `int` is `floor` |
| `sign` | ±0 returns its input (`-0` survives until stored); anything else, NaN included, is ±1 by the sign bit |
| `invsqrt` | float32 magic-number step, Newton step in double (`invsqrt(4) = 0.49915…`) |
| truthiness (`if`, `&&`, `\|\|`, `!`, `while`) | false iff `\|x\| < 1e-5` (NaN is false); `band` / `bor` use the strict `> 1e-5`; `above` / `below` / `<=` / `>=` have no epsilon; `==` is `\|a-b\| < 1e-5`, `!=` its complement |
| `loop`, `while` | count rounded; at most 1,048,576 iterations |
| `megabuf(i)` | index `round(i + 1e-5)`; valid `0 <= w < 8,388,608`, otherwise reads 0 and writes are dropped; per context |
| `gmegabuf(i)` | one 1,048,576-entry array, index masked (wraps, never fails), shared by every context **and every preset**, never cleared |
| `reg00..reg99` | shared by every context and every preset, never cleared |
| `memcpy`, `memset` | truncating indices (no `+ 0.0001`) |
| function set | the 53 table entries (`_if … memset`) and the nine aliases; `_add _sub _mul _div _neg` (callable in projectm-eval) are not callable; `noteq` / `beleq` / `aboeq` are not aliases |
| `$PI`, `$E`, `$PHI`, `$Xhex`, `$'c'` | `3.141592653589793`, `2.71828183`, `1.61803399`, 32-bit saturating hex, ASCII code; bare `pi`, `e`, `phi` are ordinary variables (0) |
| names | case-insensitive, 128 significant bytes; a bare `gmem` is a variable (special only before `[`) |
| `rand(n)` | MT19937 word `* (1/0xFFFFFFFF) * max(1, floor(n))`, a real in `[0, n]` |

### Measured corrections of the spec

These spec statements were inferred or derived, not measured. They were checked on the real program (MD3 3.37 under
Wine, `md3shot.sh`) with probe presets that write expression results into the border colours (`fixtures/md3/eel/`;
the measured bytes are in `crates/lactea/tests/md3_eel_oracle.rs`). Lactea reproduces the border pixels of all 16
probes exactly (13 are kept as fixtures).

| Spec statement | Measured on MD3 | Implemented |
|---|---|---|
| x87 conversions round to nearest even, or truncate (OQ1) | nearest even: `3.7\|0 = 4`, `10%3.5 = 2`, `loop(3.5)` runs 4 times, `megabuf(1.6)` is cell 2, `megabuf(-1)` is invalid, `gmegabuf(-1)` is cell 1048575 | nearest even |
| identifiers may contain `.`, so `mouse.x` parses (OQ8) | **`mouse.x` and `a.b` do not compile** (the whole code part is dropped); `my_var` does | `.` ends an identifier in both dialects. A host can still register the name `mouse.x` (looked up verbatim), but preset code cannot read it |
| number literals are C `atof` | **no exponent notation**: `1e-5`, `1e300` do not compile (`.5`, `5.` do) | no exponent suffix under NsEel2 |
| `-7%3 = 1` (vector A §7.1) | `-7%3 = -1`: the sign applies after `%`; `a=-7; a%3 = 1`, `7%-3 = 1` | `%` and `^` are one left-associative level above `*`; a unary `-` covers a whole `%` / `^` chain (`-2^2 = -4`, `-2^2^2 = -16`, `2^3^2 = 64`, `7%2^2 = 1`, `2^7%4 = 0`) but binds tighter than `&` / `\|` (`-5&3 = 3`); `+ - * /`, `&`, `\|`, `!` as in projectm-eval |
| (not in the spec's grammar notes) `c ? x` without `:` | accepted, `c ? x : 0` | accepted under NsEel2 |

Confirmed as in the spec: assignment filtering and raw compound operators (`a=1; a/=0; b=a` gives `a = Inf`,
`b = 0`), `%` by zero, `int(-2.5) = -3`, `invsqrt(4) != 0.5`, bare `pi` / `e` are 0, the 1,048,576 caps of `loop`
and `while`, `$PI` `$E` `$PHI` `$X` `$'c'`, case-insensitive names, truthiness thresholds (`if(0.000001,…)`,
`0.00001&&1`, `band` / `bor`), `above(1,0.999999)`, `equal(1,1.000001)`.

Consequently lactea registers no `mouse.*` variables in any VM: whether MD3 keeps such names in its tables is
unobservable, no pack preset uses them in EEL, and `mouse.x` appears only inside HLSL, where `mouse` is a `float4`
(shader inputs). The dialect is a property of the engine's `GlobalMemory`, so every VM of a load follows it: the
per-frame / per-vertex / wave / shape contexts, both halves of a `.milk2` and the sprite VM.

### Not modelled

- `sin` / `cos` / `tan` use libm, MD3 uses x87 `fsin` / `fcos` / `fptan` (last-bit differences; `fsin` of huge arguments).
- The 0x7ec temp-slot limit per `;` statement, 750 variables per block and the SEH guard have no equivalent (no JIT);
  nesting is limited by `Context::set_max_depth` as before.
- NaN in comparisons follows the formulas of the spec (`==` true for NaN, `!=` false, `<=` / `>=` false); the x87
  status-word details were not measured.
- ns-eel2 builds `$E` / `$PI` by text substitution, so `$E5` would be the digits `2.718281835`; here it is `$E`
  followed by `5` (a syntax error).
- Operator levels were measured for `%`, `^`, unary `-`, `&`, `|`, `!`, `+ - * /` only; assignment to a non-variable
  is not rejected.

### `rand` and the run seed

`rand` is MT19937 as in the spec, one stream shared by every context and preset of the engine. The spec's stream is
seeded with `0x4141f00d` once per process; Lactea seeds it from the run seed (`EngineConfig::seed`), and the default
seed 1 maps to `0x4141f00d`, so a default engine reproduces the spec's stream (`rand(1)` gives `0.2277…`, `0.3407…`,
`0.8936…`, `0.9495…`); other seeds give other reproducible streams. MD3's own stream is not controllable.

### Compile failures (`md3_drop_failed_code`)

MD3 never rejects a preset for a code error: the whole failing part (init, per-frame, per-vertex, one wave's init /
per-frame / per-point, one shape's init / per-frame) is dropped and a warning is queued. A failed init part leaves
`q`, `t` and `monitor` at 0. Lactea pushes `"<part> dropped: <error>"` to `PresetReport::warnings` and goes on; the
other parts run (`runtime::compile_part`, the single place). Lactea rejects the preset (`EngineError::Compile`)
under ProjectM and under MilkDrop 2, although BeatDrop (`state.cpp`, `RecompileExpressions`) also only reports the
error and goes on; MilkDrop 2 was left as it was.

### Verification

- `crates/lactea-eel/tests/ns_eel2.rs`: the Annex A §7 tables (over 100 rows) and the measured corrections; the
  existing `semantics.rs` tests and `lactea-eel-oracle` run on the default dialect, unchanged.
- `crates/lactea/tests/md3_eel_oracle.rs`: 13 probe presets rendered through the engine under `Compat::MilkDrop3`,
  compared with the bytes MD3 3.37 produced under Wine.
- `crates/lactea/tests/md3_eel.rs` and the `runtime.rs` tests: dialect selection, dropped parts, ns-eel2-only syntax.
- Pack smoke, `fdrender --compat md3 --frames 30` over the 477 files of the MD3 pack: exit codes before (foundation)
  364 × 0, 110 × 3, 3 × 2; after 365 × 0, 112 × 3. No file got worse: `.milk2` files that failed with 2 now load
  (one runs clean, two stop at the next failure, which is a shader translation fallback). The 9 `.milk` files with
  exit 3 are shader fallbacks (`get_fft`, `uv2`), unrelated to this slice. `.milk2` playback belongs to another slice.
- Default / MilkDrop 2 unchanged: `fdrender --stages final` frame 30 PNG hashes of five corpus presets in `projectm`
  and `md2` are identical before and after.
- Visual: `Martin - 2077 candies` (gmegabuf particle system, `%`-heavy) and `Flexi + Amandio c - piercing 01` were
  rendered by `fdrender --compat md3` (silence, frame 700) and shot from MD3 under Wine (silence, 12 s) and looked at
  side by side. Both show the same scene: coloured particle dots over dark for `candies`, orange blobs on dark
  violet cells for `piercing`. Particle placement and the petal pattern differ because `time`, `rand` and the audio
  level cannot be controlled (MD3 reports `bass = 1.0` in silence, which belongs to the audio slice), and `candies`
  has a hazy blurred backdrop in Lactea that MD3 does not draw (a shader / blur difference outside this slice,
  not investigated). No EEL difference can be read off those two frames; the exact evidence is the probe table.

## Shader inputs: audio, `get_fft`, mouse, `q1..q64`

Rules: `md3_audio_analysis`, `md3_shader_fft`, `md3_extended_vars`, `md3_shader_prelude`, `md3_shader_time`,
`md3_md2_comp_effects` (all false for `ProjectM` / `MilkDrop2`, true for `MilkDrop3`).

**Audio** (`AnalysisMode::Md3`, `docs/audio.md`): PCM is quantised to signed 8 bit (`trunc(128 s)`, clamp -128..127),
multiplied by `VolumeSensitivity` (`EngineConfig::volume_sensitivity`, `Engine::set_volume_sensitivity`, `fdrender
--volume-sensitivity`, `lactea_set_volume_sensitivity`), aligned like MD2, then a 576-sample Hann FFT zero-padded to
1024 gives the equalised spectra of both channels. `bass/mid/treb` use the left channel only (bins `[0,85)`,
`[85,170)`, `[170,256)` summed); silence gives exactly 1.0. The MD3 capture path (WASAPI ring, staleness) is the host's
job.

**Shader FFT** (`sampler_fft`, `get_fft`, `get_fft_hz`): once per rendered frame the engine feeds the left spectrum
through `lactea_audio::ShaderFft` (max-normalise, 0.2 gate, square, 3-bin in-place smear, attack `0.5a+0.5` / decay
`0.1d+0.9`; numbers verified against the spec's `md3_fft.py` vectors) and uploads the 512 floats to a 512x1 `R32Float`
texture (`TexSource::Fft`; MD3 has two identical rows, one is enough because `v` is ignored). The translator lowers
every lookup of that sampler to a manual clamp + bilinear (or point, wrap, per the `fw_/fc_/pw_/pc_` prefix) fetch with
`textureLoad`, since `R32Float` is not filterable: `get_fft(0.5)` is the mean of bins 255 and 256. The state survives
preset changes; `FFTAttack` / `FFTDecay` are global, replaced by each preset load (file value, else 0.5 / 0.7; a
non-finite file value falls back to the default, not copied from MD3), never blended. Open (spec C open question 3/E
Annex C 1.6): whether an ordinary preset *without* the keys keeps the previous preset's values; the specs disagree (C 3.4:
reset to 0.5 / 0.7, E: sticky). Lactea resets. For `.milk2` the spec is clear (A section 3.1.3): every half's import
resets the globals to 0.5 / 0.7 and then reads its own keys, PRESET1 first, so after the double preset is loaded the
values are PRESET2's keys, or 0.5 / 0.7 if PRESET2 has none; the engine's per-import rule gives exactly that, also for
the soft transition (PRESET1 at the request, PRESET2 when the first leg completes).

**Prelude** (`lactea-hlsl`, `Options::md3`): `_c14` and `mouse`, `mouse_x/_y/_z/_w`, `mouse_pos`; `_qi.._qp` and
`q33..q64`; `sampler_fft`; macros `get_fft(pos)` = `tex2D(sampler_fft, (clamp(pos,0,1), 0.5)).r` (no `sqrt`) and
`get_fft_hz(f)` = `get_fft(clamp(f/24000,0,1))`. No `get_fft_peak*`, `get_wave*`, `_c15` / `_c16`. `sampler_wave` stays an
ordinary user texture named `wave`; the engine binds the 1x1 black placeholder (MD3 binds a texture it never fills).
`texsize_fft` answers (512, 1) (MD3 leaves it unresolved). All PSVERSIONs are one dialect. Without the rule the names
do not exist (a preset using them falls back, as before).

**Uniforms** (`lactea-abi`): the frozen `MilkUniforms` is untouched; `MilkExtUniforms { mouse, q_hi }` is a second block at
`@group(0) @binding(1)` (`mx` in `WGSL_COMMON`), always bound by the renderer and neutral outside MD3
(`mouse = (-1, -1, 0, 0)`). Shader `time` (`_c2.x`) is the time since the state's preset was loaded, modulo 10000; the
outgoing state of a soft cut uses its own start time and its own `q33..q64` / mouse. Both halves of a `.milk2` are states:
a hard load builds them together (the same start), a soft transition starts PRESET1 with the request and PRESET2 when the
first leg completes; each half's shaders see their own `q33..q64` and the shared mouse / FFT.

**Expressions**: `q1..q64` and `t1..t16` in the per-frame, per-vertex, custom wave (per-frame and per-point) and shape
VMs, with the data flow of the spec (all 64 q restart from the post-init snapshot every frame, pf -> pv copy of 64,
waves / shapes get the current preset q, init snapshot of all 64). `monitor2` / `monitor3` are plain user variables and
persist exactly like `monitor` (lactea's contexts never reset them).

**The mouse is a shader input only.** The spec lists EEL variables `mouse.x/.y/.z/.w`, but MD3 3.37 does not compile a
`.` inside an identifier (measured on the real program, "Expression engine"), so preset code can never read them and
lactea registers no such variables. The shaders see it as `_c14` / `mouse` / `mouse_x` .. (`x = px/W`, `y = py/H`
top origin, `-1, -1` outside the window, `z` = left button, `w` = one frame after release).

**Host input**: `Engine::set_mouse(MouseState { position: Option<[f32;2]>, left_down })` (cursor as fractions of the
client area, top-left origin, `None` outside); the engine samples it once per rendered frame and derives the release
edge (a press and release between two frames is invisible, as in MD3). The app forwards `CursorMoved/Entered/Left` and
the left button (window events only: MD3 reads the system-wide button state), `lactea_set_mouse` in the C API,
`fdrender --mouse X,Y[,down]`.

**Legacy comp generator**: presets with a composite version but no `comp_N` text get MD2's original brighten
(`sqrt(ret)`) and solarize (`ret*(1-ret)*4`).

Not implemented (by design): `_mxd3q*` accumulators (locked shaders only), webcam, hot-key effect injection, `get_wave*`.

### Verification

- Unit/GPU tests: audio vectors (`crates/lactea-audio/tests/md3.rs`), prelude and lowering (`lactea-hlsl/tests/md3.rs`),
  extension block, FFT texture and shader time (`lactea-render/tests/shader_ext.rs`), engine data flow
  (`lactea/tests/md3_inputs.rs`: a hand-made preset writes `q40` and the comp shader shows it; a tone lights bin 256 of
  `get_fft`; mouse reaches the shader and its release edge; shader time; the old state's q in a soft cut; other compat
  modes see none of it).
- MD3 pack, `fdrender --compat md3`, 331 `.milk`, 4 frames each: shader fallbacks 9 -> 0 (the 8 `Equalizer*` presets using
  `get_fft`, `[MOUSE]` using `mouse`); every file loads. projectM / MD2 frames of five pack presets are byte-identical
  to the pre-change build.
- Visual (frames opened and looked at): `MilkDrop3 - Equalizer1` with a synthetic tone-rich WAV shows plausible coloured
  spectrum bars rising from the bottom edge, low bins taller; with silence it is entirely black, which is also what the
  Wine MD3 reference (silent audio) shows (uniform black in all three shots). The `[MOUSE]` preset renders the same
  blue fractal as the Wine reference when no mouse is given (cursor outside the window); `--mouse 0.2,0.5` turns it
  purple and `--mouse 0.8,0.5,down` green, i.e. the cursor reaches the shader. The reference cursor position is not
  controllable and the fractal is time dependent, so only look, colour and structure were compared, not pixels. The
  magnitude of the bars against a real MD3 with real audio has not been compared (the oracle has no audio).

## Main waveform and render deltas

Rules: `md3_wave_modes`, `max_shape_sides` (100 / 100 / **500**), `md3_warp_skip_tiny`, `md3_border_gate`
(`CompatRules`, true only for MD3 except `max_shape_sides`), plus `EngineConfig::md3_disable_box` (default
`true`, MD3's `DisableBox`).

**Main waveform** (`lactea_render::wavegen_md3`). The mode is `(int)wave_mode % 17` of the per-frame variable
`wave_mode` (`md3_wave_mode`: C truncation, `INT_MIN` for NaN / overflow, C remainder, so `17 -> 0`, `20 -> 3`,
`16.9 -> 16`, `-17 -> 0`, `-1` and `-18` stay negative). The MD2 port (`wavegen::main_wave_geometry`, modes 0..8, no
mode 9..17) is untouched; MD3 has its own geometry function because even the modes that exist in both read other
channels:

| mode | MD3 |
|---|---|
| 0 | circle, right channel only, +1 closing vertex unless blending; the first 24 vertices cross-fade into the next 240 samples |
| 1, 2, 3, 5 | as MD2 (alpha tables by texture width: modes 2 / 5 x 0.07 / 0.09 / 0.11 / 0.13 for 256 / 512 / 1024 / 2048, mode 3 `0.075 / 0.15 / 0.22 / 0.33 x 1.3 x treble^2`; other widths get no table gain) |
| 4 | script with momentum, `y` from the left channel only |
| 6, 7 | angled line(s), left channel (7: left over right, `sep = (y/2+0.5)^2`) |
| 8 | spectrum line, `0.1 ln(L[2i] + L[2i+1])` (left bins only, no 0.5) |
| 9 | "large" line, amplitude 1.0 (4x mode 6); the second strip (`nBreak = N`, `2N` vertices) stays at the origin, so dots / thick draw 240 points at the screen centre |
| 10 | X: lines at fixed angles -0.75 / +0.75, `wave_mystery` ignored |
| 11 | two vertical lines at `x -0.45 / +0.45` after clipping |
| 12 | skewed spiral, alpha x 1.25, the final alpha enters the angle |
| 13 | star: right channel, tail `R[360 + i]` runs past the 576 samples into the left spectrum (`i >= 216`) |
| 14 | flower: `rad` and `ang` really halved (BeatDrop's `==` is a no-op), the cross-fade reuses the same sample, `cos(3.1416)` mirrors the position |
| 15 | lasso: left only, no zero guard on `ang` (`tan(t/ang)` spikes), no `wave_x`, no aspect in `x`, alpha x 1.25 |
| 16 | triangle, right channel, spin `-0.2 t` (BeatDrop: `+0.2 t`), no clamps |

BeatDrop's mode 17 (fireworks) does not exist: it is mode 0. A mode outside `0..=16` (a negative remainder, or an
old preset's value in the second pass of a blend: the old mode is *not* wrapped) draws 480 zero vertices with the plain
`wave_a` alpha (no volume scaling, no clamp): invisible lines, centre dots with `wave_usedots`. The tail is the MD2 one
(blend of the old and new mode when the mode changed, cosine mix, colour, `y = -y`, `SmoothWave`, four thick / dot passes
from a texture width of 512). `wave_mystery` wraps into `[-1, 1)` for modes 0, 1 and 4 only. Modes 8 and 13 read
`MainWave::spectrum`, i.e. whatever the audio analysis mode provides; the MD3 spectrum scale (`eq[k] * |FFT|`) belongs
to the audio slice, the geometry here is exact for any input.

**Other deltas.**
- Shape `sides`: `clamp(int(sides), 3, 500)` (`sides + 2` fan vertices; MD2 / projectM 3..100).
- Warp UV: the four warp `sin` / `cos` terms are added only for `|warp| > 0.0001` (`WarpAnim::skip_tiny_warp`,
  CPU mesh path; projectM's GPU vertex stage is not used by MD2 / MD3).
- Borders (`DrawSprites` gate): while a preset blend runs and `DisableBox` is on, `ob_*` / `ib_*` are not drawn
  (`FrameInput::borders.hidden`, set by the engine's blend frame). While a `.milk2` is loaded they are never drawn,
  also with `DisableBox` off: the double-preset blend state sets the same field.
- Not applicable here (documented, no code): texture size snapping to a multiple of 16 and the absence of anisotropic
  filtering (the host picks the render size; the renderer's samplers are already linear / point only), `_c1` zeros
  (the MD2 rule `c1_is_zero = false` stays, no shader reads it), the simple waveform being hidden when audio capture is
  stopped (the engine has no capture state; it always draws), the `m_bScreenDependentRenderMode` branches (the port
  never had them).

**Verification.** `wavegen_md3` unit tests reproduce the Annex B vectors of the spec for every mode (tolerance 1e-4,
1e-3 for modes 10, 15, 16): vertices at the sampled indices, vertex counts, `nBreak`, alpha tables, the
`wave_mystery` wrap, mode selection, the zero strip of mode 9, the invalid-mode zero vertices. `lactea` test
`md3_core_render` renders through the engine: modes 9..16 draw under MD3 and nothing under MD2, `nWaveMode=17` equals 0
pixel for pixel, 500-side shape area, border gate on / off / MD2. Smoke run on the MD3 pack (`fdrender --compat md3`,
30 frames, 320x180, `synth128.wav`): of the 58 `.milk` presets with `nWaveMode >= 9`, 22 still have `wave_a >= 0.004` at
frame 30 (the rest zero it in per-frame code and use custom waves), and 20 of those render differently from
`--compat md2` (which draws nothing for these modes): mode 9 (8 of 9 presets), 11 (6 of 6), 10 (1), 13 (1), 15 (4 of
5; the two presets that do not differ were not investigated). projectM and MD2 renders of 5 corpus presets (and a
soft cut) are bit-identical to the foundation branch.

**Open questions** (spec Annex B section 10, not resolved here): spectrum scaling of modes 8 / 13 and the mode-13
spike on loud bass, mode 9 with dots / thick on real D3D, the exact alpha of an unmapped mode, mode 14 / 16 direction
against the real program. They need Wine screenshots (`~/M0Rf30/md3-re/oracle/md3shot.sh`).

## Double presets (`.milk2`) and blend patterns

Rules: `CompatRules::md3_double_preset` (`.milk2` playback; the other compat modes refuse the file with
`EngineError::Unsupported`) and `CompatRules::md3_blend_patterns` (ordinary soft cuts draw MD3's 30 patterns).
Both are `false` for `ProjectM` and `MilkDrop2`, `true` for `MilkDrop3`. Rows of `divergences.md`: "MilkDrop 3".

### What a `.milk2` is

Two complete `.milk` texts and eight header keys that describe a *frozen MilkDrop 2 soft cut*: PRESET1 is the old
state (`m_pOldState`), PRESET2 the new one, `blending_progress` (`P`) the progress the blend is held at, the pattern
(`blending_pattern`, `blending_direction`, `random_1..5`) the per-vertex mix `clamp(a * P + c, 0, 1)`. Both presets
run all their equations every frame; per-frame variables are mixed by `CosineInterp(P)`, shapes and waves are drawn
with alpha `P` (PRESET2) and `1 - P` (PRESET1), warp / composite shaders are drawn as MD2's alpha-tiled pass pairs,
the booleans snap at the snap point (PRESET1's value below it). All of that is the MilkDrop 2 blend of
`engine/md2_blend.rs` with the progress pinned. Outer / inner borders are never drawn while a `.milk2` is on screen
(`Borders::hidden`, also with `DisableBox` off). A sprite block after `[PRESET2_END]` (`sprite=1`) belongs to PRESET2:
its text is part of `Milk2::preset2` and is honoured only with the header flag; PRESET1 never carries a sprite, and a
sprite block lying inside either half is ignored (the engine builds each half with an explicit "sprite allowed" flag).
A hard load builds the sprite with PRESET2; a soft transition imports PRESET2 and its sprite when the first leg
completes (the sprite's `time` and `frame` start then), as in MD3.

### Container (`lactea_preset::milk2`)

`parse_milk2(bytes, &Milk2Header) -> Result<Milk2, Milk2Error>` follows the MD3 loader step by step: the marker scan in
`fgets(255)` chunks (a line of 254 bytes or more counts twice and shifts every later marker; reproduced), header keys by
substring on every `fgets(256)` chunk, also inside the blocks (first key of a chunk wins, duplicates: last wins, value
after the chunk's first `=`), `sprite=1` extends PRESET2 to the last `[SPRITE1_END]` line (none: PRESET2 is empty),
block extraction by physical lines (fewer lines than the range needs: a warning in `Milk2::warnings` and an empty
text), names from a `NAME=` first line (a trailing `.milk` / `.milk2` removed). `Milk2Error::NotDouble` (a marker is
missing) means the file loads as an ordinary `.milk`. Header numbers that do not parse (`std::stof` / `std::stoi`
throw in MD3, no handler) are `Milk2Error::InvalidNumber` / `NumberOutOfRange`; no panic.

MD3 keeps the header values in process-global variables: a key that is missing keeps what the previous `.milk2` or
ordinary soft cut left. `Milk2Header` is that state as an explicit input (`Default` = process start: pattern `""`,
progress 1.0, direction 0, randoms 0.0); the engine owns one and updates it on every `.milk2` load and every ordinary
MD3 soft cut. `write_milk2(&Milk2, created, LineEnding)` is the saver byte for byte (3.37 layout; `%.2f` progress,
`%.6f` randoms; CRLF like MD3 on Windows or LF); `Milk2::new` builds the model from two bodies.

### Patterns (`engine/md3_blend.rs`)

`Md3Pattern` (30 variants, MD3's index and name order), `Md3PatternInstance { pattern, direction, r: [f32; 5],
progress }` (= the header values) and the pure generator `md3_fill_vertinfo(&instance, gx, gy, aspect, &mut [[a, c]])`.
Every random value is drawn up front, `md3_random_instance(rng, enabled, double_preset, auto_hardcut, locks)` draws, in
MD3's order, pattern, direction (unless locked), `r1..r5`, then (double-preset mode) the `AutoHardCutMode` pattern
restriction and the progress (`md3_draw_progress`, per-pattern ranges in `Md3Pattern::progress_range`). The mapping onto
the seeded `md3_blend_pattern` stream: `rand() = next32 >> 17`, `FRAND = (rand() % 7381) / 7380`, pattern index
`rand() % len`, direction `2 * (rand() & 1) - 1`, progress `rand() / 32767`; MD3 reseeds the C `rand()` from the clock
before each block, which has no meaning in a seeded engine. `horizontal` fills the mesh with transposed loops and a
running index, `plasma3` seeds its extremes with `0.5 * h(0, 0)`, `cross2` zeroes the vertices that never blend: all
copied as the spec describes them. `arrow` clamps the `atanh` argument to +-0.9999.

Ordinary soft cuts under MD3 draw from `EngineConfig::md3_transition_patterns` (default: the 13 patterns MD3 enables
without a `[transitions]` section: zoom side plasma plasma2 plasma3 cercle snail triangle corner patches clock arrow
curtain; empty = zoom only; `Engine::set_md3_transition_patterns`); `Engine::set_md2_blend_pattern(Some(k))` forces any
of the 30 (`forcePattern`). `Compat::MilkDrop2` keeps BeatDrop's 19.

### Playback (`Engine`)

`Engine::load_preset` recognises a double preset by content (the marker scan) unless the name ends in `.milk`;
a `.milk2`-named file that fails the scan is loaded as the plain `.milk` it is. `Engine::double_preset_info()` reports
the header, the half names, the phase (`DoubleMode`) and the blend's progress for a UI. Config: `blend_milk2`
(MD3's `BlendMilk2`, default on) and `blend_time_user` (`BlendTimeUser`, 1.7 s).

| request | MD3 `progressMode` | what the engine does |
|---|---|---|
| `.milk2`, nothing running / `Transition::Cut` / `blend_milk2` off | 2 (steady) | hard load: both halves built into one renderer, pattern generated, progress `P` from the first frame; base values slide for 1.0 s |
| `.milk2`, `Soft` with a running preset | 3, then 4, then 2 | leg A: the running preset blends to PRESET1 with the file's pattern in `blend_time_user`; when the ramp passes 1 PRESET2 is built on top of PRESET1 (leg B): `progress = min((t - start) / blend_time_user, P)`, then held forever |
| any `Soft` request while a `.milk2` is on screen | 5 | deferred (see below): `start := now - P * dur` (not during leg A), the ramp runs on to 1 (PRESET2 alone), then the saved request loads with its own time (an ordinary soft cut, or the soft-in legs for another `.milk2`) |
| `Cut`, `Soft { seconds <= 0 }`, `blend_milk2` off | - | hard cut at once |

Every `Compat::MilkDrop3` blend runs through the same `BlendState` as MD2's (`ProgressMode` carries `progressMode`,
the target `P` and the no-borders flag; `ProgressMode::Normal` is MD2's ramp and is all `Compat::MilkDrop2` ever uses).
A resize regenerates the frozen pattern for the new aspect.

### Deviations from MD3 and open points

- **Staged hard load.** MD3 shows PRESET1 alone for about seven frames while its staged load runs. Here the first frame
  already shows the frozen mix.
- **Deferral.** MD3 defers requests from the playlist (timer, next / previous) but loads menu and drag&drop requests
  directly. The engine only sees `load_preset`, so every `Soft` request is deferred, a second request while leaving is
  ignored (MD3 returns early too) and the report of a deferred request carries only the parsed name and version. The
  deferred preset is built when the ramp ends; a failure there goes to the switch-failed callback and the engine stays
  on PRESET2. PRESET2 failing to build at the end of leg A does the same (PRESET1 stays).
- **Number errors** are load errors, not process death. The `fgets` chunk quirk is reproduced (the spec suggests
  fixing it; the container is meant to behave like the file reader MD3 uses).
- **Unknown pattern names** leave `m_vertinfo` as it was (uniform `a = 1, c = 0` at the start, else the previous
  mask), as in MD3. `DoublePresetMode` pairs (F9) and the `p P o U u` live edits are not implemented;
  `md3_random_instance` has the draws for hosts that build pairs. MD3's `FFTAttack` / `FFTDecay` side effect of importing
  a half is implemented (PRESET2's keys win, "Shader inputs").
- Non-ASCII bytes of the half texts are kept as they are (MD3 re-encodes them through a wide stream; only comments
  are affected).
- `blending_progress` is printed with the platform's correctly rounded `{:.2}`; MD3's CRT may round exact ties
  differently.

### Hosts

`lactea` lists `*.milk2` below `--dir` only with `--compat milkdrop3` and keeps the `.milk2` extension in the preset
name; `--preset FILE.milk2` and `fdrender --preset FILE.milk2 --compat md3` load them (the window has no drag&drop
input, so there is nothing to wire there). The C API needs nothing beyond `LACTEA_COMPAT_MILKDROP3`: a `.milk2`
passed to `lactea_load_preset` is detected by content.

### Verification

- `lactea-preset` unit tests: the structure of the pack's `000.milk2` (markers 13 / 414 / 416 / 1335, blocks of 400 and
  918 lines, CRLF), spec A's tolerance vector, the chunk quirk at 252 / 253 / 300 bytes, `sprite=1`, duplicates and
  junk after numbers, malformed numbers, the saver's layout and byte-exact round trips with and without a sprite.
- `md3_blend` unit tests: the full 4x2 reference table of all 30 patterns (both directions where they matter) within
  1e-3, purity, the horizontal / plasma3 / cross2 quirks, the MSVC `srand(1)` vector of the random block.
- `tests/double_preset.rs` (GPU): two flat-colour presets with `side`, `P = 0.5`: PRESET1 left, PRESET2 right, the
  blend in between, identical frames 300 frames later; roles at `P = 0` and `P = 1`; both soft-in legs and the hold
  against the 60 fps clock; the soft-out ramp (start rewrite, 30 frames from `P = 0.5`, ignored second request, the
  saved request as an ordinary soft cut); a request during leg A; `BlendMilk2` off; cuts; carry-over of missing
  keys; borders; seeded / forced / configured patterns; resize.
- The MD3 pack (146 `.milk2`) under `fdrender --compat md3`: all load and render 30 frames (exit 0 for 142; the 4 exit-3
  runs are shader fallbacks for `get_fft` inside the halves, a feature of the shader-inputs slice). Before this
  change the same files loaded as plain `.milk` (the first block of keys wins, header and halves interleaved): 42 x
  exit 0, 3 x exit 2, 101 x exit 3.
- `fdrender --stages final` of four corpus presets under `projectm` and `md2`, plain and with a soft cut (320
  frames): identical hashes before and after.
- **Wine oracle, `md3shot.sh --via-ui`** (the only path that shows the real mix: load the `.milk2` from the `L` menu
  after starting on a single preset; a `.milk2` given as `StartPreset` renders PRESET1 alone). Flat-colour pairs
  (PRESET1 red, PRESET2 blue, composite shaders, `blending_progress = 0.5`), red channel = `1 - weight of PRESET2`, centre
  row, lactea (`fdrender --compat md3 --size ... --mesh 48x36`) against the MD3 shots in `oracle/samples/via_ui/`:

  | case | contour (weight of PRESET2) | MD3 | lactea |
  |---|---|---|---|
  | `side`, 800x450, R1 = 0, R2 = 0.5 | x at 0.05 / 0.5 / 0.95 | 293 / 395 / 501 | 291 / 393 / 497 |
  | `cercle` dir 1, 800x450 | half width of the contour at 0.05 / 0.5 / 0.95 | 213 / 278 / 347 | 213 / 278 / 348 |
  | `cercle` dir 1, 800x800 | same | 213 / 278 / 347 | 213 / 278 / 348 |
  | `cercle` dir -1, 800x450 | the exact inverse | yes | yes (`w1 + w2 = 1` within 3 %) |

  The mean absolute difference of the red channel against the shots is 0.3 % (`side`) and 0.7 - 0.8 % (`cercle`),
  the largest pixel difference 8 %. This settles spec B open question 2: `cercle` is a **circle in screen pixels**
  whose radius scales with the window *width* (the disc is cut off at the top and bottom of a 16:9 window), which is
  what `AspX = 1`, `AspY = h / w` gives; no change of the aspect handling was needed. Roles confirmed: PRESET1 =
  old = inside / left, PRESET2 = new. `tests/double_preset_oracle.rs` pins the contours (+-6 px). Note the contour
  needs composite shaders: with warp shaders the tiles are alpha-culled per vertex and the edge is coarser.
- Real presets, lactea (800x450, silence, frame 400) next to the MD3 shots (`via_ui/{pack_000, milk2_sprite,
  milk2_plasma_sprite, golden_mirror}`; time and randomness are not reproducible, so only structure is compared):
  `000.milk2` (cercle 0.45, -1): the same red-forest disc with its red ring in the teal PRESET1 blocks; `5 Stars`
  (stars, sprite): the same green / teal kaleidoscope with the mandala sprite and orange speckle edges; `Manda4` (plasma,
  sprite): the grey mandala sprite over the glassy PRESET1 in both; `golden mirror` (plasma): the same family (dark
  blue sky, coloured checker floor, magenta rings), details differ with time.
- Hand-made flat-colour pairs, lactea only (800x450): `cross2` P = 0.85 a cross of blended / PRESET2 bands with the four
  corner rectangles pure PRESET1 (the vertices that never blend), `checkerboard` P = 0.5 four isolated blobs of
  PRESET1: as the formulas predict, no oracle counterpart.

## Sprites

A preset may carry one sprite: an image reference, placement numbers and per-frame code (`SpriteName` ..
`SpriteRepeatY`, `code_N`; keys are read wherever they are, first occurrence wins, `SpriteName` last).
`lactea-preset` parses it into `Preset::sprite` (`PresetSprite`, round-trips through `to_milk`); the engine
(`crates/lactea/src/sprites.rs`) loads and runs it, the renderer draws it. Rules: `md3_preset_sprites`,
`md3_sprite_burn_twice`, `md3_gif_textures` (true for `Compat::MilkDrop3` only; projectM and MilkDrop 2 never draw
preset sprites, output unchanged).

- **Loading.** `TextureProvider::load_sprite(path)` gets `SpriteName` (`sprites\x.png`, relative to the MD3 data
  root) and returns a `Media`: a still image or an animated GIF (frames + delays). A static image gets the colour
  key (texels `== (key, alpha 255)` become transparent black before the mip chain) and a full mip chain, sampled
  trilinear + wrap; a GIF is never keyed and has no mips. A sprite that cannot be loaded is a report warning, the
  preset runs without it (`PresetReport::sprite` names the loaded sprite).
- **Per frame.** One EEL context per sprite with the variables of the specification (`time frame fps progress bass..
  treb_att` in; `x y sx sy rot flipx flipy r g b a repeatx repeaty done burn blendmode` out, defaults `x=y=.5 sx=sy=1
  r=g=b=a=1 burn=1`); `time`/`frame` count from the sprite's load. Geometry is the specification's chain (image
  aspect, rotation, translation, screen aspect; `SpriteX/Y/SX/SY` are additive, `SpriteRot` 1.0 / 2.0 is a
  rotation mode with `SpriteSpeed` times the absolute time, repeat multipliers, `SpriteAlpha` multiplies `a` without a
  re-clamp, so the vertex alpha byte wraps like `& 0xFF`). The last frame of a burning sprite (`done && burn`) gets
  MD3's extra 4:3 correction. A layer outside 0..4 is never drawn and its code never runs. `done` kills the sprite
  after its last draw; loading any preset replaces it (also for a MilkDrop 2 style soft cut: the new preset's sprite
  is at full opacity from its first frame, the old one is gone).
- **`progress` and failing code.** The `progress` variable of sprite code is the blend's progress
  (`m_pState->m_fBlendProgress`: `blending_progress` in a steady `.milk2`, the ramp during a soft cut, 1 when no blend
  runs; the last is inferred, MD3 leaves the value just above 1), not the preset progress; no pack sprite reads it. The
  sprite VM is built from the engine's `GlobalMemory`, so its code runs on ns-eel2 like all other MD3 code. Code that
  does not compile is not executed, silently in MD3 (spec D 4.2: "a broken sprite code is silent: it is simply not
  executed") and with a `sprite code dropped: ...` warning here; the sprite is still drawn. That is the
  `md3_drop_failed_code` rule, which therefore needs no special case for sprites.
- **In a `.milk2`.** The block belongs to PRESET2 and needs the header's `sprite=1` (see "Double presets"); `Preset::sprite`
  of PRESET1 is cleared before the half is built, and so is a block inside PRESET2 without the flag.
- **GIF.** `lactea::decode_gif`: GIF89a composition (transparent start canvas, per-frame disposal `Background` =
  clear the frame rectangle to *transparent*, `Previous` = restore, `Keep`), delay `1/100 s`, `0` means 0.1 s. This
  is a documented choice for the open question about GDI+'s frame composition (the pack's `dmt.gif` uses "do not
  dispose", where every decoder agrees). Frame choice: `t = fmod(time since the sprite started, total)`, the first frame
  whose cumulative delay is strictly greater than `t` (`gif_frame_index`, unit-tested with the `[10, 20, 5]` vector).
- **Blend modes.** 11 pipelines, one fragment entry point per mode (stage-0 colour / alpha) and the table's
  blend factors (`crates/lactea-render/src/sprite.rs`); vertex colours quantised like `D3DCOLOR`. Tested on the GPU,
  all 11 modes x two colours x opaque / keyed texel, against a CPU model of the table (and the CPU model against the
  specification's worked example, +-1).
- **Layers.** 1..4: into the feedback image after warp+blur / shapes / custom waves / main wave + borders (the
  overlay batch is split into passes only when a layer 2 or 3 sprite exists); 0: onto the composited output,
  `burn` also into the feedback image after the composite has read it. Layer ordering is tested (re-warped and
  composited vs not).
- **Burn in layers 1..4 is drawn twice (`md3_sprite_burn_twice`).** The specification's open question 1 is real:
  a layer-1 additive grey sprite (alpha 0.05, decay 0.9) settles at 56/255 with `SpriteBurn=0` and 116/255 with
  `SpriteBurn=1` under MD3 (Wine oracle, 800x600); lactea reproduces 55 and 115. Verified for layer 1 only; layers 2..4
  share the code path in the specification and are assumed to behave the same.
- **Animated GIFs as shader textures** (`md3_gif_textures`): `TextureProvider::load_md3_texture` may return
  `Media::Animated`; the renderer (`set_user_animation`) shows frame `gif_frame_index(delays, time)` with `time` =
  the frame's absolute time (not the shader's wrapped `time`). Search order for a shader texture in the hosts:
  per extension `jpg jpeg jfif png tga bmp dib gif`, folders `textures`, the given folder, `sprites`, `gifs`
  (`MediaFiles::find_texture`). The preset's own folder is not searched (the engine does not know it).
- **Hosts.** `lactea-app`, `fdrender --texture-dir`, the C API directory provider: when the directory is itself
  `textures` / `sprites` / `gifs`, its parent is the MD3 data root, so `--texture-dir <pack>/textures` finds
  `sprites\x.png`; a directory that is the root works as is. Deviation: a sprite path found nowhere is looked up by
  file name in the given folders (MD3 would show an error banner). `.dds` stills are not decoded.

Not done: `sprites.ini` / digit-key launched sprites and the user sprite (host features), the two 100-entry
texture caches and `PreLoadSprites`, the webcam layers, Spout. The colour-key quirk (a global flag that disables
the key for later loads) is not replicated.

### Verification

- Unit / GPU tests: key parsing and round trip, geometry vectors G1..G9, blend table, GIF timing and disposal, colour
  key, layer order, burn, engine-level sprite / GIF-sprite / GIF-texture / lifetime / compat gating tests.
- MD3 pack smoke (`fdrender --compat md3 --texture-dir <pack>/textures`, 30 frames at 640x360): 97 pack presets contain
  `SpriteName=` (the task said 98); 96 load their sprite image and 94 of those render differently from the same preset
  under `--compat md2` (identical rules otherwise), i.e. the sprite is visible. The 2 that are identical
  (`Classic.Wave.078g2` / `081g`, blend 5 = multiplicative over a black scene) cannot show anything by construction. The
  one that does not load was a `.milk2` (`R192 Cubes.milk2`) before the double-preset parser was merged: with it all 24
  `.milk2` that carry `sprite=1` load and draw their sprite.
- Oracle (Wine, MD3 3.37) vs lactea, looked at side by side (`out/` shots of this work, not committed): a quad-colour test
  sprite drawn at `x=y=.75` appears at the lower right, upright, with the red quadrant top-left, and with
  `rot=1.5708` turned clockwise (red to top-right) — pixel-identical in both renderers (open questions 3 and 4:
  y down, clockwise rotation, landscape centre scaling confirmed). Pack presets: `GIF test.milk` (dmt.gif layer,
  lilac / teal marbling), `Waves and Grid.milk` (primrose sprite, layer 1, blue tiling with two bright bars) and
  `Chrome 001s.milk` (blend 7, torus) match in structure, palette and sprite placement; the frames differ in detail
  because MD3 under Wine runs on wall-clock time and unseeded randomness.

## Auto hard cuts (`AutoHardCutMode`)

Spec: MD3 3.37 hard-cut block, preset-change timers. Implemented in `crates/lactea/src/auto_cut.rs` (decision
logic and the shader text rules), `crates/lactea/src/engine/comp_effects.rs` (engine driver, effect injection),
`PresetControl` (`control.rs`, switch requests, display time) and the hosts.

Rules (`CompatRules`): `md3_auto_hard_cut` (true only for `MilkDrop3`) gates the whole feature in the engine;
`md3_preset_time_rand` (true only for `MilkDrop3`) tells hosts to use the MD3 display time. Under projectM and
MD2 the mode setting is stored and never acts.

### Modes

`EngineConfig::auto_hard_cut_mode` (0..=7, default 0; clamped), `Engine::{set_,}auto_hard_cut_mode`,
`Engine::auto_hard_cut_active`. `t` is the time since the last event; the thresholds are strict.

| mode | fires when | action |
|---|---|---|
| 1 | `bass > 1.5` and `t >= 0.2 s` | cut |
| 2 | `treb > 2.9` and `t >= 0.5 s` | cut |
| 3 | `treb > 2.9` and `t >= 1.0 s` | cut |
| 4 | `treb > 2.9`, then `t >= 3 s` (cut) **and**, with the same old `t`, `t >= 1 s && treb > 8` (cut again) | 0, 1 or 2 cuts |
| 5 | `treb > 2.9` and `t >= 5 s` | cut |
| 6 | `bass > 1.5`, no minimum delay | cut (consecutive frames possible) |
| 7 | `treb > 2.9` and `t >= 0.5 s` | inject one effect, no preset change |

`2.9` is the float constant widened to double (`2.9000000953674316`); `t` is a float of the elapsed seconds, so
exactly 200 ms counts as `t >= 0.2`. A cut or an injection restarts `t`; only cuts count in
`AutoCut::cuts_total`. Mode 4's two cuts come from one frame's `t`.

Inputs: `bass` and `treb` are read from the running preset's per-frame variables at the **start of
`Engine::render`**, i.e. the values the previous frame's per-frame code left (which may have assigned them).
Without audio they are 1.0 (silence gives relative levels of 1.0), so nothing fires. The clock is the frame
clock (`FrameClock::time`), not wall time; the mode-7 effect comes from a seeded stream (`rand() % 6` in MD3).

### Switch requests

A cut is not a preset change by itself (the engine never loads presets): `Engine::take_auto_cuts()` returns the
cuts requested since the last call and hosts hand them to
`PresetControl::update_with_auto_cuts(time, vol, cuts)`, which returns `SwitchRequest { hard_cut: true }`
(poll / callback in the C API). The host loads the next preset with `Transition::Cut` (blend 0). It is
independent of the projectM `hard_cut_*` settings (volume jump) and of the MD2 loudness cut
(`bHardCutsDisabled`, unchanged and not modelled by `Compat::MilkDrop2`). Hosts: `lactea-app` F8 (cycles
0 -> 1 -> ... -> 7 -> 0, title shows `hardcut<N>`) and `--auto-hard-cut N`; `fdrender --auto-hard-cut N` (renders
one preset: counts the cuts, mode 7 shows its effects); C API `lactea_set_auto_hard_cut_mode` /
`lactea_get_auto_hard_cut_mode` and `LacteaConfig::auto_hard_cut_mode`.

### Mode 7 effects

The six effects mode 7 can pick, in MD3's `rand() % 6` order (`auto_cut::MODE7_EFFECTS`):

| # | effect | marker line | code line | where |
|---|---|---|---|---|
| 0 | solarize | `//MilkDrop3 Solarize Mode:` | `ret = ret*(1-ret)*4;` | before the last `}` |
| 1 | split | `//MilkDrop3 Split Mode:` | `uv = float2 (abs(uv.x-0.5), uv.y);` | after the first `{` following `shader_body` |
| 2 | multiplicate | `//MilkDrop3 Multiplicate Mode:` | `uv = 1- abs( frac((uv-.5)*1.5 * 0.5 ) * 2 - 1.0 );` | body start |
| 3 | rotate | `//MilkDrop3 Rotate Mode:` | `uv = uv.yx;` | body start |
| 4 | flip | `//MilkDrop3 Flip Mode:` | `uv = 1-uv;` | body start |
| 5 | invert | `//MilkDrop3 Invert Mode:` | `ret = 1 - ret;` | before the last `}` |

`Engine::inject_comp_effect(effect)` (also public, MD2 family only) edits the **loaded preset's composite
shader text** (lines separated by `0x01`) and rebuilds the shader through the load path:

* **Toggle.** If the marker is already in the text, the marker line and the code line after it are removed;
  otherwise `<marker> 0x01 <code> 0x01` is inserted. Coordinate effects go right after the first `{` after
  `shader_body`; colour effects replace everything from the last `}` with `<marker> 0x01 <code> 0x01 } 0x01`.
* **Accumulation.** Effects stay in the preset's text until the next preset load (they are not undone by the
  next frame; injecting twice removes). They apply in text order: coordinate effects first (the most recently
  injected one runs first), then the preset's own code, then the colour effects in injection order.
* **No composite text.** A preset with `PSVERSION_COMP < 2` or without `comp_N` lines first gets the default
  composite shader generated from its legacy parameters (gamma, video echo, hue shader, brighten / darken /
  solarize / invert), PS version 4 and the composite shader on, then the effect is injected.
* **Failure.** No insertion point (no `shader_body` + `{`, no `}`), a translation failure or a module wgpu
  rejects changes nothing; the previous composite shader keeps running.
  The insertion point is found by plain substring search on the raw text, comments included: a pack preset
  whose helper function is preceded by a comment that
  mentions `shader_body` gets its coordinate effects inside that function, which does not compile (`_uv` is not
  in scope) and is therefore dropped. Over the 331 `.milk` of the MD3 pack, all six effects inject and
  translate on every composite shader that translates on its own, except those four coordinate-effect
  injections on that one preset.

The effects use `uv`, `ret`, `frac` and `abs` only, so they need nothing from the MD3 shader prelude.

### Display time

`PresetControl::set_md3_time_rand(Some(rand))` (hosts do it when `md3_preset_time_rand`; the C API's
`LacteaSwitchSettings::preset_duration_rand`, the app's `--duration-rand`; default 10 s, `TimeBetweenPresetsRand`):
a preset is shown for `soft cut duration (BlendTimeAuto) + preset duration (TimeBetweenPresets) +
(r % 1000) * rand * 0.001` seconds, `r` from a seeded stream (`rand() % 1000` in MD3), replacing projectM's
gaussian easter egg. With MD3's defaults 2.7 + 16 + `[0, 9.99]` s = 18.7 .. 28.69 s. The engine's `progress`
variable still divides by the nominal `preset_duration` (the same limitation as the easter egg, see
`docs/parity.md`).

### Deviations and open questions

* **README vs binary.** MD3's README promises mode 6 loading `Bass/WHITE.milk` for `bass > 4.9` and mode 7 with a
  1.5 s delay. The 3.37 binary has neither (mode 6 = plain `bass > 1.5` without delay, mode 7 = 0.5 s); this
  follows the binary. The README behaviour is not offered. Not checked under Wine: the oracle cannot feed a
  loud-bass stream (silent loopback), so the mismatch stays "binary analysis only".
* **Locks.** MD3's block does not test preset locks (inferred). Here a locked `PresetControl` / app browser
  never emits a request, so a locked preset ignores auto cuts (mode 7 still injects: it does not change the
  preset).
* **Levels of a fresh preset.** MD3 reads the new state's registered variables, which hold the audio levels of
  the load; here a freshly loaded preset has run no per-frame code, so its first frame sees 0 / 0 and cannot
  cut. Mode 6 therefore cuts at most every second frame while the same loud bass lasts.
* **Mode 4's second cut.** MD3 calls `NextPreset` twice (two presets loaded in a row, the first never shown).
  Hosts get one request and load one preset, so the first of the two is shown.
* **Clock.** Frame clock instead of wall time; a clock that jumps backwards re-anchors the last event; the
  timer starts at the first frame the mode is active and survives switching the mode off and on again (as MD3's
  static does).
* **Effects not offered.** MD3's effect helpers also back an effects menu (lava, zoom, rad x / rad y,
  vertical / horizontal flip, monochrome, burn, blow, rainbow, shadows, red dot); mode 7 cannot reach them
  (its branches for them are dead code) and they are not implemented.
* **Persistence and PS versions.** `settings.ini` (`AutoHardCutMode`) is the host's business; the stored "max PS
  version" MD3 sets to 3 is derived from the warp / comp versions here (comp 4).

### Verification

Unit vectors from the spec's section 7.3 for every mode, mode 4's two-cut case and mode 5's second vector
(`auto_cut::tests`); a seeded-stream test that all six effects are reachable; text insertion / toggle / removal
rules; translation of every effect (alone and all six together) on three typical composite texts; engine tests
(`crates/lactea/tests/auto_cut.rs`, GPU, skipped without an adapter): mode 1 cuts at the right frames under MD3 and
never under projectM / MD2 / mode 0, the runtime setter, a cut reaching `PresetControl` as a hard-cut request,
output of every effect against its HLSL (+-3/255) on a `(uv.x, uv.y, 0.25)` composite, toggling off,
accumulation, injection into a preset without composite text, mode 7 changing the running picture without a cut.

Visual check (looked at the frames, `fdrender --compat md3`, `martin - blue haze MilkDrop2077rmx.milk` of the MD3
pack, 320x180, frame 99 of each variant with the effect's text inserted as above): solarize turns the white core
dark and folds the mid-tones into saturated green / yellow rims; split mirrors the picture about the vertical
centre line; multiplicate tiles it into a mirrored kaleidoscope; rotate transposes it (the diagonal stretch of
`uv.yx`); flip turns it by 180 degrees; invert gives the colour negative. With `--auto-hard-cut 7` on the same
preset and a synthetic loud-treble stream, frames 129 of seeds 2-5 show the accumulated random effects (mirror
symmetry, inversion, solarized rims, 4-way kaleidoscope) while the unmodified run shows the plain picture. These
were compared against the HLSL of the effects, **not** against an MD3 reference shot: the Wine oracle cannot
trigger a mode (needs a loud-treble audio stream and a `settings.ini` mode), so the on-screen look of MD3's own
injection is unverified.

## .wave / .shape files and the MD3 .milk writer

**Files.** A `.wave` / `.shape` file is one custom wave / shape written with index 0 only
(`wavecode_0_*` + `wave_0_init<n>` / `wave_0_per_frame<n>` / `wave_0_per_point<n>`; `shapecode_0_*` +
`shape_0_init<n>` / `shape_0_per_frame<n>`), no header, no `[section]`, `\n` line ends. Field order and
printf formats are MD3's: ints `%d`, wave `scaling` / `smoothing` and shape `rad` / `ang` / `tex_ang` /
`tex_zoom` `%.5f`, everything else `%.3f`. Values therefore round-trip only up to that printed precision
(a colour of `0.123456` comes back as `0.123`); code blocks round-trip exactly.

**Library (`lactea-preset`).**
- `import_wave_file(&mut Preset, slot, bytes)` / `import_shape_file(..)`: read the file with the MD2
  line rules (first `=`/space, first duplicate wins, lenient numbers) into slot `slot` **without resetting
  the slot**: keys missing from the file keep the slot's values (a slot with `a=0.7` and a file without
  `wavecode_0_a` stays at 0.7), the code blocks are cleared and re-read (empty if the file has none).
  Errors: `ParseError::SlotOutOfRange { slot, count }` for `slot >= 16`, `Empty` / `Utf16` as for `.milk`.
  The preset is unchanged on error. The projectM-only shape `image` key is not read.
- `export_wave_file(&Preset, slot) -> Option<String>` / `export_shape_file(..)`, also
  `CustomWave::to_file_text()` / `CustomShape::to_file_text()`.
- `Preset::to_milk_md3()`: the MD3 `.milk` writer. Optional first lines `MD31=` / `MD32=` (written only if
  the preset carries that key in `extra` and the warp / comp text contains the stub marker `This shader`),
  then, for ps version > 0, `MILKDROP_PRESET_VERSION=` (`300` with a sprite, else `201`) and `PSVERSION*`,
  `[preset00]`, the base values in MD3's order, the waves then shapes with `enabled == 1` **renumbered from 0**
  (`enabled == 2` is not written), `per_frame_init_` / `per_frame_` / `per_pixel_`, and `warp_` / `comp_`
  for ps >= 2 with a backtick on every line. No banner, no CR. Everything MD3 does not write is dropped:
  other `extra` keys and the shape `image`. `Preset::to_milk` (lossless, used by MD2 /
  projectM) is unchanged.
- The sprite block (`Preset::sprite`, see "Sprites") is written after `comp_N`: a blank line, `[SPRITE1_BEGIN]`, the 13
  keys in MD3's order (`%.6f` scalars, `%d` ints, `0x%X` colour key without padding except the literal `0x000000`),
  `code_N`, `[SPRITE1_END]`; the version line is `300` for a preset with a sprite (only written for ps version > 0,
  like MD3).

**Engine.** `Engine::import_custom_wave(slot, bytes)` / `import_custom_shape(..)` apply the import to the
source preset of the preset on screen (the incoming one during a soft cut) and rebuild its whole runtime:
expression contexts, init code (q/t reset), waves, shapes, shaders. It is not a preset change: no soft cut,
the clock is not restarted, and the preset's renderer keeps its feedback image. It goes through the regular
load path with the modified preset serialised by `Preset::to_milk`, so the returned `PresetReport` reports
compile / shader problems like `load_preset`; on any error the preset is left as it was. Errors:
`EngineError::NoPreset` (new variant), `EngineError::Parse(SlotOutOfRange | Empty | Utf16)`, compile errors.
`Engine::export_custom_wave(slot)` / `export_custom_shape(slot)` return the file text (`None` if nothing is
loaded or `slot >= 16`).

**Hosts.** MD3 sends a dropped `.wave` / `.shape` file to slot 8 of the respective kind; the app UI is a host
concern and not implemented here: a host should call `import_custom_wave(8, bytes)` /
`import_custom_shape(8, bytes)` for dropped files with those extensions.

**Deviations / limits.**
- The recompile does not touch a running MD2 blend (`Compat::MilkDrop2` soft cut): the blend keeps its
  old-state copy.
- MD3 writes `FFTAttack` / `FFTDecay` from process globals; here they are the preset's own values.
- `MD31` / `MD32` handles are passed through verbatim only to keep the file loadable by MD3; lactea ignores
  them when rendering.
- Under `Compat::ProjectM` only 4 custom slots run (`CompatRules::custom_slots`), so an import into slot >= 4
  edits the preset but draws nothing; hosts that send dropped files to slot 8 need a MilkDrop-family compat.
