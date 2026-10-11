# lactea-audio: analysis, and where projectM / MD2 / BeatDrop differ

Sources read: projectM master `src/libprojectM/Audio/{PCM,Loudness,MilkdropFFT,WaveformAligner}.cpp`
(behaviour only, nothing copied), `ProjectM.cpp::RenderFrame`, `TimeKeeper.cpp`;
`third_party/md2/{plugin.cpp::DoCustomSoundAnalysis, fft.cpp, plugin.h}` (this is the BeatDrop
fork of vis_milk2, not the original Winamp one).

## API

```rust
let mut a = Analyzer::new(44100);                 // AnalysisMode::ProjectM (oracle-compatible)
a.push_pcm_f32(&interleaved, 2);                  // before frame n: pcm_range(n, sr, fps)
let f: &AudioFrame = a.update(60.0);              // once per frame; or update_dt(seconds)
```

`AudioFrame { bass, mid, treb, vol, bass_att, mid_att, treb_att, vol_att,
wave: [[f32;576];2], wave_len (=480), spectrum: [[f32;512];2] }`

* `bass/mid/treb` = MD2 `imm_rel[0..3]`; `*_att` = `avg_rel`. These feed the preset variables
  `bass`, `bass_att`, ... exactly as `milkdropfs.cpp` does (lines 606-611).
* `wave` is in MD2's 8-bit scale (float PCM x 128). Only the first **480** samples are valid
  (`NUM_WAVEFORM_SAMPLES` in this MD2 build, `WaveformSamples` in projectM). The array is 576
  long because the aligner works on 576; the tail is not meaningful.
* `spectrum` = `fSpecLeft/Right` = `m_sound.fSpectrum`: 512 equalised magnitudes per channel
  (bin k = k * sr/1024 Hz in BeatDrop mode; see "FFT" for projectM).

## Float PCM -> MD2 scale, buffering (projectM `projectm_pcm_add_float`)

* every sample is multiplied by **128** (`128 * s / 1`), i.e. +-1.0 float = +-128, MD2's
  "-128..127" waveform range. `push_pcm_i16` (`128 * s / 32768`) and `push_pcm_u8` (`128 * (s - 128) / 128`)
  land on the same scale (`projectm_pcm_add_int16` / `_uint8`); `MAX_SAMPLES` = 576 is
  `projectm_pcm_get_max_samples`.
* channel 0 = L, channel 1 = R, further channels ignored, mono is copied to both.
* one circular buffer of **576 sample frames** per channel. `Add` writes at `start`, then
  `start += count (mod 576)`. There is no per-frame consumption count: each analysis snapshots
  the **latest 576 frames, oldest first** (`start` is the oldest). At 44.1 kHz / 60 fps a frame
  pushes 735 frames, so only its last 576 are seen (the first 159 are never analysed). At
  fps > 76.5 (sr 44100) a frame pushes < 576 frames and the window overlaps the previous frame.
  Before the first 576 frames exist the missing history is zeros.
* the snapshot is taken in `UpdateFrameAudioData`, called at the start of `RenderFrame` after
  `UpdateTimers`.

## Analysis steps (ProjectM mode = default)

1. snapshot L, R (576 each).
2. spectrum per channel: input damped as `x'[i] = 0.5*(x[i] + x[i-1])` (`x[-1]` := `x[0]`),
   then FFT with `samples_in = 480` (only the first 480 damped samples enter, the rest of the
   1024-point transform is zero), envelope `0.5+0.5*sin(i*2pi/480 - pi/2)` (power 1),
   equalise table `-0.02 * ln((512-i)/512)`, magnitude by `hypotf`. Radix-2 DIT, f32, twiddles
   by recurrence `w *= wp` exactly like MD2 `fft.cpp`.
3. mono spectrum `0.5*(L+R)`; band energy `imm` = sum of bins `[512*b/6, 512*(b+1)/6)` for
   b = 0,1,2 -> bins `[0,85) [85,170) [170,256)` ("only the bottom half of the spectrum").
4. `rate(r) = (r^30)^dt`  (dt = seconds since previous frame; MD2: `1/fps`)
   `avg      = avg*rate(imm>avg ? 0.2 : 0.5) + imm*(1-rate)`
   `long_avg = long*rate(frame<50 ? 0.9 : 0.992) + imm*(1-rate)`
   `imm_rel = |long|<0.001 ? 1 : imm/long`, `avg_rel` likewise with `avg`.
5. waveform alignment per channel (6-octave pyramid, centre-weighted, offsets 0..95, never
   negative; result = 480 samples starting at the best offset, tail zeroed when shifted). It runs
   *after* the spectrum so the spectrum is from the unaligned data. Previous-frame mips are
   rebuilt from the shifted waveform.
6. `vol = (bass+mid+treb)*0.333`, `vol_att` likewise.

## Deviations (what projectM does vs MD2 as in `third_party/md2`) and what we follow

| # | Topic | MD2 (BeatDrop `plugin.cpp`) | projectM | lactea |
|---|---|---|---|---|
| 1 | band split | Hz: bass 0-250, mid 250-4000, treb 4000-20000 Hz (`bin = hz/(sr/2/512)`, **inclusive** end bin), `.ini`-configurable | fixed thirds of the lower half: bins 0-84 / 85-169 / 170-255 (= 0-3.7 / 3.7-7.3 / 7.3-11 kHz @44.1k) | `ProjectM` default; `BeatDrop` reproduces Hz bands using the sample rate given to `Analyzer::new` |
| 2 | channel combine | `(L+R)/2` per bin | `0.5*(L+R)` mono spectrum, same | same |
| 3 | FFT window | 576 samples zero-padded into a 1024 window, envelope over 1024 (`Init(1024,512,..)`), `sqrtf` magnitude, literal 6.2831853f/1.5707963268f | 480 samples in, 576 buffer damped first; envelope over 480; `hypotf`; PI-based constants | per mode (`FftStyle::{Md2,ProjectM}`) |
| 4 | input damping `0.5*(x[i]+x[i-1])` | none | yes | ProjectM mode only |
| 5 | silence | `imm_rel`/`avg_rel` = **0** when `|long_avg|<0.001` (BeatDrop changed it; original MD2 and projectM use 1) | 1 | ProjectM: 1, BeatDrop: 0. Also the initial values before the first update |
| 6 | dt for smoothing | `AdjustRateToFPS(rate, 30, GetFps())`, i.e. `1/fps` smoothed fps | `time(n)-time(n-1)` in float; frame 0 sees `~0` (the TimeKeeper's reference is the construction wall-clock, so the first dt is `-(small wall time)`, not exactly 0 -> non-deterministic unless the oracle pins it) | `update(fps)` uses `1/fps` for every frame (deterministic contract). `update_dt(0.0)` reproduces projectM's frame 0 (no averaging at all: rate = 1). The oracle slice should pin its first dt to 0 or `1/fps`; compare tests call `update_dt(0.0)` on frame 0 |
| 7 | `vol` | not a variable in MD2 per-frame code; shader `_c3.w = 0.3333f*(imm_rel[0], imm_rel[1], imm_rel[2])` is a **comma-operator bug** in MD2 (evaluates to `0.3333*treb`) | `(bass+mid+treb)*0.333` | `AudioFrame.vol` is the projectM value; the render/hlsl slice must decide whether to reproduce the MD2 `0.3333*treb` bug for `_c3.w`/`_c4.w` (projectM does not) |
| 8 | waveform source | `m_sound.fWaveform` 576 -> aligned -> `NUM_WAVEFORM_SAMPLES`=480 (the shell file is not in `third_party/md2`; alignment algorithm taken from projectM which states it mirrors MD2) | same, 480 valid | same |
| 9 | frame counter for the `<50` long-average switch | `GetFrame()` | `m_frameCount`, 0-based, incremented after each render | internal counter, 0-based, +1 per `update` |
| 10| spectrum scale | un-normalised FFT of 128-scaled input | same | same; DC bin is always 0 (equalise[0] = -0.02*ln 1 = 0) |
| 11| extra BeatDrop-only analysis (`get_fft()` shader texture with RMS normalisation, 2048-window `myfftshader`, `FFTAttack/FFTDecay` smoothing) | yes | none | **not implemented** (no projectM counterpart; presets from the corpus cannot use it) |

## Numerics

All maths is `f32`, in the C++ operation order (`powf`, `sinf`, `cosf`, `logf`, `hypotf` via
libm). Expect agreement with an x86-64 non-FMA, non-fast-math projectM build to a few ulp in
the spectrum; the ratios (`imm/long_avg`) amplify that slightly.

## Verification

`CARGO_TARGET_DIR=target-presetaudio cargo test -p lactea-audio --release`

* exact: silence (all rel = 1 / 0 in BeatDrop), mono/quad channel handling, chunked vs single push
* FFT vs an f64 DFT (1e-3 relative), equalise/envelope tables, DC bin = 0
* sine 1 kHz / 5 kHz / 9 kHz: peak bin within 2 bins, the matching projectM band holds >20x the
  energy of the other two; onset after silence raises only that band's `*_rel`/`*_att`
* click train spikes `bass/mid`; fps dependence: 15 frames @30 fps == 30 frames @60 fps
  within 0.2 % (analytic `1/(1-0.9^(n+1))` for `n` = 0,10,40 checked), long-average switch at 50
* aligner reduces frame-to-frame waveform RMS error on a drifting sine to < 20 % of the raw one
* oracle: `tests/oracle_vars.rs` (ignored) compares `bass,mid,treb,*_att` to
  `out/ref/starter/*/vars/*.json`; run with
  `cargo test -p lactea-audio --release --test oracle_vars -- --ignored --nocapture`
  (`LACTEA_ORACLE_OUT` overrides the directory). No oracle output existed when this was
  written, so no numeric comparison has been made yet.
