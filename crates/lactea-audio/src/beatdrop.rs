//! BeatDrop's shader FFT (`plugin.cpp`, `DoCustomSoundAnalysis`): the 1024-bin spectrum and the waveform behind
//! `sampler_fft` / `sampler_wave` (`get_fft`, `get_fft_peak`, `get_wave` in BeatDrop shaders).
//!
//! It is an analysis of its own and does not touch [`Analyzer`](crate::Analyzer): the last 2048 sample frames of each
//! channel go through a raised-cosine envelope and a 2048-point transform (no equalisation); the mono magnitude is
//! divided by a smoothed RMS of itself, scaled, floored, tilted down below bin 24 and then follows an attack / decay
//! recursion whose per-frame coefficients are converted from their 60 fps meaning to the real frame rate. A peak-hold
//! row falls behind it. Arithmetic is `f32` in the operation order of the C++ where it is `float` there, `f64` where
//! the C++ promotes (`.25 + decay * .75`, `exp(-1 / fps / .3)`).
//!
//! `m_nFFTShaderScaling` and `m_nFFTShaderNoiseFloor` come from BeatDrop's `.ini`; the compiled-in defaults
//! (`.175`, `.03`) are used.

use crate::fft::{Fft, FftStyle};

/// `MY_FFT_SHADER_SAMPLES`: bins per spectrum row.
pub const SHADER_FFT_BINS: usize = 1024;
/// `MY_FFT_SHADER_WINDOW`: sample frames per transform.
pub const SHADER_FFT_WINDOW: usize = 2 * SHADER_FFT_BINS;
/// `NUM_WAVEFORM_SAMPLES`: valid samples of the waveform rows.
pub const SHADER_WAVE_SAMPLES: usize = 480;

/// `m_nFFTShaderScaling`.
const SCALING: f32 = 0.175;
/// `m_nFFTShaderNoiseFloor`.
const NOISE_FLOOR: f32 = 0.03;
/// Bins below this are attenuated (`kBassRampEnd`).
const BASS_RAMP_END: usize = 24;
/// Smoothed values below this are cut to zero (`kVisibleFloor`).
const VISIBLE_FLOOR: f32 = 5e-8;

/// The state of BeatDrop's shader FFT: the PCM history, the smoothed RMS and the two spectrum rows.
///
/// One per engine; it survives preset changes (the C++ keeps it in the plugin). Push PCM with
/// [`push_pcm_f32`](Self::push_pcm_f32), then call [`update`](Self::update) once per rendered frame.
pub struct BeatDropShaderFft {
    fft: Fft,
    /// The newest [`SHADER_FFT_WINDOW`] sample frames per channel, as a ring starting at `start`.
    ring: [Vec<f32>; 2],
    start: usize,
    /// `s_fftRMS`.
    rms: f32,
    smoothed: [f32; SHADER_FFT_BINS],
    peak: [f32; SHADER_FFT_BINS],
    /// `m_nFFTPeakHold`: frames left before a peak starts to fall.
    peak_hold: [i32; SHADER_FFT_BINS],
}

impl Default for BeatDropShaderFft {
    fn default() -> Self {
        Self::new()
    }
}

impl BeatDropShaderFft {
    /// A silent analyser (empty history, `s_fftRMS = 0.1`).
    pub fn new() -> Self {
        BeatDropShaderFft {
            fft: Fft::new(
                SHADER_FFT_WINDOW,
                SHADER_FFT_BINS,
                false,
                1.0,
                FftStyle::Md2,
            ),
            ring: [vec![0.0; SHADER_FFT_WINDOW], vec![0.0; SHADER_FFT_WINDOW]],
            start: 0,
            rms: 0.1,
            smoothed: [0.0; SHADER_FFT_BINS],
            peak: [0.0; SHADER_FFT_BINS],
            peak_hold: [0; SHADER_FFT_BINS],
        }
    }

    /// Append interleaved float PCM (`-1..1`). Channel 0 is left, channel 1 right (further channels are ignored,
    /// mono feeds both). Only the newest [`SHADER_FFT_WINDOW`] frames are kept; `channels == 0` or a slice shorter
    /// than one frame is a no-op, a trailing partial frame is ignored, a non-finite sample counts as 0.
    pub fn push_pcm_f32(&mut self, interleaved: &[f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let clean = |s: f32| if s.is_finite() { s } else { 0.0 };
        let frames = interleaved.chunks_exact(channels);
        // frames older than one window can never be read
        let skip = frames.len().saturating_sub(SHADER_FFT_WINDOW);
        for frame in frames.skip(skip) {
            let l = clean(frame[0]);
            let r = if channels > 1 { clean(frame[1]) } else { l };
            self.ring[0][self.start] = l;
            self.ring[1][self.start] = r;
            self.start = (self.start + 1) % SHADER_FFT_WINDOW;
        }
    }

    /// One rendered frame: transform the history, normalise and smooth. `fps` is the frame rate (`GetFps()`),
    /// `attack` / `decay` the state's `FFTAttack` / `FFTDecay` (0.5 / 0.7 unless the preset says otherwise). A
    /// non-finite `fps` or one below `1e-8` is treated as `1e-8`; a non-finite `attack` / `decay` as 0.5 / 0.7.
    pub fn update(&mut self, fps: f32, attack: f32, decay: f32) {
        let fps = if fps.is_finite() { fps.max(1e-8) } else { 1e-8 };
        let attack = if attack.is_finite() { attack } else { 0.5 };
        let decay = if decay.is_finite() { decay } else { 0.7 };

        let mut spec = [[0.0f32; SHADER_FFT_BINS]; 2];
        let mut wave = vec![0.0f32; SHADER_FFT_WINDOW];
        for (ring, s) in self.ring.iter().zip(spec.iter_mut()) {
            let (tail, head) = wave.split_at_mut(SHADER_FFT_WINDOW - self.start);
            tail.copy_from_slice(&ring[self.start..]);
            head.copy_from_slice(&ring[..self.start]);
            self.fft.time_to_frequency_domain(&wave, s);
        }

        let mut mono_spec = [0.0f32; SHADER_FFT_BINS];
        for ((m, l), r) in mono_spec.iter_mut().zip(&spec[0]).zip(&spec[1]) {
            *m = (l + r) * 0.5;
        }
        // RMS of the mono spectrum, smoothed with a fast attack (tau 2 s) and a slow release (tau 5 s)
        let mut sum_sq = 0.0f32;
        for m in &mono_spec {
            sum_sq += m * m;
        }
        let rms = (sum_sq / SHADER_FFT_BINS as f32).sqrt().max(1e-5);
        let tau = if rms > self.rms { 2.0f32 } else { 5.0 };
        let rms_decay = (-1.0f32 / fps / tau).exp();
        self.rms = self.rms * rms_decay + rms * (1.0 - rms_decay);

        let decay_factor = (1.0f32 - decay) * (1.0 - decay);
        let attack_per_frame = 1.0f32 - (1.0f32 - attack).powf(60.0 / fps);
        let decay_per_frame = 1.0f32 - (1.0f32 - decay_factor).powf(60.0 / fps);
        for (i, (s, m)) in self.smoothed.iter_mut().zip(&mono_spec).enumerate() {
            let mut mono = *m;
            mono = (mono / self.rms) * SCALING;
            mono -= NOISE_FLOOR;
            if mono < 0.0 {
                mono = 0.0;
            }
            if i < BASS_RAMP_END {
                let t = i as f32 / BASS_RAMP_END as f32;
                mono *= 0.15 + 0.85 * (t * t);
            }
            if mono > *s {
                *s += (mono - *s) * attack_per_frame;
            } else {
                *s += (mono - *s) * decay_per_frame;
            }
            if *s < VISIBLE_FLOOR {
                *s = 0.0;
            }
        }

        // peak hold: a bin holds for `fps * (.25 + .75 * decay)` frames, then falls with a 0.3 s time constant
        let hold_frames = (f64::from(fps) * (0.25 + f64::from(decay) * 0.75)) as i32;
        let fall = ((f64::from(-1.0f32 / fps)) / 0.3).exp();
        for i in 0..SHADER_FFT_BINS {
            if self.smoothed[i] >= self.peak[i] {
                self.peak[i] = self.smoothed[i];
                self.peak_hold[i] = hold_frames;
            } else if self.peak_hold[i] > 0 {
                self.peak_hold[i] -= 1;
            } else {
                self.peak[i] = (f64::from(self.peak[i]) * fall) as f32;
                if self.peak[i] < 0.0 {
                    self.peak[i] = 0.0;
                }
            }
        }
    }

    /// Row 0 of `sampler_fft`: the smoothed spectrum (`m_fFFTSmoothed`).
    pub fn smoothed(&self) -> &[f32; SHADER_FFT_BINS] {
        &self.smoothed
    }

    /// Row 1 of `sampler_fft`: the peak-hold spectrum (`m_fFFTPeak`).
    pub fn peak(&self) -> &[f32; SHADER_FFT_BINS] {
        &self.peak
    }
}
