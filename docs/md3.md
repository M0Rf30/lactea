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
