# AGENTS.md — working on Lactea

Lactea is a pure-Rust MilkDrop 2 / projectM-compatible visualizer on wgpu. Rendering changes are
justified by numbers against projectM, never by eye: the method, gate and current results are in
[`docs/fidelity.md`](docs/fidelity.md); `CONTRIBUTING.md` has the workflow.

## Read first
- `docs/ARCHITECTURE.md`: crates, contracts, determinism rules.
- `docs/divergences.md`: every projectM vs MilkDrop 2 difference and which one we follow.
- `docs/render.md`, `docs/engine.md`, `docs/corpus.md`, `oracle/README.md`.

## Ground rules
- **projectM is the reference.** Default behaviour matches projectM; MilkDrop 2 behaviour stays
  reachable via `Compat::MilkDrop2` / a `CompatRules` field (`crates/lactea-render/src/config.rs`).
  A new divergence needs a new rule field (both variants) and a row in `docs/divergences.md`.
- **Port, don't approximate.** Semantics come from `third_party/md2/` (BSD-3). projectM
  (`third_party/projectm`, LGPL) may be read to learn behaviour, never transliterated.
- **`crates/lactea-abi` is a frozen contract.** Changing it is a cross-crate change in one commit.
- Library crates take bytes and a host wgpu `Device`/`Queue`: no windows, audio devices,
  filesystem or threads in their core API; no panics on caller input (return errors).
- Internal images are `Rgba8Unorm`, never sRGB. Determinism: frame n time = n/fps, PCM per
  `abi::pcm_range`, every RNG seeded from the run seed.
- Comments say why or state an invariant; no history of how the code got here.

## Before you commit
- `cargo fmt --all`, `cargo clippy --workspace --all-targets --exclude lactea-app --exclude lactea-eel-oracle -- -D warnings -A clippy::approx_constant`,
  `cargo test --workspace --exclude lactea-app` green; one regression test per root cause.
- Conventional commit messages (`feat(render): ...`), see `CONTRIBUTING.md`.
- The fidelity loop is slow: run it when a change is meant to alter rendering or when asked.
  Refactors are verified with tests, clippy and cheap equivalence checks (e.g. hashing outputs of
  pure functions). When it is run, report before/after numbers and update `docs/fidelity.md`.

## Resource limits
- Constrained machines: `CARGO_BUILD_JOBS=2`, one `CARGO_TARGET_DIR` per worktree, `JOBS`/`FDQ_JOBS` ≤ 2-3.
- One render/compare loop at a time. Parallel agents work in separate git worktrees
  (`../lactea-wt/<slice>`, symlink `out/ref`, `oracle/build`, `third_party/*`, fixtures).
- `out/` and `third_party/projectm*` are gitignored and large; a full corpus run needs about 25 GB.
