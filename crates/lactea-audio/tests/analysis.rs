use lactea_audio::*;

const SR: u64 = 44100;

fn rng(seed: &mut u32) -> f32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    (*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
}

/// Frame `n` worth of stereo samples (interleaved) from a generator `g(sample_index, ch)`,
/// tiled exactly like `abi::pcm_range`.
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

/// A NaN / infinite sample must not poison the running averages for the rest of the session.
#[test]
fn non_finite_samples_do_not_poison_the_analysis() {
    let mut a = Analyzer::new(SR as u32);
    let mut pcm = frame_pcm(0, 60, &sine(440.0, 0.5));
    pcm[10] = f32::NAN;
    pcm[11] = f32::INFINITY;
    pcm[12] = f32::NEG_INFINITY;
    a.push_pcm_f32(&pcm, 2);
    a.update(60.0);
    let clean = run(&mut a, 1..30, 60, &sine(440.0, 0.5));
    for v in [clean.bass, clean.mid, clean.treb, clean.vol_att] {
        assert!(v.is_finite());
    }
    assert!(clean.spectrum[0].iter().all(|v| v.is_finite()));
}

#[test]
fn non_finite_volume_sensitivity_is_ignored() {
    let mut a = Analyzer::with_mode(SR as u32, AnalysisMode::Md3);
    a.set_volume_sensitivity(2.0);
    a.set_volume_sensitivity(f32::NAN);
    a.set_volume_sensitivity(f32::INFINITY);
    assert_eq!(a.volume_sensitivity(), 2.0);
}

fn run(
    a: &mut Analyzer,
    frames: std::ops::Range<u64>,
    fps: u64,
    g: &dyn Fn(u64, usize) -> f32,
) -> AudioFrame {
    let mut last = None;
    for n in frames {
        a.push_pcm_f32(&frame_pcm(n, fps, g), 2);
        last = Some(a.update(fps as f32).clone());
    }
    last.unwrap()
}

fn band_energy(f: &AudioFrame) -> [f32; 3] {
    let mut e = [0.0; 3];
    for (b, band) in e.iter_mut().enumerate() {
        for i in 512 * b / 6..512 * (b + 1) / 6 {
            *band += 0.5 * (f.spectrum[0][i] + f.spectrum[1][i]);
        }
    }
    e
}

#[test]
fn silence_projectm_exact() {
    let mut a = Analyzer::new(44100);
    let f0 = a.frame().clone();
    assert_eq!((f0.bass, f0.mid_att, f0.vol), (1.0, 1.0, 3.0 * 0.333));
    let f = run(&mut a, 0..100, 60, &|_, _| 0.0);
    assert_eq!((f.bass, f.mid, f.treb), (1.0, 1.0, 1.0));
    assert_eq!((f.bass_att, f.mid_att, f.treb_att), (1.0, 1.0, 1.0));
    assert_eq!(f.vol, (1.0f32 + 1.0 + 1.0) * 0.333);
    assert_eq!(f.vol_att, f.vol);
    assert!(f.wave.iter().all(|c| c.iter().all(|&x| x == 0.0)));
    assert!(f.spectrum.iter().all(|c| c.iter().all(|&x| x == 0.0)));
    assert_eq!(f.wave_len, 480);
}

#[test]
fn silence_beatdrop_is_zero() {
    let mut a = Analyzer::with_mode(44100, AnalysisMode::BeatDrop);
    let f = run(&mut a, 0..50, 60, &|_, _| 0.0);
    assert_eq!(
        (f.bass, f.mid, f.treb, f.bass_att, f.mid_att, f.treb_att),
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
    );
    assert_eq!(f.vol, 0.0);
}

#[test]
fn pcm_scale_and_ring() {
    let mut a = Analyzer::new(44100);
    // 1000 frames: sample k = k/1000 on L, -k/1000 on R; only the last 576 survive, oldest first.
    let mut v = Vec::new();
    for k in 0..1000 {
        v.push(k as f32 / 1000.0);
        v.push(-(k as f32) / 1000.0);
    }
    a.push_pcm_f32(&v, 2);
    // inspect through the aligner-free path: use update then compare unaligned prefix when
    // the aligner picks offset 0 or shifts; reconstruct via search.
    let f = a.update(60.0).clone();
    // a ramp has no structure to lock onto: the aligner may shift by 0..95 samples
    let first = f.wave[0][0];
    let off = (0..96)
        .find(|&o| (first - 128.0 * (424 + o) as f32 / 1000.0).abs() < 1e-3)
        .expect("offset");
    for i in 0..480 {
        let want = 128.0 * (424 + off + i) as f32 / 1000.0;
        assert!((f.wave[0][i] - want).abs() < 1e-3, "L[{i}]");
        assert!((f.wave[1][i] + want).abs() < 1e-3, "R[{i}]");
    }
}

#[test]
fn chunked_push_equals_single_push() {
    let g = |i: u64, ch: usize| ((i * 7 + ch as u64 * 3) % 101) as f32 / 101.0 - 0.5;
    let all = frame_pcm(0, 10, &g); // 4410 sample frames
    let mut a = Analyzer::new(44100);
    let mut b = Analyzer::new(44100);
    a.push_pcm_f32(&all, 2);
    for c in all.chunks(2 * 123) {
        b.push_pcm_f32(c, 2);
    }
    assert_eq!(a.update(60.0), b.update(60.0));
}

#[test]
fn mono_is_duplicated_and_extra_channels_ignored() {
    let mut m = Analyzer::new(44100);
    let mut s = Analyzer::new(44100);
    let mut q = Analyzer::new(44100);
    let mono: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    let st: Vec<f32> = mono.iter().flat_map(|&x| [x, x]).collect();
    let quad: Vec<f32> = mono.iter().flat_map(|&x| [x, x, 0.9, -0.9]).collect();
    m.push_pcm_f32(&mono, 1);
    s.push_pcm_f32(&st, 2);
    q.push_pcm_f32(&quad, 4);
    let (fm, fs, fq) = (
        m.update(60.0).clone(),
        s.update(60.0).clone(),
        q.update(60.0).clone(),
    );
    assert_eq!(fm, fs);
    assert_eq!(fm, fq);
}

/// `projectm_pcm_add_int16` / `projectm_pcm_add_uint8` / `projectm_pcm_get_max_samples`: the integer
/// paths land on exactly the same ring contents as the float path fed the normalised values.
#[test]
fn integer_pcm_matches_normalised_float_pcm() {
    assert_eq!(MAX_SAMPLES, 576);
    let n = 700; // more than the ring holds: only the newest 576 survive
    let i16s: Vec<i16> = (0..n * 2)
        .map(|i| (((i as f32 * 0.071).sin() * 20000.0) as i16).wrapping_add((i % 3) as i16))
        .collect();
    let as_f32: Vec<f32> = i16s.iter().map(|&s| f32::from(s) / 32768.0).collect();
    let (mut a, mut b) = (Analyzer::new(44100), Analyzer::new(44100));
    a.push_pcm_i16(&i16s, 2);
    b.push_pcm_f32(&as_f32, 2);
    assert_eq!(a.update(60.0), b.update(60.0));

    let u8s: Vec<u8> = (0..n)
        .map(|i| (128.0 + (i as f32 * 0.05).sin() * 100.0) as u8)
        .collect();
    let as_f32: Vec<f32> = u8s
        .iter()
        .map(|&s| (f32::from(s) - 128.0) / 128.0)
        .collect();
    let (mut a, mut b) = (Analyzer::new(44100), Analyzer::new(44100));
    a.push_pcm_u8(&u8s, 1); // mono: duplicated to both channels
    b.push_pcm_f32(&as_f32, 1);
    let (fa, fb) = (a.update(60.0).clone(), b.update(60.0).clone());
    assert_eq!(fa, fb);
    assert_eq!(fa.wave[0], fa.wave[1]);
    assert!(fa.wave[0].iter().any(|&x| x != 0.0));

    // silence (128) and zero-channel pushes change nothing
    let mut c = Analyzer::new(44100);
    c.push_pcm_u8(&[128; 100], 2);
    c.push_pcm_i16(&[5; 10], 0);
    assert!(
        c.update(60.0)
            .wave
            .iter()
            .all(|w| w.iter().all(|&x| x == 0.0))
    );
}

#[test]
fn sine_peak_bin_and_band_dominance() {
    // projectM bands (of the 512-bin / 22050 Hz spectrum): [0,85) [85,170) [170,256)
    // = 0-3.66 kHz, 3.66-7.3 kHz, 7.3-11 kHz.
    let bin_hz = SR as f32 / 2.0 / 512.0;
    for (freq, band) in [(1000.0f32, 0usize), (5000.0, 1), (9000.0, 2)] {
        let mut a = Analyzer::new(SR as u32);
        let f = run(&mut a, 0..10, 60, &sine(freq, 0.5));
        let e = band_energy(&f);
        let dom = (0..3)
            .max_by(|&x, &y| e[x].partial_cmp(&e[y]).unwrap())
            .unwrap();
        assert_eq!(dom, band, "{freq} Hz energies {e:?}");
        let others: f32 = (0..3).filter(|&b| b != band).map(|b| e[b]).sum();
        assert!(e[band] > 20.0 * others, "{freq} Hz: {e:?}");
        let peak = (0..512)
            .max_by(|&x, &y| f.spectrum[0][x].partial_cmp(&f.spectrum[0][y]).unwrap())
            .unwrap();
        assert!(
            (peak as f32 * bin_hz - freq).abs() < 2.0 * bin_hz,
            "peak bin {peak} for {freq}"
        );
    }
}

#[test]
fn onset_in_each_band_raises_only_that_band() {
    // silence, then a tone: its band's *_rel jumps; the other bands stay at ~1 (no energy).
    let tones = [(1000.0f32, 0usize), (5000.0, 1), (9000.0, 2)];
    for (freq, band) in tones {
        let mut a = Analyzer::new(SR as u32);
        run(&mut a, 0..60, 60, &|_, _| 0.0);
        let g = sine(freq, 0.5);
        let f = run(&mut a, 60..61, 60, &|i, c| g(i, c));
        let rel = [f.bass, f.mid, f.treb];
        let att = [f.bass_att, f.mid_att, f.treb_att];
        assert!(rel[band] > 1.5, "{freq}: rel {rel:?}");
        assert!(att[band] > 1.5, "{freq}: att {att:?}");
        assert!(rel[band] >= att[band] * 0.9);
        // the energy in the other bands is leakage only: relative to the long average it may
        // be anything, but the absolute spectrum there must be tiny
        let e = band_energy(&f);
        let others: f32 = (0..3).filter(|&b| b != band).map(|b| e[b]).sum();
        assert!(others < e[band] / 20.0);
    }
}

#[test]
fn impulse_train_spikes_bass() {
    // a click every 30 frames (0.5 s at 60 fps) on a 3 kHz-ish burst
    let mut a = Analyzer::new(SR as u32);
    let click_every = SR / 2;
    let g = move |i: u64, _c: usize| {
        let k = (i + click_every - 600) % click_every;
        if k < 64 {
            (k as f32 * 0.4).sin() * 0.8
        } else {
            0.0
        }
    };
    let mut hits = Vec::new();
    let mut quiet = Vec::new();
    for n in 0..240u64 {
        a.push_pcm_f32(&frame_pcm(n, 60, &g), 2);
        let f = a.update(60.0);
        // a frame sees a click iff its (576-sample) window overlaps one
        let (s, e) = (n * SR / 60, (n + 1) * SR / 60);
        let has = (s / click_every..=e / click_every).any(|c| {
            let st = c * click_every + 600;
            st < e && st + 64 > e.saturating_sub(576)
        });
        if n > 60 {
            if has {
                hits.push(f.bass.max(f.mid));
            } else {
                quiet.push(f.bass.max(f.mid));
            }
        }
    }
    let hmax = hits.iter().cloned().fold(0.0, f32::max);
    assert!(hmax > 3.0, "click frames peak {hmax}");
    // frames with clicks beat frames without by a wide margin on average
    let avg = |v: &Vec<f32>| v.iter().sum::<f32>() / v.len() as f32;
    assert!(
        avg(&hits) > 3.0 * avg(&quiet) || quiet.iter().all(|&q| q <= 1.0 + 1e-6),
        "{} vs {}",
        avg(&hits),
        avg(&quiet)
    );
}

#[test]
fn fps_dependence_of_smoothing() {
    // identical 576-sample block each frame => `imm` constant => avg follows
    // avg_n = imm * (1 - rate^n); half the frames at half the fps (same wall time) must agree.
    let mut seed = 99;
    let block: Vec<f32> = (0..576).map(|_| rng(&mut seed) * 0.3).collect();
    let stereo: Vec<f32> = block.iter().flat_map(|&x| [x, -x * 0.5]).collect();
    let mut a30 = Analyzer::new(SR as u32);
    let mut a60 = Analyzer::new(SR as u32);
    let (mut f30, mut f60) = (None, None);
    for _ in 0..15 {
        a30.push_pcm_f32(&stereo, 2);
        f30 = Some(a30.update(30.0).clone());
    }
    for _ in 0..30 {
        a60.push_pcm_f32(&stereo, 2);
        f60 = Some(a60.update(60.0).clone());
    }
    let (f30, f60) = (f30.unwrap(), f60.unwrap());
    for (x, y) in [
        (f30.bass, f60.bass),
        (f30.bass_att, f60.bass_att),
        (f30.mid_att, f60.mid_att),
        (f30.treb_att, f60.treb_att),
        (f30.vol_att, f60.vol_att),
    ] {
        assert!((x - y).abs() < 2e-3 * y.abs().max(1.0), "{x} vs {y}");
    }
    // and the smoothing really is fps dependent per frame: at one frame, 30fps moves further
    let mut b30 = Analyzer::new(SR as u32);
    let mut b60 = Analyzer::new(SR as u32);
    b30.push_pcm_f32(&stereo, 2);
    b60.push_pcm_f32(&stereo, 2);
    let (x30, x60) = (b30.update(30.0).clone(), b60.update(60.0).clone());
    // frame 0: long_avg = imm*(1-0.9^(30*dt)) -> rel = imm/long = 1/(1-rate): bigger rel for 60fps
    assert!(x60.bass > x30.bass, "{} {}", x60.bass, x30.bass);
}

#[test]
fn frame_below_50_uses_fast_long_average() {
    // constant block; relative value = imm/long_avg = 1/(1-r^n) shrinks toward 1; the switch
    // to 0.992 at frame 50 makes it jump up again.
    let mut seed = 5;
    let block: Vec<f32> = (0..576).map(|_| rng(&mut seed) * 0.3).collect();
    let stereo: Vec<f32> = block.iter().flat_map(|&x| [x, x]).collect();
    let mut a = Analyzer::new(SR as u32);
    let mut series = Vec::new();
    for _ in 0..60 {
        a.push_pcm_f32(&stereo, 2);
        series.push(a.update(30.0).bass);
    }
    // at 30 fps rate = 0.9 (frames<50) -> long_avg_n = imm*(1-0.9^(n+1))
    for n in [0usize, 10, 40] {
        let want = 1.0 / (1.0 - 0.9f32.powi(n as i32 + 1));
        assert!(
            (series[n] - want).abs() < 1e-3 * want,
            "n={n}: {} vs {want}",
            series[n]
        );
    }
    assert!(
        series[55] > series[49] * 0.99,
        "long average slows down after frame 50"
    );
    assert_eq!(a.frame_index(), 60);
}

#[test]
fn aligner_stabilises_a_drifting_sine() {
    // 735 samples advance per frame; a 437.5 Hz-ish sine has a phase that moves every frame.
    let g = sine(437.3, 0.5);
    let mut a = Analyzer::new(SR as u32);
    let mut prev: Option<Vec<f32>> = None;
    let mut aligned_err = 0.0f64;
    let mut raw_err = 0.0f64;
    let mut prev_raw: Option<Vec<f32>> = None;
    for n in 0..40u64 {
        let pcm = frame_pcm(n, 60, &g);
        a.push_pcm_f32(&pcm, 2);
        let f = a.update(60.0).clone();
        let w: Vec<f32> = f.wave[0][..480].to_vec();
        let raw: Vec<f32> = pcm
            .chunks(2)
            .map(|c| 128.0 * c[0])
            .rev()
            .take(576)
            .rev()
            .take(480)
            .collect();
        if n >= 5
            && let (Some(p), Some(pr)) = (&prev, &prev_raw)
        {
            aligned_err += w
                .iter()
                .zip(p)
                .map(|(x, y)| ((x - y) as f64).powi(2))
                .sum::<f64>();
            raw_err += raw
                .iter()
                .zip(pr)
                .map(|(x, y)| ((x - y) as f64).powi(2))
                .sum::<f64>();
        }
        prev = Some(w);
        prev_raw = Some(raw);
    }
    assert!(
        aligned_err < 0.2 * raw_err,
        "aligned {aligned_err} vs raw {raw_err}"
    );
}

#[test]
fn spectrum_equalize_dc_bin_is_zero() {
    let mut a = Analyzer::new(SR as u32);
    let f = run(&mut a, 0..3, 60, &|_, _| 0.25);
    assert_eq!(f.spectrum[0][0], 0.0); // equalize[0] = -0.02*ln(1) = 0
    assert!(f.spectrum[0][1] > 0.0);
}

#[test]
fn beatdrop_bands_follow_hz() {
    // 1 kHz is bass in projectM's 0-3.6 kHz band, but *mid* for BeatDrop (250-4000 Hz).
    let mut a = Analyzer::with_mode(SR as u32, AnalysisMode::BeatDrop);
    run(&mut a, 0..60, 60, &|_, _| 0.0);
    let g = sine(1000.0, 0.5);
    let f = run(&mut a, 60..61, 60, &|i, c| g(i, c));
    assert!(f.mid > 1.5, "mid {}", f.mid);
    let bin_hz = SR as f32 / 2.0 / 512.0;
    let peak = (0..512)
        .max_by(|&x, &y| f.spectrum[0][x].partial_cmp(&f.spectrum[0][y]).unwrap())
        .unwrap();
    assert!((peak as f32 * bin_hz - 1000.0).abs() < 2.0 * bin_hz);
}

#[test]
fn update_dt_zero_first_frame_leaves_averages_untouched() {
    // projectM's first frame sees dt ~ 0: rate = 1 => nothing is averaged in.
    let mut a = Analyzer::new(SR as u32);
    a.push_pcm_f32(&frame_pcm(0, 60, &sine(1000.0, 0.5)), 2);
    let f = a.update_dt(0.0).clone();
    assert_eq!((f.bass, f.mid, f.treb), (1.0, 1.0, 1.0));
}
