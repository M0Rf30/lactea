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
