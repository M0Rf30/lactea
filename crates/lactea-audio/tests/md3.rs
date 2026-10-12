//! MilkDrop 3 audio path: `md3_spectrum`, `ShaderFft` and `AnalysisMode::Md3`.
//!
//! The spectrum / shader FFT numbers are the behavioural spec's test vectors (a float32 model
//! of the 3.37 data path); everything else is derived from the documented formulas.

use lactea_audio::*;

const SR: u64 = 44100;

fn md3() -> Analyzer {
    Analyzer::with_mode(SR as u32, AnalysisMode::Md3)
}

/// Frame `n` worth of stereo samples (interleaved) from a generator `g(sample_index, ch)`.
fn frame_pcm(n: u64, fps: u64, g: &dyn Fn(u64, usize) -> f32) -> Vec<f32> {
    let (s, e) = (n * SR / fps, (n + 1) * SR / fps);
    let mut v = Vec::with_capacity(((e - s) * 2) as usize);
    for i in s..e {
        v.push(g(i, 0));
        v.push(g(i, 1));
    }
    v
}

fn sine(freq: f32, amp: f32) -> impl Fn(u64, usize) -> f32 {
    move |i, _| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / SR as f32).sin()
}

/// The fs/4 tone of the spec vectors after the capture quantiser: `0, 64, 0, -64, ...`.
fn fs4_tone() -> [f32; WAVE_BUF] {
    let mut w = [0.0; WAVE_BUF];
    for (i, v) in w.iter_mut().enumerate() {
        *v = [0.0, 64.0, 0.0, -64.0][i % 4];
    }
    w
}

fn argmax(v: &[f32]) -> usize {
    (0..v.len())
        .max_by(|&a, &b| v[a].partial_cmp(&v[b]).unwrap())
        .unwrap()
}

fn assert_close(got: f32, want: f32, tol: f32, what: &str) {
    assert!(
        (got - want).abs() <= tol,
        "{what}: {got} vs {want} (tol {tol})"
    );
}

// ---------------------------------------------------------------------------------------------
// md3_spectrum
// ---------------------------------------------------------------------------------------------

#[test]
fn md3_spectrum_of_the_fs4_tone() {
    let sp = md3_spectrum(&fs4_tone());
    assert_eq!(argmax(&sp), 256);
    assert_close(sp[256], 127.7609, 127.7609 * 1e-3, "peak bin 256");
    for (bin, want) in [
        (254, 51.4947),
        (255, 103.1458),
        (257, 104.315),
        (258, 52.6687),
    ] {
        assert_close(sp[bin], want, 1e-2, &format!("bin {bin}"));
    }
    for (bin, &v) in sp.iter().enumerate().skip(261) {
        assert!(v < 1.2, "bin {bin} = {v}");
    }
}

#[test]
fn md3_spectrum_matches_an_f64_dft_of_the_enveloped_waveform() {
    // Independent of the f32 radix-2 code: 576 samples x Hann(576), zero padded to 1024,
    // DFT, times -0.02 ln((512-k)/512). A signal with energy up to Nyquist also shows that the
    // input is not damped (`0.5 * (x[i] + x[i-1])` would remove the alternating part).
    let mut seed = 12345u32;
    let mut w = [0.0f32; WAVE_BUF];
    for (i, v) in w.iter_mut().enumerate() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = ((seed >> 24) as i32 - 128) as f32 / 4.0;
        *v = noise + if i % 2 == 0 { 40.0 } else { -40.0 };
    }
    let sp = md3_spectrum(&w);
    for k in [0usize, 1, 2, 17, 100, 255, 256, 400, 510, 511] {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, &x) in w.iter().enumerate() {
            let env = 0.5
                + 0.5
                    * (n as f64 * 2.0 * std::f64::consts::PI / 576.0 - std::f64::consts::FRAC_PI_2)
                        .sin();
            let ph = -2.0 * std::f64::consts::PI * (k * n) as f64 / 1024.0;
            re += x as f64 * env * ph.cos();
            im += x as f64 * env * ph.sin();
        }
        let eq = -0.02 * ((512 - k) as f64 / 512.0).ln();
        let want = eq * (re * re + im * im).sqrt();
        assert!(
            (sp[k] as f64 - want).abs() <= 1e-3 * want.max(0.1),
            "bin {k}: {} vs {want}",
            sp[k]
        );
    }
    assert_eq!(sp[0], 0.0, "equalize[0] = -0.02 ln 1 = 0");
    assert!(sp[511] > 1.0, "the Nyquist component of the input survives");
}

#[test]
fn md3_spectrum_of_silence_is_zero() {
    assert!(md3_spectrum(&[0.0; WAVE_BUF]).iter().all(|&v| v == 0.0));
}

// ---------------------------------------------------------------------------------------------
// ShaderFft
// ---------------------------------------------------------------------------------------------

/// Defaults of a preset without `FFTAttack` / `FFTDecay`.
const ATTACK: f32 = 0.5;
const DECAY: f32 = 0.7;

fn tone_spectrum() -> [f32; SPECTRUM_BINS] {
    md3_spectrum(&fs4_tone())
}

#[test]
fn shader_fft_tone_frames_one_two_three_and_six() {
    // S[252..=260] after frame N, then get_fft(0.5) (= average of S[255], S[256]).
    let rows: [(usize, [f32; 9], f32); 4] = [
        (
            1,
            [
                0.0, 0.0, 0.1218, 0.4888, 0.7500, 0.5000, 0.2812, 0.1875, 0.1055,
            ],
            0.6194,
        ),
        (
            2,
            [
                0.0917, 0.1833, 0.3117, 0.6111, 0.9375, 0.6523, 0.4373, 0.2928, 0.1911,
            ],
            0.7743,
        ),
        (
            3,
            [
                0.1398, 0.2750, 0.4295, 0.6801, 0.9844, 0.7168, 0.5125, 0.3615, 0.2511,
            ],
            0.8322,
        ),
        (
            6,
            [
                0.2587, 0.3870, 0.5493, 0.7473, 0.9998, 0.7491, 0.5604, 0.4182, 0.3110,
            ],
            0.8735,
        ),
    ];
    let spec = tone_spectrum();
    let mut fft = ShaderFft::new();
    for frame in 1..=6 {
        let s = *fft.step(&spec, ATTACK, DECAY);
        assert_eq!(&s, fft.state());
        let Some((_, want, mid)) = rows.iter().find(|r| r.0 == frame) else {
            continue;
        };
        for (j, w) in want.iter().enumerate() {
            assert_close(
                s[252 + j],
                *w,
                1e-4,
                &format!("frame {frame} S[{}]", 252 + j),
            );
        }
        assert_close(
            fft.get_fft(0.5),
            *mid,
            1e-4,
            &format!("frame {frame} get_fft(0.5)"),
        );
        // texel 256 has its centre at (256 + 0.5) / 512
        assert_eq!(fft.get_fft(256.5 / 512.0), s[256]);
    }
}

#[test]
fn shader_fft_peak_bin_limit_cycle() {
    // S[256] over frames 1..=40: converges to 1.0, saturates, dips to the decay gain 0.97 and
    // recovers, a period-11 limit cycle in float32 (frames 14, 25, 36 = 0.97).
    const WANT: [f32; 40] = [
        0.75, 0.9375, 0.98438, 0.99609, 0.99902, 0.99976, 0.99994, 0.99998, 1.0, 1.0, 1.0, 1.0,
        1.0, 0.97, 0.9925, 0.99813, 0.99953, 0.99988, 0.99997, 0.99999, 1.0, 1.0, 1.0, 1.0, 0.97,
        0.9925, 0.99813, 0.99953, 0.99988, 0.99997, 0.99999, 1.0, 1.0, 1.0, 1.0, 0.97, 0.9925,
        0.99813, 0.99953, 0.99988,
    ];
    let spec = tone_spectrum();
    let mut fft = ShaderFft::new();
    let mut got = [0.0f32; 40];
    for g in &mut got {
        *g = fft.step(&spec, ATTACK, DECAY)[256];
    }
    for (n, (g, w)) in got.iter().zip(WANT).enumerate() {
        // the vector lists 5 decimals
        assert_close(*g, w, 6e-6, &format!("frame {} S[256]", n + 1));
    }
    for n in 13..40 - 11 {
        assert_eq!(got[n], got[n + 11], "period 11 at frame {}", n + 1);
    }
}

#[test]
fn shader_fft_silence_after_the_tone_diffuses_before_it_decays() {
    // spectrum 0 -> m = 0.001, every bin gated to 0; only the smear and the decay act
    let spec = tone_spectrum();
    let mut fft = ShaderFft::new();
    for _ in 0..6 {
        fft.step(&spec, ATTACK, DECAY);
    }
    let silent = [0.0f32; SPECTRUM_BINS];
    let s = *fft.step(&silent, ATTACK, DECAY);
    assert_close(s[256], 0.9698, 1e-4, "frame 7 S[256]");
    assert_close(s[255], 0.7492, 1e-4, "frame 7 S[255]");
    assert_close(s[254], 0.5577, 1e-4, "frame 7 S[254]");
    assert_close(s[253], 0.4058, 1e-4, "frame 7 S[253]");
    let s = *fft.step(&silent, ATTACK, DECAY);
    assert_close(s[256], 0.9407, 1e-4, "frame 8 S[256]");
}

#[test]
fn shader_fft_attack_one_decay_zero() {
    // ga = 1.0, gd = 0.9. Frame 2 shows `t == s` taking the decay branch: S[256] drops to 0.9.
    let rows: [[f32; 5]; 3] = [
        [0.1625, 0.6518, 1.0, 0.75, 0.5625],
        [0.5, 0.75, 0.9, 0.675, 0.5062],
        [0.5625, 0.675, 1.0, 0.75, 0.5625],
    ];
    let spec = tone_spectrum();
    let mut fft = ShaderFft::new();
    for (f, want) in rows.iter().enumerate() {
        let s = fft.step(&spec, 1.0, 0.0);
        for (j, w) in want.iter().enumerate() {
            assert_close(
                s[254 + j],
                *w,
                1e-4,
                &format!("frame {} S[{}]", f + 1, 254 + j),
            );
        }
    }
}

#[test]
fn shader_fft_digital_silence_stays_zero() {
    let mut fft = ShaderFft::default();
    assert_eq!(fft, ShaderFft::new());
    let silent = [0.0f32; SPECTRUM_BINS];
    for _ in 0..100 {
        fft.step(&silent, ATTACK, DECAY);
    }
    assert!(fft.state().iter().all(|&v| v == 0.0));
    for pos in [0.0, 0.1, 0.5, 0.9, 1.0] {
        assert_eq!(fft.get_fft(pos), 0.0);
    }
    assert_eq!(fft.get_fft_hz(1000.0), 0.0);
}

#[test]
fn shader_fft_peak_floor_gate_and_in_place_smear() {
    // m is at least 0.001: a 0.0005 bin is v = 0.5 -> t = 0.25 -> S = 0.25 * 0.75
    let mut fft = ShaderFft::new();
    let mut quiet = [0.0f32; SPECTRUM_BINS];
    quiet[100] = 0.0005;
    fft.step(&quiet, ATTACK, DECAY);
    assert_close(fft.state()[100], 0.1875, 1e-6, "floor of m");

    // gate: v = 0.19 is dropped, v = 0.2 passes (t = 0.04). The bins are far from the full-scale
    // one (the in-place smear cascades to the right, but only by a factor ~0.56 per bin).
    let mut fft = ShaderFft::new();
    let mut spec = [0.0f32; SPECTRUM_BINS];
    spec[10] = 1.0;
    spec[200] = 0.19;
    spec[300] = 0.2;
    let s = fft.step(&spec, ATTACK, DECAY);
    assert!(s[200] < 1e-6, "gated bin: {}", s[200]);
    assert_close(s[300], 0.04 * 0.75, 1e-6, "gated-in bin");
    assert_close(s[10], 0.75, 1e-6, "full-scale bin");

    // one isolated bin: the left neighbours are still the old zeros when they are visited,
    // the right ones read the freshly updated S (0.75 * 0.75 * 0.75, 0.5 * 0.75 * 0.75)
    let mut fft = ShaderFft::new();
    let mut spec = [0.0f32; SPECTRUM_BINS];
    spec[100] = 3.0;
    let s = fft.step(&spec, ATTACK, DECAY);
    assert!(s[..100].iter().all(|&v| v == 0.0));
    assert_close(s[100], 0.75, 1e-6, "S[100]");
    assert_close(s[101], 0.421875, 1e-6, "S[101]");
    assert_close(s[102], 0.28125, 1e-6, "S[102]");
    // the next frame is visible on both sides
    let s = fft.step(&spec, ATTACK, DECAY);
    assert!(s[97] > 0.0 && s[98] > 0.0 && s[99] > 0.0);

    // the array ends are not special
    let mut fft = ShaderFft::new();
    let mut spec = [0.0f32; SPECTRUM_BINS];
    spec[0] = 1.0;
    spec[511] = 1.0;
    let s = fft.step(&spec, ATTACK, DECAY);
    assert_close(s[0], 0.75, 1e-6, "S[0]");
    assert_close(s[511], 0.75, 1e-6, "S[511]");
}

#[test]
fn shader_fft_ignores_nan_bins() {
    let mut fft = ShaderFft::new();
    let mut spec = [0.0f32; SPECTRUM_BINS];
    spec[7] = f32::NAN;
    spec[20] = 2.0;
    fft.step(&spec, ATTACK, DECAY);
    assert!(fft.state().iter().all(|v| v.is_finite()));
    assert_close(
        fft.state()[20],
        0.75,
        1e-6,
        "NaN bin does not become the normaliser",
    );
}

#[test]
fn shader_fft_sampling() {
    // an asymmetric ramp-like spectrum so that neighbouring texels differ
    let mut spec = [0.0f32; SPECTRUM_BINS];
    for (i, v) in spec.iter_mut().enumerate() {
        *v = (i + 1) as f32 / SPECTRUM_BINS as f32;
    }
    let mut fft = ShaderFft::new();
    fft.step(&spec, ATTACK, DECAY);
    let s = *fft.state();
    assert!(s[0] != s[511]);

    assert_eq!(fft.get_fft(0.0), s[0]);
    assert_eq!(fft.get_fft(1.0), s[511]);
    assert_eq!(fft.get_fft(-3.0), s[0]);
    assert_eq!(fft.get_fft(7.0), s[511]);
    // the first and last half texel are flat (clamped), the centres are exact
    assert_eq!(fft.get_fft(0.25 / 512.0), s[0]);
    assert_eq!(fft.get_fft(511.75 / 512.0), s[511]);
    assert_eq!(fft.get_fft(100.5 / 512.0), s[100]);
    assert_eq!(fft.get_fft_hz(24000.0), s[511]);
    assert_eq!(fft.get_fft_hz(1.0e9), s[511]);
    assert_eq!(fft.get_fft_hz(-5.0), s[0]);

    // 11025 Hz samples u = 0.459375 -> x = u*512 - 0.5 = 234.7
    let want = s[234] as f64 * 0.3 + s[235] as f64 * 0.7;
    assert!(s[234] != s[235]);
    let got = fft.get_fft_hz(11025.0) as f64;
    assert!((got - want).abs() < 1e-5, "{got} vs {want}");
    assert_eq!(fft.get_fft_hz(11025.0), fft.get_fft(0.459375));

    // against an f64 evaluation of the same definition over the whole range
    for k in 0..=400 {
        let pos = k as f32 / 400.0;
        let x = (pos as f64 * 512.0 - 0.5).clamp(0.0, 511.0);
        let i0 = x.floor() as usize;
        let i1 = (i0 + 1).min(511);
        let fr = x - i0 as f64;
        let want = s[i0] as f64 * (1.0 - fr) + s[i1] as f64 * fr;
        let got = fft.get_fft(pos) as f64;
        assert!((got - want).abs() < 1e-5, "pos {pos}: {got} vs {want}");
    }
}

// ---------------------------------------------------------------------------------------------
// Analyzer, AnalysisMode::Md3
// ---------------------------------------------------------------------------------------------

/// Push a constant level on both channels, analyse once, return the (equal) left/right value of
/// the first 480 samples of the published waveform.
fn dc_wave_f32(level: f32) -> f32 {
    let mut a = md3();
    a.push_pcm_f32(&vec![level; 2 * WAVE_BUF], 2);
    let f = a.update(60.0);
    let v = f.wave[0][0];
    assert!(
        f.wave
            .iter()
            .all(|c| c[..WAVE_VALID].iter().all(|&x| x == v)),
        "constant input -> constant waveform"
    );
    v
}

#[test]
fn md3_quantiser_float_pcm() {
    assert_eq!(md3().mode(), AnalysisMode::Md3);
    for (s, want) in [
        (0.0f32, 0.0f32),
        (0.5, 64.0),
        (1.0, 127.0),
        (2.0, 127.0),
        (1.0e9, 127.0),
        (f32::INFINITY, 127.0),
        (-1.0, -128.0),
        (-1.5, -128.0),
        (-1.0e9, -128.0),
        (f32::NEG_INFINITY, -128.0),
        (0.999, 127.0),     // 127.87 truncates
        (-0.999, -127.0),   // -127.87 truncates toward zero, not to -128
        (0.7, 89.0),        // 89.6
        (-0.7, -89.0),      // -89.6
        (1.0 / 128.0, 1.0), // exactly one LSB
        (0.0078, 0.0),      // 0.998 LSB
        (-0.0078, 0.0),
        (1.0 / 128.0 - 1.0 / 1024.0, 0.0),
    ] {
        assert_eq!(dc_wave_f32(s), want, "float sample {s}");
    }
}

#[test]
fn md3_quantiser_integer_pcm() {
    // i16 full scale 32768: v = s/256 in int8 units
    for (s, want) in [
        (16384i16, 64.0f32),
        (32767, 127.0),
        (-32768, -128.0),
        (-32767, -127.0),
        (255, 0.0),
        (256, 1.0),
        (-1, 0.0),
        (-256, -1.0),
        (-257, -1.0),
    ] {
        let mut a = md3();
        a.push_pcm_i16(&vec![s; 2 * WAVE_BUF], 2);
        assert_eq!(a.update(60.0).wave[0][0], want, "i16 sample {s}");
    }
    // u8: silence 128, v = s - 128 (already integral)
    for (s, want) in [
        (255u8, 127.0f32),
        (0, -128.0),
        (128, 0.0),
        (129, 1.0),
        (127, -1.0),
        (192, 64.0),
    ] {
        let mut a = md3();
        a.push_pcm_u8(&vec![s; 2 * WAVE_BUF], 2);
        assert_eq!(a.update(60.0).wave[1][0], want, "u8 sample {s}");
    }
    // mono is duplicated after quantising
    let mut a = md3();
    a.push_pcm_f32(&vec![0.5; WAVE_BUF], 1);
    let f = a.update(60.0);
    assert_eq!((f.wave[0][0], f.wave[1][0]), (64.0, 64.0));
}

#[test]
fn only_md3_quantises_the_pushed_pcm() {
    for mode in [AnalysisMode::ProjectM, AnalysisMode::BeatDrop] {
        let mut a = Analyzer::with_mode(SR as u32, mode);
        a.push_pcm_f32(&vec![0.999; 2 * WAVE_BUF], 2);
        let w = a.update(60.0).wave[0][0];
        assert!((w - 127.872).abs() < 1e-3, "{mode:?}: {w}");
    }
}

#[test]
fn md3_nan_pcm_is_silence_not_poison() {
    let mut a = md3();
    a.push_pcm_f32(&vec![f32::NAN; 2 * WAVE_BUF], 2);
    let f = a.update(60.0);
    assert!(f.wave[0].iter().all(|&v| v == 0.0));
    assert_eq!((f.bass, f.mid, f.treb), (1.0, 1.0, 1.0));
}

#[test]
fn md3_volume_sensitivity_acts_after_the_quantiser() {
    let mut a = md3();
    assert_eq!(a.volume_sensitivity(), 1.0);
    a.push_pcm_f32(&vec![1.0; 2 * WAVE_BUF], 2);
    // set after the push: the gain is applied at update, to the stored 8-bit values
    a.set_volume_sensitivity(2.0);
    assert_eq!(a.volume_sensitivity(), 2.0);
    let f = a.update(60.0);
    // 127 * 2, not 128 * 2: the clip to 127 happened before the gain
    assert_eq!(f.wave[0][0], 254.0);
    assert_eq!(f.wave[1][0], 254.0);

    // no clamp: the waveform may leave +-128
    let mut a = md3();
    a.set_volume_sensitivity(3.0);
    a.push_pcm_f32(&vec![-1.5; 2 * WAVE_BUF], 2);
    assert_eq!(a.update(60.0).wave[0][0], -384.0);

    let mut a = md3();
    a.set_volume_sensitivity(0.5);
    a.push_pcm_f32(&vec![0.5; 2 * WAVE_BUF], 2);
    assert_eq!(a.update(60.0).wave[0][0], 32.0);
}

#[test]
fn md3_volume_sensitivity_scales_wave_and_spectrum_before_the_analysis() {
    // gain 2.0 is a power of two: aligner, FFT and equalisation scale exactly
    let g = sine(437.3, 0.5);
    let mut a1 = md3();
    let mut a2 = md3();
    a2.set_volume_sensitivity(2.0);
    for n in 0..30u64 {
        let pcm = frame_pcm(n, 60, &g);
        a1.push_pcm_f32(&pcm, 2);
        a2.push_pcm_f32(&pcm, 2);
        let f1 = a1.update(60.0).clone();
        let f2 = a2.update(60.0).clone();
        for ch in 0..2 {
            for i in 0..WAVE_BUF {
                assert_eq!(
                    f2.wave[ch][i],
                    2.0 * f1.wave[ch][i],
                    "n={n} wave[{ch}][{i}]"
                );
            }
            for i in 0..SPECTRUM_BINS {
                assert_eq!(
                    f2.spectrum[ch][i],
                    2.0 * f1.spectrum[ch][i],
                    "n={n} spec[{ch}][{i}]"
                );
            }
        }
        // the bands are ratios to the long-term average: the strongest one does not notice
        assert_eq!(f1.bass, f2.bass, "n={n}");
    }
}

#[test]
fn volume_sensitivity_is_ignored_by_the_other_modes() {
    let g = sine(1000.0, 0.5);
    for mode in [AnalysisMode::ProjectM, AnalysisMode::BeatDrop] {
        let mut a1 = Analyzer::with_mode(SR as u32, mode);
        let mut a2 = Analyzer::with_mode(SR as u32, mode);
        a2.set_volume_sensitivity(4.0);
        assert_eq!(a2.volume_sensitivity(), 4.0);
        for n in 0..20u64 {
            let pcm = frame_pcm(n, 60, &g);
            a1.push_pcm_f32(&pcm, 2);
            a2.push_pcm_f32(&pcm, 2);
            assert_eq!(a1.update(60.0), a2.update(60.0), "{mode:?} frame {n}");
        }
    }
}

#[test]
fn md3_bands_use_the_left_channel_only() {
    // right-only tone: the left spectrum is empty, so imm = 0 and every value is exactly 1.0
    let tone = sine(1000.0, 0.5);
    let mut a = md3();
    let mut last = None;
    for n in 0..60u64 {
        a.push_pcm_f32(
            &frame_pcm(n, 60, &|i, c| if c == 1 { tone(i, c) } else { 0.0 }),
            2,
        );
        last = Some(a.update(60.0).clone());
    }
    let f = last.unwrap();
    assert_eq!((f.bass, f.mid, f.treb), (1.0, 1.0, 1.0));
    assert_eq!((f.bass_att, f.mid_att, f.treb_att), (1.0, 1.0, 1.0));
    assert!(f.spectrum[0].iter().all(|&v| v == 0.0));
    assert!(
        f.spectrum[1].iter().any(|&v| v > 1.0),
        "the right spectrum is still published"
    );

    // what the right channel carries does not change the six values
    let noise = |i: u64, _c: usize| ((i * 2654435761 % 255) as f32 - 127.0) / 200.0;
    let mut both = md3();
    let mut left_only = md3();
    let mut odd = md3();
    for n in 0..40u64 {
        both.push_pcm_f32(&frame_pcm(n, 60, &|i, c| tone(i, c)), 2);
        left_only.push_pcm_f32(
            &frame_pcm(n, 60, &|i, c| if c == 0 { tone(i, c) } else { 0.0 }),
            2,
        );
        odd.push_pcm_f32(
            &frame_pcm(n, 60, &|i, c| if c == 0 { tone(i, c) } else { noise(i, c) }),
            2,
        );
        let (x, y, z) = (
            both.update(60.0).clone(),
            left_only.update(60.0).clone(),
            odd.update(60.0).clone(),
        );
        for f in [&y, &z] {
            assert_eq!(
                (x.bass, x.mid, x.treb, x.bass_att, x.mid_att, x.treb_att),
                (f.bass, f.mid, f.treb, f.bass_att, f.mid_att, f.treb_att),
                "frame {n}"
            );
            assert_eq!(x.spectrum[0], f.spectrum[0]);
            assert_eq!(x.wave[0], f.wave[0]);
        }
    }
}

#[test]
fn md3_silence_is_exactly_one() {
    let mut a = md3();
    let f0 = a.frame().clone();
    assert_eq!(
        (
            f0.bass,
            f0.mid,
            f0.treb,
            f0.bass_att,
            f0.mid_att,
            f0.treb_att
        ),
        (1.0, 1.0, 1.0, 1.0, 1.0, 1.0)
    );
    assert_eq!(f0.vol, (1.0f32 + 1.0 + 1.0) * 0.333);
    for n in 0..100u64 {
        a.push_pcm_f32(&frame_pcm(n, 60, &|_, _| 0.0), 2);
        let f = a.update(60.0);
        assert_eq!(
            (f.bass, f.mid, f.treb, f.bass_att, f.mid_att, f.treb_att),
            (1.0, 1.0, 1.0, 1.0, 1.0, 1.0),
            "frame {n}"
        );
        assert_eq!(f.vol, (1.0f32 + 1.0 + 1.0) * 0.333);
        assert_eq!(f.vol_att, f.vol);
        assert!(f.wave.iter().all(|c| c.iter().all(|&x| x == 0.0)));
        assert!(f.spectrum.iter().all(|c| c.iter().all(|&x| x == 0.0)));
        assert_eq!(f.wave_len, WAVE_VALID);
    }
    // a signal below one 8-bit LSB is digital silence too
    let mut a = md3();
    for n in 0..60u64 {
        a.push_pcm_f32(&frame_pcm(n, 60, &sine(440.0, 0.004)), 2);
        let f = a.update(60.0);
        assert_eq!((f.bass, f.mid, f.treb), (1.0, 1.0, 1.0));
    }
}

#[test]
fn md3_publishes_the_aligned_waveform_and_its_spectra() {
    // The published spectrum is the md3_spectrum of the published (aligned) waveform: the
    // alignment runs before the analysis. A drifting 437.3 Hz tone makes the aligner shift.
    let g = sine(437.3, 0.5);
    let quant = |v: f32| -> f32 {
        if v >= 1.0 {
            127.0
        } else if v < -1.0 {
            -128.0
        } else {
            (v * 128.0).trunc()
        }
    };
    let mut a = md3();
    let mut shifted = 0;
    for n in 0..40u64 {
        a.push_pcm_f32(&frame_pcm(n, 60, &g), 2);
        let f = a.update(60.0).clone();
        for ch in 0..2 {
            assert_eq!(
                f.spectrum[ch],
                md3_spectrum(&f.wave[ch]),
                "frame {n} channel {ch}"
            );
        }
        assert_eq!(f.wave_len, WAVE_VALID);
        // the unaligned window is the newest 576 pushed samples
        let end = (n + 1) * SR / 60;
        let raw: Vec<f32> = (end - WAVE_BUF as u64..end)
            .map(|i| quant(g(i, 0)))
            .collect();
        if f.wave[0][..WAVE_VALID] != raw[..WAVE_VALID] {
            shifted += 1;
            let off = (1..96)
                .find(|&o| f.wave[0][..WAVE_VALID] == raw[o..o + WAVE_VALID])
                .unwrap_or_else(|| panic!("frame {n}: not a shifted window"));
            assert!(off > 0);
            assert!(
                f.wave[0][WAVE_VALID..].iter().all(|&v| v == 0.0),
                "tail zeroed"
            );
        }
    }
    assert!(shifted > 0, "the aligner never moved the window");
}

#[test]
fn md3_fs4_tone_end_to_end() {
    // exact 0, 0.5, 0, -0.5 samples quantise to 0, 64, 0, -64 (the spec's fs/4 tone)
    let mut a = md3();
    let pat = [0.0f32, 0.5, 0.0, -0.5];
    let mut pcm = Vec::new();
    for i in 0..WAVE_BUF {
        pcm.push(pat[i % 4]);
        pcm.push(pat[i % 4]);
    }
    for _ in 0..3 {
        a.push_pcm_f32(&pcm, 2);
        let f = a.update(60.0);
        assert!(
            f.wave[0]
                .iter()
                .all(|&v| v == 0.0 || v == 64.0 || v == -64.0)
        );
        assert_eq!(argmax(&f.spectrum[0]), 256);
        assert_eq!(f.spectrum[0], f.spectrum[1]);
    }
}

#[test]
fn md3_low_tone_after_a_high_tone_raises_bass_and_not_treb() {
    // The 8-bit quantisation noise floor makes every band rise at an onset from digital
    // silence, so the contrast is made against a settled high tone: the loud low tone is a
    // large jump in the bass band and a drop in the treble band.
    let hi = sine(9000.0, 0.5);
    let lo = sine(200.0, 0.8);
    let mut a = md3();
    for n in 0..120u64 {
        a.push_pcm_f32(&frame_pcm(n, 60, &hi), 2);
        a.update(60.0);
    }
    for n in 120..126u64 {
        a.push_pcm_f32(&frame_pcm(n, 60, &lo), 2);
        let f = a.update(60.0);
        assert!(f.bass > 3.0, "frame {n}: bass {}", f.bass);
        assert!(f.treb < 0.1, "frame {n}: treb {}", f.treb);
        assert!(
            f.bass > f.mid && f.mid > f.treb,
            "frame {n}: {} {} {}",
            f.bass,
            f.mid,
            f.treb
        );
        assert_eq!(f.vol, (f.bass + f.mid + f.treb) * 0.333);
    }
}

#[test]
fn md3_loud_constant_level_hits_bass_only() {
    // A constant level (0 Hz) has no alignment shift and no quantisation noise: its energy sits
    // in the lowest bins (bass), the other bands stay below the 0.001 long-average floor -> 1.0.
    // After 60 silent frames (long average 0, frame >= 50 so rate 0.992 at 60 fps):
    // long_avg_k = imm * (1 - r^(k+1)), bass_k = 1 / (1 - r^(k+1)), r = (0.992^30)^(1/60).
    let mut a = md3();
    for n in 0..60u64 {
        a.push_pcm_f32(&frame_pcm(n, 60, &|_, _| 0.0), 2);
        a.update(60.0);
    }
    let r = 0.992f64.powf(0.5);
    let q = 0.2f64.powf(0.5);
    for k in 0..4 {
        a.push_pcm_f32(&vec![0.8; 2 * 735], 2);
        let f = a.update(60.0).clone();
        let want = 1.0 / (1.0 - r.powi(k + 1));
        assert!(
            (f.bass as f64 - want).abs() < 1e-4 * want,
            "k={k}: bass {} vs {want}",
            f.bass
        );
        assert_eq!((f.mid, f.treb), (1.0, 1.0), "k={k}");
        // the short average rises at rate 0.2 while imm > avg: avg_k = imm * (1 - q^(k+1))
        let want_att = (1.0 - q.powi(k + 1)) / (1.0 - r.powi(k + 1));
        assert!(
            (f.bass_att as f64 - want_att).abs() < 1e-4 * want_att,
            "k={k}: bass_att {} vs {want_att}",
            f.bass_att
        );
    }
}

#[test]
fn md3_long_average_rate_switches_at_frame_50() {
    // constant level, 30 fps from frame 0: rate 0.9 while frame < 50, then 0.992
    let mut a = md3();
    let mut bass = Vec::new();
    let mut att0 = 0.0;
    for n in 0..60 {
        a.push_pcm_f32(&vec![0.25; 2 * WAVE_BUF], 2);
        let f = a.update(30.0);
        if n == 0 {
            att0 = f.bass_att;
        }
        bass.push(f.bass);
    }
    for n in [0usize, 10, 40] {
        let want = 1.0 / (1.0 - 0.9f32.powi(n as i32 + 1));
        assert!(
            (bass[n] - want).abs() < 1e-3 * want,
            "n={n}: {} vs {want}",
            bass[n]
        );
    }
    // frame 0: avg = imm * (1 - 0.2) (rising, rate 0.2), long = imm * (1 - 0.9)
    assert!((att0 - 8.0).abs() < 1e-2, "bass_att at frame 0: {att0}");
    assert!(
        bass[55] > bass[49] * 0.99,
        "the slower average takes over after frame 50"
    );
    assert_eq!(a.frame_index(), 60);
}

#[test]
fn md3_smoothing_vectors_t1_and_t2() {
    // Spec vectors T1/T2 (fps 60, relative values do not depend on the magnitude of imm): a
    // constant level gives a constant `imm`, silence gives 0.
    // T1, imm constant from frame 0 (frame < 50: long rate 0.9^0.5):
    const BASS: [f64; 6] = [19.487, 10.000, 6.8406, 5.2632, 4.3184, 3.6900];
    const ATT: [f64; 6] = [10.772, 8.000, 6.2288, 5.0526, 4.2412, 3.6605];
    let mut a = md3();
    for k in 0..6 {
        a.push_pcm_f32(&vec![0.8; 2 * 735], 2);
        let f = a.update(60.0);
        assert!(
            (f.bass as f64 - BASS[k]).abs() < 1e-3 * BASS[k],
            "T1 frame {k}: {}",
            f.bass
        );
        assert!(
            (f.bass_att as f64 - ATT[k]).abs() < 1e-3 * ATT[k],
            "T1 frame {k}: att {}",
            f.bass_att
        );
    }

    // T2, imm = [0,0,0,X,X,X,0,0,0]: long average 0 -> all 1.0; the step is T1 frames 0-2; once
    // imm falls to 0 (<= avg: rate 0.5^0.5) bass is 0 and the short average decays
    let mut a = md3();
    let mut got = Vec::new();
    for n in 0..9 {
        let level = if (3..6).contains(&n) { 0.8 } else { 0.0 };
        a.push_pcm_f32(&vec![level; 2 * 735], 2);
        let f = a.update(60.0);
        got.push((f.bass as f64, f.bass_att as f64));
    }
    for (n, g) in got.iter().enumerate().take(3) {
        assert_eq!(*g, (1.0, 1.0), "frame {n}");
    }
    for (n, want) in [
        (3, (19.487, 10.772)),
        (4, (10.000, 8.000)),
        (5, (6.8406, 6.2288)),
    ] {
        assert!(
            (got[n].0 - want.0).abs() < 1e-3 * want.0,
            "T2 frame {n}: {:?}",
            got[n]
        );
        assert!(
            (got[n].1 - want.1).abs() < 1e-3 * want.1,
            "T2 frame {n}: {:?}",
            got[n]
        );
    }
    for (n, att) in [(6, 4.6427), (7, 3.4604), (8, 2.5793)] {
        assert_eq!(got[n].0, 0.0, "T2 frame {n}");
        assert!(
            (got[n].1 - att).abs() < 1e-3 * att,
            "T2 frame {n}: att {}",
            got[n].1
        );
    }
}

#[test]
fn md3_spectrum_sine_vectors_t4() {
    // Spec vector T4: 576 samples of a 100-amplitude sine at 44.1 kHz used directly (no
    // quantiser): peak bin and the three band sums (bins [0,85) [85,170) [170,256)). The
    // vector was computed in double precision; the f32 transform has a noise floor of about
    // 1e-3 per band sum, so the sidelobe-only bands (< 0.03) are only compared to that level.
    for (freq, peak, imm) in [
        (1000.0f64, 23usize, [49.58f32, 0.0068, 0.0015]),
        (5000.0, 116, [0.0208, 275.10, 0.0233]),
        (12000.0, 279, [0.00025, 0.0030, 0.1867]),
    ] {
        let mut w = [0.0f32; WAVE_BUF];
        for (n, v) in w.iter_mut().enumerate() {
            *v = (100.0 * (2.0 * std::f64::consts::PI * freq * n as f64 / SR as f64).sin()) as f32;
        }
        let sp = md3_spectrum(&w);
        assert_eq!(argmax(&sp), peak, "{freq} Hz");
        for (b, want) in imm.iter().enumerate() {
            let got: f32 = sp[512 * b / 6..512 * (b + 1) / 6].iter().sum();
            assert!(
                (got - want).abs() <= 0.01 * want + 3e-3,
                "{freq} Hz band {b}: {got} vs {want}"
            );
        }
    }
}
