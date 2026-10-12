# projectM feature parity

Audit of Lactea against **projectM 4.2** (`third_party/projectm`: public headers
`src/api/include/projectM-4/*.h`, playlist library `src/playlist/`, runtime `src/libprojectM/`) and
against the MilkDrop 2 / BeatDrop sources (`third_party/md2`) for the features projectM does not have.
Every row says where the behaviour is defined, what Lactea does today, and the plan.

Rendering fidelity is a separate track (`docs/fidelity.md`); this file is about *features*.
Preset **transitions** (soft cuts, hard-cut blending, preset blending, `start_clean`) belong to the
transitions slice and are only listed as "pending" here.

Status words: **Done** (already there), **Added** (implemented in this change), **Equivalent** (same
capability, Rust/wgpu-shaped API), **Missing**, **Upstream no-op** (projectM declares it but its body is
an empty stub, so there is nothing to match), **Pending** (transitions slice), **N/A** (OpenGL- or
C-memory-specific).

Reading the Lactea column: `Engine::…` is `crates/lactea/src/engine.rs` (+ `params.rs`), `PresetControl`
is `crates/lactea/src/control.rs`, `Playlist` is `crates/lactea/src/playlist.rs`, `capi` is
`crates/lactea-capi/src/lib.rs`.

## 1. Core and lifecycle (`core.h`, `ProjectM.cpp`)

| feature | projectM | Lactea | plan |
|---|---|---|---|
| create / destroy an instance | `core.h:60`, `:88` | **Equivalent**: `Engine::new(&device, &queue, cfg)`, `lactea_create/destroy` | – |
| create with a GL loader | `core.h:78` | **N/A**: the host hands over a wgpu `Device`/`Queue` | – |
| load a preset from memory | `core.h:125`, `ProjectM.cpp:76` | Implemented: `Engine::load_preset(bytes, name)`; failure leaves the previous preset running | – |
| load a preset from a path / `file://` | `core.h:108`, `PresetFactoryManager.hpp:64` | **Equivalent**: libraries never touch the filesystem (rule in `AGENTS.md`); the app/CLI read the file | hosts resolve URLs |
| `idle://` + idle preset when nothing is loaded | `ProjectM.cpp:153-162, 290`, `IdlePreset.cpp:11-193` | **Added**: `Engine::load_idle_preset()` (`IDLE_PRESET`, `lactea_load_idle_preset`). Opt-in: an engine without a loaded preset renders no preset at all instead of switching to the idle one automatically. The preset is original, not a copy of projectM's HDR "M" logo | – |
| smooth-transition flag of the load calls | `core.h:108,125`, `ProjectM.cpp:62-88,303` | Implemented: `Engine::load_preset(.., Transition::{Cut,Soft})`; C: `lactea_load_preset_ex(.., transition_seconds)` (`0` = cut, `> 0` = soft cut over that many seconds; the old `lactea_load_preset` is the cut) | – |
| `projectm_reset_textures` | `core.h:137`, `ProjectM.cpp:100` | **Missing**: textures are resolved from the `TextureProvider` (through the engine's decode cache, cleared by `set_texture_provider`) on every `load_preset`; there is no call to re-resolve the *running* preset's textures after the provider changed | `Engine::reload_textures()`: keep the shader interfaces and shape image names in `Loaded`, re-run `resolve_textures` |
| version components | `core.h:149` | **Added**: `lactea_version_components` | – |
| version string | `core.h:160` | Implemented: `lactea_version` (static string, nothing to free) | – |
| VCS revision string | `core.h:173` | **Missing** (the CLI dump writes the git sha into `meta.json`, the library does not expose it) | `build.rs` env var + `lactea_vcs_version` |
| string alloc/free helpers | `memory.h:43,56` | **N/A**: no API returns heap strings | – |
| `.prjm` preset extension | `MilkdropPreset/Factory.hpp:32` (`".milk .prjm"`) | **Missing** in the app's directory scan (`PresetBrowser::scan_dir` only takes `*.milk`); the parser itself is extension-agnostic | accept `.prjm` in `scan_dir` / `Playlist` hosts |

## 2. Parameters (`parameters.h`, `ProjectM.cpp`, `TimeKeeper.cpp`)

| feature | projectM | Lactea | plan |
|---|---|---|---|
| texture search paths | `parameters.h:46`, `ProjectM.cpp:90` | **Equivalent**: `TextureProvider` trait; app `--texture-dir`, `lactea_set_texture_dir` (affects presets loaded afterwards) | see `reset_textures` above |
| set frame time (`< 0` = system clock) | `parameters.h:70`, `TimeKeeper.cpp:29-48` | **Equivalent**: the host always passes `FrameClock.time`; there is no hidden system clock (determinism contract) | – |
| last frame time | `parameters.h:82` | **Added**: `Engine::last_frame_time()`, `lactea_last_frame_time` | – |
| beat sensitivity | `parameters.h:93`, `ProjectM.cpp:458-466` (clamp 0..2) | **Added**: `PresetControl::{set,}beat_sensitivity`. Stored only: projectM never reads the field anywhere else (`ProjectM.hpp:321` is referenced only by its accessors), so neither do we | – |
| hard cuts: enable / min display time / volume sensitivity, detection on beats | `parameters.h:115-162`, `ProjectM.cpp:134-150` (`vol - previous vol > sensitivity`, `frame > 50`, `CanHardCut`), `TimeKeeper.cpp:74` | **Added**: `PresetControl::update` returns `SwitchRequest { hard_cut: true }` (all of projectM's conditions; `hard_cut_duration()` getter returns the *truncated* integer like `ProjectM.cpp:486`). **Pending**: the instant cut/blend that follows is the host's `load_preset(.., Transition)` | – |
| soft cut duration | `parameters.h:174`, `ProjectM.cpp:473` | Implemented: stored in `PresetControl` (drives its smoothing state), the blend itself is `Transition::Soft`. C: `lactea_load_preset_ex` takes the duration per call; while that blend runs the control uses it, afterwards the `LacteaSwitchSettings::soft_cut_duration` setting is back | – |
| preset display duration | `parameters.h:194`, `TimeKeeper.cpp:94-102` | **Added**: `PresetControl::set_preset_duration` (request) and `Engine::set_preset_duration` (the `progress` variable's denominator). `progress` keeps using the nominal duration, not the easter-egg sample | feed the sampled duration to `progress` once the transitions slice settles `PresetClock` |
| mesh size (clamp 8..300, odd → even) | `parameters.h:219`, `ProjectM.cpp:557-576` | **Added**: `Engine::{set,}mesh_size`, `lactea_{set,get}_mesh_size`. Defaults differ: projectM 32×24, Lactea 48×36 (`RunConfig` contract; the oracle sets the size explicitly) | – |
| texel offset | `parameters.h:242` | **Added**: `Engine::{set,}texel_offset`, `lactea_{set,get}_texel_offset` | – |
| fps | `parameters.h:269` | **Added**: `Engine::{set_fps,fps}`, `lactea_{set,get}_fps`. Default 60 (projectM's header says 35 for the setter and 60 for the getter) | – |
| aspect correction | `parameters.h:292`, `ProjectM.cpp:535` | **Added** (stored only: `m_aspectCorrection` is never read, aspect handling in `GetRenderContext` `:624-627` is unconditional) | – |
| easter egg | `parameters.h:315`, `TimeKeeper.cpp:129-138` | **Added**: seeded gaussian display time, `max(1 s, N(duration, egg))`, exact below 0.001 | – |
| preset lock | `parameters.h:335`, `ProjectM.cpp:425-436` | **Added**: `PresetControl::set_preset_locked` (unlock re-arms the request, as in projectM). The app has its own `L` key/timer | – |
| window size get/set | `parameters.h:362,371` | **Equivalent**: `Engine::resize` + `Engine::window_size()`. (projectM skips rendering at size 0; Lactea asserts a non-zero size) | – |
| preset start clean | `parameters.h:387`, `ProjectM.cpp:321` | **Pending**: `EngineConfig::start_clean` in the transitions branch | – |
| "per-pixel mesh toggles" | none beyond the mesh size above | **N/A**: per-pixel code runs only when the preset has it, in both | – |
| MilkDrop 3 auto hard cut mode (not in projectM) | – (MD3 `AutoHardCutMode`, `docs/md3.md`) | **Added** (only `Compat::MilkDrop3`): `EngineConfig::auto_hard_cut_mode`, `Engine::{set,}auto_hard_cut_mode`, `Engine::take_auto_cuts` + `PresetControl::update_with_auto_cuts` (a hard-cut request); C: `LacteaConfig::auto_hard_cut_mode`, `lactea_{set,get}_auto_hard_cut_mode`, the request arrives through `lactea_poll_switch_request` / the switch-requested callback with `is_hard_cut = true`; `fdrender --auto-hard-cut N` counts the cuts (`auto_cuts`) | – |
| MilkDrop 3 display time (not in projectM) | – (MD3 `TimeBetweenPresetsRand`, `docs/md3.md`) | **Added** (only `Compat::MilkDrop3`): `PresetControl::set_md3_time_rand`; C: `LacteaSwitchSettings::preset_duration_rand` (default 10.0 s, stored but inert with the other compat modes) | – |

## 3. Callbacks and logging (`callbacks.h`, `logging.h`)

| feature | projectM | Lactea | plan |
|---|---|---|---|
| preset switch requested | `callbacks.h:43,72`, `ProjectM.cpp:54,134-150` | **Added**: `PresetControl::update(time, vol) -> Option<SwitchRequest>` and `PresetControl::set_switch_requested_callback` (a `callbacks::Hook`, fired synchronously from `update` with every request it returns); C: `lactea_poll_switch_request` and the callback `lactea_set_switch_requested_callback(engine, fn(is_hard_cut, user), user)` (runs inside `lactea_render*` after the frame is submitted; the request stays pollable). The app's own timer logs the event (`preset switch requested`, debug level); it does not own a `PresetControl` yet (see the browser row of §8) | – |
| preset switch failed | `callbacks.h:57,87`, `ProjectM.cpp:58,69-73` | **Added**: `Engine::set_preset_switch_failed_callback(Fn(&SwitchFailure{name, message}))`, fired from every failing `Engine::load_preset` / `load_idle_preset` (the call still returns `Err(EngineError)`, the previous preset keeps running); C: `LACTEA_STATUS_PRESET` + `lactea_last_error` and the callback `lactea_set_switch_failed_callback(engine, fn(preset_name, message, user), user)` (from `lactea_load_preset[_ex]` / `lactea_load_idle_preset`, implemented in `crates/lactea-capi/src/events.rs`); the app registers the engine callback (debug log) and keeps its skip-and-retry loop; `Playlist` raises its failure event after the retries. Tests: `failed_loads_raise_the_callback_and_keep_the_previous_preset`, `touch_stage_read_and_callbacks` (capi) | – |
| texture load callback | `callbacks.h:109-160`, `TextureManager.cpp:189-236` | **Equivalent**: `TextureProvider::load(name) -> Option<Image>` (raw pixels; the GL-texture-id path is N/A) | – |
| log callback + level | `logging.h:78,89`, `ProjectMCWrapper.cpp:565-587` | **Missing**: the engine crates emit no log records (shader fallbacks and warnings are returned in `PresetReport`); the app uses `log`/`RUST_LOG`; the C API has no log hook | `lactea_set_log_callback` + a `log::Log` bridge once the engine logs |

## 4. Audio (`audio.h`, `Audio/PCM.cpp`)

| feature | projectM | Lactea | plan |
|---|---|---|---|
| add float PCM | `audio.h:62`, `PCM.cpp:39` | Implemented: `Engine::push_pcm_f32`, `lactea_push_pcm_f32` | – |
| add int16 PCM | `audio.h:80`, `PCM.cpp:47` (`128·s/32768`) | **Added**: `Analyzer::push_pcm_i16`, `Engine::push_pcm_i16`, `lactea_push_pcm_i16` | – |
| add uint8 PCM | `audio.h:98`, `PCM.cpp:43` (`128·(s-128)/128`) | **Added**: `push_pcm_u8` (same three layers) | – |
| max samples | `audio.h:44`, `AudioConstants.hpp:8` (576) | **Added**: `lactea_audio::MAX_SAMPLES`, `Engine::pcm_max_samples()`, `lactea_pcm_max_samples` | – |
| mono / stereo / N channels | `audio.h:58-59` | Implemented: the channel argument is the interleave count; mono is duplicated, channels > 2 are ignored | – |

## 5. Touch (`touch.h`) — upstream no-op, mirrored

| feature | projectM | Lactea | plan |
|---|---|---|---|
| `projectm_touch`, `_touch_drag`, `_touch_destroy`, `_touch_destroy_all` | `touch.h:49-79`; the bodies are `// UNIMPLEMENTED` stubs (`ProjectM.cpp:595-613`); `projectm_touch_type` (`types.h:68-79`) names 9 waveform shapes that nothing draws | **Added, no-op by design**: `Engine::{touch, touch_drag, touch_destroy, touch_destroy_all}` + `TouchType` (the 9 values), C `lactea_touch{,_drag,_destroy,_destroy_all}` (touch type 0..=8 validated). projectM is the reference and draws **nothing**, so drawing waveforms would *diverge* from it. Verified against the oracle: `refrender --touch 'N:touch,x,y,pressure,type;N:drag,x,y,p;N:destroy,x,y;N:destroy_all'` calls the real `projectm_touch*` and `oracle/check_touch.sh` shows every PNG / vars / oracle.json byte-identical to the run without it (Elastoid, Memories Of The Castle, 40 frames); Lactea test `touch_is_projectms_no_op_and_debug_images_cover_every_stage` asserts the same for our frames | if projectM ever implements it, the oracle diff will show it and the waveforms land behind these entry points |

## 6. Rendering API and debugging (`render_opengl.h`, `debug.h`)

| feature | projectM | Lactea | plan |
|---|---|---|---|
| render a frame / into a given FBO | `render_opengl.h:40,49` | **Equivalent**: `Engine::render(&mut encoder, &target_view, clock)` renders into any target view of the configured format; C: `lactea_render[_to_buffer]` | – |
| render into a host-owned image (projectM: the host's GL FBO) | `render_opengl.h:49` | **Added** for Vulkan hosts: `lactea_create_vulkan` (the engine runs on the host's `VkDevice`, see `crates/lactea-capi/src/vulkan.rs`) + `lactea_render_to_vk_image(engine, VkImage, time, frame, wait)`; tested by `examples/c/vulkan.c` (a Vulkan host that reads the image back). Metal/D3D12 hosts: Rust `wgpu` only | – |
| burn a texture into the main image | `render_opengl.h:64`, `ProjectM.cpp:408-423` | **Missing** (`Renderer::inject_main`, feature `diagnostics`, only replaces the whole image from CPU bytes, for teacher forcing) | `Renderer::burn_texture(view, rect)`: one textured quad pass into the current main image (both presets during a transition) |
| write a debug image of the main texture | `debug.h:53`; body is `// UNIMPLEMENTED` (`ProjectMCWrapper.cpp:493`) | **Added** (pull style, no filesystem in the library): `Engine::debug_image(Stage)`, `debug_images()`, `debug_image_incoming(Stage)` return `DebugImage { stage, width, height, rgba }` for `Final`, `Main`, `Warped`, `Blur1..3` of the last submitted frame; C `lactea_read_stage(engine, stage 0..5, out, out_len, &w, &h)` (size query with `out = NULL`); app: key `D` writes `lactea-debug-<frame>-<stage>.png`; plus `Renderer::read_stage`, `fdrender --stages`, `lactea_render_to_buffer`. Blur stages hold data only when the preset samples blur or after `set_min_blur_levels(3)` | – |

## 7. User sprites (`user_sprites.h`, `UserSprites/`)

| feature | projectM | Lactea | plan |
|---|---|---|---|
| `projectm_sprite_create/_destroy/_destroy_all/_get_sprite_count/_get_sprite_ids/_set_max_sprites/_get_max_sprites/_get_var/_set_var` (the MilkDrop `imgNN` sprite sections: `img`, `init_N`, `code_N`; per-frame variables `time frame fps progress bass mid treb *_att` in, `done burn x y sx sy rot flipx flipy repeatx repeaty blendmode r g b a` out; limit 16; ids; `burn` draws into the preset's main image) | `user_sprites.h:55-146`, `ProjectM.cpp:363-406`, `UserSprites/MilkdropSprite.cpp:72-96,330-358`, `SpriteManager.cpp`; same feature in MD2 | **Missing** (no sprite code in `crates/`; the eel/preset infrastructure it needs exists) | new `lactea::sprites` module: parse an `imgNN` section with `lactea-preset`'s `Lookup`, one `lactea-eel` context per sprite, quads drawn after the composite (and `burn` into main) through a new `Renderer` pass; ids/limit/variables map 1:1 onto the C API |

## 8. Playlist library (`projectm-4-playlist`)

`crates/lactea/src/playlist.rs` is a pure-data port of the behaviour (no filesystem, no engine
access): the host passes a `load` closure to `play_*`, which is the equivalent of projectM's
preset-load event; Rust hosts get `Result`s instead of C callbacks.

| feature | projectM | Lactea | plan |
|---|---|---|---|
| create / connect / destroy | `playlist_core.h:57,69,95`, `PlaylistCWrapper.cpp:9-35` | **Equivalent**: `Playlist::new(seed)`; the host calls `PresetControl::update` and `play_next` itself | – |
| size / clear / items / item | `playlist_items.h:44-84` | **Added**: `len`, `clear`, `items`, `items_range`, `item` | – |
| add / insert preset(s), remove | `playlist_items.h:143-228`, `Playlist.cpp:49-89,154-181` | **Added**: `add_preset(s)`, `insert_preset(s)`, `remove_preset(s)`; duplicates, filter, history index fix-ups as in projectM | – |
| add / insert **path** (directory scan) | `playlist_items.h:103,126`, `Playlist.cpp:92-151` | **Missing** by design (no filesystem in library crates); the app has `scan_dir` | `Playlist::add_presets(scan_dir(..))` helper in the app |
| sort (full path / file name, asc / desc) | `playlist_items.h:252`, `Playlist.cpp:196-243` | **Added**: `sort` (stable; clears the history) | – |
| shuffle | `playlist_playback.h:44`, `Playlist.cpp:255-259` | **Added**: seeded (`StreamRng`), deterministic per seed | – |
| history (1000 entries, no consecutive duplicates) + `play_last` | `playlist_playback.h:146`, `Playlist.cpp:302-322,411-424` | **Added** | – |
| position / next / previous | `playlist_playback.h:88-127`, `Playlist.cpp:246-300,336-358` | **Added**: `position`, `set_position`, `play_next`, `play_previous`, `next_index` ... (empty list → 0) | – |
| filters (`+`/`-` globs, `**`, `?`, first match wins) + apply | `playlist_filter.h:75-101`, `Filter.cpp:19-156` | **Added**: own matcher built from the documented semantics (`Filter`, `Playlist::{set_filter,filter,apply_filter}`); not bug-for-bug with `Filter.cpp` for pathological patterns (`**` inside a name) | – |
| retry count (default 500) + failure event | `playlist_playback.h:63`, `PlaylistCWrapper.cpp:113-192` | **Added**: failed items are removed and the walk continues in the direction of travel; the failure event fires after `retry_count` failures. projectM's quirk that the item sliding into the freed slot is skipped is kept (`failed_presets_are_skipped_and_removed`) | – |
| events: switched / switch failed / preset load | `playlist_callbacks.h:49,69,105` | **Added**: `set_preset_switched_callback`, `set_preset_switch_failed_callback`, the `load` closure | – |
| string-array memory helpers | `playlist_memory.h:44,55` | **N/A** | – |
| C API (`projectm_playlist_*`) | `playlist*.h` | **Missing**: Rust only (the C handle has no playlist) | `lactea_playlist_*` over a `Playlist` + a host load callback |
| the app's own browser | – | `crates/lactea-app/src/presets.rs` keeps its own next/prev/random/history(256)/lock/timer logic; it has no filters, retry/removal, sorting, beat hard cuts or easter egg | migrate `PresetBrowser` onto `Playlist` + `PresetControl` |

## 9. Preset format and runtime

| feature | projectM / MD2 | Lactea | plan |
|---|---|---|---|
| shape `image=` key (`shapecode_N_image`) | projectM only: `CustomShape.cpp:59`, draw `:154-175` (texture by name through `TextureManager`, `fw_/fc_/pw_/pc_` sampler prefixes, `textureAspectY = 1`, missing texture → 1×1 placeholder) | Implemented (touches rendering, see below): `CustomShape::image`, `ShapeInstance::image`, `Kind::TexImage` draw path, provider lookup in `shape_images.rs` | – |
| main-wave modes beyond 15 | `WaveformMode.hpp:6-26`: 16 modes, the value wraps with `Count` | Implemented: `PM_WAVE_MODES = 16` (`wavegen.rs`); MD2 mode 0-8 under `Compat::MilkDrop2` | – |
| 4 custom waves / 4 shapes (projectM) vs 16 (MD2) | – | Implemented: `CompatRules::custom_slots` | – |
| every preset key projectM's MilkDrop loader reads | `MilkdropPreset/*.cpp` `Get*("key")` calls | Implemented: a script over those calls finds every key name in `lactea-preset` (after adding `image`, the only key it lacked) | – |
| built-in idle textures `idlem`, `idleheadphones` | `TextureManager.cpp:85-96` | **N/A**: our idle preset needs no textures | – |
| texture cache with age-based eviction | `TextureManager.cpp:115-176` (`PurgeTextures`) | **Missing**: the renderer drops all user textures at every load (`Renderer::reset_images`); providers may cache decodes (`lactea-app` does; the C API's directory provider does not and re-decodes on each `load`) | only worth it when blending keeps two presets' textures alive |
| shader cache | `Renderer/ShaderCache.cpp`: caches *built-in* GL programs by name | **Equivalent**: `Pipelines::new` builds the fixed pipelines once per renderer; preset shaders are compiled per load in both | – |
| DDS textures | listed in the extension set, decoded by stb (which cannot) | **N/A**: same | – |
| preset blending / transition shaders | `Renderer/PresetTransition.cpp`, `TransitionShaders/` | **Pending** (transitions slice) | – |
| `sampler_fft`, `sampler_wave` (R32F spectrum / waveform textures, `get_fft()`) | BeatDrop only: `md2/plugin.cpp:2404-2408, 3554-3565, 11630-11729`; projectM has none (nothing in `third_party/projectm` registers them) | **Not added for projectM / MilkDrop 2** (the sampler resolves to an unresolved user texture, so projectM-compat output matches the oracle). **Implemented under `Compat::MilkDrop3`**: `sampler_fft` is MD3's 512-bin smoothed left spectrum with `get_fft` / `get_fft_hz` (`md3_shader_fft`, `md3_shader_prelude`, `docs/md3.md` "Shader inputs"); `sampler_wave` is MD3's never-filled texture (the 1x1 black placeholder) | BeatDrop's own 1024-bin RMS-normalised variant stays unimplemented |
| `monitor`, `mousex/y/down/click`, `ctrllt/rt/dn/up` | BeatDrop only: `md2/state.cpp:328,349-356,402-409`, `milkdropfs.cpp:616,687-695`; projectM does not register them | **Missing** for projectM / MilkDrop 2: ordinary user variables (0). **`Compat::MilkDrop3`**: MD3 replaces them: `monitor2` / `monitor3` are plain persistent user variables, the mouse is a shader input (`_c14`, `mouse*`, `Engine::set_mouse`; `mouse.x` does not compile in MD3 preset code), `ctrl*` do not exist | `mousex/y/down/click`, `ctrl*`, `monitor` stay out of the projectM-compat dumps |
| date/time variables (`year … totalseconds`) | BeatDrop 1.5: `md2/state.h:432-433`, `milkdropfs.cpp:916-923`; not in projectM | **Missing** | same as above |
| title / text overlays, preset info | MD2/BeatDrop (`plugin.cpp` text engine); projectM 4 has no text rendering (`libprojectM` contains no font code) | **Not added (not in projectM)**: the app shows the preset name in the window title and, with `RUST_LOG`, in the logs | host-level overlay (egui or the window title); not an engine feature |

## Implemented in this change

* **PCM**: `Analyzer/Engine::push_pcm_i16`, `push_pcm_u8`, `MAX_SAMPLES`; C `lactea_push_pcm_i16/u8`,
  `lactea_pcm_max_samples`. Tests: `integer_pcm_matches_normalised_float_pcm`
  (`lactea-audio/tests/analysis.rs`), `projectm_parity_calls_round_trip`, `version_and_pcm_limits`
  (capi).
* **`PresetControl`** (`control.rs`): settings with projectM's defaults/clamps, easter-egg sampling,
  lock, soft-cut smoothing state, hard-cut detection on beats, switch-requested events. Tests:
  `defaults_and_clamps_follow_projectm`, `display_time_up_requests_a_soft_cut_once`,
  `hard_cut_needs_flag_frames_minimum_time_and_a_volume_jump`,
  `lock_suppresses_requests_and_unlock_rearms_the_current_preset`,
  `soft_cut_state_machine_blocks_requests_then_restarts_the_timer`,
  `easter_egg_samples_a_seeded_gaussian_of_at_least_one_second`.
  C: `lactea_{get,set}_switch_settings`, `lactea_poll_switch_request` (capi test
  `display_time_up_is_polled_once_and_the_idle_preset_renders`).
* **`Playlist`** (`playlist.rs`): items, filters, shuffle, history, retry, sort, events. Tests:
  `glob_filter_semantics`, `filter_list_first_match_decides`,
  `add_insert_remove_keep_history_consistent`, `next_previous_wrap_and_history_deduplicates`,
  `last_walks_back_and_never_ping_pongs`, `history_is_capped`,
  `shuffle_is_seeded_covers_the_list_and_set_position_ignores_it`, `empty_playlist_answers_zero`,
  `sort_by_path_or_filename_both_orders`, `play_next_loads_and_fires_the_switched_event`,
  `failed_presets_are_skipped_and_removed`,
  `retry_count_limits_the_attempts_and_raises_the_failure_event`,
  `failed_preset_while_going_back_uses_the_previous_direction`.
* **Runtime parameters** (`params.rs`): `set_mesh_size` (projectM clamp; new `Renderer::set_mesh_size`),
  `set_texel_offset` (`Renderer::set_texel_offset`), `set_fps`, `set_preset_duration`,
  `last_frame_time`, `window_size` and getters; C `lactea_{set,get}_{mesh_size,texel_offset,fps}`,
  `lactea_last_frame_time`, `lactea_version_components`. Tests: `mesh_clamp_follows_projectm`,
  `runtime_parameters_round_trip_and_take_effect` (GPU, asserts `fps`/`meshx`/`meshy`/`progress` seen
  by a preset).
* **Idle preset** (`idle.rs`): `IDLE_PRESET`, `Engine::load_idle_preset`, `lactea_load_idle_preset`.
  Tests: `idle_preset_parses_with_its_code_and_shape`, `idle_preset_loads_and_renders_something` (GPU).
* **Shape `image=`** (`lactea-preset`, `lactea-render`, `lactea`): tests
  `projectm_shape_image_key` (preset parse/serialise in both flavours),
  `shape_image_uses_unit_texture_aspect` (UVs), `quality_prefixes_pick_filter_and_wrap`
  (`ExtractTextureSettings`), `textured_shape_samples_its_image` (GPU: a red image shows red, no key
  shows the black main image, a missing image is reported and drawn as a placeholder).

* **Touch, callbacks, debug images** (`lactea`: `callbacks.rs`, `extras.rs`; `lactea-capi`; `lactea-app`;
  `oracle/refrender.c`): see the callbacks, touch and debug-image rows above. Tests: `hook_fires_until_removed`,
  `touch_is_projectms_no_op_and_debug_images_cover_every_stage`,
  `failed_loads_raise_the_callback_and_keep_the_previous_preset` (lactea), `touch_stage_read_and_callbacks`
  (capi); oracle proof `oracle/check_touch.sh` (`refrender --touch`).

**Rendering code was touched** (`lactea-render`): `ShapeInstance::image: Option<ShapeImage>` (default
`None`), a new `Kind::TexImage` batch command and `shape_geometry_tex`; `Renderer::set_mesh_size` /
`set_texel_offset`. The shape path only changes for *textured shapes of projectM-flavour presets with a
non-empty `shapecode_N_image`*; every other draw takes the old code (`Kind::Tex` / `TexLeak`, `shape_geometry`).
The oracle loop was **not** run. The image sampler uses no mip chain (projectM's samplers set only
`MIN/MAG_FILTER`), the `Compat::MilkDrop2` flavour never reads the key.

C ABI additions are all additive (`lactea.h` regenerated; no existing symbol changed). `lactea-abi` is
untouched.

## Count of missing features

| category | missing |
|---|---|
| core / lifecycle (`reset_textures`, VCS string, `.prjm`) | 3 |
| callbacks / logging (log callback) | 1 |
| rendering API (burn texture) | 1 |
| user sprites (whole subsystem, 9 C functions) | 1 |
| playlist (`add_path`, C playlist API) | 2 |
| preset runtime (texture eviction, `monitor`/mouse/ctrl vars, date vars) | 3 |
| **total missing** | **11** |

Not counted: upstream no-ops (touch ×4, debug image: 5 functions that projectM stubs out; both are now
mirrored, the touch calls proven pixel-neutral against the oracle), BeatDrop-only features projectM does
not have and that were deliberately not added (`sampler_fft`/`sampler_wave`, text overlays: projectM
compat output cannot be measured for them) and the transitions-slice items (smooth flag, soft-cut
blend, `start_clean`, preset blending).
Of the 11, 9 are in projectM (`reset_textures`, VCS string, `.prjm`, log callback, burn texture, sprites,
`add_path`, the C playlist API, texture eviction) and 2 exist only in BeatDrop / MilkDrop 2
(`monitor`/mouse/ctrl variables, date variables).
