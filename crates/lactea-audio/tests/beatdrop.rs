//! BeatDrop's shader FFT (`sampler_fft` rows): a quarter-sample-rate tone lights bin 512, silence stays dark, the
//! peak row holds behind the falling spectrum.

use lactea_audio::{BeatDropShaderFft, SHADER_FFT_BINS};

/// Interleaved stereo tone with a period of four sample frames (a quarter of the sample rate).
fn tone(frames: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|i| {
            let s = 0.5 * (std::f32::consts::FRAC_PI_2 * i as f32).sin();
            [s, s]
        })
        .collect()
}

fn argmax(v: &[f32]) -> usize {
    v.iter()
        .enumerate()
        .fold((0, f32::MIN), |b, (i, &x)| if x > b.1 { (i, x) } else { b })
        .0
}

#[test]
fn silence_gives_a_dark_texture() {
    let mut f = BeatDropShaderFft::new();
    for _ in 0..10 {
        f.update(60.0, 0.5, 0.7);
    }
    assert!(f.smoothed().iter().all(|&x| x == 0.0));
    assert!(f.peak().iter().all(|&x| x == 0.0));
}

#[test]
fn a_tone_peaks_at_its_bin_and_the_peak_row_outlives_it() {
    let mut f = BeatDropShaderFft::new();
    f.push_pcm_f32(&tone(2048), 2);
    for _ in 0..30 {
        f.update(60.0, 0.5, 0.7);
    }
    let bin = argmax(f.smoothed());
    assert!(
        (bin as i32 - (SHADER_FFT_BINS as i32 / 2)).abs() <= 1,
        "bin {bin}"
    );
    assert!(f.smoothed()[bin] > 0.05, "{}", f.smoothed()[bin]);
    let loud = f.smoothed()[bin];

    // the tone stops: the spectrum decays, the peak row is held at its top for a while
    f.push_pcm_f32(&vec![0.0; 2 * 2048], 2);
    for _ in 0..5 {
        f.update(60.0, 0.5, 0.7);
    }
    assert!(f.smoothed()[bin] < loud);
    assert!(f.peak()[bin] >= f.smoothed()[bin]);
    assert!(f.peak()[bin] >= loud * 0.99, "held: {}", f.peak()[bin]);
}

#[test]
fn a_higher_attack_rises_faster() {
    let one_frame = |attack: f32| {
        let mut f = BeatDropShaderFft::new();
        f.push_pcm_f32(&tone(2048), 2);
        f.update(60.0, attack, 0.7);
        f.smoothed()[512]
    };
    // the attack coefficient is `1 - (1 - attack)^(60 / fps)`
    assert!(one_frame(0.9) > one_frame(0.1));
    assert!(one_frame(0.5) > 0.0);
}
