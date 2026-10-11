# lactea-compare

Fidelity tooling for lactea: dump reader/writer (layout in `lactea_abi::dump`), image
metrics, variable diffs, reports and the `fdcompare` CLI. Everything here is measured against
the projectM oracle's dumps — never eyeballed.

## Dump conventions

```
<dir>/meta.json                DumpMeta (lactea_abi::dump)
<dir>/<stage>/<frame:05>.png   RGBA8, row 0 = top
<dir>/vars/<frame:05>.json     {"name": number, ...}
<dir>/textures/<name>.png|.bin
```

### NaN / Inf in `vars/*.json` (producer contract)

JSON has no NaN/Inf. **Writers (oracle, engine) encode non-finite values as the strings
`"nan"`, `"inf"`, `"-inf"`**; finite values are plain JSON numbers (shortest round-trip form,
so f64 is lossless). `DumpWriter::write_vars` does exactly this.

Readers (`parse_vars`) additionally accept `null` (= NaN), any-case strings
(`"NaN"`, `"Infinity"`), and — as a safety net for C++ `printf`-style writers — bare
`nan` / `-nan` / `inf` / `-inf` / `Infinity` tokens outside strings. Booleans read as 0/1.
Variable names are lowercase; `rand_frame.x` style names are plain keys.

`meta.json` is optional for reading: a missing/non-matching file yields `meta() == None`
plus `meta_error()`, and a warning in the report. Stage directories are discovered by
scanning for `NNNNN.png`, so partial dumps (e.g. only `final`) work.

## Library API

* `DumpWriter::create(dir, DumpMeta)`, `write_stage(stage, frame, w, h, &rgba8)`,
  `write_vars(frame, &BTreeMap<String,f64>)`, `write_texture(name, w, h, &rgba8)`,
  `write_texture_bin(name, bytes)` (all `&self`, thread-shareable).
* `DumpReader::open(dir)` → `meta()`, `stages()`, `frames(stage)`, `var_frames()`,
  lazy `load_stage(stage, frame) -> Image`, `load_vars(frame)`, `load_texture(name)`.
* `compare_images(&a, &b, MetricOpts) -> ImageMetrics`, `ssim_luma`, `psnr_from_mse`,
  `diff_heatmap`, `triptych(ref, cand, amp)`, `contact_sheet(&[(frame, Image)], cols, thumb_w)`.
* `VarsAccumulator` / `vars::diff_sequences` → `VarsDiff`.
* `run_diff(ref, cand, out, &DiffOptions) -> Report`, `write_report`, `corpus::run_corpus`.

## Metrics (units: 8-bit code values 0..255)

| metric | definition |
|---|---|
| `max_abs` | max \|a-b\| per R, G, B (alpha excluded; `max_abs_alpha` reported separately) |
| `mean_abs`, `rmse` | over all pixels × RGB channels (`--alpha` adds A as 4th channel) |
| `psnr` | `10 log10(255² / MSE)`; **`inf` when identical** (JSON string `"inf"`) |
| `ssim` | mean SSIM of Rec.601 luma, 11×11 Gaussian σ=1.5 (normalised), K1=0.01, K2=0.03, L=255, "valid" region (no border padding); verified against an independent numpy/scipy implementation to 1e-9 |
| `frac_gt2/8/32` | fraction of pixels whose any-channel \|diff\| exceeds 2 / 8 / 32 |

Non-finite floats in `report.json` follow the same `"nan"` / `"inf"` / `"-inf"` string rule.
`psnr_mean` in summaries caps each frame at 100 dB so identical frames don't make it `inf`;
`psnr_min` / `psnr_median` are uncapped.

Variable diffs: `abs = |a-b|`, `rel = abs/max(|a|,|b|)`; a value diverges when
`abs > 1e-9 + 1e-6·max(|a|,|b|)` (`--var-abs-tol`, `--var-rel-tol`); NaN vs NaN is equal,
NaN vs number diverges with `abs = inf`.

Heat-map colour = `sqrt(max-channel |diff| / 255)` through black→blue→cyan→yellow→red→white
(any non-zero diff is visible; zero is pure black). Triptych = `ref | cand | |diff|×8`.

## CLI

```
fdcompare diff   REF CAND --out REPORT [--stages final,main] [--frames A..B] \
                 [--gate psnr=40,ssim=0.98] [--images worst|all|none] [--worst N] [--jobs N]
fdcompare step   REF CAND --out REPORT   # same, report.json has "mode": "single_step"
fdcompare corpus REF_ROOT CAND_ROOT --out REPORT [--baseline old/corpus.json] [--filter TEXT] [--strict]
fdcompare sheet  DIR --stage final --out sheet.png [--every N] [--cols C] [--thumb-width 160]
fdcompare vars   REF CAND [--top 20] [--all] [--frames A..B]
```

Frame ranges: `A..B` half-open, `A..=B` inclusive, `A..`, `..B`, or a single `N`.

Exit codes: `0` ok · `1` a gate failed / structural problem (frames missing in candidate,
size mismatch, requested stage absent) / corpus regression (or any failing preset with
`--strict`) · `2` usage or IO error. Without `--gate`, `diff` only fails on structural problems.

`diff`/`step` write `REPORT/report.json` (per stage per frame metrics, per-stage summary with
min/median/mean PSNR and SSIM, `first_frame_below_gate`, `worst_frames`, var diffs, warnings,
`passed`/`failures`), `REPORT/summary.md`, and `REPORT/images/<stage>/<frame>_{triptych,heat}.png`
for the worst `--worst N` (default 5) frames per stage (`--images all` = every differing frame).
`step` is the teacher-forced single-step mode (architecture rung 3): the layout is identical, the
mode flag is what tells downstream tooling that frame n was rendered from the reference's n-1
state.

`corpus` discovers dump directories (dirs with `meta.json` or a stage subdir, up to 4 levels
deep) under both roots, matches them by relative path and writes `REPORT/corpus.json`,
`REPORT/index.html` (thumbnails of each preset's worst frame, linked to the full triptych and
per-preset `summary.md`/`report.json`; worst presets first), `REPORT/summary.md` and
`REPORT/presets/<id>/…`. Default gate: `psnr=40,ssim=0.98`; default 1 worst image per stage.
Statuses: `pass`, `fail`, `error` (e.g. no common stage), `missing_cand`, `missing_ref`.
With `--baseline`, regressions are: pass→non-pass, min PSNR drop > 0.5 dB
(`--regress-psnr`, PSNR capped at 100), min SSIM drop > 0.005 (`--regress-ssim`),
earlier first diverging frame, and disappeared presets/stages.

Performance: frames are loaded on demand, `--jobs` workers (default all cores) each hold only
their in-flight pair; `corpus` parallelises across presets. Demo below: 240 image pairs of
640×360 (PSNR+SSIM, 2 stages) in 0.48 s wall on 20 threads.

## Demo

```sh
cargo run -p lactea-compare --release --example gen_demo -- /tmp/fd-demo 120   # synthetic ref/cand
cargo run -p lactea-compare --release -- diff /tmp/fd-demo/ref /tmp/fd-demo/cand \
    --out /tmp/fd-demo/report --gate psnr=40,ssim=0.98
```

```
FAIL: /tmp/fd-demo/ref vs /tmp/fd-demo/cand [free run]
stage    frames   PSNRmin   PSNRmed  PSNRmean  SSIMmin  SSIMmed  1stbad status
final       120     21.40     36.99     58.31   0.9693   0.9963      56   FAIL
main        120     31.65     47.15     64.12   0.9953   0.9987      75   FAIL
vars: 1 diverging of 3 (first at frame 50)
failure: final: 64 frames below gate [psnr>=40, ssim>=0.98], first at frame 56, min PSNR 21.40 dB, min SSIM 0.9693
failure: main: 45 frames below gate [psnr>=40, ssim>=0.98], first at frame 75, min PSNR 31.65 dB, min SSIM 0.9953
report: /tmp/fd-demo/report/report.json
(exit code 1)
```

```sh
fdcompare vars /tmp/fd-demo/ref /tmp/fd-demo/cand --top 5
fdcompare sheet /tmp/fd-demo/ref --stage final --every 15 --out sheet.png
fdcompare corpus ROOT_REF ROOT_CAND --out corp --baseline previous/corpus.json
```

```
120 frames compared; 3 vars compared, 1 diverging (first diverging frame: 50)
var                            1st div  div/frames        max abs (frame)        max rel (frame)
q1                                  50      70/120         5.917e-4 (119)         1.989e-5 (119)
```

(`bass` is NaN on both sides at some frames and is correctly treated as equal.)

## Tests

`CARGO_TARGET_DIR=target-compare cargo test -p lactea-compare` — unit tests (analytic PSNR,
SSIM closed form for constant images and a numpy/scipy reference for a textured pair, dump
round trip incl. NaN/±Inf vars, tolerant var parsing, var diffs, gating, corpus + baseline
regressions) and `tests/cli.rs` (CLI smoke test on tiny generated dumps).
