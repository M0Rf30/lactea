# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Breaking

- **lactea:** `Engine::new` returns `Result<Engine, EngineError>` (it sanitises the config: mesh clamp, size and fps of at least 1); `Engine::resize` returns `Result<(), EngineError>` and `Engine::set_mesh_size` `Result<[u32; 2], EngineError>`, both keep the old size on failure
- **lactea-render:** `Renderer::new`, `resize` and `set_mesh_size` return `RenderError::Invalid` for a size beyond the device's `max_texture_dimension_2d` or a mesh beyond 300 cells
- **lactea-render:** `noise::generate` and `gen_noise_vol` take `&CompatRules`; `CompatRules` gains `max_rand_slot`, `rand_filter_raw_name`, `noisevol_row_stride`, `shader_time_wrap`, `md2_shader_inputs`, `md2_input_vars` (the struct is `#[non_exhaustive]`)
- **lactea-capi:** `LacteaCompat` is an `int32_t` with `LACTEA_COMPAT_{PROJECTM,MILKDROP2,MILKDROP3,AUTO}` constants instead of an enum; an unknown value makes `lactea_create` return NULL; `lactea_set_fps(0)` and a non-finite `preset_duration` are `InvalidArgument`
- **lactea-preset:** `ParseError` gains `TooLarge`, `BinaryData` and `NoKeys`; `Flavor::ProjectM` now rejects presets over 1 MiB, with a NUL byte, or without a key line, like projectM (the MilkDrop 2 flavour is unchanged)
- **lactea-jpeg:** the `image` fallback for JPEGs stb rejects is gone (the `jpeg` feature of the C API and the CLI is removed); such files now fail to decode, as in projectM
- **lactea-hlsl:** `Options` gains `rand_max_slot` and `rand_strict_name`
- **lactea-render, lactea-app, lactea-capi:** wgpu is built without default features (`std`, `parking_lot`, `wgsl` plus one backend per platform); the OpenGL ES / WebGL backend is the opt-in `gles` feature of `lactea-render`, `lactea-app` and `lactea-capi`
- **lactea-abi:** `serde` is an optional feature (enabled by `lactea-cli` and `lactea-compare`)
- **web example:** `resize()` returns `Result<(), JsValue>`

### Added

- **sprites:** projectM user sprites: `Engine::{sprite_create, sprite_destroy, sprite_destroy_all, sprite_count, sprite_ids, set_max_sprites, max_sprites, sprite_var, set_sprite_var}`, `lactea_preset::parse_user_sprite` and the C API `lactea_sprite_*`; during a soft cut they are drawn after the blend onto the output and `burn` goes into both presets
- **playlist:** `lactea_playlist_*`, a C API for every `projectm_playlist_*` call (a connected playlist loads files and answers the engine's switch requests); `lactea_host::PlaylistFs::{add_path, insert_path}` and `scan_presets`, `.prjm` accepted wherever presets are scanned
- **api:** `Engine::burn_texture` / `lactea_burn_texture_rgba`, `Engine::reload_textures` / `lactea_reset_textures`, `lactea::VCS_VERSION` / `lactea_vcs_version`, `lactea_set_log_callback` / `lactea_set_log_level` (a `log` facade bridge), `lactea_set_texture_load_callback`
- **md2:** BeatDrop inputs under `Compat::MilkDrop2`: `mousex mousey mousedown mouseclick`, `ctrllt ctrlrt ctrldn ctrlup` and `year .. totalseconds` expression variables, shader `_c14` / `_c15` / `_c16`, `Engine::{set_date_time, clear_date_time, set_ctrl_keys, monitor}` and their C exports, and BeatDrop's shader FFT (`lactea_audio::BeatDropShaderFft`, `sampler_fft` / `sampler_wave`, `get_fft*`, `get_wave`)
- **engine:** `PresetControl::preset_duration_sampled` and `Engine::set_loaded_preset_duration`: `progress` runs over the sampled display time once a host reports it (the C API and the app do)
- **hlsl:** `wrapped_hlsl(kind, user, md3, projectm)` and `rand_slot`
- **eel:** `Context::try_run`
- **host:** new crate `lactea-host` (directory textures with an optional decode cache, image / media / WAV decode, GPU setup, preset scanning) shared by the C API, the CLI and the app; new unpublished crate `lactea-testutil` (shared GPU test helpers)
- **render:** `readback_rgba8` is public

### Changed

- **render:** projectM-flavour shaders write alpha 1.0 (`MilkdropShader.cpp:468`); the `noisevol_*` textures use projectM's `z * size` slice stride (`noisevol_row_stride`); `_c2.x` is the raw time under MilkDrop 2 (projectM and MilkDrop 3 wrap it modulo 10000, `shader_time_wrap`)
- **engine:** one `randNN` parser for samplers and `texsize_randNN`, slot range 15 (projectM) / 99 (MilkDrop 2); the pool prefix filter reads the raw sampler name in projectM mode (a quality prefix defeats it) and the peeled name in MilkDrop 2
- **engine:** the first rendered frame has `dt = 0`; `set_fps(0)` is ignored; a transition of duration 0 reports progress 1
- **audio:** non-finite PCM samples read as 0 (MD3: ±inf saturates, NaN is 0) and a non-finite volume sensitivity is ignored
- **app:** the preset browser runs on `Playlist` and `PresetControl` (the app's Random can repeat the current preset, as `Playlist`'s pick does; seeded display-time draws differ from before); textures come from `lactea_host::DirTextures` in projectM scan order (directory order, then path) and `.dds` files are indexed; `--duration`, `--transition-duration` and `--duration-rand` must be finite and at most 1e9 s; `ResetRequired` rebuilds the audio decoder from the new default track; symphonia is built with an explicit format list
- **jpeg:** decoding is bounded to 2 GiB, allocations are fallible, and a frame without a scan is an error
- **wav:** a short `fmt` chunk is an error
- **perf:** release builds use fat LTO and strip symbols and wgpu keeps one backend per platform (the `fdrender` release binary shrinks from 87 MB to 7.3 MB); per-frame wave, overlay, motion-vector, border and vertex buffers are reused, blit and blur bind groups are cached, and the MD3 spectrum FFT is cached
- **refactor:** `lactea-render`'s `renderer.rs` is split into `renderer/{mod, pipelines, uv_readback, orchestrate, passes, fixed, burn}.rs`; the MD2 and MD3 blend patterns share `gen_plasma` (`engine/blend_patterns.rs`); wave, shader-snippet and sprite-fragment helpers are shared

### Fixed

- **engine:** `Engine::new` no longer panics on a zero size or a tiny mesh (an MD2 soft cut with mesh `[3, 5]` used to)
- **eel:** parse paths no longer panic; `Context::run` returns 0 for a foreign program and `Context::get` / `set`, `GlobalMemory::reg` / `set_reg` ignore foreign variables and out-of-range registers; `megabuf` / `gmegabuf` blocks are allocated on first write
- **render:** a zero-width `volume_alpha_mod` is a step, not NaN; the MD2 main wave and its raw geometry return nothing for fewer than 576 samples; the MD2 shape border gate is `> 0`; byte-length and read-back arithmetic uses `usize` / `u64`
- **preset:** `SpriteName=` is read after its own key, not after the first `=`
- **capi:** the read-back buffer is released on every error path; the `lactea_set_texture_dir` extension list and the `LacteaConfig::compat` documentation are corrected
- **sprites:** a user sprite over a soft cut is no longer distorted by the transition

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
