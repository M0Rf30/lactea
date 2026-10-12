//! cpal capture: an input device, or on Windows/macOS an output device recorded as loopback
//! ("what is playing"). Fallback for hosts without PipeWire and the only path on other
//! operating systems.

use std::sync::Arc;
use std::time::Instant;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample, StreamConfig};

use super::devices::{
    CaptureKind, OS_HAS_LOOPBACK, default_capture_device, default_capture_plan, find_capture, label,
};
use super::{
    AudioSource, CHANNELS, LIVE_RATE, RING_FRAMES, SourceShared, drain_ring, push_frames, to_stereo,
};
use crate::cli::AudioBackend;

pub struct CpalCapture {
    consumer: rtrb::Consumer<f32>,
    shared: Arc<SourceShared>,
    rate: u32,
    desc: String,
    kind: CaptureKind,
    // Dropping the stream stops capture. cpal streams are !Send on some hosts, so the source
    // stays on the render thread.
    _stream: cpal::Stream,
}

/// Pick the config. Input: prefer `LIVE_RATE`, then stereo, then f32; fall back to the device
/// default. Loopback: always the output's own mix format. CoreAudio applies the requested rate
/// to the physical device, so asking a loopback for `LIVE_RATE` would reconfigure the user's
/// speakers, and the visualizer follows `sample_rate()` anyway.
fn choose_config(
    dev: &cpal::Device,
    kind: CaptureKind,
) -> Result<(StreamConfig, SampleFormat), String> {
    if kind == CaptureKind::Loopback {
        let cfg = dev
            .default_output_config()
            .map_err(|e| format!("no default output config: {e}"))?;
        return Ok((cfg.into(), cfg.sample_format()));
    }
    let default = dev
        .default_input_config()
        .map_err(|e| format!("no default input config: {e}"))?;
    let mut best: Option<(u32, cpal::SupportedStreamConfig)> = None;
    if let Ok(ranges) = dev.supported_input_configs() {
        for r in ranges {
            let Some(cfg) = r.try_with_sample_rate(LIVE_RATE) else {
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

fn build<T>(
    dev: &cpal::Device,
    config: StreamConfig,
    mut producer: rtrb::Producer<f32>,
    shared: Arc<SourceShared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + 'static,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels as usize;
    let mut scratch: Vec<f32> = Vec::with_capacity(16 * 1024);
    let mut frame = Vec::<f32>::new();
    let err_shared = shared.clone();
    dev.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            scratch.clear();
            scratch.extend(data.iter().map(|s| s.to_sample::<f32>()));
            frame.clear();
            to_stereo(&scratch, channels, &mut frame);
            push_frames(&mut producer, &frame, &shared);
        },
        move |err: cpal::Error| match err.kind() {
            cpal::ErrorKind::Xrun
            | cpal::ErrorKind::RealtimeDenied
            | cpal::ErrorKind::DeviceChanged => {
                log::warn!("audio capture: {err}");
            }
            _ => {
                log::error!("audio capture error: {err}");
                err_shared.set_failure(Some(err.to_string()));
            }
        },
        None,
    )
}

impl CpalCapture {
    /// Open `wanted` (`--capture-device`: an input, or on Windows/macOS an output captured as
    /// loopback), else the default device `backend` selects, trying its fallback when the first
    /// choice cannot be opened.
    pub fn open(wanted: Option<&str>, backend: AudioBackend) -> Result<Self, String> {
        let host = cpal::default_host();
        if let Some(w) = wanted {
            let (dev, kind) = find_capture(&host, w)?;
            return Self::open_device(&dev, kind);
        }
        let plan = default_capture_plan(OS_HAS_LOOPBACK, backend);
        let first = default_capture_device(&host, plan.first)
            .and_then(|dev| Self::open_device(&dev, plan.first));
        match (first, plan.fallback) {
            (Err(e), Some(kind)) => {
                log::warn!(
                    "audio: {} capture of the default device failed ({e}); using the default {kind} device",
                    plan.first
                );
                default_capture_device(&host, kind).and_then(|dev| Self::open_device(&dev, kind))
            }
            (result, _) => result,
        }
    }

    fn open_device(dev: &cpal::Device, kind: CaptureKind) -> Result<Self, String> {
        let desc = label(dev);
        let (config, format) = choose_config(dev, kind)?;
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_FRAMES * CHANNELS);
        let shared = Arc::new(SourceShared::default());
        let stream = match format {
            SampleFormat::F32 => build::<f32>(dev, config, producer, shared.clone()),
            SampleFormat::I16 => build::<i16>(dev, config, producer, shared.clone()),
            SampleFormat::U16 => build::<u16>(dev, config, producer, shared.clone()),
            SampleFormat::I32 => build::<i32>(dev, config, producer, shared.clone()),
            SampleFormat::U32 => build::<u32>(dev, config, producer, shared.clone()),
            SampleFormat::I8 => build::<i8>(dev, config, producer, shared.clone()),
            SampleFormat::U8 => build::<u8>(dev, config, producer, shared.clone()),
            SampleFormat::F64 => build::<f64>(dev, config, producer, shared.clone()),
            other => return Err(format!("unsupported capture sample format {other}")),
        }
        .map_err(|e| format!("cannot open {kind} capture stream on {desc}: {e}"))?;
        stream
            .play()
            .map_err(|e| format!("cannot start {kind} capture on {desc}: {e}"))?;
        log::info!(
            "audio: capturing {desc} ({kind}; {} ch, {} Hz, {format})",
            config.channels,
            config.sample_rate
        );
        Ok(Self {
            consumer,
            shared,
            rate: config.sample_rate,
            desc: format!("cpal {kind} '{desc}'"),
            kind,
            _stream: stream,
        })
    }

    pub fn kind(&self) -> CaptureKind {
        self.kind
    }
}

impl AudioSource for CpalCapture {
    fn describe(&self) -> String {
        self.desc.clone()
    }
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn poll(&mut self, _now: Instant, out: &mut Vec<f32>) {
        drain_ring(&mut self.consumer, out);
    }
    fn shared(&self) -> &SourceShared {
        &self.shared
    }
}
