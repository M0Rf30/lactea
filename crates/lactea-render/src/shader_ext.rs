// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! MilkDrop 3 shader inputs that are not part of the frozen [`lactea_abi::MilkUniforms`] block: the mouse
//! (`_c14`), `q33..q64` (`_qi.._qp`), the shader FFT and the per-state shader time. Everything here is neutral
//! (`mouse = (-1, -1, 0, 0)`, zero `q`, no FFT update, absolute time) outside MilkDrop 3.

use lactea_abi::{FFT_BINS, MilkExtUniforms};
use std::sync::Arc;

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
