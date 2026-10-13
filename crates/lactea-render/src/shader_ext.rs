// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shader inputs that are not part of the frozen [`lactea_abi::MilkUniforms`] block: for MilkDrop 3 the mouse
//! (`_c14`), `q33..q64` (`_qi.._qp`), the shader FFT and the per-state shader time; for BeatDrop (MilkDrop 2 mode) the
//! mouse (`_c14`), time / date (`_c15` / `_c16`) and the spectrum / waveform texture. Everything here is neutral
//! (`mouse = (-1, -1, 0, 0)`, zero `q`, no FFT update, absolute time) outside those modes.

use lactea_abi::{FFT_BINS, MilkExtUniforms};
use std::sync::Arc;

/// Texels per row of BeatDrop's `sampler_fft` (`MY_FFT_SHADER_SAMPLES`); equals `lactea_audio::SHADER_FFT_BINS`.
pub const BEATDROP_FFT_BINS: usize = 1024;
/// Texels per row of BeatDrop's `sampler_wave` (`NUM_WAVEFORM_SAMPLES`); equals `lactea_audio::SHADER_WAVE_SAMPLES`.
pub const BEATDROP_WAVE_SAMPLES: usize = 480;

/// BeatDrop's two shader textures, both `R32F` with two rows (`plugin.cpp`, `m_lpFFTTexture` / `m_lpWaveTexture`,
/// refilled every frame by `DoCustomSoundAnalysis`): `sampler_fft` is [`BEATDROP_FFT_BINS`] x 2 (row 0 the smoothed
/// spectrum, row 1 the peak hold) and `sampler_wave` [`BEATDROP_WAVE_SAMPLES`] x 2 (row 0 left, row 1 right, the
/// waveform divided by 128). The renderer keeps them as two textures of their own.
#[derive(Clone, Debug, PartialEq)]
pub struct BeatDropTexture {
    /// Row 0 of `sampler_fft`: `m_fFFTSmoothed`.
    pub spectrum: [f32; BEATDROP_FFT_BINS],
    /// Row 1 of `sampler_fft`: `m_fFFTPeak`.
    pub peak: [f32; BEATDROP_FFT_BINS],
    /// Rows 0 (left) and 1 (right) of `sampler_wave`.
    pub wave: [[f32; BEATDROP_WAVE_SAMPLES]; 2],
}

impl BeatDropTexture {
    /// The texels in upload order: the two rows of `sampler_fft` ([`BEATDROP_FFT_BINS`] each), then the two rows of
    /// `sampler_wave` ([`BEATDROP_WAVE_SAMPLES`] each): `2 * (BEATDROP_FFT_BINS + BEATDROP_WAVE_SAMPLES)` floats.
    pub fn texels(&self) -> Vec<f32> {
        let mut t = Vec::with_capacity(2 * (BEATDROP_FFT_BINS + BEATDROP_WAVE_SAMPLES));
        t.extend_from_slice(&self.spectrum);
        t.extend_from_slice(&self.peak);
        for w in &self.wave {
            t.extend_from_slice(w);
        }
        t
    }
}

/// See the module docs. One per shader pass owner: [`crate::FrameInput::ext`] for the state the frame is built for,
/// [`crate::Md2OldPass::ext`] for the outgoing state of a MilkDrop 2 / 3 blend.
#[derive(Clone, Debug, PartialEq)]
pub struct ShaderExt {
    /// `_c14` = `(x, y, held, released)`: `x = px/W`, `y = py/H` (top origin), `-1, -1` outside the window.
    pub mouse: [f32; 4],
    /// `q33..q64` (`q_hi[0] == q33`) of the state after its per-frame code (`_qi.._qp`).
    pub q_hi: [f32; 32],
    /// What `_c2.x` shows instead of [`crate::FrameInput::time`]: seconds since this state's preset started (the
    /// renderer wraps it modulo 10000 like it does the absolute time). `None` = `FrameInput::time`.
    pub time: Option<f64>,
    /// The smoothed shader FFT (`sampler_fft`, [`FFT_BINS`] values), uploaded to the renderer's FFT texture before
    /// the frame is drawn. `None` keeps the texture as it is (zeros until the first upload). The texture is
    /// renderer-global: for [`crate::Md2OldPass::ext`] this field is ignored, the new state's one is used.
    pub fft: Option<Arc<[f32; FFT_BINS]>>,
    /// BeatDrop's spectrum / waveform texture (`CompatRules::md2_shader_inputs`), uploaded like [`fft`](Self::fft)
    /// (renderer-global, ignored for [`crate::Md2OldPass::ext`]). `None` keeps the texture as it is.
    pub beatdrop: Option<Arc<BeatDropTexture>>,
    /// BeatDrop's `_c15` (`hour, minute, second, totalSeconds`) and `_c16` (`year, month, day, weekday`); they travel
    /// in the first two `q_hi` registers of the extension block, which nothing else uses outside MilkDrop 3.
    /// `None` leaves `q_hi` as it is.
    pub time_date: Option<[[f32; 4]; 2]>,
    /// The `_mxd3qxa .. _mxd3qsc` audio-envelope / beat-latch float4s (the MilkDrop 3 block).
    pub mxd3q: [[f32; 4]; 7],
}

impl Default for ShaderExt {
    fn default() -> Self {
        Self {
            mouse: [-1.0, -1.0, 0.0, 0.0],
            q_hi: [0.0; 32],
            time: None,
            fft: None,
            beatdrop: None,
            time_date: None,
            mxd3q: [[0.0; 4]; 7],
        }
    }
}

impl ShaderExt {
    /// The uniform block uploaded at `@group(0) @binding(1)`.
    pub fn uniforms(&self) -> MilkExtUniforms {
        let mut u = MilkExtUniforms {
            mouse: self.mouse,
            mxd3q: self.mxd3q,
            ..Default::default()
        };
        for (i, bank) in u.q_hi.iter_mut().enumerate() {
            bank.copy_from_slice(&self.q_hi[i * 4..i * 4 + 4]);
        }
        if let Some(td) = self.time_date {
            u.q_hi[0] = td[0];
            u.q_hi[1] = td[1];
        }
        u
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_banks_pack_four_by_four() {
        let mut e = ShaderExt::default();
        for (i, q) in e.q_hi.iter_mut().enumerate() {
            *q = 33.0 + i as f32;
        }
        let u = e.uniforms();
        assert_eq!(u.q_hi[0], [33.0, 34.0, 35.0, 36.0]);
        assert_eq!(u.q_hi[7], [61.0, 62.0, 63.0, 64.0]);
        assert_eq!(u.mouse, [-1.0, -1.0, 0.0, 0.0]);
    }
}
