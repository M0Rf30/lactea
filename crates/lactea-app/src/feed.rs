//! Chooses and owns the audio input for a run.

use std::time::Instant;

use crate::audio::deterministic::WavFeed;
use crate::audio::monitor::AudioStatus;
use crate::audio::player::FilePlayer;
use crate::audio::{AudioSource, LIVE_RATE, SourceShared};
use crate::cli::{Args, AudioBackend};

pub enum AudioFeed {
    Live {
        source: Box<dyn AudioSource>,
        tag: &'static str,
    },
    Deterministic(WavFeed),
    /// `--no-audio`.
    Silent,
    /// Opening the input failed; the app still runs (and says so in the title).
    Unavailable(String),
}

impl AudioFeed {
    pub fn open(args: &Args) -> AudioFeed {
        if args.no_audio {
            return AudioFeed::Silent;
        }
        if let Some(wav) = &args.audio_wav {
            let fps = args.fps.unwrap_or(60);
            return match WavFeed::load(wav, fps) {
                Ok(f) => {
                    log::info!(
                        "deterministic audio: {} ({} Hz, {} video frames at {fps} fps)",
                        wav.display(),
                        f.sample_rate(),
                        f.video_frames()
                    );
                    AudioFeed::Deterministic(f)
                }
                Err(e) => AudioFeed::Unavailable(e),
            };
        }
        if let Some(file) = &args.audio_file {
            return match FilePlayer::open(
                file,
                args.output_device.as_deref(),
                args.loop_audio,
                args.audio_delay_ms,
            ) {
                Ok(p) => AudioFeed::Live {
                    source: Box::new(p),
                    tag: "file",
                },
                Err(e) => AudioFeed::Unavailable(e),
            };
        }
        Self::open_capture(args)
    }

    fn open_capture(args: &Args) -> AudioFeed {
        let backend = if args.capture_device.is_some() {
            AudioBackend::Cpal
        } else {
            args.audio_backend
        };
        #[cfg(all(feature = "pipewire", target_os = "linux"))]
        if matches!(backend, AudioBackend::Auto | AudioBackend::Pipewire) {
            match crate::audio::pw::PipewireCapture::start(LIVE_RATE, args.pipewire_target.clone())
            {
                Ok(pw) => {
                    log::info!(
                        "audio: capturing the default sink monitor through PipeWire ({LIVE_RATE} Hz stereo)"
                    );
                    return AudioFeed::Live {
                        source: Box::new(pw),
                        tag: "pw",
                    };
                }
                Err(e) if backend == AudioBackend::Pipewire => {
                    return AudioFeed::Unavailable(e);
                }
                Err(e) => log::warn!("PipeWire capture unavailable ({e}); falling back to cpal"),
            }
        }
        #[cfg(not(all(feature = "pipewire", target_os = "linux")))]
        if backend == AudioBackend::Pipewire {
            return AudioFeed::Unavailable(
                "built without PipeWire support (feature `pipewire`, Linux only)".into(),
            );
        }
        let _ = LIVE_RATE;
        match crate::audio::cpal_input::CpalCapture::open(args.capture_device.as_deref()) {
            Ok(c) => AudioFeed::Live {
                source: Box::new(c),
                tag: "cpal",
            },
            Err(e) => AudioFeed::Unavailable(e),
        }
    }

    /// Sample rate of the PCM handed to the visualizer.
    pub fn sample_rate(&self) -> u32 {
        match self {
            AudioFeed::Live { source, .. } => source.sample_rate(),
            AudioFeed::Deterministic(w) => w.sample_rate(),
            AudioFeed::Silent | AudioFeed::Unavailable(_) => LIVE_RATE,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            AudioFeed::Live { source, .. } => source.describe(),
            AudioFeed::Deterministic(w) => format!(
                "deterministic WAV ({} Hz, {} fps)",
                w.sample_rate(),
                w.fps()
            ),
            AudioFeed::Silent => "no audio".into(),
            AudioFeed::Unavailable(e) => format!("unavailable: {e}"),
        }
    }

    /// Short tag for the title.
    pub fn tag(&self) -> &'static str {
        match self {
            AudioFeed::Live { tag, .. } => tag,
            AudioFeed::Deterministic(_) => "wav",
            AudioFeed::Silent => "off",
            AudioFeed::Unavailable(_) => "none",
        }
    }

    pub fn shared(&self) -> Option<&SourceShared> {
        match self {
            AudioFeed::Live { source, .. } => Some(source.shared()),
            _ => None,
        }
    }

    /// Fixed-step feeds are exempt from live-health monitoring.
    pub fn is_live(&self) -> bool {
        matches!(self, AudioFeed::Live { .. })
    }

    /// Audible position of file playback.
    pub fn position_secs(&self, now: Instant) -> Option<f64> {
        match self {
            AudioFeed::Live { source, .. } => source.position_secs(now),
            _ => None,
        }
    }

    /// Fill `out` with the PCM for this frame (interleaved stereo).
    pub fn poll(&mut self, now: Instant, frame: u64, out: &mut Vec<f32>) {
        match self {
            AudioFeed::Live { source, .. } => source.poll(now, out),
            AudioFeed::Deterministic(w) => out.extend_from_slice(w.frame_pcm(frame)),
            AudioFeed::Silent | AudioFeed::Unavailable(_) => {}
        }
    }

    /// Initial status before any monitoring (for the title).
    pub fn static_status(&self) -> Option<AudioStatus> {
        match self {
            AudioFeed::Unavailable(e) => Some(AudioStatus::Failed(e.clone())),
            _ => None,
        }
    }
}
