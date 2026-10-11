//! "No audio" vs "broken capture": the render thread watches both whether *frames arrive* and
//! whether they are *non-silent*, and reports the distinction in the log and the window title.
//! (A previous projectM integration silently analysed digital silence for hours.)

use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum AudioStatus {
    /// Waiting for the first frames.
    Starting,
    /// Frames arrive and carry signal; `peak_db` is the recent peak in dBFS.
    Active { peak_db: f32 },
    /// Frames arrive but are (near-)digital silence: capture works, nothing is playing.
    Silent { secs: u64 },
    /// No frames arrive at all: capture is stalled or the stream is not running.
    NoData { secs: u64 },
    /// The source reported a fatal problem.
    Failed(String),
    /// File playback finished.
    Ended,
}

impl AudioStatus {
    fn kind(&self) -> u8 {
        match self {
            Self::Starting => 0,
            Self::Active { .. } => 1,
            Self::Silent { .. } => 2,
            Self::NoData { .. } => 3,
            Self::Failed(_) => 4,
            Self::Ended => 5,
        }
    }

    /// Short text for the window title.
    pub fn title(&self) -> String {
        match self {
            Self::Starting => "audio: starting".into(),
            Self::Active { peak_db } => format!("audio: ok {peak_db:.0} dB"),
            Self::Silent { secs } => {
                format!("audio: SILENT {secs}s (capture works, nothing playing)")
            }
            Self::NoData { secs } => format!("audio: NO DATA {secs}s (capture stalled)"),
            Self::Failed(e) => format!("audio: FAILED ({e})"),
            Self::Ended => "audio: end of file".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MonitorConfig {
    /// Peak amplitude (linear) at or below which a frame batch counts as silence (-80 dBFS).
    pub silence_peak: f32,
    /// Silence longer than this is reported as `Silent`.
    pub silent_after: Duration,
    /// No frame progress for this long is reported as `NoData`.
    pub no_data_after: Duration,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            silence_peak: 1e-4,
            silent_after: Duration::from_secs(3),
            no_data_after: Duration::from_secs(2),
        }
    }
}

pub struct AudioMonitor {
    cfg: MonitorConfig,
    start: Instant,
    /// First instant frames were seen arriving.
    first_frames: Option<Instant>,
    last_loud: Option<Instant>,
    last_progress: Instant,
    last_frames: u64,
    peak: f32,
    status: AudioStatus,
}

impl AudioMonitor {
    pub fn new(cfg: MonitorConfig, now: Instant) -> Self {
        Self {
            cfg,
            start: now,
            first_frames: None,
            last_loud: None,
            last_progress: now,
            last_frames: 0,
            peak: 0.0,
            status: AudioStatus::Starting,
        }
    }

    pub fn status(&self) -> &AudioStatus {
        &self.status
    }

    /// Recent peak level in dBFS (-120 when silent).
    pub fn peak_db(&self) -> f32 {
        if self.peak > 1e-6 {
            20.0 * self.peak.log10()
        } else {
            -120.0
        }
    }

    /// Feed one frame's worth of observations. `frames_in` is the source's cumulative frame
    /// counter, `pcm` the PCM drained this frame. Returns the new status when its *kind*
    /// changed (for logging).
    pub fn update(
        &mut self,
        now: Instant,
        frames_in: u64,
        ended: bool,
        failure: Option<String>,
        pcm: &[f32],
    ) -> Option<AudioStatus> {
        if frames_in != self.last_frames {
            self.last_frames = frames_in;
            self.last_progress = now;
            self.first_frames.get_or_insert(now);
        }
        let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if peak > self.cfg.silence_peak {
            self.last_loud = Some(now);
        }
        // Peak meter: instant attack, ~0.5 s release at 60 fps.
        self.peak = if peak >= self.peak {
            peak
        } else {
            self.peak * 0.95
        };

        let since = |t: Instant| now.saturating_duration_since(t);
        let new = if let Some(e) = failure {
            AudioStatus::Failed(e)
        } else if ended {
            AudioStatus::Ended
        } else if self.first_frames.is_none() {
            if since(self.start) >= self.cfg.no_data_after.max(Duration::from_secs(3)) {
                AudioStatus::NoData {
                    secs: since(self.start).as_secs(),
                }
            } else {
                AudioStatus::Starting
            }
        } else if since(self.last_progress) >= self.cfg.no_data_after {
            AudioStatus::NoData {
                secs: since(self.last_progress).as_secs(),
            }
        } else {
            // Frames flow. Signal seen within the window -> Active; nothing yet -> keep
            // `Starting` until the silence window has elapsed, then `Silent`.
            let reference = self.last_loud.or(self.first_frames).unwrap_or(self.start);
            let quiet = since(reference);
            if quiet >= self.cfg.silent_after {
                AudioStatus::Silent {
                    secs: quiet.as_secs(),
                }
            } else if self.last_loud.is_some() {
                AudioStatus::Active {
                    peak_db: self.peak_db(),
                }
            } else {
                AudioStatus::Starting
            }
        };
        let changed = new.kind() != self.status.kind();
        self.status = new;
        changed.then(|| self.status.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(
        m: &mut AudioMonitor,
        t0: Instant,
        ms: u64,
        frames: u64,
        pcm: &[f32],
    ) -> Option<AudioStatus> {
        m.update(t0 + Duration::from_millis(ms), frames, false, None, pcm)
    }

    #[test]
    fn distinguishes_silence_from_no_data() {
        let t0 = Instant::now();
        let mut m = AudioMonitor::new(MonitorConfig::default(), t0);
        assert_eq!(*m.status(), AudioStatus::Starting);

        // Signal flowing.
        let tone = [0.5f32, -0.5];
        let s = step(&mut m, t0, 100, 735, &tone).unwrap();
        assert!(matches!(s, AudioStatus::Active { .. }));

        // Frames keep arriving but are digital silence -> Silent after 3 s, not NoData.
        let mut frames = 735u64;
        let mut ms = 100;
        let mut reported = None;
        while ms < 6000 {
            ms += 16;
            frames += 735;
            if let Some(s) = step(&mut m, t0, ms, frames, &[0.0, 0.0]) {
                reported = Some(s);
            }
        }
        assert!(
            matches!(reported, Some(AudioStatus::Silent { .. })),
            "{reported:?}"
        );
        assert!(matches!(m.status(), AudioStatus::Silent { secs } if *secs >= 2));

        // Sound returns.
        let s = step(&mut m, t0, ms + 16, frames + 735, &tone).unwrap();
        assert!(matches!(s, AudioStatus::Active { .. }));

        // Frames stop arriving -> NoData (distinct from Silent).
        let mut reported = None;
        for k in 1..=200 {
            if let Some(s) = step(&mut m, t0, ms + 16 + k * 16, frames + 735, &[]) {
                reported = Some(s);
            }
        }
        assert!(
            matches!(reported, Some(AudioStatus::NoData { .. })),
            "{reported:?}"
        );
    }

    #[test]
    fn silent_from_the_start_is_never_reported_active() {
        let t0 = Instant::now();
        let mut m = AudioMonitor::new(MonitorConfig::default(), t0);
        let mut frames = 0u64;
        let mut seen = Vec::new();
        for k in 1..=400u64 {
            frames += 735;
            if let Some(s) = step(&mut m, t0, k * 16, frames, &[0.0, 0.0]) {
                seen.push(s);
            }
        }
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(matches!(seen[0], AudioStatus::Silent { .. }));
    }

    #[test]
    fn never_started_is_no_data() {
        let t0 = Instant::now();
        let mut m = AudioMonitor::new(MonitorConfig::default(), t0);
        assert!(step(&mut m, t0, 1000, 0, &[]).is_none());
        let s = step(&mut m, t0, 3500, 0, &[]).unwrap();
        assert!(matches!(s, AudioStatus::NoData { .. }));
    }

    #[test]
    fn failure_and_end_override() {
        let t0 = Instant::now();
        let mut m = AudioMonitor::new(MonitorConfig::default(), t0);
        let s = m
            .update(t0, 10, false, Some("device unplugged".into()), &[0.3])
            .unwrap();
        assert_eq!(s, AudioStatus::Failed("device unplugged".into()));
        let s = m.update(t0, 10, true, None, &[]).unwrap();
        assert_eq!(s, AudioStatus::Ended);
    }
}
