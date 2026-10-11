//! Port of MD2's `FFT` class (`fft.cpp`): radix-2 decimation-in-time FFT over real input,
//! optional sine-bell envelope, optional log "equalize" table, magnitude output.
//!
//! All arithmetic is `f32` in the same order as the C++ so results track the reference
//! to the last ulp or two.

use std::f32::consts::PI;

/// Which flavour of constants / magnitude function to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FftStyle {
    /// `fft.cpp` verbatim: `sqrtf(re*re + im*im)`, literal `6.2831853f` / `1.5707963268f` (= TAU / FRAC_PI_2 as f32)
    /// in the envelope.
    Md2,
    /// What projectM's port of the same FFT computes: `std::abs(complex<float>)`
    /// (= `hypotf`) and `PI`-derived envelope constants.
    ProjectM,
}

/// Real-input FFT with precomputed tables and reusable scratch buffers (the transform runs
/// every frame, so it must not allocate).
pub(crate) struct Fft {
    samples_in: usize,
    nfreq: usize,
    style: FftStyle,
    envelope: Option<Vec<f32>>,
    equalize: Option<Vec<f32>>,
    bitrev: Vec<usize>,
    cossin: Vec<(f32, f32)>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Fft {
    /// `FFT::Init(samples_in, samples_out, bEqualize, envelope_power)`.
    /// `samples_out` must be a power of two; the transform size is `2*samples_out`.
    /// `envelope_power <= 0` disables the envelope.
    pub(crate) fn new(
        samples_in: usize,
        samples_out: usize,
        equalize: bool,
        envelope_power: f32,
        style: FftStyle,
    ) -> Self {
        let nfreq = samples_out * 2;
        assert!(
            nfreq.is_power_of_two(),
            "FFT size is chosen by this crate and is a power of two"
        );
        let mut f = Fft {
            samples_in,
            nfreq,
            style,
            envelope: None,
            equalize: None,
            bitrev: Vec::new(),
            cossin: Vec::new(),
            re: vec![0.0; nfreq],
            im: vec![0.0; nfreq],
        };
        f.init_bitrev();
        f.init_cossin();
        if envelope_power > 0.0 {
            f.init_envelope(envelope_power);
        }
        if equalize {
            f.init_equalize();
        }
        f
    }

    fn init_equalize(&mut self) {
        let scaling = -0.02f32;
        let half = self.nfreq / 2;
        let inv_half = 1.0f32 / half as f32;
        self.equalize = Some(
            (0..half)
                .map(|i| scaling * ((half - i) as f32 * inv_half).ln())
                .collect(),
        );
    }

    fn init_envelope(&mut self, power: f32) {
        let (mult, off) = match self.style {
            FftStyle::Md2 => (
                1.0f32 / self.samples_in as f32 * std::f32::consts::TAU,
                std::f32::consts::FRAC_PI_2,
            ),
            FftStyle::ProjectM => (1.0f32 / self.samples_in as f32 * 2.0f32 * PI, PI * 0.5f32),
        };
        self.envelope = Some(
            (0..self.samples_in)
                .map(|i| {
                    let b = 0.5f32 + 0.5f32 * (i as f32 * mult - off).sin();
                    if power == 1.0 { b } else { b.powf(power) }
                })
                .collect(),
        );
    }

    fn init_bitrev(&mut self) {
        let n = self.nfreq;
        let mut t: Vec<usize> = (0..n).collect();
        let mut j = 0usize;
        for i in 0..n {
            if j > i {
                t.swap(i, j);
            }
            let mut m = n >> 1;
            while m >= 1 && j >= m {
                j -= m;
                m >>= 1;
            }
            j += m;
        }
        self.bitrev = t;
    }

    fn init_cossin(&mut self) {
        let mut dft = 2usize;
        while dft <= self.nfreq {
            let theta = -2.0f32 * PI / dft as f32;
            self.cossin.push((theta.cos(), theta.sin()));
            dft <<= 1;
        }
    }

    /// `time_to_frequency_domain`: `wave` must hold at least `samples_in` values and `out`
    /// at least `samples_out` (half the transform size); the first `samples_out` magnitudes
    /// are written to `out`.
    pub(crate) fn time_to_frequency_domain(&mut self, wave: &[f32], out: &mut [f32]) {
        let n = self.nfreq;
        assert!(
            wave.len() >= self.samples_in && out.len() >= n / 2,
            "caller passes buffers sized for this transform"
        );
        let re = &mut self.re;
        let im = &mut self.im;
        re.fill(0.0);
        im.fill(0.0);
        for (r, &idx) in re.iter_mut().zip(&self.bitrev) {
            if idx < self.samples_in {
                *r = match &self.envelope {
                    Some(e) => wave[idx] * e[idx],
                    None => wave[idx],
                };
            }
        }
        let mut dftsize = 2usize;
        let mut t = 0usize;
        while dftsize <= n {
            let (wpr, wpi) = self.cossin[t];
            let mut wr = 1.0f32;
            let mut wi = 0.0f32;
            let h = dftsize >> 1;
            for m in 0..h {
                let mut i = m;
                while i < n {
                    let j = i + h;
                    let tr = wr * re[j] - wi * im[j];
                    let ti = wr * im[j] + wi * re[j];
                    re[j] = re[i] - tr;
                    im[j] = im[i] - ti;
                    re[i] += tr;
                    im[i] += ti;
                    i += dftsize;
                }
                let wt = wr;
                wr = wt * wpr - wi * wpi;
                wi = wi * wpr + wt * wpi;
            }
            dftsize <<= 1;
            t += 1;
        }
        for i in 0..n / 2 {
            let mag = match self.style {
                FftStyle::Md2 => (re[i] * re[i] + im[i] * im[i]).sqrt(),
                FftStyle::ProjectM => re[i].hypot(im[i]),
            };
            out[i] = match &self.equalize {
                Some(e) => e[i] * mag,
                None => mag,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    }

    /// FFT against a straightforward f64 DFT.
    #[test]
    fn fft_matches_naive_dft() {
        let n_in = 480;
        let mut fft = Fft::new(n_in, 512, false, -1.0, FftStyle::Md2);
        let mut seed = 7;
        let x: Vec<f32> = (0..n_in).map(|_| rng(&mut seed) * 100.0).collect();
        let mut out = vec![0.0f32; 512];
        fft.time_to_frequency_domain(&x, &mut out);
        for k in [0usize, 1, 2, 17, 100, 255, 256, 400, 511] {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, &v) in x.iter().enumerate() {
                let ph = -2.0 * std::f64::consts::PI * (k * n) as f64 / 1024.0;
                re += v as f64 * ph.cos();
                im += v as f64 * ph.sin();
            }
            let want = (re * re + im * im).sqrt();
            assert!(
                (out[k] as f64 - want).abs() < 1e-3 * want.max(1.0),
                "bin {k}: {} vs {want}",
                out[k]
            );
        }
    }

    #[test]
    fn fft_equalize_and_envelope_tables() {
        // impulse at idx 0 -> flat magnitude = envelope[0]*1 = 0.5 - 0.5 = 0 (bell starts at 0)
        let mut fft = Fft::new(480, 512, true, 1.0, FftStyle::ProjectM);
        let mut x = vec![0.0f32; 480];
        x[240] = 1.0; // envelope peak = 1.0
        let mut out = vec![0.0f32; 512];
        fft.time_to_frequency_domain(&x, &mut out);
        // equalize[i] = -0.02 * ln((512-i)/512)
        for i in [0usize, 1, 100, 511] {
            let eq = -0.02f32 * ((512 - i) as f32 / 512.0).ln();
            assert!((out[i] - eq).abs() < 1e-5, "bin {i}: {} vs {eq}", out[i]);
        }
        assert_eq!(out[0], 0.0);
    }

    #[test]
    fn repeated_transforms_do_not_leak_scratch_state() {
        let mut fft = Fft::new(480, 512, true, 1.0, FftStyle::ProjectM);
        let mut seed = 3;
        let x: Vec<f32> = (0..480).map(|_| rng(&mut seed)).collect();
        let (mut a, mut b) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        fft.time_to_frequency_domain(&x, &mut a);
        fft.time_to_frequency_domain(&[0.5; 480], &mut b);
        fft.time_to_frequency_domain(&x, &mut b);
        assert_eq!(a, b);
    }
}
