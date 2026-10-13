# lactea-audio: analysis, and where projectM / MD2 / BeatDrop differ

Sources read: projectM master `src/libprojectM/Audio/{PCM,Loudness,MilkdropFFT,WaveformAligner}.cpp`
(behaviour only, nothing copied), `ProjectM.cpp::RenderFrame`, `TimeKeeper.cpp`;
`third_party/md2/{plugin.cpp::DoCustomSoundAnalysis, fft.cpp, plugin.h}` (the band analysis); the shader FFT / waveform textures
(see "BeatDrop shader FFT") are cited to BeatDrop-Music-Visualizer (OfficialIncubo, master) `plugin.cpp`.

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
* a non-finite sample (NaN, +-inf) becomes 0 in `ProjectM` / `BeatDrop` mode; a single one would poison the running
  averages for good (`MD3`: below).
* one circular buffer of **576 sample frames** per channel. `Add` writes at `start`, then
  `start += count (mod 576)`. There is no per-frame consumption count: each analysis snapshots
  the **latest 576 frames, oldest first** (`start` is the oldest; two slice copies of the ring). At 44.1 kHz / 60 fps a frame
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
| 6 | dt for smoothing | `AdjustRateToFPS(rate, 30, GetFps())`, i.e. `1/fps` smoothed fps | `time(n)-time(n-1)` in float; frame 0 sees `~0` (the TimeKeeper's reference is the construction wall-clock, so the first dt is `-(small wall time)`, not exactly 0 -> non-deterministic unless the oracle pins it) | `update(fps)` uses `1/fps` for every frame (deterministic contract). `update_dt(0.0)` reproduces projectM's frame 0 (no averaging at all: rate = 1). A negative or non-finite dt (also `update(0.0)` / negative fps) is treated as 0. `Engine` calls `update_dt(clock.time - last_time)`, with dt = 0 on the first rendered frame. The oracle slice should pin its first dt to 0 or `1/fps`; compare tests call `update_dt(0.0)` on frame 0 |
| 7 | `vol` | not a variable in MD2 per-frame code; shader `_c3.w = 0.3333f*(imm_rel[0], imm_rel[1], imm_rel[2])` is a **comma-operator bug** in MD2 (evaluates to `0.3333*treb`) | `(bass+mid+treb)*0.333` | `AudioFrame.vol` is the projectM value; the render/hlsl slice must decide whether to reproduce the MD2 `0.3333*treb` bug for `_c3.w`/`_c4.w` (projectM does not) |
| 8 | waveform source | `m_sound.fWaveform` 576 -> aligned -> `NUM_WAVEFORM_SAMPLES`=480 (the shell file is not in `third_party/md2`; alignment algorithm taken from projectM which states it mirrors MD2) | same, 480 valid | same |
| 9 | frame counter for the `<50` long-average switch | `GetFrame()` | `m_frameCount`, 0-based, incremented after each render | internal counter, 0-based, +1 per `update` (saturating; `Analyzer::frame_index`) |
| 10| spectrum scale | un-normalised FFT of 128-scaled input | same | same; DC bin is always 0 (equalise[0] = -0.02*ln 1 = 0) |
| 11| extra BeatDrop-only analysis (`get_fft()` shader texture with RMS normalisation, 2048-window `myfftshader`, `FFTAttack/FFTDecay` smoothing; BeatDrop-Music-Visualizer `plugin.cpp:11621-11729`) | yes | none | `BeatDropShaderFft` (separate from `Analyzer`, fed by the engine under `Compat::MilkDrop2` only, `CompatRules::md2_shader_inputs`): see "BeatDrop shader FFT" below; no projectM counterpart |
| 12| MilkDrop 3 (3.37) analysis | rows 1-5: no 8-bit quantiser, no gain, `(L+R)/2` spectrum | rows 1-5 | `AnalysisMode::Md3` (opt-in): 8-bit quantised input, `VolumeSensitivity`, alignment **before** the analysis, bands from the **left** channel only, 576-sample window, silence -> 1; plus `ShaderFft` for the `get_fft` shader helper. See "MilkDrop 3 mode" below |

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

## MilkDrop 3 mode (`AnalysisMode::Md3`, `Compat::MilkDrop3`)

Source: the behavioural description of MilkDrop 3 (3.37) audio analysis and shader FFT (see
`docs/md3.md`, shader inputs); no MD3 code or assets are used. It is an opt-in mode: the default
(projectM) and `BeatDrop` numerics are untouched.

```rust
let mut a = Analyzer::with_mode(44100, AnalysisMode::Md3);   // sample_rate is not used
a.set_volume_sensitivity(1.0);                               // default; only Md3 reads it; non-finite values are ignored
a.push_pcm_f32(&interleaved, 2);                             // quantised to int8 on the way in
let f = a.update(60.0);                                      // bass/mid/treb, wave, spectrum
// every rendered frame, with the sticky FFTAttack / FFTDecay globals (defaults 0.5 / 0.7):
let s: &[f32; 512] = shader_fft.step(&f.spectrum[0], attack, decay);
```

### Per frame

1. **Quantiser (every `push_pcm_*`).** The sample is scaled to int8 units, `v = 128 * s`
   (`push_pcm_i16`: `s / 256`, `push_pcm_u8`: `s - 128`), then `v >= 128 -> 127`, `v < -128 ->
   -128`, otherwise `trunc(v)` (toward zero: `0.999 -> 127`, `-0.999 -> -127`, `0.0078 -> 0`).
   This is MD3's capture quantiser `>= 1.0 -> 127`, `< -1.0 -> -128`, `(int)(128 * s)`; 0 dBFS
   maps to 127 / -128 and there is no +128 offset. The result is stored as `f32`. A NaN becomes 0
   and +-inf saturates to 127 / -128 (`md3::quantise_i8`).
   The other modes keep the unquantised `128 * s`.
2. **Snapshot + gain.** Both channels, newest 576 frames oldest first (as in the other modes),
   multiplied by `volume_sensitivity` (default 1.0, `set_volume_sensitivity` /
   `volume_sensitivity`). The gain acts on the *already quantised* values, so it cannot recover
   a clipped peak (`1.0` full scale with gain 2 is `254`, not `256`) and the waveform may leave
   +-128; it is not clamped. It is applied at `update`, so changing it takes effect on the
   samples already buffered. A non-finite value passed to `set_volume_sensitivity` is ignored (the previous gain stays).
   Ignored by `ProjectM` and `BeatDrop`.
3. **Alignment first.** The existing per-channel `Aligner` (nSamples 480, octaves 6) runs
   *before* the analysis, so the spectra and the bands see the aligned waveform including its
   zeroed tail (samples 480..575 are zero whenever the best offset is > 0). This is the opposite
   of the other modes (spectrum of the unaligned ring, alignment afterwards).
4. **Spectrum (`md3_spectrum`).** Both channels: the 576 aligned samples times
   `env[i] = 0.5 + 0.5 * sin(i * (1/576 * 6.2831853) - 1.5707963268)` (literal `f32`
   constants of MD2's `fft.cpp`), zero padded to 1024, radix-2 FFT, `out[i] = equalize[i] *
   |X[i]|` for `i < 512` with `equalize[i] = -0.02 * ln((512 - i) / 512)`. It is the MD2 FFT
   (`FftStyle::Md2`, `samples_in = 576`) with **no** `0.5 * (x[i] + x[i-1])` input damping.
   The public `md3_spectrum(&[f32; 576]) -> [f32; 512]` is exactly this step for an already
   prepared int8-unit waveform (no alignment, no gain); the analyzer produces identical values. `md3_spectrum` uses one
   thread-local FFT, built on first use and reused by later calls; the analyzer keeps its own.
5. **Bands, left channel only.** `imm[b] = sum of the left spectrum over bins
   [512*b/6, 512*(b+1)/6)`, b = 0, 1, 2 -> `[0,85) [85,170) [170,256)` (a plain sum, no mean over
   the channels; with a 44.1 kHz effective rate 0-3.6 / 3.6-7.3 / 7.3-11 kHz). Whatever the
   right channel carries does not change `bass/mid/treb`.
6. **Smoothing.** As the other modes: `rate = imm > avg ? 0.2 : 0.5`, `avg = avg * rate +
   imm * (1 - rate)`, `long_avg` rate 0.9 for frame < 50 else 0.992, every rate through
   `AdjustRateToFPS` (`(r^30)^dt`), `imm_rel = avg_rel = 1.0` while `|long_avg| < 0.001`, else
   `imm / long_avg` and `avg / long_avg`. Initial values are 1.0 (not 0, unlike `BeatDrop`);
   `vol = (bass + mid + treb) * 0.333` as in `ProjectM`. Digital silence therefore gives exactly
   1.0 for all six values on every frame (`long_avg` stays 0).
7. **Published.** `AudioFrame.wave` = the aligned, gain-scaled int8-unit waveform (only the first
   `wave_len` = 480 samples are meaningful, as before); `AudioFrame.spectrum` = both channels'
   `md3_spectrum` of that very waveform; `bass/mid/treb(_att)`, `vol(_att)` as above.

Consequences worth knowing (they follow from the steps, they are not special cases): the 8-bit
quantisation noise (truncation, roughly 0.6 LSB rms) and the abrupt end of a shifted, zero-tailed
waveform put energy into every band, so an onset from digital silence raises `bass`, `mid` and
`treb` together (`1 / (1 - r)` on the first frame); only a settled signal followed by a change
shows band contrast. A constant level (no shift, no noise) lands only in the lowest bins, i.e.
`bass`. `imm_rel / avg_rel` are ratios, so the gain changes them only through the 0.001 floor of
`long_avg` (a power-of-two gain scales wave and spectrum exactly).

### Shader FFT (`ShaderFft`)

The `get_fft` / `get_fft_hz` shader helpers read a 512-texel state `S` that MD3 updates once per
rendered frame from the **left** spectrum above. `ShaderFft` is that state and recursion in
`f32`; attack/decay are not stored (they are MD3's sticky globals: the preset's `FFTAttack` /
`FFTDecay` when present, otherwise the previous preset's values, initially 0.5 / 0.7; the engine
passes them each frame, values are used as given). One instance per engine, never reset on
preset changes; there is no frame-rate compensation (the recursion advances once per call).

```text
step(spec, attack, decay):
  m  = max(0.001, max_i spec[i])            ga = 0.5*attack + 0.5     gd = 0.1*decay + 0.9
  for i in 0..512 ascending, S updated in place:
      v = spec[i] / m ;  if !(0.2 <= v) { v = 0 } ;  t = v*v
      for k in {-3,-2,-1,1,2,3} with 0 <= i+k < 512:  t = max(t, (1 - 0.25*|k|) * S[i+k])
      s = S[i] ;  S[i] = if t > s { s + (t - s)*ga } else { gd * s }
```

* The neighbours are read from the partially updated state: the three left ones are already new,
  the three right ones still old. An isolated bin therefore smears to the right in one frame
  (at the default attack `S[i+1] = 0.75 * 0.75 * S[i]`, ...) and to the left only on the next one.
* `t == s` takes the decay branch: a bin that reached its target decays; in `f32` a saturated
  peak bin runs an 11-frame limit cycle (`... 1, 1, 1, 1, 0.97, 0.9925, 0.99813, ...`).
* The output is volume invariant (about the squared relative spectrum): the gain only matters
  through the 0.001 floor of `m`. Silence (`m = 0.001`, every bin gated to 0) lets the smear and
  the decay act on what is left (`S[256]`: 1 -> 0.9698 -> 0.9407 ...); digital silence from the
  start stays 0.
* `get_fft(pos)`: `u = clamp(pos, 0, 1)`, `x = clamp(u * 512 - 0.5, 0, 511)`, linear
  interpolation of `S[floor x]` and `S[min(floor x + 1, 511)]` (texel `i` has its centre at
  `(i + 0.5) / 512`). `get_fft_hz(hz) = get_fft(clamp(hz / 24000, 0, 1))` (a fixed 48 kHz rate).
  These are the CPU reference of the shader macros; the GPU side uploads `state()` as a 512x1
  `R32Float` texture and does the clamp + bilinear by hand because `R32Float` is not filterable.

### Not modelled

* MD3's capture layer is outside this crate: 8-bit capture ring of 2304 samples, box decimation
  of rates above 44.1 kHz (`R / 44100`), "no packet for a while -> empty ring -> zeros". The host
  pushes the PCM it wants analysed (zeros for stale capture); the effective rate only affects
  what a bin means in Hz.
* MD3 returns silence for the first fetches until 576 samples have been captured; this crate,
  like the other modes, treats the missing history as zeros (the partial window is analysed).
* The classic `imm / avg / med / long` that MD3's shell also computes are never read by it and are
  not reproduced.

### Verification (`tests/md3.rs`, no external data)

* `md3_spectrum`: fs/4 tone (`0, 64, 0, -64, ...`) peak bin 256 = 127.7609, flanks
  51.4947 / 103.1458 / 104.3150 / 52.6687, bins >= 261 < 1.2; an f64 DFT of the enveloped,
  zero-padded input (no damping); sine vectors at 1 / 5 / 12 kHz (peak bin and band sums);
  silence.
* `ShaderFft`: frames 1, 2, 3, 6 of the fs/4 tone for bins 252..260 and `get_fft(0.5)`;
  `S[256]` over 40 frames (limit cycle, period 11); silence after frame 6; the `attack = 1`,
  `decay = 0` variant; floor of `m`, 0.2 gate, in-place smear, array ends, NaN bins;
  `get_fft` / `get_fft_hz` edge cases and an f64 evaluation of the interpolation. The recursion
  is also bit-identical to the float32 reference model on random spectra (checked once, not
  kept as a test).
* `Analyzer`: quantiser table for f32 / i16 / u8 / mono, other modes unquantised, gain after
  the quantiser (254, not 256; unclamped; applied at `update`; exact doubling of wave and
  spectrum), gain ignored by the other modes, left channel only (right-only tone -> exactly 1.0),
  silence -> exactly 1.0, spectrum == `md3_spectrum(wave)` on a drifting tone (alignment first,
  shifted window + zero tail), the 60 fps smoothing vectors T1 / T2 (`bass` 19.487, 10.000,
  6.8406, ...), the 0.9 -> 0.992 switch at frame 50, a loud low tone after a high tone
  (`bass > 3`, `treb < 0.1`), a constant level (`bass` only, analytic `1 / (1 - r^(k+1))`).

## BeatDrop shader FFT (`BeatDropShaderFft`)

Source: `plugin.cpp:11621-11729` of BeatDrop-Music-Visualizer (OfficialIncubo, master; `m_nFFTShaderScaling` / `m_nFFTShaderNoiseFloor` in `plugin.h:429-430`). It feeds the `sampler_fft` / `sampler_wave` textures of BeatDrop shaders
(`get_fft`, `get_fft_peak`, `get_wave`; `docs/hlsl.md` "BeatDrop prelude") and does not touch `Analyzer`.

```rust
let mut s = BeatDropShaderFft::new();          // empty history, s_fftRMS = 0.1
s.push_pcm_f32(&interleaved, 2);               // newest 2048 sample frames per channel are kept
s.update(fps, attack, decay);                  // once per rendered frame; attack / decay = FFTAttack / FFTDecay (0.5 / 0.7 default)
let (row0, row1) = (s.smoothed(), s.peak());   // 1024 bins each
```

One instance per engine, never reset by a preset change. Constants: `SHADER_FFT_BINS` = 1024, `SHADER_FFT_WINDOW` = 2048, `SHADER_WAVE_SAMPLES` = 480.
A non-finite PCM sample counts as 0, a non-finite `fps` or one below 1e-8 is 1e-8, a non-finite `attack` / `decay` is 0.5 / 0.7. Arithmetic is `f32` in the C++
operation order, `f64` where the C++ promotes (`.25 + decay * .75`, `exp(-1 / fps / .3)`).

Per `update`:

1. Both channels: the latest 2048 frames, oldest first, through the raised-cosine envelope and a 2048-point FFT with **no** equalisation
   (`Fft::new(2048, 1024, false, 1.0, FftStyle::Md2)`), 1024 magnitude bins; the spectrum is `0.5 * (L + R)`.
2. RMS normalisation: `rms = max(sqrt(mean(spec^2)), 1e-5)`, smoothed `s_rms = s_rms * e + rms * (1 - e)` with `e = exp(-1 / fps / tau)`, `tau` = 2 s while `rms > s_rms`
   (attack) and 5 s otherwise (release).
3. Per bin: `v = spec / s_rms * 0.175 - 0.03` (scaling `m_nFFTShaderScaling` .175 and noise floor `m_nFFTShaderNoiseFloor` .03, the compiled-in defaults, not the
   `.ini` values), floored at 0; bins below 24 (`kBassRampEnd`) are multiplied by `0.15 + 0.85 * (i / 24)^2`.
4. FPS-independent attack / decay (the coefficients are 60 fps values): `smoothed += (v - smoothed) * a` when `v > smoothed`, else `* d`, with
   `a = 1 - (1 - attack)^(60 / fps)` and `d = 1 - (1 - (1 - decay)^2)^(60 / fps)`; values below 5e-8 are cut to 0 (row 0, `m_fFFTSmoothed`).
5. Peak hold (row 1, `m_fFFTPeak`): `peak = smoothed` when `smoothed >= peak` and the bin holds for `int(fps * (.25 + .75 * decay))` frames; afterwards it
   falls by `exp(-1 / fps / .3)` per frame.

The engine builds the texture rows `spectrum`, `peak`, wave L, wave R (two textures: `sampler_fft` 1024 x 2, `sampler_wave` 480 x 2): the waves are `m_sound.fWaveform[ch][i] / 128.0f` for the first 480 samples of the 576-sample capture
(`plugin.cpp:11700-11750`; lactea feeds `AudioFrame.wave / 128`, scaled to `-1..1`, zero beyond). `FFTAttack` / `FFTDecay` are the active state's `m_fFFTAttack.eval(GetTime())` /
`m_fFFTDecay.eval(GetTime())` (`plugin.cpp:11665-11666`): blended like every timed float while a soft cut runs (`BlendBase::fft_params`). Tests: `tests/beatdrop.rs`.
