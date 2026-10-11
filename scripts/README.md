# scripts

Python 3 (stdlib plus numpy for the image tools) and shell helpers for the fidelity loop; they work
under `out/` (gitignored) and need the reference build from `oracle/build.sh`. Method:
[`docs/fidelity.md`](../docs/fidelity.md); whole-corpus runs: [`docs/corpus.md`](../docs/corpus.md).

Primary:

| tool | purpose |
|---|---|
| `fidelity.sh`, `fidelity_summary.py` | free + teacher-forced runs of a reference set and its summary |
| `fdq.py` | quick per-stage PSNR probe of one preset |
| `fdregress.py` | compare two corpus reports (per-stage PSNR regressions) |
| `corpus_run.py`, `corpus_report.py`, `corpus_sample.py` | whole-corpus run, bucketed report, seeded sample |
| `release_package.sh` | packages a release archive (used by the release workflow) |
| `probes/gen_*.py` | regenerate the one-feature probe presets and lists in `probes/` |
