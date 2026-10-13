//! MilkDrop 3 (3.37) pieces of the audio path: the 8-bit capture quantiser, the spectrum of an
//! already prepared waveform, and the smoothed spectrum behind the `get_fft` shader helpers.
//!
//! Behaviour is taken from `docs/audio.md` ("MilkDrop 3 mode"); all arithmetic is `f32` in the
//! operation order of the reference model.

use crate::fft::{Fft, FftStyle};
use crate::{SPECTRUM_BINS, WAVE_BUF};

/// The FFT MD3 runs on every waveform: 576 samples in, 1024-point transform, 512 equalised bins
/// out, raised-cosine envelope `0.5 + 0.5 * sin(i * 2π / 576 - π / 2)`, MD2 `fft.cpp` constants.
pub(crate) fn md3_fft() -> Fft {
    Fft::new(WAVE_BUF, SPECTRUM_BINS, true, 1.0, FftStyle::Md2)
}

/// MD3's capture quantiser applied to a sample already scaled to int8 units (`128 * s`):
/// `s >= 1.0 -> 127`, `s < -1.0 -> -128`, else `(int)(128 * s)` (truncation toward zero).
/// The result is the signed 8-bit value stored as `f32`. A NaN input becomes 0 (the integer
/// conversion saturates instead of leaving a NaN to poison the running averages).
pub(crate) fn quantise_i8(v: f32) -> f32 {
    if v >= 128.0 {
        127.0
    } else if v < -128.0 {
        -128.0
    } else {
        (v as i32) as f32
    }
}

/// MD3 spectrum of an already prepared waveform (signed 8-bit units, i.e. `128 * float PCM`,
/// after the volume sensitivity and the alignment; MD3 3.37, docs/md3.md, shader inputs).
///
/// The 576 samples are multiplied by the envelope
/// `0.5 + 0.5 * sin(i * (1/576 * 6.2831853) - 1.5707963268)`, zero padded to 1024 and
/// transformed (radix 2); bin `i` of the result is `equalize[i] * |X[i]|` with
/// `equalize[i] = -0.02 * ln((512 - i) / 512)`. There is no input damping and no alignment.
///
/// The FFT tables are built once per thread and reused; [`Analyzer`](crate::Analyzer) in
/// [`AnalysisMode::Md3`](crate::AnalysisMode::Md3) keeps its own and produces identical values.
pub fn md3_spectrum(wave: &[f32; WAVE_BUF]) -> [f32; SPECTRUM_BINS] {
    thread_local! {
        static FFT: std::cell::RefCell<Fft> = std::cell::RefCell::new(md3_fft());
    }
    let mut out = [0.0f32; SPECTRUM_BINS];
    FFT.with(|f| f.borrow_mut().time_to_frequency_domain(wave, &mut out));
    out
}

/// Neighbour weights `1 - 0.25 * |k|` of the smear, indexed by `|k| - 1`.
const SMEAR: [f32; 3] = [0.75, 0.5, 0.25];

/// MD3's shader FFT smoother: the 512 floats behind `sampler_fft` / `get_fft` (MD3 3.37,
/// docs/md3.md, shader inputs).
///
/// Every rendered frame the engine feeds it the left-channel [`md3_spectrum`] of that frame and
/// the sticky preset globals `FFTAttack` / `FFTDecay` (they are not stored here: MD3 keeps them
/// outside the state and does not blend them). The state is one instance per engine and survives
/// preset changes; there is no frame rate compensation, the recursion advances once per call.
#[derive(Clone, Debug, PartialEq)]
pub struct ShaderFft {
    s: [f32; SPECTRUM_BINS],
}

impl Default for ShaderFft {
    fn default() -> Self {
        Self::new()
    }
}

impl ShaderFft {
    /// All-zero state (digital silence stays zero forever).
    pub fn new() -> Self {
        ShaderFft {
            s: [0.0; SPECTRUM_BINS],
        }
    }

    /// Advance one frame with the equalised spectrum `spec` (512 bins) and return the new state.
    ///
    /// With `m = max(0.001, max(spec))`, `ga = 0.5 * attack + 0.5`, `gd = 0.1 * decay + 0.9`,
    /// for `i` ascending and the state `S` updated in place:
    ///
    /// * `v = spec[i] / m`, zeroed unless `0.2 <= v` (a NaN is gated away too), `t = v * v`;
    /// * for `k` in `-3..=3` without 0 and `0 <= i + k < 512`:
    ///   `t = max(t, (1 - 0.25 * |k|) * S[i + k])`, reading the partially updated `S` (the three
    ///   left neighbours are already new, the three right ones still old);
    /// * `S[i] = if t > s { s + (t - s) * ga } else { gd * s }` with `s = S[i]`; `t == s` takes
    ///   the decay branch, so a bin that has reached its target decays.
    ///
    /// `attack` / `decay` are used as given (no clamping, as in MD3).
    pub fn step(
        &mut self,
        spec: &[f32; SPECTRUM_BINS],
        attack: f32,
        decay: f32,
    ) -> &[f32; SPECTRUM_BINS] {
        // `x > m` (not `max`) so that a NaN bin cannot become the normaliser
        let mut m = 0.001f32;
        for &x in spec {
            if x > m {
                m = x;
            }
        }
        let ga = 0.5f32 * attack + 0.5f32;
        let gd = 0.1f32 * decay + 0.9f32;
        let s = &mut self.s;
        for i in 0..SPECTRUM_BINS {
            // noise gate `!(0.2 <= v)`: also true for NaN
            let mut v = spec[i] / m;
            if v < 0.2f32 || v.is_nan() {
                v = 0.0;
            }
            let mut t = v * v;
            for (d, w) in SMEAR.iter().enumerate() {
                let k = d + 1;
                if i >= k {
                    t = t.max(w * s[i - k]);
                }
                if i + k < SPECTRUM_BINS {
                    t = t.max(w * s[i + k]);
                }
            }
            let cur = s[i];
            s[i] = if t > cur {
                cur + (t - cur) * ga
            } else {
                gd * cur
            };
        }
        &self.s
    }

    /// The current state, i.e. the 512 texels the shader samples (both rows of the texture).
    pub fn state(&self) -> &[f32; SPECTRUM_BINS] {
        &self.s
    }

    /// CPU reference of the shader macro `get_fft(pos)`: `pos` is clamped to `[0, 1]`, texel
    /// centres sit at `(i + 0.5) / 512`, so `x = clamp(pos * 512 - 0.5, 0, 511)` and the result
    /// is the linear interpolation of `S[floor(x)]` and `S[min(floor(x) + 1, 511)]`.
    pub fn get_fft(&self, pos: f32) -> f32 {
        let u = pos.clamp(0.0, 1.0);
        let x = (u * SPECTRUM_BINS as f32 - 0.5).clamp(0.0, (SPECTRUM_BINS - 1) as f32);
        let i0 = x.floor() as usize;
        let i1 = (i0 + 1).min(SPECTRUM_BINS - 1);
        let fr = x - i0 as f32;
        self.s[i0] * (1.0 - fr) + self.s[i1] * fr
    }

    /// CPU reference of the shader macro `get_fft_hz(hz)`: `get_fft(clamp(hz / 24000, 0, 1))`
    /// (a fixed 48 kHz sample rate, whatever the real capture rate is).
    pub fn get_fft_hz(&self, hz: f32) -> f32 {
        self.get_fft((hz / 24000.0).clamp(0.0, 1.0))
    }
}
