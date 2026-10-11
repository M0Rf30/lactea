//! cpal capture: the default (or `--capture-device`) input device. Fallback for hosts without
//! PipeWire and the only path on other operating systems.

use std::sync::Arc;
use std::time::Instant;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample, StreamConfig};

use super::devices::{find_input, label};
use super::{
    AudioSource, CHANNELS, LIVE_RATE, RING_FRAMES, SourceShared, drain_ring, push_frames, to_stereo,
};

pub struct CpalCapture {
    consumer: rtrb::Consumer<f32>,
    shared: Arc<SourceShared>,
    rate: u32,
    desc: String,
    // Dropping the stream stops capture. cpal streams are !Send on some hosts, so the source
    // stays on the render thread.
    _stream: cpal::Stream,
}

/// Pick the config: prefer `LIVE_RATE`, then stereo, then f32; fall back to the device default.
fn choose_config(dev: &cpal::Device) -> Result<(StreamConfig, SampleFormat), String> {
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
    pub fn open(wanted: Option<&str>) -> Result<Self, String> {
        let host = cpal::default_host();
        let dev = find_input(&host, wanted)?;
        let desc = label(&dev);
        let (config, format) = choose_config(&dev)?;
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_FRAMES * CHANNELS);
        let shared = Arc::new(SourceShared::default());
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&dev, config, producer, shared.clone()),
            SampleFormat::I16 => build::<i16>(&dev, config, producer, shared.clone()),
            SampleFormat::U16 => build::<u16>(&dev, config, producer, shared.clone()),
            SampleFormat::I32 => build::<i32>(&dev, config, producer, shared.clone()),
            SampleFormat::U32 => build::<u32>(&dev, config, producer, shared.clone()),
            SampleFormat::I8 => build::<i8>(&dev, config, producer, shared.clone()),
            SampleFormat::U8 => build::<u8>(&dev, config, producer, shared.clone()),
            SampleFormat::F64 => build::<f64>(&dev, config, producer, shared.clone()),
            other => return Err(format!("unsupported capture sample format {other}")),
        }
        .map_err(|e| format!("cannot open capture stream on {desc}: {e}"))?;
        stream
            .play()
            .map_err(|e| format!("cannot start capture on {desc}: {e}"))?;
        log::info!(
            "cpal capture: {desc} ({} ch, {} Hz, {format})",
            config.channels,
            config.sample_rate
        );
        Ok(Self {
            consumer,
            shared,
            rate: config.sample_rate,
            desc: format!("cpal input '{desc}'"),
            _stream: stream,
        })
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
