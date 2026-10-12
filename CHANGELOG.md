# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2] - 2026-10-12

### Added

- Add MilkDrop 3 support (Compat::MilkDrop3)
- Auto compat, md3 hardening, perf, fidelity and ux improvements
- **md3:** Compute the _mxd3q* audio-envelope / beat-latch uniforms

### Fixed

- **lactea:** Fall back to milkdrop 2 when projectm cannot translate a shader


## [0.1.2] - 2026-10-12

### Added

- Add MilkDrop 3 support (Compat::MilkDrop3)
- Auto compat, md3 hardening, perf, fidelity and ux improvements
- **md3:** Compute the _mxd3q* audio-envelope / beat-latch uniforms


## [0.1.2] - 2026-10-12

### Added

- Add MilkDrop 3 support (Compat::MilkDrop3)


## [0.1.2] - 2026-10-12

### Added

- Add MilkDrop 3 support (Compat::MilkDrop3)
- Auto compat, md3 hardening, perf, fidelity and ux improvements
- **md3:** Compute the _mxd3q* audio-envelope / beat-latch uniforms


## [0.1.2] - 2026-10-12

### Added

- Add MilkDrop 3 support (Compat::MilkDrop3)
- Auto compat, md3 hardening, perf, fidelity and ux improvements


## [0.1.2] - 2026-10-12

### Added

- Add MilkDrop 3 support (Compat::MilkDrop3)
- Auto compat, md3 hardening, perf, fidelity and ux improvements
- **md3:** Compute the _mxd3q* audio-envelope / beat-latch uniforms


## [0.1.2] - 2026-10-12

### Added

- Add MilkDrop 3 support (Compat::MilkDrop3)
- Auto compat, md3 hardening, perf, fidelity and ux improvements


## [0.1.2] - 2026-10-12

### Added

- Add MilkDrop 3 support (Compat::MilkDrop3)
- **md3:** Compute the _mxd3q* audio-envelope / beat-latch uniforms


### Added

- `lactea`: the MilkDrop 3 `_mxd3qxa.._d` / `_mxd3qsa.._c` audio-envelope / beat-latch shader uniforms (and the
  `MXD3QX1..16` / `MXD3QS1..12` prelude macros) are computed per frame and uploaded instead of being undeclared.
- `lactea` app: `--shader-override-warp` / `--shader-override-comp HANDLE=FILE`, as in `fdrender`.

## [0.1.1] - 2026-10-12

### Fixed

- Release: the Windows archive is built again (the 0.1.0 Windows job produced no zip, so no GitHub release was published).

## [0.1.0] - 2026-10-11

First public release.

### Added

- `lactea`: embeddable MilkDrop 2 / projectM-compatible engine rendering into any host-provided wgpu
  texture, with MilkDrop 2 (`Compat::MilkDrop2`) or projectM (`Compat::ProjectM`) semantics, soft and
  hard preset transitions, playlist and preset-switch control, runtime parameters, WebAssembly support.
- `lactea-render`: wgpu port of the MilkDrop 2 frame pipeline (warp mesh, blur chain, waves, shapes,
  borders, motion vectors, composite) with exact GL line and point rasterisation.
- `lactea-preset`: `.milk` parser and serialiser with MilkDrop 2 quirks.
- `lactea-eel`: ns-eel2 / projectm-eval compatible expression language, bitwise equal to projectm-eval
  and faster.
- `lactea-hlsl`: MilkDrop HLSL (ps_2/ps_3 plus `include.fx`) to WGSL translator.
- `lactea-audio`: bass/mid/treb, attenuated values, waveform and spectrum analysis.
- `lactea-jpeg`: JPEG decoder reproducing `stb_image` byte for byte.
- `lactea-abi`: shared shader-constant, binding and dump-layout contract.
- `lactea-compare`: dump reader/writer, PSNR/SSIM and variable diffs, reports and the `fdcompare` CLI.
- `lactea-capi`: C ABI with a cbindgen header, read-back rendering and Vulkan image interop.
- `lactea-app`: standalone visualizer with PipeWire/cpal capture, file playback and preset browser.
- Fidelity tooling: instrumented projectM reference build (`oracle/`), `fdrender`, corpus runner and
  reports; method and numbers in `docs/fidelity.md`.
- Release pipeline: binaries for Linux, Windows and macOS; crates.io publishing with release-plz.

[Unreleased]: https://github.com/M0Rf30/lactea/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/M0Rf30/lactea/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/M0Rf30/lactea/releases/tag/v0.1.0
