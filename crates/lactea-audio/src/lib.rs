//! MilkDrop 2 audio analysis: waveform / spectrum buffers and the bass / mid / treb
//! beat values (`imm_rel`, `avg_rel`), driven by float PCM.
//!
//! Default mode ([`AnalysisMode::ProjectM`]) reproduces what projectM computes for the same
//! PCM; [`AnalysisMode::BeatDrop`] follows BeatDrop's `plugin.cpp` instead (Hz-defined bands,
//! 1024-point window, silence -> 0); [`AnalysisMode::Md3`] follows MilkDrop 3 3.37 (8-bit
//! quantised input, volume sensitivity, alignment before the analysis, left channel only,
//! 576-sample window, silence -> 1) and comes with the [`ShaderFft`] smoother that feeds the
//! `get_fft` shader helpers.
//!
//! No I/O, no threads: push interleaved PCM frames, call [`Analyzer::update`] once per
//! rendered frame.

#![warn(missing_docs)]

mod align;
mod fft;
mod md3;

use align::Aligner;
use fft::{Fft, FftStyle};
pub use md3::{ShaderFft, md3_spectrum};

/// Samples kept in the PCM ring and fed to the aligner (`AudioBufferSamples`).
pub const WAVE_BUF: usize = 576;
/// `projectm_pcm_get_max_samples()`: sample frames per channel that are stored (`WAVE_BUF`);
/// anything beyond the newest this many frames of one push is discarded.
pub const MAX_SAMPLES: usize = WAVE_BUF;
/// Leading samples of each waveform that are valid after alignment (`NUM_WAVEFORM_SAMPLES`).
pub const WAVE_VALID: usize = 480;
/// Spectrum bins per channel.
pub const SPECTRUM_BINS: usize = 512;
/// float PCM (-1..1) is scaled by this to MD2's 8-bit-like range (-128..127).
pub const PCM_SCALE: f32 = 128.0;

/// Which implementation the analysis reproduces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnalysisMode {
    /// Bit-for-bit intent: projectM `Audio/PCM.cpp` + `Loudness.cpp` + `MilkdropFFT.cpp`.
    ProjectM,
    /// BeatDrop's vis_milk2 fork (`plugin.cpp::DoCustomSoundAnalysis`):
    /// Hz-defined bands, 1024-point zero-padded window, silence -> 0 (not 1), `fps`-based
    /// rate adjustment.
    BeatDrop,
    /// MilkDrop 3 3.37 (`Compat::MilkDrop3`; docs/audio.md, "MilkDrop 3 mode"): the PCM is
    /// quantised to signed 8 bit when pushed, scaled by
    /// [`volume_sensitivity`](Analyzer::volume_sensitivity), aligned *before* the analysis, and
    /// only the left channel's 576-sample Hann spectrum feeds `bass/mid/treb`
    /// (bins `[0,85) [85,170) [170,256)`); silence gives 1.0 for all six values.
    /// `sample_rate` is not used.
    Md3,
}

/// BeatDrop band edges in Hz (`m_nBassStart..m_nTrebEnd` defaults).
pub const BEATDROP_BANDS_HZ: [(i32, i32); 3] = [(0, 250), (250, 4000), (4000, 20000)];

/// Everything the renderer needs for one frame.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AudioFrame {
    /// `imm_rel[0]`: current bass relative to its long-term average.
    pub bass: f32,
    /// `imm_rel[1]`: current mid relative to its long-term average.
    pub mid: f32,
    /// `imm_rel[2]`: current treble relative to its long-term average.
    pub treb: f32,
    /// `(bass+mid+treb) * 0.333` (projectM's `vol`; MD2 itself has no `vol`).
    pub vol: f32,
    /// `avg_rel[0]`: smoothed bass relative to its long-term average.
    pub bass_att: f32,
    /// `avg_rel[1]`: smoothed mid.
    pub mid_att: f32,
    /// `avg_rel[2]`: smoothed treble.
    pub treb_att: f32,
    /// `(bass_att+mid_att+treb_att) * 0.333`.
    pub vol_att: f32,
    /// Left/right waveform, MD2 scale (float PCM * 128). Only the first `wave_len`
    /// (= [`WAVE_VALID`]) samples are valid, the tail is whatever the ring held.
    /// In [`AnalysisMode::Md3`]: the aligned waveform in signed 8-bit units after the volume
    /// sensitivity (the exact input of the published spectrum).
    pub wave: [[f32; WAVE_BUF]; 2],
    /// Number of valid leading samples in `wave`.
    pub wave_len: usize,
    /// Left/right equalised FFT magnitudes (MD2 `fSpecLeft/Right`, `m_sound.fSpectrum`).
    pub spectrum: [[f32; SPECTRUM_BINS]; 2],
}

impl Default for AudioFrame {
    fn default() -> Self {
        AudioFrame {
            bass: 0.0,
            mid: 0.0,
            treb: 0.0,
            vol: 0.0,
            bass_att: 0.0,
            mid_att: 0.0,
            treb_att: 0.0,
            vol_att: 0.0,
            wave: [[0.0; WAVE_BUF]; 2],
            wave_len: WAVE_VALID,
            spectrum: [[0.0; SPECTRUM_BINS]; 2],
        }
    }
}

/// One band's running state (`mysound.imm / avg / long_avg`).
#[derive(Clone, Copy, Debug, Default)]
struct Band {
    imm: f32,
    avg: f32,
    long_avg: f32,
    imm_rel: f32,
    avg_rel: f32,
}

/// `AdjustRateToFPS`: per-frame rate defined at 30 fps -> per-frame rate for a frame
/// lasting `dt` seconds.
fn adjust_rate(rate: f32, dt: f32) -> f32 {
    rate.powf(30.0f32).powf(dt)
}

/// PCM ring buffer plus the analysis state that turns it into one [`AudioFrame`] per call to
/// [`update`](Self::update).
pub struct Analyzer {
    mode: AnalysisMode,
    sample_rate: u32,
    ring: [[f32; WAVE_BUF]; 2],
    start: usize,
    fft: Fft,
    align: [Aligner; 2],
    bands: [Band; 3],
    frame_no: u32,
    volume_sensitivity: f32,
    out: AudioFrame,
}

impl Analyzer {
    /// projectM-compatible analyzer.  `sample_rate` only matters for [`AnalysisMode::BeatDrop`]
    /// (projectM has no notion of sample rate: bands are fixed fractions of the spectrum).
    pub fn new(sample_rate: u32) -> Self {
        Self::with_mode(sample_rate, AnalysisMode::ProjectM)
    }

    /// Analyzer for the given input `sample_rate` (Hz) and analysis `mode`
    /// (`sample_rate` only matters for [`AnalysisMode::BeatDrop`]).
    pub fn with_mode(sample_rate: u32, mode: AnalysisMode) -> Self {
        let fft = match mode {
            // 480 samples in, 512 bins out, equalize, envelope power 1
            AnalysisMode::ProjectM => {
                Fft::new(WAVE_VALID, SPECTRUM_BINS, true, 1.0, FftStyle::ProjectM)
            }
            // MY_FFT_WINDOW=1024 in, MY_FFT_SAMPLES=512 out
            AnalysisMode::BeatDrop => {
                Fft::new(2 * SPECTRUM_BINS, SPECTRUM_BINS, true, 1.0, FftStyle::Md2)
            }
            // 576 samples in, 512 bins out, equalize, envelope power 1, MD2 constants
            AnalysisMode::Md3 => md3::md3_fft(),
        };
        let init_rel = match mode {
            AnalysisMode::ProjectM | AnalysisMode::Md3 => 1.0,
            AnalysisMode::BeatDrop => 0.0,
        };
        let band = Band {
            imm_rel: init_rel,
            avg_rel: init_rel,
            ..Band::default()
        };
        let vol = (init_rel + init_rel + init_rel) * 0.333f32;
        let out = AudioFrame {
            bass: init_rel,
            mid: init_rel,
            treb: init_rel,
            bass_att: init_rel,
            mid_att: init_rel,
            treb_att: init_rel,
            vol,
            vol_att: vol,
            ..AudioFrame::default()
        };
        Analyzer {
            mode,
            sample_rate,
            ring: [[0.0; WAVE_BUF]; 2],
            start: 0,
            fft,
            align: [Aligner::new(), Aligner::new()],
            bands: [band; 3],
            frame_no: 0,
            volume_sensitivity: 1.0,
            out,
        }
    }

    /// The analysis mode this analyzer was created with.
    pub fn mode(&self) -> AnalysisMode {
        self.mode
    }

    /// Frames analysed so far (the `frame` counter used by the long-average switch at 50).
    pub fn frame_index(&self) -> u32 {
        self.frame_no
    }

    /// The latest result (before the first `update`: neutral defaults).
    pub fn frame(&self) -> &AudioFrame {
        &self.out
    }

    /// Linear gain MD3 applies to the quantised waveform (`VolumeSensitivity`, default 1.0).
    pub fn volume_sensitivity(&self) -> f32 {
        self.volume_sensitivity
    }

    /// Set the linear gain applied to the 8-bit quantised waveform of every
    /// [`AnalysisMode::Md3`] update, before the alignment and the analysis. It is used as given
    /// (no clamping, so the waveform may exceed +-128; the value must be finite) and does not
    /// recover clipped peaks. Ignored by [`AnalysisMode::ProjectM`] and
    /// [`AnalysisMode::BeatDrop`].
    pub fn set_volume_sensitivity(&mut self, s: f32) {
        self.volume_sensitivity = s;
    }

    /// Append interleaved float PCM (-1..1) (`projectm_pcm_add_float`).  Channel 0 is left,
    /// channel 1 right, further channels are ignored; mono is duplicated to both.  Only the most
    /// recent 576 sample frames matter (circular buffer), so pushing more than that per call is
    /// fine.  `channels == 0` or a slice shorter than one frame is a no-op; a trailing partial
    /// frame is ignored.
    ///
    /// In [`AnalysisMode::Md3`] every sample is quantised to signed 8 bit on the way in
    /// (`s >= 1.0 -> 127`, `s < -1.0 -> -128`, otherwise `trunc(128 * s)`).
    pub fn push_pcm_f32(&mut self, interleaved: &[f32], channels: usize) {
        self.push_scaled(interleaved, channels, 0.0, 1.0);
    }

    /// Append interleaved signed 16-bit PCM (`projectm_pcm_add_int16`): full scale is 32768.
    pub fn push_pcm_i16(&mut self, interleaved: &[i16], channels: usize) {
        self.push_scaled(interleaved, channels, 0.0, 32768.0);
    }

    /// Append interleaved unsigned 8-bit PCM (`projectm_pcm_add_uint8`): silence is 128.
    pub fn push_pcm_u8(&mut self, interleaved: &[u8], channels: usize) {
        self.push_scaled(interleaved, channels, 128.0, 128.0);
    }

    /// projectM's `PCM::AddToBuffer`: `128 * (s - offset) / amplitude`, written at the ring start.
    /// In [`AnalysisMode::Md3`] the value is then quantised like MD3's capture: `>= 128 -> 127`,
    /// `< -128 -> -128`, otherwise truncated toward zero.
    fn push_scaled<T: Copy + Into<f32>>(
        &mut self,
        interleaved: &[T],
        channels: usize,
        offset: f32,
        amplitude: f32,
    ) {
        if channels == 0 {
            return;
        }
        let count = interleaved.len() / channels;
        if count == 0 {
            return;
        }
        let quantise = self.mode == AnalysisMode::Md3;
        let conv = |s: T| {
            let v = PCM_SCALE * (s.into() - offset) / amplitude;
            if quantise { md3::quantise_i8(v) } else { v }
        };
        for i in 0..count {
            let off = (self.start + i) % WAVE_BUF;
            let l = conv(interleaved[i * channels]);
            self.ring[0][off] = l;
            self.ring[1][off] = if channels > 1 {
                conv(interleaved[i * channels + 1])
            } else {
                l
            };
        }
        self.start = (self.start + count) % WAVE_BUF;
    }

    /// Analyse the current ring contents for a frame lasting `1/fps` seconds (`fps` must be
    /// positive and finite; otherwise the frame is treated as lasting 0 seconds).
    pub fn update(&mut self, fps: f32) -> &AudioFrame {
        self.update_dt(1.0f64 / fps as f64)
    }

    /// Like [`update`](Self::update) with an explicit frame duration in seconds
    /// (projectM uses `time(n) - time(n-1)`; its very first frame sees ~0).  A negative or
    /// non-finite `dt` is treated as 0 (rates unchanged) instead of poisoning the averages.
    pub fn update_dt(&mut self, dt: f64) -> &AudioFrame {
        let dt = if dt.is_finite() && dt >= 0.0 { dt } else { 0.0 };
        // 1. snapshot the ring, oldest sample first
        let mut wave = [[0.0f32; WAVE_BUF]; 2];
        for (w, ring) in wave.iter_mut().zip(&self.ring) {
            for (i, v) in w.iter_mut().enumerate() {
                *v = ring[(self.start + i) % WAVE_BUF];
            }
        }
        // 1b. Md3 (`AnalyzeNewSound`, then `AlignWaves`, then the analysis): the gain applies
        // to the already quantised samples and the alignment precedes the spectra
        if self.mode == AnalysisMode::Md3 {
            for (aligner, w) in self.align.iter_mut().zip(wave.iter_mut()) {
                for v in w.iter_mut() {
                    *v *= self.volume_sensitivity;
                }
                aligner.align(w);
            }
        }
        // 2. spectra (Md3: of the aligned waveform; the other modes: of the unaligned one)
        let mut spec = [[0.0f32; SPECTRUM_BINS]; 2];
        for (w, s) in wave.iter().zip(spec.iter_mut()) {
            match self.mode {
                AnalysisMode::ProjectM => {
                    // damp the FFT input: average with the previous sample
                    let mut damped = [0.0f32; WAVE_BUF];
                    let mut old_i = 0usize;
                    for (i, d) in damped.iter_mut().enumerate() {
                        *d = 0.5f32 * (w[i] + w[old_i]);
                        old_i = i;
                    }
                    self.fft.time_to_frequency_domain(&damped, s);
                }
                AnalysisMode::BeatDrop => {
                    // 576 samples zero-padded into the 1024 window
                    let mut padded = [0.0f32; 2 * SPECTRUM_BINS];
                    padded[..WAVE_BUF].copy_from_slice(w);
                    self.fft.time_to_frequency_domain(&padded, s);
                }
                AnalysisMode::Md3 => {
                    // already scaled and aligned (step 1): 576 samples, no input damping
                    self.fft.time_to_frequency_domain(w, s);
                }
            }
        }
        // 3. band energies
        let dt_f = dt as f32;
        match self.mode {
            AnalysisMode::ProjectM => {
                let mut mono = [0.0f32; SPECTRUM_BINS];
                for (i, m) in mono.iter_mut().enumerate() {
                    *m = 0.5f32 * (spec[0][i] + spec[1][i]);
                }
                for (b, band) in self.bands.iter_mut().enumerate() {
                    let start = SPECTRUM_BINS * b / 6;
                    let end = SPECTRUM_BINS * (b + 1) / 6;
                    band.imm = mono[start..end].iter().fold(0.0f32, |sum, &m| sum + m);
                }
            }
            AnalysisMode::BeatDrop => {
                let bin_size = (self.sample_rate as f32 / 2.0f32) / SPECTRUM_BINS as f32;
                for (b, (fmin, fmax)) in BEATDROP_BANDS_HZ.iter().enumerate() {
                    let s = ((*fmin as f32 / bin_size) as i32).max(0);
                    let e = ((*fmax as f32 / bin_size) as i32).min(SPECTRUM_BINS as i32 - 1);
                    let mut sum = 0.0f32;
                    for j in s..=e {
                        sum += (spec[0][j as usize] + spec[1][j as usize]) / 2.0;
                    }
                    self.bands[b].imm = sum;
                }
            }
            AnalysisMode::Md3 => {
                // left channel only, plain sum of the bins of each third of the lower half
                for (b, band) in self.bands.iter_mut().enumerate() {
                    let start = SPECTRUM_BINS * b / 6;
                    let end = SPECTRUM_BINS * (b + 1) / 6;
                    band.imm = spec[0][start..end].iter().fold(0.0f32, |sum, &m| sum + m);
                }
            }
        }
        // 4. temporal blending
        let silent_rel = match self.mode {
            AnalysisMode::ProjectM | AnalysisMode::Md3 => 1.0f32,
            AnalysisMode::BeatDrop => 0.0f32,
        };
        for bd in &mut self.bands {
            let r = if bd.imm > bd.avg { 0.2f32 } else { 0.5f32 };
            let rate = adjust_rate(r, dt_f);
            bd.avg = bd.avg * rate + bd.imm * (1.0f32 - rate);
            let r = if self.frame_no < 50 { 0.9f32 } else { 0.992f32 };
            let rate = adjust_rate(r, dt_f);
            bd.long_avg = bd.long_avg * rate + bd.imm * (1.0f32 - rate);
            if bd.long_avg.abs() < 0.001 {
                bd.imm_rel = silent_rel;
                bd.avg_rel = silent_rel;
            } else {
                bd.imm_rel = bd.imm / bd.long_avg;
                bd.avg_rel = bd.avg / bd.long_avg;
            }
        }
        // 5. waveform alignment (modifies `wave` in place); Md3 already did it in step 1
        if self.mode != AnalysisMode::Md3 {
            for (aligner, w) in self.align.iter_mut().zip(wave.iter_mut()) {
                aligner.align(w);
            }
        }
        // 6. publish
        let o = &mut self.out;
        o.wave = wave;
        o.wave_len = WAVE_VALID;
        o.spectrum = spec;
        o.bass = self.bands[0].imm_rel;
        o.mid = self.bands[1].imm_rel;
        o.treb = self.bands[2].imm_rel;
        o.bass_att = self.bands[0].avg_rel;
        o.mid_att = self.bands[1].avg_rel;
        o.treb_att = self.bands[2].avg_rel;
        o.vol = (o.bass + o.mid + o.treb) * 0.333f32;
        o.vol_att = (o.bass_att + o.mid_att + o.treb_att) * 0.333f32;
        self.frame_no += 1;
        &self.out
    }
}
