//! Audio input of the standalone app.
//!
//! Every source delivers **interleaved stereo f32** (`CHANNELS`) at `sample_rate()` through a
//! lock-free SPSC ring; the render thread drains it once per frame via [`AudioSource::poll`]
//! and hands the result to the visualizer. Sources:
//!
//! * [`pw::PipewireCapture`] — native PipeWire capture of the default sink's monitor,
//! * [`cpal_input::CpalCapture`] — cpal default/selected input device,
//! * [`player::FilePlayer`] — symphonia decode + cpal playback, visualizer fed in sync with
//!   what is audible,
//! * [`deterministic::WavFeed`] — fixed-step PCM slices of a WAV (no device, no clock).

pub(crate) mod cpal_input;
pub(crate) mod decode;
pub(crate) mod deterministic;
pub(crate) mod devices;
pub(crate) mod monitor;
pub(crate) mod player;
#[cfg(all(feature = "pipewire", target_os = "linux"))]
pub(crate) mod pw;
pub(crate) mod resample;

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// All sources produce/consume interleaved stereo.
pub const CHANNELS: usize = 2;
/// Ring capacity in frames (1.5 s at 44.1 kHz): a stalled render thread loses data instead of
/// blocking the audio thread.
pub const RING_FRAMES: usize = 1 << 16;
/// Sample rate the app asks live sources for (MilkDrop's analysis assumes ~44.1 kHz).
pub const LIVE_RATE: u32 = 44_100;

/// State shared between a source's producer side (audio thread) and the render thread.
#[derive(Default)]
pub struct SourceShared {
    /// Frames delivered by the device/stream so far (progress ⇒ the capture is alive).
    pub frames_in: AtomicU64,
    /// Frames dropped because the ring was full.
    pub dropped_frames: AtomicU64,
    /// File playback reached its end.
    pub ended: AtomicBool,
    detail: Mutex<String>,
    failure: Mutex<Option<String>>,
}

impl SourceShared {
    pub fn set_detail(&self, s: impl Into<String>) {
        *self.detail.lock() = s.into();
    }
    pub fn detail(&self) -> String {
        self.detail.lock().clone()
    }
    pub fn set_failure(&self, s: Option<String>) {
        *self.failure.lock() = s;
    }
    pub fn failure(&self) -> Option<String> {
        self.failure.lock().clone()
    }
    pub fn add_frames(&self, n: usize) {
        self.frames_in.fetch_add(n as u64, Ordering::Relaxed);
    }
    pub fn add_dropped(&self, n: usize) {
        self.dropped_frames.fetch_add(n as u64, Ordering::Relaxed);
    }
}

pub trait AudioSource {
    /// Human-readable description (device / node being listened to), for logs and the title.
    fn describe(&self) -> String;
    /// Sample rate of the PCM `poll` returns.
    fn sample_rate(&self) -> u32;
    /// Append the interleaved-stereo PCM that became available/audible since the last call.
    fn poll(&mut self, now: Instant, out: &mut Vec<f32>);
    fn shared(&self) -> &SourceShared;
    /// Audible playback position in seconds (file playback only).
    fn position_secs(&self, _now: Instant) -> Option<f64> {
        None
    }
}

/// Convert interleaved `channels`-channel samples to interleaved stereo, appending to `out`.
/// Mono is duplicated; 6/8 channels (5.1/7.1, SMPTE order) are folded down; anything else keeps
/// the first two channels.
pub fn to_stereo(input: &[f32], channels: usize, out: &mut Vec<f32>) {
    match channels {
        0 => {}
        1 => out.extend(input.iter().flat_map(|&s| [s, s])),
        2 => out.extend_from_slice(input),
        6 | 8 => {
            const C: f32 = std::f32::consts::FRAC_1_SQRT_2;
            let norm = 1.0 / (1.0 + 2.0 * C);
            for f in input.chunks_exact(channels) {
                // FL FR FC LFE BL BR [SL SR]
                let (mut l, mut r) = (f[0] + C * f[2] + C * f[4], f[1] + C * f[2] + C * f[5]);
                if channels == 8 {
                    l += C * f[6];
                    r += C * f[7];
                }
                let n = if channels == 8 { norm * 0.8 } else { norm };
                out.extend_from_slice(&[l * n, r * n]);
            }
        }
        n => {
            for f in input.chunks_exact(n) {
                out.extend_from_slice(&[f[0], f[1]]);
            }
        }
    }
}

/// Drain everything currently in `consumer` into `out` (whole stereo frames only).
pub fn drain_ring(consumer: &mut rtrb::Consumer<f32>, out: &mut Vec<f32>) {
    let n = consumer.slots() & !1;
    if n == 0 {
        return;
    }
    if let Ok(chunk) = consumer.read_chunk(n) {
        let (a, b) = chunk.as_slices();
        out.extend_from_slice(a);
        out.extend_from_slice(b);
        chunk.commit_all();
    }
}

/// Push whole stereo frames; drops the whole slice (counted) if it does not fit.
pub fn push_frames(producer: &mut rtrb::Producer<f32>, samples: &[f32], shared: &SourceShared) {
    shared.add_frames(samples.len() / CHANNELS);
    if producer.slots() < samples.len() {
        shared.add_dropped(samples.len() / CHANNELS);
        return;
    }
    if let Ok(mut chunk) = producer.write_chunk_uninit(samples.len()) {
        let (a, b) = chunk.as_mut_slices();
        let (head, tail) = samples.split_at(a.len());
        for (dst, src) in a.iter_mut().zip(head) {
            dst.write(*src);
        }
        for (dst, src) in b.iter_mut().zip(tail) {
            dst.write(*src);
        }
        // SAFETY: every slot of both slices was initialised above.
        unsafe { chunk.commit_all() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_conversion() {
        let mut o = Vec::new();
        to_stereo(&[0.1, 0.2], 1, &mut o);
        assert_eq!(o, [0.1, 0.1, 0.2, 0.2]);
        o.clear();
        to_stereo(&[0.1, 0.2, 0.3, 0.4], 2, &mut o);
        assert_eq!(o, [0.1, 0.2, 0.3, 0.4]);
        o.clear();
        to_stereo(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3, &mut o);
        assert_eq!(o, [1.0, 2.0, 4.0, 5.0]);
        // 5.1: a full-scale front-left-only signal stays within [-1, 1].
        o.clear();
        to_stereo(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0], 6, &mut o);
        assert!(o[0] > 0.0 && o[0] <= 1.0 && o[1] == 0.0);
    }

    #[test]
    fn ring_roundtrip_with_wraparound() {
        let shared = SourceShared::default();
        let (mut p, mut c) = rtrb::RingBuffer::<f32>::new(8);
        let mut out = Vec::new();
        for round in 0..5 {
            let data: Vec<f32> = (0..6).map(|i| (round * 10 + i) as f32).collect();
            push_frames(&mut p, &data, &shared);
            out.clear();
            drain_ring(&mut c, &mut out);
            assert_eq!(out, data, "round {round}");
        }
        // Overfull push drops everything and is counted.
        push_frames(&mut p, &[0.0; 10], &shared);
        assert_eq!(shared.dropped_frames.load(Ordering::Relaxed), 5);
        out.clear();
        drain_ring(&mut c, &mut out);
        assert!(out.is_empty());
    }
}
