//! `--audio-file`: decode with symphonia, play through cpal, and feed the visualizer **the
//! samples that are audible now**.
//!
//! ```text
//!  decode thread ──(ring A: decoded, device rate)──▶ cpal output callback ──▶ speakers
//!                                                       │  every frame handed to the device
//!                                                       ├──(ring B: tap)────────▶ render thread
//!                                                       └─ ClockSample{tapped_before, at, latency}
//! ```
//!
//! The callback tags each chunk with the wall-clock instant it ran (`at`) and the device latency
//! cpal predicts for it (`playback - callback`). A sample of absolute index `k` in the tap is
//! audible at `at + latency + (k - tapped_before) / rate`, so at render time `now` the audible
//! position is `tapped_before + (now - at - latency) * rate`. The render thread keeps the tapped
//! samples in a history and releases only those up to that position: the visualizer sees what
//! the speakers play now, not what the decoder produced ahead of time.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use parking_lot::Mutex;

use super::decode::AudioDecoder;
use super::devices::{find_output, label};
use super::resample::StreamResampler;
use super::{AudioSource, CHANNELS, SourceShared, drain_ring};

/// Timing record published by the output callback (see module docs).
#[derive(Clone, Copy, Debug)]
pub struct ClockSample {
    /// Tap frames written before this callback's chunk.
    pub tapped_before: u64,
    /// When the callback ran.
    pub at: Instant,
    /// Predicted delay between the callback and the chunk reaching the speakers.
    pub latency: Duration,
}

/// Where in the tap is the sound the listener hears at `now`, as a fractional absolute frame.
pub fn audible_position(cs: &ClockSample, now: Instant, rate: u32, extra_delay: f64) -> f64 {
    let dt = if now >= cs.at {
        now.duration_since(cs.at).as_secs_f64()
    } else {
        -cs.at.duration_since(now).as_secs_f64()
    };
    cs.tapped_before as f64 + (dt - cs.latency.as_secs_f64() - extra_delay) * rate as f64
}

/// History of tapped frames with absolute numbering; releases frames up to an audible position.
#[derive(Default)]
pub struct TapHistory {
    samples: VecDeque<f32>,
    /// Absolute index of the first frame in `samples`.
    base: u64,
    /// Absolute index of the next frame to hand out.
    fed: u64,
}

impl TapHistory {
    pub fn extend(&mut self, interleaved: &[f32]) {
        self.samples.extend(interleaved.iter().copied());
    }

    pub fn end(&self) -> u64 {
        self.base + (self.samples.len() / CHANNELS) as u64
    }

    pub fn fed(&self) -> u64 {
        self.fed
    }

    /// Append frames `[fed, upto)` to `out` (clamped to what exists, never going backwards).
    /// A backlog of more than `max_backlog` frames (render thread stalled) is skipped.
    pub fn release(&mut self, upto: f64, max_backlog: u64, out: &mut Vec<f32>) {
        let target = (upto.floor().max(0.0) as u64).min(self.end());
        if target <= self.fed {
            return;
        }
        if target - self.fed > max_backlog {
            self.fed = target - max_backlog;
        }
        let a = (self.fed - self.base) as usize * CHANNELS;
        let b = (target - self.base) as usize * CHANNELS;
        let (s0, s1) = self.samples.as_slices();
        if b <= s0.len() {
            out.extend_from_slice(&s0[a..b]);
        } else if a >= s0.len() {
            out.extend_from_slice(&s1[a - s0.len()..b - s0.len()]);
        } else {
            out.extend_from_slice(&s0[a..]);
            out.extend_from_slice(&s1[..b - s0.len()]);
        }
        self.fed = target;
        // Nothing before `fed` is ever needed again.
        let drop = (self.fed - self.base) as usize * CHANNELS;
        self.samples.drain(..drop);
        self.base = self.fed;
    }
}

struct OutState {
    dec: rtrb::Consumer<f32>,
    tap: rtrb::Producer<f32>,
    clock: Arc<Mutex<Option<ClockSample>>>,
    shared: Arc<SourceShared>,
    eof: Arc<AtomicBool>,
    tapped: u64,
    channels: usize,
    underruns: u64,
}

fn fill<T: SizedSample + FromSample<f32>>(
    st: &mut OutState,
    data: &mut [T],
    info: &cpal::OutputCallbackInfo,
) {
    let at = Instant::now();
    let ts = info.timestamp();
    let latency = ts.playback.duration_since(ts.callback);
    let ch = st.channels;
    let frames = data.len() / ch;
    let tap_room = st.tap.slots() >= frames * CHANNELS;
    let mut starved = false;
    for f in 0..frames {
        let (l, r) = if st.dec.slots() >= 2 {
            (st.dec.pop().unwrap_or(0.0), st.dec.pop().unwrap_or(0.0))
        } else {
            starved = true;
            (0.0, 0.0)
        };
        let out = &mut data[f * ch..(f + 1) * ch];
        match ch {
            1 => out[0] = T::from_sample((l + r) * 0.5),
            _ => {
                out[0] = T::from_sample(l);
                out[1] = T::from_sample(r);
                for o in &mut out[2..] {
                    *o = T::from_sample(0.0);
                }
            }
        }
        if tap_room {
            let _ = st.tap.push(l);
            let _ = st.tap.push(r);
        }
    }
    if starved {
        if st.eof.load(Ordering::Acquire) {
            st.shared.ended.store(true, Ordering::Release);
        } else {
            st.underruns += 1;
        }
    }
    if tap_room {
        if let Some(mut g) = st.clock.try_lock() {
            *g = Some(ClockSample {
                tapped_before: st.tapped,
                at,
                latency,
            });
        }
        st.tapped += frames as u64;
    } else {
        st.shared.add_dropped(frames);
    }
    st.shared.add_frames(frames);
}

fn build_stream<T>(
    dev: &cpal::Device,
    config: StreamConfig,
    mut st: OutState,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32> + 'static,
{
    let shared = st.shared.clone();
    dev.build_output_stream(
        config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| fill(&mut st, data, info),
        move |err: cpal::Error| match err.kind() {
            cpal::ErrorKind::Xrun
            | cpal::ErrorKind::RealtimeDenied
            | cpal::ErrorKind::DeviceChanged => {
                log::warn!("audio playback: {err}");
            }
            _ => {
                log::error!("audio playback error: {err}");
                shared.set_failure(Some(err.to_string()));
            }
        },
        None,
    )
}

/// Prefer the file's native rate (no resampling), then stereo, then f32.
fn choose_output(
    dev: &cpal::Device,
    file_rate: u32,
) -> Result<(StreamConfig, SampleFormat), String> {
    let default = dev
        .default_output_config()
        .map_err(|e| format!("no default output config: {e}"))?;
    let mut best: Option<(u32, cpal::SupportedStreamConfig)> = None;
    if let Ok(ranges) = dev.supported_output_configs() {
        for r in ranges {
            let Some(cfg) = r.try_with_sample_rate(file_rate) else {
                continue;
            };
            let score = (cfg.channels() == 2) as u32 * 2
                + (cfg.sample_format() == SampleFormat::F32) as u32;
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, cfg));
            }
        }
    }
    let chosen = best.map(|(_, c)| c).unwrap_or(default);
    Ok((chosen.into(), chosen.sample_format()))
}

fn decode_loop(
    mut dec: AudioDecoder,
    out_rate: u32,
    mut prod: rtrb::Producer<f32>,
    stop: Arc<AtomicBool>,
    eof: Arc<AtomicBool>,
    looping: bool,
    shared: Arc<SourceShared>,
) {
    let path = dec.path().to_path_buf();
    let mut rs = (dec.sample_rate() != out_rate)
        .then(|| StreamResampler::new(dec.sample_rate(), out_rate, CHANNELS));
    let mut pcm = Vec::new();
    let mut pending: Vec<f32> = Vec::new();
    let mut off = 0usize;
    while !stop.load(Ordering::Relaxed) {
        if off < pending.len() {
            let room = prod.slots() & !1;
            let n = room.min(pending.len() - off);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(4));
                continue;
            }
            if let Ok(mut chunk) = prod.write_chunk_uninit(n) {
                let (a, b) = chunk.as_mut_slices();
                let (head, tail) = pending[off..off + n].split_at(a.len());
                for (d, s) in a.iter_mut().zip(head) {
                    d.write(*s);
                }
                for (d, s) in b.iter_mut().zip(tail) {
                    d.write(*s);
                }
                // SAFETY: all `n` slots were initialised above.
                unsafe { chunk.commit_all() };
                off += n;
            }
            continue;
        }
        pending.clear();
        off = 0;
        pcm.clear();
        match dec.next_packet(&mut pcm) {
            Ok(true) => match rs.as_mut() {
                Some(rs) => rs.process(&pcm, &mut pending),
                None => std::mem::swap(&mut pending, &mut pcm),
            },
            Ok(false) => {
                if looping {
                    match AudioDecoder::open(&path) {
                        Ok(d) => dec = d,
                        Err(e) => {
                            shared.set_failure(Some(e));
                            break;
                        }
                    }
                } else {
                    break;
                }
            }
            Err(e) => {
                log::error!("{e}");
                shared.set_failure(Some(e));
                break;
            }
        }
    }
    eof.store(true, Ordering::Release);
}

pub struct FilePlayer {
    _stream: cpal::Stream,
    tap: rtrb::Consumer<f32>,
    clock: Arc<Mutex<Option<ClockSample>>>,
    shared: Arc<SourceShared>,
    stop: Arc<AtomicBool>,
    decode_thread: Option<JoinHandle<()>>,
    rate: u32,
    history: TapHistory,
    scratch: Vec<f32>,
    /// Extra user offset (seconds): positive delays what the visualizer sees.
    delay: f64,
    desc: String,
    total_secs: Option<f64>,
    last_info: Option<(Instant, f64)>,
}

impl FilePlayer {
    pub fn open(
        path: &Path,
        output_device: Option<&str>,
        looping: bool,
        delay_ms: f64,
    ) -> Result<Self, String> {
        let dec = AudioDecoder::open(path)?;
        let file_rate = dec.sample_rate();
        let total_secs = dec.total_frames().map(|n| n as f64 / file_rate as f64);
        let host = cpal::default_host();
        let dev = find_output(&host, output_device)?;
        let dev_label = label(&dev);
        let (config, format) = choose_output(&dev, file_rate)?;
        let rate = config.sample_rate;

        let (dec_prod, dec_cons) = rtrb::RingBuffer::<f32>::new(rate as usize * CHANNELS);
        let (tap_prod, tap_cons) = rtrb::RingBuffer::<f32>::new(rate as usize * CHANNELS * 4);
        let shared = Arc::new(SourceShared::default());
        let stop = Arc::new(AtomicBool::new(false));
        let eof = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(Mutex::new(None));

        let decode_thread = {
            let (stop, eof, shared) = (stop.clone(), eof.clone(), shared.clone());
            std::thread::Builder::new()
                .name("lactea-decode".into())
                .spawn(move || decode_loop(dec, rate, dec_prod, stop, eof, looping, shared))
                .map_err(|e| format!("cannot spawn decode thread: {e}"))?
        };

        let st = OutState {
            dec: dec_cons,
            tap: tap_prod,
            clock: clock.clone(),
            shared: shared.clone(),
            eof: eof.clone(),
            tapped: 0,
            channels: config.channels as usize,
            underruns: 0,
        };
        let stream = match format {
            SampleFormat::F32 => build_stream::<f32>(&dev, config, st),
            SampleFormat::I16 => build_stream::<i16>(&dev, config, st),
            SampleFormat::U16 => build_stream::<u16>(&dev, config, st),
            SampleFormat::I32 => build_stream::<i32>(&dev, config, st),
            SampleFormat::F64 => build_stream::<f64>(&dev, config, st),
            other => {
                stop.store(true, Ordering::Relaxed);
                return Err(format!("unsupported playback sample format {other}"));
            }
        }
        .map_err(|e| {
            stop.store(true, Ordering::Relaxed);
            format!("cannot open playback stream on {dev_label}: {e}")
        })?;
        stream
            .play()
            .map_err(|e| format!("cannot start playback on {dev_label}: {e}"))?;
        log::info!(
            "audio file: {} ({} Hz{}, {})",
            path.display(),
            file_rate,
            total_secs
                .map(|s| format!(", {s:.1} s"))
                .unwrap_or_default(),
            if rate == file_rate {
                format!("played natively on {dev_label}")
            } else {
                format!("resampled to {rate} Hz for {dev_label}")
            }
        );
        Ok(Self {
            _stream: stream,
            tap: tap_cons,
            clock,
            shared,
            stop,
            decode_thread: Some(decode_thread),
            rate,
            history: TapHistory::default(),
            scratch: Vec::new(),
            delay: delay_ms / 1000.0,
            desc: format!(
                "file '{}' via {dev_label}",
                path.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            total_secs,
            last_info: None,
        })
    }

    /// One-line sync diagnostics for the log: audible position vs the position of the samples the
    /// device callback last consumed, and the predicted output latency.
    pub fn sync_line(&self, now: Instant) -> Option<String> {
        let cs = (*self.clock.lock())?;
        let audible = audible_position(&cs, now, self.rate, self.delay);
        let ahead = (cs.tapped_before as f64
            + (now.saturating_duration_since(cs.at)).as_secs_f64() * self.rate as f64)
            - audible;
        Some(format!(
            "pos {:.2}s{} | visualizer fed to {:.2}s | device runs {:.0} ms ahead of audible (latency {:.0} ms)",
            audible / self.rate as f64,
            self.total_secs
                .map(|t| format!(" / {t:.1}s"))
                .unwrap_or_default(),
            self.history.fed() as f64 / self.rate as f64,
            ahead / self.rate as f64 * 1000.0,
            cs.latency.as_secs_f64() * 1000.0,
        ))
    }

    pub fn tick_log(&mut self, now: Instant, every: Duration) {
        if self
            .last_info
            .is_none_or(|(t, _)| now.duration_since(t) >= every)
        {
            if let Some(l) = self.sync_line(now) {
                log::debug!("playback sync: {l}");
            }
            self.last_info = Some((now, 0.0));
        }
    }
}

impl AudioSource for FilePlayer {
    fn describe(&self) -> String {
        self.desc.clone()
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn poll(&mut self, now: Instant, out: &mut Vec<f32>) {
        self.scratch.clear();
        drain_ring(&mut self.tap, &mut self.scratch);
        self.history.extend(&self.scratch);
        let Some(cs) = *self.clock.lock() else {
            return;
        };
        let pos = audible_position(&cs, now, self.rate, self.delay);
        self.history.release(pos, self.rate as u64, out);
        self.tick_log(now, Duration::from_secs(2));
    }

    fn shared(&self) -> &SourceShared {
        &self.shared
    }

    /// Audible position (what the speakers play now), None before the first callback.
    fn position_secs(&self, now: Instant) -> Option<f64> {
        let cs = (*self.clock.lock())?;
        Some((audible_position(&cs, now, self.rate, self.delay) / self.rate as f64).max(0.0))
    }
}

impl Drop for FilePlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.decode_thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(before: u64, at: Instant, latency_ms: u64) -> ClockSample {
        ClockSample {
            tapped_before: before,
            at,
            latency: Duration::from_millis(latency_ms),
        }
    }

    #[test]
    fn audible_position_is_delayed_by_device_latency() {
        let t0 = Instant::now();
        let c = cs(10_000, t0, 50);
        // 20 ms after the callback ran with 50 ms latency: 30 ms *before* the chunk start.
        let p = audible_position(&c, t0 + Duration::from_millis(20), 48_000, 0.0);
        assert!((p - (10_000.0 - 0.030 * 48_000.0)).abs() < 1.0, "{p}");
        // 80 ms after: 30 ms into the chunk.
        let p = audible_position(&c, t0 + Duration::from_millis(80), 48_000, 0.0);
        assert!((p - (10_000.0 + 0.030 * 48_000.0)).abs() < 1.0, "{p}");
        // User delay shifts it back, `now` before the callback is handled.
        let p = audible_position(&c, t0 - Duration::from_millis(10), 48_000, 0.010);
        assert!((p - (10_000.0 - 0.070 * 48_000.0)).abs() < 1.0, "{p}");
    }

    #[test]
    fn history_releases_only_audible_frames_and_never_repeats() {
        let mut h = TapHistory::default();
        // 1000 stereo frames, value = frame index.
        let pcm: Vec<f32> = (0..1000).flat_map(|i| [i as f32, i as f32]).collect();
        h.extend(&pcm);
        let mut out = Vec::new();
        h.release(250.7, 10_000, &mut out);
        assert_eq!(out.len(), 2 * 250);
        assert_eq!(out[2 * 249], 249.0);
        // Going backwards or standing still releases nothing.
        h.release(200.0, 10_000, &mut out);
        h.release(250.9, 10_000, &mut out);
        assert_eq!(out.len(), 2 * 250);
        // Cannot run past what the device has produced.
        h.release(5000.0, 10_000, &mut out);
        assert_eq!(out.len(), 2 * 1000);
        assert_eq!(h.fed(), 1000);
        // The released stream is exactly the tap, in order, with no gaps.
        assert!(
            out.as_chunks::<2>()
                .0
                .iter()
                .enumerate()
                .all(|(i, f)| f[0] == i as f32)
        );
    }

    #[test]
    fn history_handles_wraparound_and_backlog() {
        let mut h = TapHistory::default();
        let mut out = Vec::new();
        let mut next = 0u32;
        // Many small extend/release rounds force the deque to wrap internally.
        for round in 0..200u32 {
            let pcm: Vec<f32> = (0..37).flat_map(|i| [(next + i) as f32; 2]).collect();
            next += 37;
            h.extend(&pcm);
            h.release((next - 5) as f64, 10_000, &mut out);
            assert_eq!(h.fed(), (next - 5) as u64, "round {round}");
        }
        assert!(
            out.as_chunks::<2>()
                .0
                .iter()
                .enumerate()
                .all(|(i, f)| f[0] == i as f32)
        );
        // A stalled render thread: backlog beyond the cap is skipped, newest kept.
        let mut h = TapHistory::default();
        let pcm: Vec<f32> = (0..5000).flat_map(|i| [i as f32; 2]).collect();
        h.extend(&pcm);
        let mut out = Vec::new();
        h.release(5000.0, 1000, &mut out);
        assert_eq!(out.len(), 2 * 1000);
        assert_eq!(out[0], 4000.0);
    }
}
