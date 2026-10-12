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

/// Which capture backend `--audio-backend` selects.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum AudioBackend {
    /// PipeWire sink monitor on Linux when available, else cpal (loopback of the default output
    /// on Windows and macOS, the default input device elsewhere).
    Auto,
    /// Native PipeWire capture of the default sink's monitor (Linux).
    Pipewire,
    /// cpal input devices only: the default input device (microphone, line in) or
    /// `--capture-device`.
    Cpal,
    /// cpal loopback of the default output device: WASAPI (Windows) or CoreAudio tap
    /// (macOS 14.6+). An error on Linux, where PipeWire covers it.
    CpalLoopback,
}

/// Which renderer a difference between projectM, MilkDrop 2 and MilkDrop 3 resolves to (engine only).
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum CompatArg {
    /// projectM behaviour (the measurement oracle; default).
    Projectm,
    /// MilkDrop 2 behaviour.
    Milkdrop2,
    /// MilkDrop 3 behaviour (MilkDrop 2 plus the MD3 deltas, `docs/md3.md`).
    Milkdrop3,
}

impl CompatArg {
    /// Whether this mode follows MilkDrop 3 (`CompatRules::md3_auto_hard_cut` /
    /// `md3_preset_time_rand`); usable without the `engine` feature.
    pub fn md3(self) -> bool {
        self == CompatArg::Milkdrop3
    }
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

/// `--mesh WxH`: the engine's warp mesh needs even cell counts of at least 2 each.
fn parse_mesh(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = parse_size(s)?;
    if w % 2 != 0 || h % 2 != 0 || w < 2 || h < 2 {
        return Err(format!(
            "both dimensions must be even and >= 2, got {w}x{h}"
        ));
    }
    Ok((w, h))
}

/// lactea: MilkDrop 2/3 and projectM-compatible music visualizer.
///
/// Keys: N/Right/Space next preset, P/Left previous, R random, L lock/unlock auto-advance,
/// F/F11 (or double-click) fullscreen, D dump the pipeline stages as PNG (`$LACTEA_DEBUG_DIR`),
/// Esc/Q quit.
/// F8 cycles the MD3 auto hard cut mode (`--compat milkdrop3` only).
#[derive(Parser, Debug, Clone)]
#[command(name = "lactea", version)]
pub struct Args {
    /// Preset file to show first.
    #[arg(short, long, value_name = "FILE")]
    pub preset: Option<PathBuf>,

    /// Preset directory, scanned recursively for *.milk (and, with --compat milkdrop3, *.milk2 double
    /// presets; repeatable). Default (when no --preset was given): the first existing of
    /// /usr/share/projectM/presets, /usr/local/share/projectM/presets and
    /// /opt/homebrew/share/projectM/presets; none on Windows.
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
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
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

    /// Engine seed (rand, noise textures, random textures; default 1). Also seeds the shuffle
    /// and R: without --seed (and outside --deterministic) the preset order differs on every
    /// launch, and the log prints the seed that reproduces it.
    #[arg(long)]
    pub seed: Option<u32>,

    /// Engine: projectM (default) or MilkDrop 2 behaviour where they differ.
    #[arg(long, value_enum, default_value_t = CompatArg::Projectm)]
    pub compat: CompatArg,

    /// Engine: warp mesh cells (both even, at least 2).
    #[arg(long, default_value = "48x36", value_parser = parse_mesh, value_name = "WxH")]
    pub mesh: (u32, u32),

    /// Audio capture backend. auto: PipeWire (the default sink's monitor) on Linux when it is
    /// available, otherwise cpal; with cpal, auto records what plays on the default output
    /// device where the OS supports it (Windows, macOS) and falls back to the default input
    /// device, and uses the default input device on Linux.
    #[arg(long, value_enum, default_value_t = AudioBackend::Auto)]
    pub audio_backend: AudioBackend,

    /// Capture from this cpal device (name, substring or id; see --list-devices). Matches input
    /// devices and, on Windows and macOS, output devices, which are then captured as loopback
    /// (what the device plays). Implies a cpal backend: it cannot be combined with
    /// --audio-backend pipewire.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["audio_file", "audio_wav", "no_audio", "pipewire_target"])]
    pub capture_device: Option<String>,

    /// PipeWire only: capture this node (node.name or object.serial, exact; an unknown one is an
    /// error) instead of the default sink's monitor.
    #[arg(long, value_name = "NODE", conflicts_with_all = ["audio_file", "audio_wav", "no_audio"])]
    pub pipewire_target: Option<String>,

    /// List capture/playback devices (cpal inputs, outputs, PipeWire sinks) and exit; * marks the
    /// default of each list, [loopback] an output that --capture-device can record.
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

    /// Shift what the visualizer hears relative to what is audible (+ = later), ms. Needs
    /// --audio-file: it only shifts file playback.
    #[arg(
        long,
        default_value_t = 0.0,
        value_name = "MS",
        requires = "audio_file"
    )]
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

    /// Engine, `--compat milkdrop3` only: initial MilkDrop 3 AutoHardCutMode, 0 = off. 1..=6 change the
    /// preset instantly (a hard cut, whatever --transition-duration) on beat/loudness triggers; 7 injects a
    /// random effect into the composite shader without changing the preset. F8 cycles it at runtime.
    #[arg(
        long,
        default_value_t = 0,
        value_parser = clap::value_parser!(u8).range(0..=7),
        value_name = "N"
    )]
    pub auto_hard_cut: u8,

    /// `--compat milkdrop3` only: MD3 TimeBetweenPresetsRand. A preset stays for --transition-duration +
    /// --duration + a random 0..SECONDS (negative = 0) instead of --duration alone.
    #[arg(long, default_value_t = 10.0, value_name = "SECONDS")]
    pub duration_rand: f64,
}

impl Args {
    /// The engine seed: `--seed`, else 1.
    pub fn engine_seed(&self) -> u32 {
        self.seed.unwrap_or(1)
    }

    /// Cross-argument checks clap cannot express.
    pub fn validate(&self) -> Result<(), String> {
        if self.screenshot.is_some()
            && let Some(n) = self.frames
            && self.screenshot_frame >= n
        {
            return Err(format!(
                "--screenshot-frame {} is never rendered with --frames {n} (frames 0..{n} are)",
                self.screenshot_frame
            ));
        }
        if self.capture_device.is_some() && self.audio_backend == AudioBackend::Pipewire {
            return Err(
                "--capture-device selects a cpal device and conflicts with --audio-backend pipewire"
                    .into(),
            );
        }
        if self.pipewire_target.is_some()
            && matches!(
                self.audio_backend,
                AudioBackend::Cpal | AudioBackend::CpalLoopback
            )
        {
            return Err(
                "--pipewire-target only applies to PipeWire capture: use --audio-backend auto or pipewire"
                    .into(),
            );
        }
        Ok(())
    }
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
    fn audio_delay_needs_an_audio_file() {
        // The 0.0 default must not count as "given".
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--audio-delay-ms", "50"]).is_err());
        let a = parse(&["--audio-file", "a.mp3", "--audio-delay-ms", "50"]).unwrap();
        assert_eq!(a.audio_delay_ms, 50.0);
        assert_eq!(
            parse(&["--audio-file", "a.mp3"]).unwrap().audio_delay_ms,
            0.0
        );
    }

    #[test]
    fn pipewire_target_is_exclusive_with_other_sources() {
        assert!(parse(&["--pipewire-target", "n"]).is_ok());
        assert!(parse(&["--pipewire-target", "n", "--capture-device", "x"]).is_err());
        assert!(parse(&["--pipewire-target", "n", "--audio-file", "a.mp3"]).is_err());
        assert!(parse(&["--pipewire-target", "n", "--no-audio"]).is_err());
    }

    #[test]
    fn capture_device_with_a_non_pipewire_backend_is_fine() {
        // The default backend (auto) is not "explicit pipewire".
        let a = parse(&["--capture-device", "x"]).unwrap();
        assert!(a.validate().is_ok());
        for b in ["auto", "cpal", "cpal-loopback"] {
            let a = parse(&["--capture-device", "x", "--audio-backend", b]).unwrap();
            assert!(a.validate().is_ok(), "{b}");
        }
    }

    #[test]
    fn capture_device_conflicts_with_explicit_pipewire_backend() {
        let a = parse(&["--capture-device", "x", "--audio-backend", "pipewire"]).unwrap();
        let e = a.validate().unwrap_err();
        assert!(
            e.contains("--capture-device") && e.contains("pipewire"),
            "{e}"
        );
    }

    #[test]
    fn pipewire_target_needs_the_pipewire_capable_backends() {
        for b in ["auto", "pipewire"] {
            let a = parse(&["--pipewire-target", "n", "--audio-backend", b]).unwrap();
            assert!(a.validate().is_ok(), "{b}");
        }
        for b in ["cpal", "cpal-loopback"] {
            let a = parse(&["--pipewire-target", "n", "--audio-backend", b]).unwrap();
            assert!(
                a.validate().unwrap_err().contains("--pipewire-target"),
                "{b}"
            );
        }
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

    #[test]
    fn auto_hard_cut_and_duration_rand() {
        let a = parse(&[]).unwrap();
        assert_eq!(a.auto_hard_cut, 0);
        assert_eq!(a.duration_rand, 10.0);
        let a = parse(&["--auto-hard-cut", "7", "--duration-rand", "2.5"]).unwrap();
        assert_eq!((a.auto_hard_cut, a.duration_rand), (7, 2.5));
        assert_eq!(parse(&["--auto-hard-cut", "0"]).unwrap().auto_hard_cut, 0);
        assert!(parse(&["--auto-hard-cut", "8"]).is_err());
        assert!(parse(&["--auto-hard-cut", "-1"]).is_err());
        assert!(parse(&["--auto-hard-cut", "x"]).is_err());
    }

    #[test]
    fn seed_is_explicit_only_when_given() {
        let a = parse(&[]).unwrap();
        assert_eq!((a.seed, a.engine_seed()), (None, 1));
        let a = parse(&["--seed", "7"]).unwrap();
        assert_eq!((a.seed, a.engine_seed()), (Some(7), 7));
    }

    #[test]
    fn screenshot_frame_must_be_rendered() {
        // the default --screenshot-frame is 90
        let a = parse(&["--frames", "5", "--screenshot", "s.png"]).unwrap();
        assert!(a.validate().unwrap_err().contains("--screenshot-frame 90"));
        let a = parse(&[
            "--frames",
            "5",
            "--screenshot",
            "s.png",
            "--screenshot-frame",
            "4",
        ])
        .unwrap();
        assert!(a.validate().is_ok());
        assert!(
            parse(&["--screenshot", "s.png"])
                .unwrap()
                .validate()
                .is_ok()
        );
        assert!(parse(&["--frames", "5"]).unwrap().validate().is_ok());
    }

    #[test]
    fn fps_zero_and_odd_mesh_are_rejected_at_parse_time() {
        assert!(parse(&["--fps", "0"]).is_err());
        assert_eq!(parse(&["--fps", "30"]).unwrap().fps, Some(30));
        assert!(parse(&["--mesh", "47x35"]).is_err());
        assert!(parse(&["--mesh", "0x0"]).is_err());
        assert_eq!(parse(&["--mesh", "32X24"]).unwrap().mesh, (32, 24));
    }
}
