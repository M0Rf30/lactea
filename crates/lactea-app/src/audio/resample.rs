//! Streaming windowed-sinc sample-rate converter (interleaved f32), used only when the playback
//! device cannot run at the file's native rate.

const HALF_TAPS: f64 = 16.0;
const PHASES: usize = 512;

pub struct StreamResampler {
    channels: usize,
    ratio: f64, // input frames advanced per output frame
    half: usize,
    taps: usize,
    /// `(PHASES + 1)` rows of `taps` weights (row p = fractional position p / PHASES).
    table: Vec<f32>,
    /// Interleaved input frames, starting at absolute frame `base`.
    buf: Vec<f32>,
    base: i64,
    /// Absolute input position (frames) of the next output sample.
    pos: f64,
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

fn blackman(t: f64) -> f64 {
    // t in [-1, 1]
    if t.abs() >= 1.0 {
        return 0.0;
    }
    let a = std::f64::consts::PI * t;
    0.42 + 0.5 * a.cos() + 0.08 * (2.0 * a).cos()
}

impl StreamResampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Self {
        let ratio = in_rate as f64 / out_rate as f64;
        // Low-pass at the lower Nyquist when decimating.
        let cutoff = (1.0 / ratio).min(1.0);
        let half = (HALF_TAPS / cutoff).ceil() as usize;
        let taps = 2 * half;
        let mut table = vec![0.0f32; (PHASES + 1) * taps];
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let row = &mut table[p * taps..(p + 1) * taps];
            let mut sum = 0.0;
            let mut vals = vec![0.0f64; taps];
            for (k, v) in vals.iter_mut().enumerate() {
                let x = k as f64 - (half as f64 - 1.0) - frac;
                *v = cutoff * sinc(cutoff * x) * blackman(x / half as f64);
                sum += *v;
            }
            for (r, v) in row.iter_mut().zip(vals) {
                *r = (v / sum) as f32;
            }
        }
        Self {
            channels,
            ratio,
            half,
            taps,
            table,
            buf: vec![0.0; half * channels],
            base: -(half as i64),
            pos: 0.0,
        }
    }

    /// Consume `input` (interleaved) and append every output frame that is now computable.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.channels;
        self.buf.extend_from_slice(input);
        let avail = self.buf.len() / ch;
        let end = self.base + avail as i64; // exclusive absolute index
        loop {
            let i0 = self.pos.floor() as i64;
            // Taps cover i0 - (half-1) ..= i0 + half.
            if i0 + self.half as i64 >= end {
                break;
            }
            let frac = self.pos - i0 as f64;
            let ph = frac * PHASES as f64;
            let p0 = (ph.floor() as usize).min(PHASES - 1);
            let w = (ph - p0 as f64) as f32;
            let r0 = &self.table[p0 * self.taps..(p0 + 1) * self.taps];
            let r1 = &self.table[(p0 + 1) * self.taps..(p0 + 2) * self.taps];
            let first = (i0 - (self.half as i64 - 1) - self.base) as usize;
            for c in 0..ch {
                let mut acc = 0.0f32;
                for k in 0..self.taps {
                    let wk = r0[k] + (r1[k] - r0[k]) * w;
                    acc += wk * self.buf[(first + k) * ch + c];
                }
                out.push(acc);
            }
            self.pos += self.ratio;
        }
        // Drop frames that can no longer be referenced.
        let keep_from = self.pos.floor() as i64 - (self.half as i64 - 1);
        if keep_from > self.base {
            let drop_frames = ((keep_from - self.base) as usize).min(avail);
            self.buf.drain(..drop_frames * ch);
            self.base += drop_frames as i64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, freq: f64, n: usize, ch: usize) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let s =
                    (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32 * 0.5;
                std::iter::repeat_n(s, ch)
            })
            .collect()
    }

    fn rms_err(out: &[f32], rate_out: u32, freq: f64, ch: usize, skip: usize) -> f64 {
        let n = out.len() / ch;
        let mut e = 0.0;
        let mut cnt = 0.0;
        for i in skip..n - skip {
            let want = (2.0 * std::f64::consts::PI * freq * i as f64 / rate_out as f64).sin() * 0.5;
            let d = out[i * ch] as f64 - want;
            e += d * d;
            cnt += 1.0;
        }
        (e / cnt).sqrt()
    }

    #[test]
    fn upsample_44k1_to_48k_is_clean_and_in_phase() {
        let mut r = StreamResampler::new(44_100, 48_000, 2);
        let input = tone(44_100, 1_000.0, 44_100, 2);
        let mut out = Vec::new();
        // Feed in odd-sized blocks to exercise the streaming state.
        for block in input.chunks(2 * 733) {
            r.process(block, &mut out);
        }
        assert!(
            out.len() / 2 >= 47_900 && out.len() / 2 <= 48_000,
            "{}",
            out.len() / 2
        );
        let e = rms_err(&out, 48_000, 1_000.0, 2, 200);
        assert!(e < 2e-3, "rms error {e}");
        // Both channels identical.
        assert!(out.as_chunks::<2>().0.iter().all(|f| f[0] == f[1]));
    }

    #[test]
    fn downsample_96k_to_44k1_attenuates_above_nyquist() {
        let mut r = StreamResampler::new(96_000, 44_100, 1);
        let mut out = Vec::new();
        r.process(&tone(96_000, 1_000.0, 96_000, 1), &mut out);
        assert!(rms_err(&out, 44_100, 1_000.0, 1, 200) < 3e-3);
        let mut r = StreamResampler::new(96_000, 44_100, 1);
        let mut out = Vec::new();
        r.process(&tone(96_000, 30_000.0, 96_000, 1), &mut out);
        let rms = (out[400..out.len() - 400]
            .iter()
            .map(|s| (*s as f64).powi(2))
            .sum::<f64>()
            / (out.len() - 800) as f64)
            .sqrt();
        assert!(rms < 5e-3, "30 kHz leaked through at {rms}");
    }

    #[test]
    fn identity_rate_is_transparent() {
        let mut r = StreamResampler::new(48_000, 48_000, 1);
        let input = tone(48_000, 440.0, 4_800, 1);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        for i in 100..out.len() - 100 {
            assert!((out[i] - input[i]).abs() < 1e-4, "i={i}");
        }
    }
}
