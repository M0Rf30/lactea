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
| `.milk` with MD2 keys and code (what MD2 / BeatDrop read) | Supported: the MD2 importer, expressions and renderer (`Flavor::Md2`) |
| MD2 code-line joining (`CompatRules::md2_code_join`) | Supported: lines are glued without a separator, see `divergences.md` |
| `.milk` MD3 extensions (additional keys, expression variables and functions, shader variables) | Not yet |
| `.milk2` double presets (two presets plus a frozen soft cut) | Not yet |
| Blend patterns of MD3 soft cuts (30 patterns) | Not yet |
| Sprites (`[SPRITE1_BEGIN]` blocks, layers, blend modes, GIF) | Not yet |
| `.wave` / `.shape` files (single custom wave / shape import and export) | Not yet |
| Locked shaders (`MD31` / `MD32`) | Not supported: the placeholder shader in the file runs |

Feature slices add their sections below, each with the `CompatRules` fields it introduced.

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
Annex C 1.6): whether a preset *without* the keys keeps the previous preset's values; the specs disagree (C 3.4: reset
to 0.5 / 0.7, E: sticky). Lactea resets.

**Prelude** (`lactea-hlsl`, `Options::md3`): `_c14` and `mouse`, `mouse_x/_y/_z/_w`, `mouse_pos`; `_qi.._qp` and
`q33..q64`; `sampler_fft`; macros `get_fft(pos)` = `tex2D(sampler_fft, (clamp(pos,0,1), 0.5)).r` (no `sqrt`) and
`get_fft_hz(f)` = `get_fft(clamp(f/24000,0,1))`. No `get_fft_peak*`, `get_wave*`, `_c15` / `_c16`. `sampler_wave` stays an
ordinary user texture named `wave`; the engine binds the 1x1 black placeholder (MD3 binds a texture it never fills).
`texsize_fft` answers (512, 1) (MD3 leaves it unresolved). All PSVERSIONs are one dialect. Without the rule the names
do not exist (a preset using them falls back, as before).

**Uniforms** (`lactea-abi`): the frozen `MilkUniforms` is untouched; `MilkExtUniforms { mouse, q_hi }` is a second block at
`@group(0) @binding(1)` (`mx` in `WGSL_COMMON`), always bound by the renderer and neutral outside MD3
(`mouse = (-1, -1, 0, 0)`). Shader `time` (`_c2.x`) is the time since the state's preset was loaded, modulo 10000; the
outgoing state of a soft cut uses its own start time and its own `q33..q64` / mouse.

**Expressions**: `q1..q64` and `t1..t16` in the per-frame, per-vertex, custom wave (per-frame and per-point) and shape
VMs, with the data flow of the spec (all 64 q restart from the post-init snapshot every frame, pf -> pv copy of 64,
waves / shapes get the current preset q, init snapshot of all 64). `monitor2` / `monitor3` are plain user variables and
persist exactly like `monitor` (lactea's contexts never reset them). `mouse.x/.y/.z/.w` are registered in the per-frame
and per-vertex VMs (names with a dot need the MD3 EEL dialect to be *parsed*; the variables themselves are
registered here): `x = px/W`, `y = 1 - py/H` (bottom origin; the shader's `_c14.y` is `py/H`), `-1, -1` outside the
window, `z` = left button, `w` = one frame after release. `mouse.x/.y` are blended in soft cuts, `.z/.w` are not.

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
