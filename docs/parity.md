# projectM feature parity

Audit of Lactea against **projectM 4.2** (`third_party/projectm`: public headers
`src/api/include/projectM-4/*.h`, playlist library `src/playlist/`, runtime `src/libprojectM/`) and
against the MilkDrop 2 / BeatDrop sources (`third_party/md2`) for the features projectM does not have.
Every row says where the behaviour is defined, what Lactea does today, and what is left.

Rendering fidelity is a separate track (`docs/fidelity.md`); this file is about *features*.

Status words: **Done** (implemented as projectM does), **Equivalent** (same capability, Rust/wgpu-shaped API),
**Partial** (implemented with a stated gap), **Missing**, **Upstream no-op** (projectM declares it but its body is an
empty stub, so there is nothing to match), **N/A** (OpenGL- or C-memory-specific), **Not added** (not in projectM,
deliberately left out).

Reading the Lactea column: `Engine::…` is `crates/lactea/src/engine.rs` (+ `params.rs`, `extras.rs`,
`user_sprites.rs`), `PresetControl` is `crates/lactea/src/control.rs`, `Playlist` is
`crates/lactea/src/playlist.rs`, `capi` is `crates/lactea-capi/src/` (`lib.rs`, `extras.rs`, `playlist.rs`,
`events.rs`, `vulkan.rs`), `lactea-host` is `crates/lactea-host/src/`.

## 1. Core and lifecycle (`core.h`, `ProjectM.cpp`)

| feature | projectM | Lactea | left |
|---|---|---|---|
| create / destroy an instance | `core.h:60`, `:88` | **Equivalent**: `Engine::new(&device, &queue, cfg)` (a `Result`: the config is sanitised, a size beyond the device limit is an error), `lactea_create/destroy` | – |
| create with a GL loader | `core.h:78` | **N/A**: the host hands over a wgpu `Device`/`Queue` | – |
| load a preset from memory | `core.h:125`, `ProjectM.cpp:76` | **Done**: `Engine::load_preset(bytes, name, Transition)`; failure leaves the previous preset running | – |
| load a preset from a path / `file://` | `core.h:108`, `PresetFactoryManager.hpp:64` | **Equivalent**: libraries never touch the filesystem (rule in `AGENTS.md`); `lactea_playlist_*` and the app/CLI read the file | – |
| `idle://` + idle preset when nothing is loaded | `ProjectM.cpp:153-162, 290`, `IdlePreset.cpp:11-193` | **Done**, opt-in: `Engine::load_idle_preset()` (`IDLE_PRESET`, `lactea_load_idle_preset`); an engine without a preset renders none instead of switching to it. The preset is original, not projectM's HDR "M" logo | – |
| smooth-transition flag of the load calls | `core.h:108,125`, `ProjectM.cpp:62-88,303` | **Done**: `Transition::{Cut,Soft{seconds}}`; C `lactea_load_preset_ex(.., transition_seconds)` (`0` = cut, `> 0` = soft cut; `lactea_load_preset` is the cut) | – |
| `projectm_reset_textures` | `core.h:137`, `ProjectM.cpp:100` | **Done**: `Engine::reload_textures()` clears the engine's decode cache and re-resolves the running (and incoming) preset's textures and shape images from the provider without recompiling; C `lactea_reset_textures` also rescans the texture directory | – |
| version components / string | `core.h:149,160` | **Done**: `lactea_version_components`, `lactea_version` (static strings, nothing to free) | – |
| VCS revision string | `core.h:173` | **Done**: `lactea::VCS_VERSION` (`build.rs`: `git describe --always --dirty`, `"unknown"` outside a git checkout), `lactea_vcs_version` | – |
| string alloc/free helpers | `memory.h:43,56` | **Equivalent**: only the playlist returns heap strings (`lactea_playlist_item`, `_get_filter`, `_items`), freed with `lactea_playlist_free_string[_array]` | – |
| `.prjm` preset extension | `MilkdropPreset/Factory.hpp:32` (`".milk .prjm"`) | **Done**: `lactea_host::presets::{is_preset_file, scan_presets, scan_preset_files}` accept `.milk .prjm` (and `.milk2`), case-insensitively; the app scan and `lactea_playlist_add_path` use them. The parser is extension-agnostic | – |

## 2. Parameters (`parameters.h`, `ProjectM.cpp`, `TimeKeeper.cpp`)

| feature | projectM | Lactea | left |
|---|---|---|---|
| texture search paths | `parameters.h:46`, `ProjectM.cpp:90` | **Equivalent**: `TextureProvider` trait; `lactea_host::DirTextures` (several directories, projectM scan order); app `--texture-dir`; C `lactea_set_texture_dir` (one directory; affects presets loaded afterwards or after `lactea_reset_textures`) | – |
| set frame time (`< 0` = system clock) | `parameters.h:70`, `TimeKeeper.cpp:29-48` | **Equivalent**: the host always passes `FrameClock.time`; no hidden system clock (determinism contract). The first frame's `dt` is 0 | – |
| last frame time | `parameters.h:82` | **Done**: `Engine::last_frame_time()`, `lactea_last_frame_time` | – |
| beat sensitivity | `parameters.h:93`, `ProjectM.cpp:458-466` (clamp 0..2) | **Done**: `PresetControl::{set,}beat_sensitivity`. Stored only: projectM never reads the field elsewhere (`ProjectM.hpp:321`), neither do we | – |
| hard cuts: enable / min display time / volume sensitivity, detection on beats | `parameters.h:115-162`, `ProjectM.cpp:134-150`, `TimeKeeper.cpp:74` | **Done**: `PresetControl::update` returns `SwitchRequest { hard_cut: true }` under all of projectM's conditions (`vol - previous vol > sensitivity`, `frame > 50`, `CanHardCut`); `hard_cut_duration()` returns the *truncated* integer like `ProjectM.cpp:486`. The cut itself is the host's `load_preset(.., Transition)` | – |
| soft cut duration | `parameters.h:174`, `ProjectM.cpp:473` | **Done**: stored in `PresetControl` (drives its smoothing state); the blend is `Transition::Soft`. C: `lactea_load_preset_ex` takes the duration per call and the control uses it while that blend runs; afterwards `LacteaSwitchSettings::soft_cut_duration` is back | – |
| preset display duration | `parameters.h:194`, `TimeKeeper.cpp:94-102` | **Done**: `PresetControl::set_preset_duration` (request timing) and `Engine::set_preset_duration` (nominal `progress` denominator). `progress` divides by the *sampled* duration (easter egg, A/B-tracked like `TimeKeeper`) once the host calls `Engine::set_loaded_preset_duration(control.preset_duration_sampled())` after each load: the C API (`preset_started`) and the app (`Visualizer::set_loaded_preset_duration`) do; `lactea-cli` and the web example have no `PresetControl` | – |
| mesh size (clamp 8..300, odd → even) | `parameters.h:219`, `ProjectM.cpp:557-576` | **Done**: `Engine::{set,}mesh_size` (`set_mesh_size` is a `Result`, rolls back on failure), `lactea_{set,get}_mesh_size`. Defaults differ: projectM 32×24, Lactea 48×36 (`RunConfig` contract) | – |
| texel offset | `parameters.h:242` | **Done**: `Engine::{set,}texel_offset`, `lactea_{set,get}_texel_offset` | – |
| fps | `parameters.h:269` | **Done**: `Engine::{set_fps,fps}`, `lactea_{set,get}_fps`; `0` is ignored / `INVALID_ARGUMENT`. Default 60 (projectM's header says 35 for the setter and 60 for the getter) | – |
| aspect correction | `parameters.h:292`, `ProjectM.cpp:535` | **Done**, stored only (`m_aspectCorrection` is never read; `GetRenderContext` `:624-627` is unconditional) | – |
| easter egg | `parameters.h:315`, `TimeKeeper.cpp:129-138` | **Done**: seeded gaussian display time `max(1 s, N(duration, egg))`, exact below 0.001 | – |
| preset lock | `parameters.h:335`, `ProjectM.cpp:425-436` | **Done**: `PresetControl::set_preset_locked` (unlock re-arms the request, as in projectM) | – |
| C switch settings | the `projectm_set_*` / `_get_*` calls above | **Done**: `lactea_{get,set}_switch_settings` (`LacteaSwitchSettings`); a write is validated as a whole (non-finite or out-of-range values give `INVALID_ARGUMENT` and change nothing); `lactea_poll_switch_request`, `lactea_set_volume_sensitivity` | – |
| window size get/set | `parameters.h:362,371` | **Equivalent**: `Engine::resize(w, h) -> Result` (0 is raised to 1, a size beyond the device limit leaves the old size) + `Engine::window_size()`; projectM skips rendering at size 0 | – |
| preset start clean | `parameters.h:387`, `ProjectM.cpp:321` | **Equivalent**: `EngineConfig::start_clean` + `Engine::{set_,}start_clean` (default `false` = projectM's: a new preset's first frame warps from the old preset's composite via `DrawInitialImage`; the first preset always starts black); C `lactea_{set,get}_preset_start_clean`; `fdrender --start-clean` | – |
| "per-pixel mesh toggles" | none beyond the mesh size above | **N/A**: per-pixel code runs only when the preset has it, in both | – |
| MilkDrop 3 auto hard cut mode (not in projectM) | – (MD3 `AutoHardCutMode`, `docs/md3.md`) | **Done** (`Compat::MilkDrop3`): `EngineConfig::auto_hard_cut_mode`, `Engine::{set,}auto_hard_cut_mode`, `Engine::take_auto_cuts` + `PresetControl::update_with_auto_cuts`; C `LacteaConfig::auto_hard_cut_mode`, `lactea_{set,get}_auto_hard_cut_mode` (the request arrives as `is_hard_cut = true`); `fdrender --auto-hard-cut N` | – |
| MilkDrop 3 display time (not in projectM) | – (MD3 `TimeBetweenPresetsRand`, `docs/md3.md`) | **Done** (`Compat::MilkDrop3`): `PresetControl::set_md3_time_rand`; C `LacteaSwitchSettings::preset_duration_rand` (default 10.0 s, inert in the other modes) | – |

## 3. Callbacks and logging (`callbacks.h`, `logging.h`)

| feature | projectM | Lactea | left |
|---|---|---|---|
| preset switch requested | `callbacks.h:43,72`, `ProjectM.cpp:54,134-150` | **Done**: `PresetControl::update(time, vol) -> Option<SwitchRequest>` + `set_switch_requested_callback` (a `callbacks::Hook`, fired from `update`); C `lactea_poll_switch_request` and `lactea_set_switch_requested_callback`. The callback runs inside `lactea_render*` after the frame is submitted (the request stays pollable); the C layer queues events and runs callbacks after the call released the engine, so a callback may call back into the same handle (`lactea_destroy` from it is deferred). With a connected `lactea_playlist` the playlist answers the request itself | – |
| preset switch failed | `callbacks.h:57,87`, `ProjectM.cpp:58,69-73` | **Done**: `Engine::set_preset_switch_failed_callback(Fn(&SwitchFailure{name, message}))`, fired from every failing `load_preset` / `load_idle_preset` (still `Err(EngineError)`, the previous preset keeps running); C `LACTEA_STATUS_PRESET` + `lactea_last_error` and `lactea_set_switch_failed_callback` (`events.rs`). `Playlist` raises its own failure event after the retries | – |
| texture load callback | `callbacks.h:109-160`, `TextureManager.cpp:189-236` | **Done** (pixels, not a GL texture id): `TextureProvider::load(name) -> Option<Image>`; C `lactea_set_texture_load_callback` is asked first (shader textures, and shape images under MD3), then the `lactea_set_texture_dir` directory. `randNN` samplers still pick from the directory's files | – |
| log callback + level | `logging.h:78,89`, `ProjectMCWrapper.cpp:565-587` | **Done**: the engine crates log through the `log` facade (shader fallbacks warn, failed loads error, unresolved textures debug / warn); C `lactea_set_log_callback` / `lactea_set_log_level` install a process-global `log::Log` bridge (fails with `INVALID_ARGUMENT` when another logger already owns the facade; records logged by a lactea call inside the callback are dropped). The app uses `RUST_LOG` | – |

## 4. Audio (`audio.h`, `Audio/PCM.cpp`)

| feature | projectM | Lactea | left |
|---|---|---|---|
| add float PCM | `audio.h:62`, `PCM.cpp:39` | **Done**: `Engine::push_pcm_f32`, `lactea_push_pcm_f32`; non-finite samples are 0 (MD3: ±inf saturates, NaN is 0) | – |
| add int16 PCM | `audio.h:80`, `PCM.cpp:47` (`128·s/32768`) | **Done**: `Analyzer::push_pcm_i16`, `Engine::push_pcm_i16`, `lactea_push_pcm_i16` | – |
| add uint8 PCM | `audio.h:98`, `PCM.cpp:43` (`128·(s-128)/128`) | **Done**: `push_pcm_u8` (same three layers) | – |
| max samples | `audio.h:44`, `AudioConstants.hpp:8` (576) | **Done**: `lactea_audio::MAX_SAMPLES`, `Engine::pcm_max_samples()`, `lactea_pcm_max_samples` | – |
| mono / stereo / N channels | `audio.h:58-59` | **Done**: the channel argument is the interleave count; mono is duplicated, channels > 2 are ignored | – |

## 5. Touch (`touch.h`): upstream no-op, mirrored

| feature | projectM | Lactea | left |
|---|---|---|---|
| `projectm_touch`, `_touch_drag`, `_touch_destroy`, `_touch_destroy_all` | `touch.h:49-79`; bodies are `// UNIMPLEMENTED` stubs (`ProjectM.cpp:595-613`); `projectm_touch_type` (`types.h:68-79`) names 9 shapes nothing draws | **Done, no-op by design**: `Engine::{touch, touch_drag, touch_destroy, touch_destroy_all}` + `TouchType`, C `lactea_touch{,_drag,_destroy,_destroy_all}` (type 0..=8 validated). projectM is the reference and draws nothing, so drawing would *diverge*. Verified against the oracle: `oracle/check_touch.sh` (`refrender --touch`) shows every PNG / vars / `oracle.json` byte-identical to the run without touches; the Lactea test `touch_is_projectms_no_op_and_debug_images_cover_every_stage` asserts the same | the waveforms land behind these entry points if projectM ever implements them |

## 6. Rendering API and debugging (`render_opengl.h`, `debug.h`)

| feature | projectM | Lactea | left |
|---|---|---|---|
| render a frame / into a given FBO | `render_opengl.h:40,49` | **Equivalent**: `Engine::render(&mut encoder, &target_view, clock)` renders into any target view of the configured format; C `lactea_render[_to_buffer]` | – |
| render into a host-owned image | `render_opengl.h:49` | **Done** for Vulkan hosts: `lactea_create_vulkan` (the engine runs on the host's `VkDevice`, `vulkan.rs`) + `lactea_render_to_vk_image`; `examples/c/vulkan.c` reads the image back. Metal/D3D12 hosts: Rust `wgpu` only | – |
| burn a texture into the main image | `render_opengl.h:64`, `ProjectM.cpp:408-423` | **Done** (pixels, not a GL texture id): `Engine::burn_texture(rgba8, w, h, [left, top, width, height])` draws one opaque quad with projectM's placement arithmetic into the image the next warp reads, in the active and (during a soft cut) the incoming preset (`Renderer::burn_texture`, `renderer/burn.rs`); C `lactea_burn_texture_rgba`. No-op before the first preset | – |
| write a debug image of the main texture | `debug.h:53`; body is `// UNIMPLEMENTED` (`ProjectMCWrapper.cpp:493`) | **Done** (pull style): `Engine::debug_image(Stage)`, `debug_images()`, `debug_image_incoming(Stage)` return `DebugImage { stage, width, height, rgba }` for `Final`, `Main`, `Warped`, `Blur1..3`; C `lactea_read_stage(engine, stage 0..5, out, out_len, &w, &h)` (`out = NULL` queries the size); app key `D` writes `lactea-debug-<frame>-<stage>.png`; `fdrender --stages`. Blur stages hold data only when the preset samples blur or after `set_min_blur_levels(3)` | – |

## 7. User sprites (`user_sprites.h`, `UserSprites/`)

| feature | projectM | Lactea | left |
|---|---|---|---|
| `projectm_sprite_create/_destroy/_destroy_all/_get_sprite_count/_get_sprite_ids/_set_max_sprites/_get_max_sprites/_get_var/_set_var` | `user_sprites.h:55-146`, `ProjectM.cpp:363-406`, `UserSprites/MilkdropSprite.cpp:72-96,330-358`, `SpriteManager.cpp` | **Done** in every compat mode: `Engine::{sprite_create(kind, &[u8]) -> u32 (0 = failure), sprite_destroy, sprite_destroy_all, sprite_count, sprite_ids, set_max_sprites, max_sprites, sprite_var, set_sprite_var}` (`user_sprites.rs`); C `lactea_sprite_{create,destroy,destroy_all,count,get_ids,set_max,get_max,get_var,set_var}`. The sprite text is parsed by `lactea_preset::parse_user_sprite` (`PresetFileParser` rules: case-insensitive first-wins keys, > 1 MiB / NUL / no key refused, code lines joined with `\n`); one seeded eel context per sprite sharing the engine's `rand()`; lowest free id, oldest evicted at the limit (default 16, 0 makes create return 0); inputs `time frame fps progress bass mid treb *_att`, outputs `x y sx sy rot flipx flipy repeatx repeaty blendmode r g b a done burn` with projectM's clamps; images come from the `TextureProvider` (missing image: 1×1 transparent texel — projectM's placeholder pixels are unspecified). Sprites run only while a preset is loaded. During a soft cut they are drawn **after** the blend onto the output (`Renderer::draw_sprites_to_output`, so a warp / zoom transition does not distort them, as in projectM); `burn` goes into both presets' feedback images (`Renderer::burn_sprites`); `done` sprites draw their last frame, then go | – |

## 8. Playlist library (`projectm-4-playlist`)

`crates/lactea/src/playlist.rs` is a pure-data port (no filesystem, no engine access): the host passes a `load`
closure to `play_*`, the equivalent of projectM's preset-load event. `lactea_host::presets::PlaylistFs` adds
`add_path` / `insert_path` (directory scan), and `crates/lactea-capi/src/playlist.rs` exposes the whole library as
`lactea_playlist_*`.

| feature | projectM | Lactea | left |
|---|---|---|---|
| create / connect / destroy | `playlist_core.h:57,69,95`, `PlaylistCWrapper.cpp:9-35` | **Done**: `Playlist::new(seed)`; C `lactea_playlist_{create,connect,destroy}` (destroy order versus the engine does not matter). A connected playlist reads the file, loads it (soft cut of the engine's `soft_cut_duration`, hard cut when `0` or requested) and answers the engine's switch requests with `play_next` | – |
| size / clear / items / item | `playlist_items.h:44-84` | **Done**: `len`, `clear`, `items`, `items_range`, `item`; C `lactea_playlist_{size,clear,items,item}` | – |
| add / insert preset(s), remove | `playlist_items.h:143-228`, `Playlist.cpp:49-89,154-181` | **Done**: `add_preset(s)`, `insert_preset(s)`, `remove_preset(s)`; duplicates, filter, history fix-ups as in projectM; C `lactea_playlist_{add,insert,remove}_preset[s]` | – |
| add / insert **path** (directory scan) | `playlist_items.h:103,126`, `Playlist.cpp:92-151` | **Done** outside the library crate: `lactea_host::PlaylistFs::{add_path, insert_path}` (sorted, `.milk .prjm .milk2`, each directory visited once so symlink loops end, filter and duplicates from the playlist's rules); C `lactea_playlist_{add,insert}_path` | – |
| sort (full path / file name, asc / desc) | `playlist_items.h:252`, `Playlist.cpp:196-243` | **Done**: `sort` (stable; clears the history); C `lactea_playlist_sort` with `LACTEA_PLAYLIST_SORT_*` constants | – |
| shuffle | `playlist_playback.h:44`, `Playlist.cpp:255-259` | **Done**: seeded (`StreamRng`), deterministic per seed; the C playlist seeds from the clock (projectM's API has no seed argument) | – |
| history (1000 entries, no consecutive duplicates) + `play_last` | `playlist_playback.h:146`, `Playlist.cpp:302-322,411-424` | **Done** | – |
| position / next / previous | `playlist_playback.h:88-127`, `Playlist.cpp:246-300,336-358` | **Done**: `position`, `set_position`, `play_next`, `play_previous`, `next_index`, … (empty list → 0); C `lactea_playlist_{get,set}_position`, `play_{next,previous,last}` | – |
| filters (`+`/`-` globs, `**`, `?`, first match wins) + apply | `playlist_filter.h:75-101`, `Filter.cpp:19-156` | **Done**: own matcher built from the documented semantics (`Filter`, `Playlist::{set_filter,filter,apply_filter}`); not bug-for-bug with `Filter.cpp` for pathological patterns (`**` inside a name); C `lactea_playlist_{set,get,apply}_filter` | – |
| retry count (default 500) + failure event | `playlist_playback.h:63`, `PlaylistCWrapper.cpp:113-192` | **Done**: failed items are removed and the walk continues in the direction of travel; the failure event fires after `retry_count` failures. projectM's quirk that the item sliding into the freed slot is skipped is kept (`failed_presets_are_skipped_and_removed`); C `lactea_playlist_{get,set}_retry_count` | – |
| events: switched / switch failed / preset load | `playlist_callbacks.h:49,69,105` | **Done**: `set_preset_switched_callback`, `set_preset_switch_failed_callback`, the `load` closure; C `lactea_playlist_set_preset_{switched,switch_failed,load}_event_callback` (switched / failed run after the play call so they cannot alias the playlist; a load callback returning `true` suppresses the switched event) | – |
| string-array memory helpers | `playlist_memory.h:44,55` | **Done**: `lactea_playlist_free_string`, `lactea_playlist_free_string_array` | – |
| the app's own browser | – | `crates/lactea-app/src/presets.rs`: with the `engine` feature `PresetBrowser` navigates with `lactea::Playlist` and times with `lactea::PresetControl` (`.prjm` scan, MD3 display-time draw; Random can repeat the current preset like `Playlist`'s pick); the old logic stays under `cfg(not(feature = "engine"))` | – |

## 9. Preset format and runtime

| feature | projectM / MD2 | Lactea | left |
|---|---|---|---|
| shape `image=` key (`shapecode_N_image`) | projectM only: `CustomShape.cpp:59`, draw `:154-175` (texture by name through `TextureManager`, `fw_/fc_/pw_/pc_` prefixes, `textureAspectY = 1`, missing texture → 1×1 placeholder) | **Done**: `CustomShape::image`, `ShapeInstance::image`, `Kind::TexImage` draw path, provider lookup in `shape_images.rs` | – |
| main-wave modes beyond 15 | `WaveformMode.hpp:6-26`: 16 modes, the value wraps with `Count` | **Done**: `PM_WAVE_MODES = 16` (`wavegen.rs`); MD2 mode 0-8 under `Compat::MilkDrop2` | – |
| 4 custom waves / 4 shapes (projectM) vs 16 (MD2) | – | **Done**: `CompatRules::custom_slots` | – |
| every preset key projectM's MilkDrop loader reads | `MilkdropPreset/*.cpp` `Get*("key")` calls | **Done**: a script over those calls finds every key name in `lactea-preset` | – |
| preset load limits | `PresetFileParser`: > 1 MiB, NUL bytes, no key line are rejected | **Done** (`Flavor::ProjectM` only): `ParseError::{TooLarge, BinaryData, NoKeys}` | – |
| built-in idle textures `idlem`, `idleheadphones` | `TextureManager.cpp:85-96` | **N/A**: our idle preset needs no textures | – |
| texture cache with age-based eviction | `TextureManager.cpp:115-176` (`PurgeTextures`) | **Equivalent**, different policy: the engine keeps decoded media (128 MiB) and translated shaders (16 MiB) across loads with least-recently-used eviction by byte budget (`cache.rs`; the provider is asked once per name, `set_texture_provider` / `reload_textures` clear it); each renderer drops its own textures with its preset; `DirTextures::with_cache()` caches decodes for hosts | – |
| shader cache | `Renderer/ShaderCache.cpp`: caches *built-in* GL programs by name | **Equivalent**: `Pipelines::new` builds the fixed pipelines once per renderer (siblings share them); translated preset shaders are cached by the engine | – |
| DDS textures | listed in the extension set, decoded by stb (which cannot) | **N/A**: same; `DirTextures` indexes `.dds` but cannot decode it | – |
| preset blending / transition shaders | `Renderer/PresetTransition.cpp`, `TransitionShaders/` | **Done**: the six transition shaders (`lactea-render::transition`), seeded shader choice, `PresetClock` timing, `start_clean`; `Compat::MilkDrop2` uses MD2's per-vertex blend instead (`docs/engine.md`, "Transitions") | – |
| `sampler_fft`, `sampler_wave` (R32F spectrum / waveform textures, `get_fft()`) | BeatDrop only: `md2/plugin.cpp:2404-2408, 3554-3565, 11630-11729`; projectM has none | **Not added for projectM** (the sampler stays an unresolved user texture, so projectM-compat output matches the oracle). **Done under `Compat::MilkDrop2`** (`md2_shader_inputs`): `lactea_audio::BeatDropShaderFft` (2048-sample window → 1024 bins, RMS normalisation, attack / decay, peak hold) feeds a 1024×4 R32Float texture (spectrum, peak, wave L, wave R); the shader prelude adds `get_fft`, `get_fft_hz`, `get_fft_peak[_hz]`, `get_wave` (helper macro names reconstructed from BeatDrop's docs: its `include.fx` additions are not in `third_party/md2`). **Done under `Compat::MilkDrop3`**: `sampler_fft` is MD3's 512-bin smoothed left spectrum (`docs/md3.md`, "Shader inputs"); `sampler_wave` is MD3's never-filled 1×1 black texture | – |
| `mousex/y/down/click`, `ctrllt/rt/dn/up`, `monitor` | BeatDrop only: `md2/state.cpp:328,349-356,402-409`, `milkdropfs.cpp:616,687-695`; projectM does not register them | **Not added for projectM** (ordinary user variables, 0). **Done under `Compat::MilkDrop2`** (`md2_input_vars`): `Engine::set_mouse` (`mousey` bottom origin, `mouseclick` a one-frame edge) and `Engine::set_ctrl_keys(CtrlKeys)` feed the per-frame and per-vertex contexts; shader `_c14` carries the mouse; `Engine::monitor()` / `lactea_get_monitor` read it back. `monitor` is treated as persistent (`milkdropfs.cpp:616` resets it to the post-init value every frame, a comment in `plugin.cpp` says otherwise: the ambiguity is not resolved). **`Compat::MilkDrop3`**: `monitor2` / `monitor3` are plain persistent variables, the mouse is shader-only (`_c14`, `mouse*`; `mouse.x` does not compile in MD3 preset code), `ctrl*` do not exist | – |
| date/time variables (`year … totalseconds`) | BeatDrop 1.5: `md2/state.h:432-433`, `milkdropfs.cpp:916-923`; not in projectM | **Not added for projectM**. **Done under `Compat::MilkDrop2`** (`md2_input_vars`, per-frame context only; shader `_c15` / `_c16`): `Engine::set_date_time(DateTime)` / `clear_date_time` (the engine never reads the system clock; unset: date 0 and the time of day is the frame time modulo 86400 s); C `lactea_set_date_time`, `lactea_clear_date_time`, `lactea_set_ctrl_keys` | – |
| title / text overlays, preset info | MD2/BeatDrop (`plugin.cpp` text engine); projectM 4 has no text rendering (`libprojectM` contains no font code) | **Not added (not in projectM)**, out of scope: the app shows the preset name in the window title and, with `RUST_LOG`, in the logs | host-level overlay |

## Current state

Everything in projectM's public C API now has a Lactea counterpart, in the Rust API and in the C API
(`lactea.h`). The C ABI grew only by new symbols; the one change to an existing
type is `LacteaCompat`, now an `int32_t` with `LACTEA_COMPAT_*` constants instead of an enum. What changed since the
first audit, with where to read it:

* **PCM, `PresetControl`, `Playlist`, runtime parameters, idle preset, shape `image=`, touch, debug images**:
  rows above; tests live next to the code (`crates/lactea/src/control.rs`, `playlist.rs`, `params.rs`,
  `extras.rs`; `lactea-audio/tests/analysis.rs`; capi `tests`).
* **Transitions**: soft cuts, the six transition shaders, `start_clean`, MD2 / MD3 blends (`docs/engine.md`).
* **Core API gaps closed**: `reload_textures` / `lactea_reset_textures`, `burn_texture`, VCS string, log callback,
  texture-load callback.
* **User sprites**: `Engine::sprite_*` and `lactea_sprite_*` (section 7).
* **Playlist**: `lactea_playlist_*` C API and `lactea_host::PlaylistFs` path scanning; `.prjm` accepted everywhere
  presets are scanned.
* **Hosts**: `lactea-host` (texture directories, image / media / WAV decode, GPU init, preset scan) is shared by
  `lactea-capi`, `lactea-cli` and `lactea-app`; the app's browser runs on `Playlist` + `PresetControl`;
  `progress` follows the sampled display time in the C API and the app.

Rendering code touched by the parity work (`lactea-render`): `ShapeInstance::image` / `Kind::TexImage`,
`Renderer::{set_mesh_size, set_texel_offset, burn_texture, burn_sprites, draw_sprites_to_output,
has_sprite_image, remove_sprite_image}`. Default projectM output is unchanged by all of them for presets that do
not use the new feature.

## Count of missing features

Nothing in projectM's public API is missing.

Not counted: upstream no-ops (touch ×4, debug image: both mirrored, the touch calls proven pixel-neutral against
the oracle) and BeatDrop-only features projectM does not have: the mouse / ctrl / date variables and the shader FFT
are implemented under `Compat::MilkDrop2` only (rules `md2_input_vars`, `md2_shader_inputs`), text overlays are
deliberately not added.
