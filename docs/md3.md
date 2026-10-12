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
| `.milk2` double presets (two presets plus a frozen soft cut) | Supported: container, playback, progress modes, `BlendMilk2`; see "Double presets" |
| Blend patterns of MD3 soft cuts (30 patterns) | Supported: `md3_blend.rs`, enabled-pattern list |
| Sprites (`[SPRITE1_BEGIN]` blocks, layers, blend modes, GIF) | Not yet |
| `.wave` / `.shape` files (single custom wave / shape import and export) | Not yet |
| Locked shaders (`MD31` / `MD32`) | Not supported: the placeholder shader in the file runs |

Feature slices add their sections below, each with the `CompatRules` fields it introduced.

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
`engine/md2_blend.rs` with the progress pinned. Outer / inner borders are never drawn while a `.milk2` is on screen.
A sprite block after `[PRESET2_END]` (`sprite=1`) belongs to PRESET2: its text is part of
`Milk2::preset2`, for the sprite slice's parser.

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
  mask), as in MD3. `DoublePresetMode` pairs (F9), the `p P o U u` live edits and MD3's `FFTAttack` / `FFTDecay` side
  effect of importing a half are not implemented; `md3_random_instance` has the draws for hosts that build pairs.
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
- Wine oracle (`md3shot.sh`, 9 runs): not conclusive. With the setup of `oracle/README.md` MD3 3.37 shows PRESET1 alone
  for every `.milk2` loaded at startup, also for the pack's own `000.milk2` (cercle 0.45: no red-forest disc at the
  centre) and for hand-made flat-colour pairs (warp or composite shader, `zoom` at `P = 1`, also with
  `BlendTimeUser` / `BlendTimeAuto` at their defaults): the mix never appears in that environment, so spec B open
  question 2 (aspect orientation of `cercle` on 16:9) and the patterns absent from the pack stay unconfirmed by the
  oracle. lactea draws `cercle` (`P = 0.8`, band 0.02, 800x450) as a disc of about `rho * width` = 110 pixels radius in
  both directions (`AspX = 1`, `AspY = h / w`, as MD2).
- Visual comparison (looked at the PNGs; the oracle is not deterministic and runs on silence, lactea on `synth128.wav`,
  so only structure and palette can be compared):
  - `000.milk2` (cercle 0.45, direction -1), lactea 320x180 frame 29: a disc of PRESET2 (red forest) around the centre
    inside PRESET1 (teal cyberpunk blocks), the soft edge of the pattern visible: the expected cercle / -1 layout. The
    Wine oracle at 800x450 shows PRESET1 alone (no disc), see above.
  - golden mirror (plasma 0.4x): both show the same family of image (dark blue top, coloured checker floor); lactea
    additionally shows PRESET1's glass spikes and PRESET2's concentric rings / sine waves mixed in, the oracle's shot is
    purple bubble rings on the checker floor. Not comparable beyond palette; consistent with the oracle showing one
    half.
  - 5 Stars (stars, sprite): both green / teal kaleidoscopes on black; the oracle's is symmetric with a sprite
    (mandala) and thin neon lines, lactea's has orange speckle edges and no sprite (the sprite pass belongs to another
    slice). No claim of a match beyond palette.
  - Hand-made flat-colour pairs, lactea only (800x450): `cercle` P = 0.8 a soft-edged disc of PRESET1 in PRESET2
    (direction 1: the centre is the old preset), `cross2` P = 0.85 a cross of blended / PRESET2 bands with the four
    corner rectangles pure PRESET1 (the vertices that never blend), `checkerboard` P = 0.5 four isolated blobs of
    PRESET1: all as the formulas predict. None of these has an oracle counterpart (see above).
