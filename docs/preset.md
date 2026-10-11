# lactea-preset: MD2 import semantics reproduced

Port of `state.cpp`: `_GetLineByName`/`GetFast*`, `ReadCode`, `CState::Default`, `CState::Import`
(STATE_ALL), `CWave::Import`, `CShape::Import`. API: `Preset::parse(&[u8]) -> Result<Preset,_>`,
`Preset::to_milk() -> String`, helpers `code_lines`, `code_to_source`,
`strip_linefeeds_and_comments`.

Result on `/usr/share/projectM/presets/**/*.milk`: 4189 files parse (3451 version<200, 3 v200,
735 v201; 738 with shader text), all 4189 satisfy `parse(to_milk(p)) == p` and
`to_milk` idempotent.

## Quirks reproduced (each has a test in `tests/quirks.rs`)

1. **No INI parsing.** One scan over the file; every line `name<sep>rest` (sep = first `=` or
   space; names cut at 124 chars) is an entry. `[preset00]`/other sections are ignored, keys are
   found anywhere in the file. Keys are case-sensitive. Lines without `=`/space (blank, section,
   comments) produce no entry.
2. **Lookup cursor.** A lookup first tries the line after the previous hit, then searches from
   the top. So for duplicated keys the first copy wins *unless* a copy sits right after the line
   of the previously looked-up key. We replay MD2's exact lookup order (version, general, wave
   block, 16 waves, 16 shapes, motion, code, warp, comp) so this behaves identically.
3. **Scan truncation.** During the scan each line is consumed with `fgets(252)`: only 251
   chars are skipped; the rest of a longer line is scanned as new "lines" (bogus entries; a tail
   like `zoom=3` becomes a real entry). The value itself is later re-read in full (<= 252 chars for
   numbers, <= 32764 for code).
4. **0xFF byte == EOF** (signed `char`): at a line start/inside a name it ends the scan; inside a
   value it truncates the value. Embedded NUL truncates (C string).
5. Values end at `\r` or `\n`. `\r\n`, `\n` fine; **bare-CR files** are one giant line (only the
   first key is found). A UTF-8 BOM sticks to the first line's key (harmless when that is
   `[preset00]`; otherwise that key is lost).
6. **sscanf semantics** (`cnum.rs`): `%d`: leading whitespace, sign, digits, trailing junk
   ignored (`"3.9"` -> 3), no digits -> default, overflow truncated to 32 bit like glibc. `%f`:
   `.5`, `5.`, exponents, `inf`/`nan`, hex floats; a dangling exponent (`1e`, `2e+`) is a
   glibc matching failure -> default. `zoom = 1.5` (space before `=`) reads value `= 1.5` ->
   default. Booleans are `int != 0`.
7. **`wave_r/g/b/x/y` fallback is `m_fRot` (= 0.0)**, not Default()'s 1/1/1/.5/.5 (copy-paste slip
   in `Import`). `BaseValues::default()` keeps the Default() numbers; `parse` of a file without
   the keys yields 0.
8. **`mv_a` fallback** comes from the legacy `bMotionVectorsOn` key (missing -> 0.0), not from
   Default()'s 1.0. `bMotionVectorsOn` is consumed and never re-emitted (`mv_a` always is).
9. **Versions:** `MILKDROP_PRESET_VERSION` default 100; `<200`: warp/comp PS version 0 (shader
   text, if present, is still stored); `==200`: both = `PSVERSION` (default 2); `>=201`:
   `PSVERSION_WARP`/`PSVERSION_COMP` (default 2 each). Enum values 0/2/3/4/5 = none/ps_2_0/ps_2_a/
   ps_3_0/ps_4_0 (`psversion` module). `max/min_ps_version()` as `m_nMax/MinPSVersion`. (The
   `m_IsAMD` override is not modelled.) `CUR_MILKDROP_PRESET_VERSION` = 201 is what the serializer
   writes only if the model says so (it round-trips the parsed version).
10. **ReadCode:** `prefixN` for N = 1,2,... until the first missing key (so `per_pixel_2` without
    `_1` is nothing; a gap ends the block); each line is stored followed by `\u{1}`
    (`LINEFEED_CONTROL_CHAR`); one leading backtick is stripped (`` `x`` -> `x`, ```` ``x```` ->
    `` `x``); a line whose value is literally `~!@#$` ends the block; the block also ends when
    the joined length would exceed the 32768-byte buffer. Empty lines (`per_frame_2=`) are kept.
    `per_frame_init_N`, `per_frame_N`, `per_pixel_N` are distinct prefixes.
11. **Custom waves / shapes:** 16 each (`MAX_CUSTOM_*`), keys `wavecode_<i>_*`, `shapecode_<i>_*`
    (`num_inst` -> `instances`), code `wave_<i>_{init,per_frame,per_point}<n>`,
    `shape_<i>_{init,per_frame}<n>`. Index 16+ are not read (end up in `extra`). Every wave/shape
    keeps its Default() values when keys are missing; enabled is a plain int.
12. **Shaders:** `warp_<n>` / `comp_<n>` text via ReadCode; `None` when no `warp_1`/`comp_1`
    exists (MD2 then synthesises the default shader with `GenWarp/CompPShaderText` from decay,
    tex-wrap, gamma, echo, etc. — that belongs to the HLSL slice; the needed values are in
    `BaseValues`).
13. Non-UTF-8 bytes: value decoded as UTF-8 if valid, else Latin-1 per code block/key/value.

## Not MD2 behaviour (ours)

* `Preset::parse` returns `Err` only for empty input and UTF-16 BOM (MD2 would silently use all
  defaults).
* `extra` holds every *other* `key=value` line (sane line splitting, file order, duplicates
  kept, section headers dropped). Consumed keys, including duplicate copies of them and
  `PSVERSION*`, are not kept.
* `to_milk` writes floats with Rust's shortest round-trip format (not MD2's `%.3f`), emits only
  waves/shapes that differ from their defaults, writes shader lines with a leading backtick
  (as MD2's exporter does) and `extra` last. It does not write the BeatDrop banner lines.
  Hand-built presets with `warp_ps_version != comp_ps_version` and version 200 cannot round-trip
  (the file format has one `PSVERSION` there).
* Not ported: `StartBlendFrom`, the per-state `Export` conditions (`m_nMaxPSVersion>0` header,
  only-enabled customs), `m_szDesc` (file name), `RandomizePresetVars`, expression compile.
