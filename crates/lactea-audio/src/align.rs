//! Waveform alignment: picks the offset (0..96) of the newest 576-sample window that best
//! matches the previous frame's waveform, using a 6-level octave pyramid, so oscilloscope
//! style waves hold still.  Behaviour follows MD2 (and projectM, which reproduces it):
//! weights favour the centre, the search never tries negative offsets, and the result is
//! the first 480 samples starting at the chosen offset.

pub const BUF: usize = 576;
pub const VALID: usize = 480;

pub struct Aligner {
    octaves: usize,
    samples: Vec<usize>,
    spacing: Vec<usize>,
    weights: Vec<Vec<f32>>,
    first_nz: Vec<usize>,
    last_nz: Vec<usize>,
    old_mips: Vec<Vec<f32>>,
    /// Scratch for the incoming waveform's pyramid (reused every frame).
    new_mips: Vec<Vec<f32>>,
}

impl Aligner {
    pub fn new() -> Self {
        // floor(log2(BUF - VALID)) = floor(log2(96)) = 6
        let octaves = ((BUF - VALID) as f64).log2().floor() as usize;
        let mut samples = vec![0; octaves];
        let mut spacing = vec![0; octaves];
        samples[0] = BUF;
        spacing[0] = BUF - VALID;
        for o in 1..octaves {
            samples[o] = samples[o - 1] / 2;
            spacing[o] = spacing[o - 1] / 2;
        }
        let mut a = Aligner {
            octaves,
            samples,
            spacing,
            weights: vec![vec![0.0; BUF]; octaves],
            first_nz: vec![0; octaves],
            last_nz: vec![0; octaves],
            old_mips: vec![vec![0.0; BUF]; octaves],
            new_mips: vec![vec![0.0; BUF]; octaves],
        };
        a.gen_weights();
        a
    }

    fn gen_weights(&mut self) {
        for o in 0..self.octaves {
            let cs = self.samples[o] - self.spacing[o];
            for s in 0..cs {
                // triangular pdf 0..1..0, then steepen (centre matters, edges do not)
                let mut w = if s < cs / 2 {
                    (s * 2) as f32 / cs as f32
                } else {
                    ((cs - 1 - s) * 2) as f32 / cs as f32
                };
                w = (w - 0.8f32) * 5.0f32 + 0.8f32;
                self.weights[o][s] = w.clamp(0.0, 1.0);
            }
            let mut s = 0;
            while s < cs && self.weights[o][s] == 0.0 {
                s += 1;
            }
            self.first_nz[o] = s;
            let mut s = cs - 1;
            while self.weights[o][s] == 0.0 && cs > 1 {
                s -= 1;
            }
            self.last_nz[o] = s;
        }
    }

    fn mips(&self, dst: &mut [Vec<f32>], wave: &[f32; BUF]) {
        dst[0].copy_from_slice(wave);
        for o in 1..self.octaves {
            for s in 0..self.samples[o] {
                dst[o][s] = 0.5f32 * (dst[o - 1][s * 2] + dst[o - 1][s * 2 + 1]);
            }
        }
    }

    fn offset(&self, new: &[Vec<f32>]) -> i32 {
        let mut start: i32 = 0;
        let mut end: i32 = self.spacing[self.octaves - 1] as i32;
        let mut result = 0;
        for o in (0..self.octaves).rev() {
            let mut best: i32 = -1;
            let mut best_err = 0.0f32;
            for s in start..end {
                let mut err = 0.0f32;
                for i in self.first_nz[o]..=self.last_nz[o] {
                    err +=
                        ((new[o][i + s as usize] - self.old_mips[o][i]) * self.weights[o][i]).abs();
                }
                if best == -1 || err < best_err {
                    best = s;
                    best_err = err;
                }
            }
            if o > 0 {
                start = best * 2 - 1;
                end = best * 2 + 2 + 1;
                if start < 0 {
                    start = 0;
                }
                if end > self.spacing[o - 1] as i32 {
                    end = self.spacing[o - 1] as i32;
                }
            } else {
                result = best;
            }
        }
        result
    }

    /// Align in place. After the call only the first [`VALID`] samples are meaningful.
    pub(crate) fn align(&mut self, wave: &mut [f32; BUF]) {
        // `mips` overwrites every entry `offset` reads, so the scratch needs no clearing
        let mut new = std::mem::take(&mut self.new_mips);
        self.mips(&mut new, wave);
        let off = self.offset(&new);
        self.new_mips = new;
        if off > 0 {
            let off = off as usize;
            wave.copy_within(off..off + VALID, 0);
            for v in &mut wave[VALID..] {
                *v = 0.0;
            }
        }
        let mut old = std::mem::take(&mut self.old_mips);
        self.mips(&mut old, wave);
        self.old_mips = old;
    }
}

impl Default for Aligner {
    fn default() -> Self {
        Self::new()
    }
}
