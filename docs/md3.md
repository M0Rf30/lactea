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
| Sprites (`[SPRITE1_BEGIN]` blocks, layers, blend modes, GIF) | Supported, see [Sprites](#sprites) |
| `.wave` / `.shape` files (single custom wave / shape import and export) | Not yet |
| Locked shaders (`MD31` / `MD32`) | Not supported: the placeholder shader in the file runs |

Feature slices add their sections below, each with the `CompatRules` fields it introduced.

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
  one that does not load is a `.milk2` (`R192 Cubes.milk2`): the double-preset parser is another slice's, here the
  file is read as one `.milk` and its per-frame code fails.
- Oracle (Wine, MD3 3.37) vs lactea, looked at side by side (`out/` shots of this work, not committed): a quad-colour test
  sprite drawn at `x=y=.75` appears at the lower right, upright, with the red quadrant top-left, and with
  `rot=1.5708` turned clockwise (red to top-right) — pixel-identical in both renderers (open questions 3 and 4:
  y down, clockwise rotation, landscape centre scaling confirmed). Pack presets: `GIF test.milk` (dmt.gif layer,
  lilac / teal marbling), `Waves and Grid.milk` (primrose sprite, layer 1, blue tiling with two bright bars) and
  `Chrome 001s.milk` (blend 7, torus) match in structure, palette and sprite placement; the frames differ in detail
  because MD3 under Wine runs on wall-clock time and unseeded randomness.
