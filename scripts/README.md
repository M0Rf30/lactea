# scripts

Python 3 (stdlib plus numpy for the image tools) and shell helpers for the fidelity loop; they work
under `out/` (gitignored) and need the reference build from `oracle/build.sh`. Method:
[`docs/fidelity.md`](../docs/fidelity.md); whole-corpus runs: [`docs/corpus.md`](../docs/corpus.md).

Primary:

| tool | purpose |
|---|---|
| `fidelity.sh`, `fidelity_summary.py` | free + teacher-forced runs of a reference set and its summary |
| `fdq.py` | quick per-stage PSNR probe of one preset |
| `fdregress.py`, `fdfails.py` | compare two corpus reports (regressions, pass counts) |
| `corpus_run.py`, `corpus_report.py`, `corpus_sample.py`, `corpus_robust.py` | whole-corpus run, bucketed report, seeded sample, crash sweep |
| `release_package.sh` | packages a release archive (used by the release workflow) |

Development helpers: `fddiff.py`, `fdlook.py` (side-by-side images), `fdexact.py`, `fdexact_corpus.py`,
`fdstroke.py`, `fdalpha.py`, `fdtransition.py`, `mwcmp.py`, `pick_shader_set.py`. `probes/` holds generators
for one-feature probe presets.
