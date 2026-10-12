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
| `.milk` MD3 extensions (additional keys, expression variables and functions, shader variables) | Not yet |
| `.milk2` double presets (two presets plus a frozen soft cut) | Not yet |
| Blend patterns of MD3 soft cuts (30 patterns) | Not yet |
| Sprites (`[SPRITE1_BEGIN]` blocks, layers, blend modes, GIF) | Not yet |
| `.wave` / `.shape` files (single custom wave / shape import and export) | Not yet |
| Locked shaders (`MD31` / `MD32`) | Not supported: the placeholder shader in the file runs |

Feature slices add their sections below, each with the `CompatRules` fields it introduced.
