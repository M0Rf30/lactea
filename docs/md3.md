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
