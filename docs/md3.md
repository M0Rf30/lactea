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
| Expression engine (ns-eel2 numerics, name / literal / operator rules, RAM model, compile-failure policy) | Supported: `CompatRules::md3_ns_eel2`, `md3_drop_failed_code`, section "Expression engine" below |

Feature slices add their sections below, each with the `CompatRules` fields it introduced.

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

The shader-inputs slice registers `mouse.x`, `mouse.y`, `mouse.z`, `mouse.w` by name as the spec says; given the
measurement above those variables exist in MD3's tables but are unreachable from preset EEL (no pack preset uses
them in EEL; `mouse.x` appears only inside HLSL, where `mouse` is a `float4`).

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
