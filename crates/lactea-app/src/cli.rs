//! Command line.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum VisualizerKind {
    /// The MilkDrop engine (crate `lactea`).
    #[cfg(feature = "engine")]
    Engine,
    /// Built-in oscilloscope of the incoming PCM (no presets are rendered).
    Scope,
}

impl VisualizerKind {
    pub fn default_kind() -> Self {
        #[cfg(feature = "engine")]
        {
            Self::Engine
        }
        #[cfg(not(feature = "engine"))]
        {
            Self::Scope
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum AudioBackend {
    /// PipeWire sink monitor on Linux when available, else cpal.
    Auto,
    /// Native PipeWire capture of the default sink's monitor (Linux).
    Pipewire,
    /// cpal: default input device (or `--capture-device`).
    Cpal,
}

/// Which renderer a difference between projectM and MilkDrop 2 resolves to (engine only).
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum CompatArg {
    /// projectM behaviour (the measurement oracle; default).
    Projectm,
    /// MilkDrop 2 behaviour.
    Milkdrop2,
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| "expected WIDTHxHEIGHT".to_string())?;
    let w: u32 = w.trim().parse().map_err(|e| format!("width: {e}"))?;
    let h: u32 = h.trim().parse().map_err(|e| format!("height: {e}"))?;
    if w == 0 || h == 0 {
        return Err("size must be non-zero".into());
    }
    Ok((w, h))
}

/// lactea: MilkDrop 2 / projectM-compatible music visualizer.
///
/// Keys: N/Right/Space next preset, P/Left previous, R random, L lock/unlock auto-advance,
/// F/F11 (or double-click) fullscreen, Esc/Q quit.
#[derive(Parser, Debug, Clone)]
#[command(name = "lactea", version)]
pub struct Args {
    /// Preset file to show first.
    #[arg(short, long, value_name = "FILE")]
    pub preset: Option<PathBuf>,

    /// Preset directory, scanned recursively for *.milk (repeatable). Default (when no --preset
    /// was given): the first existing of /usr/share/projectM/presets,
    /// /usr/local/share/projectM/presets and /opt/homebrew/share/projectM/presets; none on Windows.
    #[arg(short, long, value_name = "DIR")]
    pub dir: Vec<PathBuf>,

    /// Seconds between automatic preset changes (0 = never); locked with L.
    #[arg(long, default_value_t = 30.0, value_name = "SECONDS")]
    pub duration: f64,

    /// Seconds a soft cut blends the old and the new preset (automatic changes and N/P/R/arrows;
    /// 0 = hard cuts). The first preset always starts at once. Default: projectM's 3 s.
    #[arg(long, default_value_t = 3.0, value_name = "SECONDS")]
    pub transition_duration: f64,

    /// Pick the next preset at random when auto-advancing.
    #[arg(long)]
    pub shuffle: bool,

    /// Cap the frame rate (default: vsync only; in --deterministic mode: the step rate, 60).
    #[arg(long, value_name = "N")]
    pub fps: Option<u32>,

    /// Present without vsync (Mailbox/Immediate when available). Use with --fps.
    #[arg(long)]
    pub no_vsync: bool,

    /// Initial window size.
    #[arg(long, default_value = "1280x720", value_parser = parse_size, value_name = "WxH")]
    pub size: (u32, u32),

    /// Interpret --size as physical pixels instead of logical (DPI-scaled) units.
    #[arg(long)]
    pub pixels: bool,

    /// Start in borderless fullscreen.
    #[arg(long)]
    pub fullscreen: bool,

    /// What draws into the window.
    #[arg(long, value_enum, default_value_t = VisualizerKind::default_kind())]
    pub visualizer: VisualizerKind,

    /// Texture directory searched recursively by base name (repeatable). Defaults:
    /// oracle/build/texpack of the repository, /usr/share/projectM/textures.
    #[arg(long = "texture-dir", value_name = "DIR")]
    pub texture_dir: Vec<PathBuf>,

    /// Engine seed (rand, noise textures, random textures).
    #[arg(long, default_value_t = 1)]
    pub seed: u32,

    /// Engine: projectM (default) or MilkDrop 2 behaviour where they differ.
    #[arg(long, value_enum, default_value_t = CompatArg::Projectm)]
    pub compat: CompatArg,

    /// Engine: warp mesh cells (both even).
    #[arg(long, default_value = "48x36", value_parser = parse_size, value_name = "WxH")]
    pub mesh: (u32, u32),

    /// Audio capture backend.
    #[arg(long, value_enum, default_value_t = AudioBackend::Auto)]
    pub audio_backend: AudioBackend,

    /// Capture from this cpal input device (name, substring or id; implies --audio-backend cpal).
    #[arg(long, value_name = "NAME", conflicts_with_all = ["audio_file", "audio_wav", "no_audio"])]
    pub capture_device: Option<String>,

    /// PipeWire only: capture this node (node.name or object serial) instead of the default
    /// sink's monitor.
    #[arg(long, value_name = "NODE")]
    pub pipewire_target: Option<String>,

    /// List capture/playback devices and exit.
    #[arg(long)]
    pub list_devices: bool,

    /// Play this audio file (wav, flac, mp3, ogg, aac/m4a, ...) and visualize it in sync.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["audio_wav", "no_audio"])]
    pub audio_file: Option<PathBuf>,

    /// Output device for --audio-file (name, substring or id). Default: system default.
    #[arg(long, value_name = "NAME", requires = "audio_file")]
    pub output_device: Option<String>,

    /// Restart --audio-file from the beginning when it ends.
    #[arg(long = "loop", requires = "audio_file")]
    pub loop_audio: bool,

    /// Shift what the visualizer hears relative to what is audible (+ = later), ms.
    #[arg(long, default_value_t = 0.0, value_name = "MS")]
    pub audio_delay_ms: f64,

    /// Fixed-step input: frame n is rendered at time n/fps with PCM `pcm_range(n)` of this WAV.
    #[arg(
        long,
        value_name = "FILE",
        requires = "deterministic",
        conflicts_with = "no_audio"
    )]
    pub audio_wav: Option<PathBuf>,

    /// Reproducible mode (needs --audio-wav): no wall clock, no capture device.
    #[arg(long, requires = "audio_wav")]
    pub deterministic: bool,

    /// Do not open any audio input (the visualizer hears silence).
    #[arg(long)]
    pub no_audio: bool,

    /// Quit after this many seconds (testing).
    #[arg(long, value_name = "SECONDS")]
    pub exit_after: Option<f64>,

    /// Quit after rendering this many frames (testing, captures).
    #[arg(long, value_name = "N")]
    pub frames: Option<u64>,

    /// Save the window's frame number N (see --screenshot-frame) as PNG and quit. Reads back
    /// the swapchain image, so it never captures anything but this window.
    #[arg(long, value_name = "FILE.png")]
    pub screenshot: Option<PathBuf>,

    /// Frame index captured by --screenshot (0 = the first frame).
    #[arg(long, default_value_t = 90, value_name = "N", requires = "screenshot")]
    pub screenshot_frame: u64,

    /// Testing: scripted key presses, e.g. "30:n,60:p,90:l,120:esc" (frame:key).
    #[arg(long, value_name = "FRAME:KEY,...", hide = true)]
    pub inject_keys: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("lactea").chain(args.iter().copied()))
    }

    #[test]
    fn defaults() {
        let a = parse(&[]).unwrap();
        assert_eq!(a.duration, 30.0);
        // projectM's default soft cut (`m_softCutDuration`)
        assert_eq!(a.transition_duration, 3.0);
        assert_eq!(
            parse(&["--transition-duration", "0"])
                .unwrap()
                .transition_duration,
            0.0
        );
        assert_eq!(a.size, (1280, 720));
        assert!(a.dir.is_empty() && a.fps.is_none() && !a.shuffle);
        assert_eq!(a.audio_backend, AudioBackend::Auto);
    }

    #[test]
    fn deterministic_needs_wav_and_vice_versa() {
        assert!(parse(&["--deterministic"]).is_err());
        assert!(parse(&["--audio-wav", "a.wav"]).is_err());
        let a = parse(&["--audio-wav", "a.wav", "--deterministic", "--fps", "30"]).unwrap();
        assert!(a.deterministic && a.fps == Some(30));
    }

    #[test]
    fn audio_sources_are_exclusive() {
        assert!(parse(&["--audio-file", "a.mp3", "--capture-device", "x"]).is_err());
        assert!(parse(&["--audio-file", "a.mp3", "--no-audio"]).is_err());
        assert!(parse(&["--loop"]).is_err());
        assert!(parse(&["--audio-file", "a.mp3", "--loop"]).is_ok());
    }

    #[test]
    fn size_and_repeatables() {
        let a = parse(&[
            "--size",
            "800X600",
            "--dir",
            "a",
            "--dir",
            "b",
            "--texture-dir",
            "t",
        ])
        .unwrap();
        assert_eq!(a.size, (800, 600));
        assert_eq!(a.dir.len(), 2);
        assert!(parse(&["--size", "0x5"]).is_err());
        assert!(parse(&["--size", "640"]).is_err());
    }
}
