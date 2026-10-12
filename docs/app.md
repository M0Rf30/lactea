# lactea standalone app (`lactea-app`, binary `lactea`)

Window + audio + preset browser around the `lactea` engine. The shell knows the renderer only
through one local trait, so it can be developed (and tested) without the engine:

```rust
trait Visualizer {
    fn load_preset(&mut self, bytes: &[u8], name: &str, change: Change) -> Result<(), String>; // Change::{Cut, Soft{seconds}}
    fn push_pcm(&mut self, interleaved: &[f32], channels: usize);
    fn render(&mut self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, time: f64, frame: u32);
    fn resize(&mut self, w: u32, h: u32);
    fn take_auto_cuts(&mut self) -> u32 { 0 }                       // default-implemented (MD3 auto hard cuts)
    fn set_auto_hard_cut_mode(&mut self, mode: u8) {}               // default-implemented
    fn auto_hard_cut_mode(&self) -> Option<u8> { None }             // None = no such feature / compat does not support it
    fn current_compat(&self) -> Option<CompatArg> { None }          // resolved compat of the last load (never Auto); None = no compat modes
}
```

Implementations: `EngineVisualizer` (feature `engine`, default; adapter in `src/engine_vis.rs`) and
`ScopeVisualizer` (oscilloscope of the incoming PCM, `--visualizer scope`).

```text
cargo build -p lactea-app                      # PipeWire + engine (default features)
cargo build -p lactea-app --no-default-features   # cpal capture only, scope only
lactea --dir /usr/share/projectM/presets        # engine, PipeWire monitor capture
```

### Compat selection (`--compat`)

`--compat auto` (the default) lets the engine resolve the mode per preset from the preset bytes
(`lactea_preset::detect_compat`): MilkDrop 3 when the file uses an MD3-only feature (`.milk2`, `MD31=`/`MD32=` keys,
sprites, `get_fft`/`mouse`/`q33..q64`, ...), projectM otherwise; MilkDrop 2 is only a fallback: a preset projectM
rejects because its code does not compile (the MD2 line glueing, `docs/md3.md`) is retried once under MilkDrop 2. When
the resolved mode changes between two presets the engine rebuilds its
mode-dependent state and does a **hard cut** whatever `--transition-duration` says. `.milk2` files are listed in
`auto` as well as `milkdrop3`. `projectm`, `milkdrop2` and `milkdrop3` fix the mode for every preset (as before).
The host follows the resolved mode after every successful load (`Visualizer::current_compat`): MD3 display-time
randomisation, the F8 key, the window title and the log. `CompatArg::md3()` is true only for the fixed `milkdrop3`.

### Build

Build needs `libpipewire-0.3` headers + clang (bindgen) for the `pipewire` feature and ALSA headers
for cpal. Version choices: winit 0.30.13 (0.31 is still beta), wgpu 30, cpal 0.18, pipewire 0.10,
symphonia 0.5.5, rtrb 0.4. `lactea-app` is not published to crates.io (`publish = false`): it
depends on a pinned git fork of symphonia, which crates.io does not accept.

## Keys

| key | action |
|---|---|
| N, Right, Space | next preset |
| P, Left | previous preset (history, then wraps) |
| R | random preset |
| L | lock / unlock auto-advance |
| F, F11, double-click | fullscreen (cursor hidden while fullscreen) |
| Esc, Q | quit |
| D | write every pipeline stage of the last frame as PNG into `$LACTEA_DEBUG_DIR` (default `<temp dir>/lactea-debug`) |
| F8 | cycle the MD3 auto hard cut mode off, 1 ... 7, off (MilkDrop 3 mode only: `--compat milkdrop3`, or `auto` on a MilkDrop 3 preset; logs `auto hard cut needs a MilkDrop 3 preset ...` otherwise) |

Auto-advance every `--duration S` (default 30, 0 = never) unless locked; `--shuffle` picks the next
preset at random. Every preset change after the first (auto-advance, N / P / R / arrows) is a **soft cut**:
the engine blends the old and the new preset over `--transition-duration S` seconds (default 3, projectM's
`m_softCutDuration`; 0 = hard cuts) while both keep running; the first preset appears at once. A preset that fails to
load (parse/compile error, GPU validation error, engine
panic) is logged and skipped in the direction of travel. The window title shows
`preset [compat] [i/n] | LOCKED | hardcut<N> | audio status | fps` (`[compat]`, e.g. `[MilkDrop 3]`, only under `--compat auto`; `LOCKED` only while locked, `hardcut<N>` only while an auto hard cut mode is on). Every load also logs `preset <name>: compat <mode>` at info level.

### MD3 auto hard cuts (MilkDrop 3 mode)

MD3's `AutoHardCutMode` (`--auto-hard-cut N`, F8 at runtime; `hardcut1` ... `hardcut7`, `off`; behaviour in
`docs/md3.md` §Auto hard cuts) is ignored under projectM and MilkDrop 2. Modes 1..6 ask for an instant preset
change on beat/loudness triggers: after each rendered frame the shell asks the visualizer for pending cuts
(`take_auto_cuts`), and the next frame loads the next preset (random when `--shuffle`) as a **hard cut**, whatever
`--transition-duration` says; the display timer restarts. Mode 7 changes no preset: the engine injects a random
effect into the composite shader. Auto cuts are independent of projectM's `hard_cut_*` settings.
Deviation: MD3 ignores its preset lock, here a locked browser (L) also ignores auto cuts, and a single preset is not reloaded by one.

In MilkDrop 3 mode (`--compat milkdrop3`, or a MilkDrop 3 preset under `--compat auto`) the auto-advance time of a preset is MD3's `--transition-duration` + `--duration` + a
random `0 ... --duration-rand` seconds (`TimeBetweenPresetsRand`, default 10, negative = 0) drawn per preset from
the `--seed`ed stream (`--duration 0` still never advances); the other modes use `--duration` alone. Under `auto` the
host switches this on or off after each successful load, following the preset's resolved compat.

## Command line

```text
--preset FILE          show FILE first (alone unless --dir is also given)
--dir DIR              recursive *.milk scan (repeatable); default: first existing of
                       /usr/share/projectM/presets, /usr/local/share/projectM/presets,
                       /opt/homebrew/share/projectM/presets (none on Windows: pass --dir)
--duration S           auto-advance period (30; 0 = never)      --shuffle
--transition-duration S  soft cut length (3; 0 = hard cuts)
--auto-hard-cut N      MD3 AutoHardCutMode 0..7 (0 = off; 1..6 hard cuts, 7 composite effects; MilkDrop 3 mode only; F8 cycles)
--duration-rand S      MD3 TimeBetweenPresetsRand (10; MilkDrop 3 mode only)
--fps N                frame cap (default: vsync only; --deterministic: 60)
--no-vsync             Mailbox/Immediate instead of Fifo
--size WxH [--pixels]  initial window size, logical (HiDPI) units unless --pixels
--fullscreen
--visualizer engine|scope   --seed N   --compat auto|projectm|milkdrop2|milkdrop3   --mesh WxH
--texture-dir DIR      (repeatable; searched before the defaults)
--capture-device NAME  cpal input (name, substring or id like alsa:default); implies cpal
--audio-backend auto|pipewire|cpal     --pipewire-target NODE
--list-devices         cpal inputs/outputs + PipeWire sinks, then exit
--audio-file F [--loop] [--output-device NAME]   --audio-delay-ms MS
--audio-wav F --deterministic                    (needs both)
--no-audio
--exit-after S   --frames N   --screenshot F.png [--screenshot-frame N]   (testing/captures)
```

Logging: `RUST_LOG` (default `info`). `RUST_LOG=lactea=debug` adds per-second fps and audio
stats, `RUST_LOG=lactea::level=trace` prints `epoch_ms peak frames` per frame, `lactea::title` logs every title.

## Window and GPU (`src/gfx.rs`, `src/app.rs`)

* winit `ApplicationHandler`; the wgpu `Instance` is created with the event loop's
  `OwnedDisplayHandle` (needed by GLES on Wayland, harmless elsewhere). App id `lactea`.
* Surface format: first of `Bgra8Unorm`, `Rgba8Unorm`; otherwise the sRGB variant (then the
  visualizer is told the sRGB format and compensates — the engine does, see
  `EngineConfig::output_format`). Present mode Fifo; `--no-vsync` prefers Mailbox, then Immediate.
* Rendering is always at physical pixels (HiDPI aware); `Resized` events are coalesced into one
  reconfigure per frame and forwarded to `Visualizer::resize` (the engine clears its feedback image
  on resize, so mid-run resizes visibly restart the feedback).
* Surface loss: `Timeout`/`Occluded` skip the frame (and throttle to ~60 Hz), `Outdated`
  reconfigures, `Lost` recreates the surface, `Suboptimal` presents and reconfigures. Device loss
  rebuilds device + surface + visualizer and reloads the current preset (3 attempts).
* Frame cap: timer based (`ControlFlow::WaitUntil`). A cap at or above ~97% of the monitor refresh
  rate is left to vsync — a deadline timer that is almost one refresh interval beats against the
  display and halves the frame rate (measured: cap 60 on a 60 Hz panel gave 24-38 fps; caps of
  30/59/61/120 gave 30.0/59/60.5/60 fps; with the vsync rule 60 gives 60.0).
* `load_preset` runs inside a wgpu validation error scope and `catch_unwind`; the device's
  uncaptured-error handler logs instead of panicking.

## Audio (`src/audio/`)

All sources deliver **interleaved stereo f32** through an `rtrb` SPSC ring (1.5 s); the render
thread drains it once per frame (`AudioSource::poll`) and calls `Visualizer::push_pcm(.., 2)`.
The visualizer is created with the source's sample rate (`EngineConfig::sample_rate`).

### PipeWire (Linux default, `src/audio/pw.rs`)

A capture stream with `stream.capture.sink=true`, F32LE stereo at 44.1 kHz (the adapter converts),
autoconnected: WirePlumber links it to the **default sink's monitor** and re-links when the default
sink changes. A registry + `default` metadata watcher on the same loop logs which node the stream
is linked to and the `default.audio.sink` value:

```text
pipewire: capture stream node 81 <- 'Audio interno Stereo analogico' (alsa_output.pci-0000_00_1f.3.analog-stereo, node 50); default sink = alsa_output.pci-...
  (default switched to a test sink)
pipewire: capture stream node 81 <- 'lactea-test-sink' (fd_test_sink, node 84); default sink = fd_test_sink
```

If the daemon goes away the thread reconnects every second. If PipeWire is unreachable at start the
app falls back to cpal (`--audio-backend pipewire` makes it an error instead).
`--pipewire-target NODE` sets `target.object` (WirePlumber falls back to the default sink for an
unknown node).

### cpal (fallback, other OSes) — `src/audio/cpal_input.rs`

Default input device, or `--capture-device NAME|ID` (exact id/name, then case-insensitive
substring). 44.1 kHz stereo f32 is preferred when the device offers it, otherwise the default
config is used and the visualizer gets that rate.

### `--audio-file` — `src/audio/player.rs`

symphonia decodes in a thread into a ring at the device rate (the output stream is opened at the
file's native rate when the device supports it; otherwise a windowed-sinc resampler,
`resample.rs`, converts). The cpal output callback feeds the device and mirrors every frame it
hands out into a *tap* ring, together with `ClockSample { tapped_before, at, latency }` where
`latency = playback - callback` from cpal's timestamps. The render thread keeps the tapped frames
in a history and releases up to the **audible position**

```text
tapped_before + (now - at - latency) * rate
```

so the visualizer hears what the speakers play now, not what the decoder produced ahead.
`--audio-delay-ms` shifts that (positive = later). Measured on this machine (two instances: one
playing `synth128.wav`, one capturing the PipeWire monitor; per-frame peak series cross-correlated):
peak at **+3 ms** (±10 ms resolution), correlation 0.87; predicted output latency 35 ms; kick onset
differences median -0.1 ms, std 8 ms over 5 kicks. `RUST_LOG=lactea=debug` prints
`playback sync: pos 6.02s / 20.0s | visualizer fed to 6.02s | device runs 35 ms ahead of audible`.

### `--audio-wav F --deterministic` — `src/audio/deterministic.rs`

No clock, no device: frame `n` is rendered at time `n / fps` and receives exactly the sample frames
`lactea_abi::pcm_range(n, sr, fps)` of the decoded WAV (the oracle's feeding rule); the frame
counter is `n`. `--fps` defaults to 60; pacing is real time only for watching. Two runs produce
byte-identical frames (checked: sha256 of two `--screenshot` PNGs equal).

### Silence vs broken capture — `src/audio/monitor.rs`

The monitor watches two things per frame: do **frames arrive** (source counter) and is the PCM
**non-silent** (peak > -80 dBFS):

| status | meaning | title / log |
|---|---|---|
| `Starting` | waiting for the first signal | `audio: starting` |
| `Active` | signal within the last 3 s | `audio: ok -12 dB` |
| `Silent` | frames arrive, digital silence ≥ 3 s: capture works, nothing plays | `audio: SILENT 12s (capture works, nothing playing)`, WARN once |
| `NoData` | no frames for ≥ 2 s: capture stalled / not linked | `audio: NO DATA 5s (capture stalled)`, ERROR once with the PipeWire stream state |
| `Failed` | the source reported an error (device lost, PipeWire connection lost, decode error) | `audio: FAILED (...)` |
| `Ended` | `--audio-file` finished | `audio: end of file` |

If the input cannot be opened at all the app still starts and shows `audio: FAILED (reason)`.

## Textures — `src/textures.rs`

`FsTextureProvider::new(dirs)` indexes the directories recursively once (earlier directories win,
then jpg > jpeg > png > tga > bmp); `load(name)` matches the base name case-insensitively with or
without extension/directory and decodes with `image` to RGBA8 (cached); `random_pool()` returns the
lowercase stems of every indexed file, sorted (the engine's `TextureProvider` contract, one entry
per file like projectM's scan). `engine_vis::FsProvider` wraps it for `lactea::TextureProvider`.
Default directories: `oracle/build/texpack` of the repository (found from the cwd, the executable's
location or the build-time source tree) and `/usr/share/projectM/textures`; `--texture-dir` entries
come first.

## Testing

`cargo test -p lactea-app` — unit tests for the ring/stereo conversion, resampler (SNR,
alias rejection, identity), symphonia decode, audible-position maths and tap history,
monitor state machine, `pcm_range` tiling, key bindings, preset navigation/lock/skip/scan
(symlink loops, 20k-entry dedupe), texture lookup, surface format/present-mode choice, CLI.

Visual checks never touch the desktop: `--screenshot F.png --screenshot-frame N` reads the
swapchain image back (`COPY_SRC`) and exits. With `--audio-wav ... --deterministic --pixels
--size 640x360` it reproduces the oracle's configuration; frame 0/1/10 of
`Aderrasi - Antidote` against `out/ref/starter/.../final` currently gives PSNR 35.5/31.1/24.3 dB
(engine-side differences, see `docs/render.md`; the shell feeds identical PCM, time and frame
counters).

Hidden `--inject-keys "30:n,60:p,90:l,120:esc"` scripts key actions by frame for end-to-end tests
of navigation and locking.

## Known limitations

* Preset load happens on the render thread (a slow shader compile stalls one frame).
* Surface-texture readback (`--screenshot`) supports 8-bit RGBA/BGRA surfaces only.
* The audible-position estimate uses the latency cpal reports for the *callback* stream; extra
  latency inside PipeWire after the ALSA plugin (a few ms) or in the display chain is not known —
  use `--audio-delay-ms`.
* Resizing the window resets the engine's feedback image (engine behaviour).
* Soft cuts need a second renderer: the first soft cut after start-up allocates it (`Renderer::sibling`; pipelines and noise
  textures are shared, so it is cheap compared to start-up) and later cuts recycle the idle one.
