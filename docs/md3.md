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
| `.wave` / `.shape` files (single custom wave / shape import and export) | Supported (library/engine API; no app UI) |
| Main waveform modes 0..16, shape sides up to 500, warp-term skip, border gate of blends | Supported (see "Main waveform and render deltas") |
| Locked shaders (`MD31` / `MD32`) | Not supported: the placeholder shader in the file runs |

Feature slices add their sections below, each with the `CompatRules` fields it introduced.

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
  other `extra` keys, the shape `image`, sprite sections. `Preset::to_milk` (lossless, used by MD2 /
  projectM) is unchanged.
- `Preset::has_sprite()` is a **stub**: true when an `extra` key starts with `sprite` (case-insensitive).
  The sprite slice parses sprites and should replace it.

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
  (`FrameInput::borders.hidden`, set by the engine's blend frame). "Never in a double preset" belongs to the `.milk2`
  slice, which sets the same field.
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
