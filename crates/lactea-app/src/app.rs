//! The winit application: window, per-frame loop, keys, preset switching, title.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Fullscreen, Window, WindowId};

use crate::audio::monitor::{AudioMonitor, AudioStatus, MonitorConfig};
use crate::cli::{Args, CompatArg, VisualizerKind};
use crate::feed::AudioFeed;
use crate::gfx::{Acquired, Gfx};
use crate::input::{Action, action_for_key, parse_script};
use crate::presets::{self, Nav, PresetBrowser, PresetEntry};
use crate::textures::FsTextureProvider;
use crate::visualizer::{Change, ScopeVisualizer, Visualizer};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Redraw interval while nothing can be presented (occluded, minimised, timeouts).
const IDLE_REDRAW: Duration = Duration::from_millis(16);

struct Running {
    window: Arc<Window>,
    gfx: Gfx,
    viz: Box<dyn Visualizer>,
}

pub struct App {
    args: Args,
    instance: wgpu::Instance,
    run: Option<Running>,
    audio: AudioFeed,
    monitor: AudioMonitor,
    static_status: Option<AudioStatus>,
    presets: PresetBrowser,
    /// `--preset` given: it is entry 0 and is shown first even with --shuffle.
    explicit_first: bool,
    texture_dirs: Vec<PathBuf>,
    start: Instant,
    frame: u64,
    pcm: Vec<f32>,
    /// Earliest instant the next frame may start (frame cap / idle throttle).
    not_before: Instant,
    period: Option<Duration>,
    last_title: String,
    last_title_at: Instant,
    fps_window: (Instant, u64),
    fps: f32,
    last_click: Option<(Instant, PhysicalPosition<f64>)>,
    cursor: PhysicalPosition<f64>,
    /// The cursor is inside the window (`CursorEntered` / `CursorLeft`); forwarded to MilkDrop 3 presets.
    cursor_inside: bool,
    /// The left mouse button is down (window events only: unlike MD3 it is not tracked outside the window).
    left_down: bool,
    fullscreen: bool,
    pending_size: Option<PhysicalSize<u32>>,
    error: Option<String>,
    gpu_rebuilds: u32,
    wav_end_logged: bool,
    /// Scripted key actions (`--inject-keys`), sorted by frame.
    script: VecDeque<(u64, Action)>,
    last_audio_stats: Instant,
    /// Cached monitor refresh rate (mHz) for the frame-cap decision.
    refresh_mhz: Option<u32>,
    refresh_checked: Option<Instant>,
    /// An MD3 auto hard cut was asked for by the last rendered frame; the next frame loads the next
    /// preset with a hard cut (ignored while the browser is locked).
    pending_auto_cut: bool,
    /// `--screenshot` was written; a run that ends without it is an error.
    screenshot_saved: bool,
}

/// Preset list from the CLI: `--preset` first, then every `--dir` (or the first existing default
/// directory, see [`presets::default_preset_dirs`], when neither was given).
pub(crate) fn collect_presets(args: &Args) -> Vec<PresetEntry> {
    let mut out: Vec<PresetEntry> = Vec::new();
    if let Some(p) = &args.preset {
        out.push(presets::entry_for_file(p));
    }
    let mut dirs = args.dir.clone();
    if dirs.is_empty() && args.preset.is_none() {
        dirs.extend(
            presets::default_preset_dirs()
                .iter()
                .map(PathBuf::from)
                .find(|d| d.is_dir()),
        );
    }
    // `.milk2` double presets play only as MilkDrop 3: `--compat milkdrop3`, or `auto` (resolved per preset)
    let milk2 = args.compat.lists_milk2();
    for d in dirs {
        let found = presets::scan_dir(&d, milk2);
        if found.is_empty() {
            log::warn!("no .milk presets found under {}", d.display());
        }
        presets::append_unique(&mut out, found);
    }
    out
}

/// Where the `D` key writes pipeline-stage images: `$LACTEA_DEBUG_DIR`, else `lactea-debug` in the
/// system temporary directory.
fn debug_dump_dir() -> PathBuf {
    std::env::var_os("LACTEA_DEBUG_DIR")
        .map_or_else(|| std::env::temp_dir().join("lactea-debug"), PathBuf::from)
}

/// MD3 auto hard cut modes are 0 (off) ..= this (`--auto-hard-cut`).
const AUTO_HARD_CUT_MODES: u8 = 7;

/// The mode F8 selects after `mode`: 0 -> 1 -> ... -> 7 -> 0.
fn next_auto_hard_cut(mode: u8) -> u8 {
    (mode.min(AUTO_HARD_CUT_MODES) + 1) % (AUTO_HARD_CUT_MODES + 1)
}

/// MD3's wording for a mode: `off`, `hardcut1` .. `hardcut7`.
fn auto_hard_cut_label(mode: u8) -> String {
    match mode {
        0 => "off".to_string(),
        n => format!("hardcut{n}"),
    }
}

/// Explicit `--preset` / `--dir` arguments that do not exist are usage errors, reported before a
/// window opens (a missing default directory is not: the idle preset is shown then).
fn check_inputs(args: &Args) -> Result<(), String> {
    if let Some(p) = &args.preset
        && !p.is_file()
    {
        return Err(format!("--preset {}: no such file", p.display()));
    }
    if let Some(d) = args.dir.iter().find(|d| !d.is_dir()) {
        return Err(format!("--dir {}: no such directory", d.display()));
    }
    Ok(())
}

pub fn run(args: Args) -> anyhow::Result<()> {
    args.validate().map_err(anyhow::Error::msg)?;
    check_inputs(&args).map_err(anyhow::Error::msg)?;
    let presets_list = collect_presets(&args);
    if presets_list.is_empty() && args.visualizer != VisualizerKind::Scope {
        if args.dir.is_empty() && args.preset.is_none() {
            log::warn!(
                "no presets found (no --preset/--dir given; looked in the default directories {:?}): \
                 showing the idle preset",
                presets::default_preset_dirs()
            );
        } else {
            log::warn!("no presets found: showing the idle preset");
        }
    }
    log::info!("{} preset(s)", presets_list.len());

    let script: VecDeque<(u64, Action)> = match &args.inject_keys {
        Some(s) => parse_script(s)
            .map_err(|e| anyhow::anyhow!("--inject-keys: {e}"))?
            .into_iter()
            .filter_map(|(f, k)| action_for_key(&k).map(|a| (f, a)))
            .collect(),
        None => VecDeque::new(),
    };
    let audio = AudioFeed::open(&args).map_err(anyhow::Error::msg)?;
    log::info!("audio input: {}", audio.describe());

    let event_loop = EventLoop::new()?;
    let display = event_loop.owned_display_handle();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
        Box::new(display),
    ));
    let mut app = App::new(args, instance, presets_list, audio);
    app.script = script;
    event_loop.run_app(&mut app)?;
    if let Some(e) = app.error.take() {
        return Err(anyhow::anyhow!(e));
    }
    if let Some(path) = &app.args.screenshot
        && !app.screenshot_saved
    {
        anyhow::bail!(
            "--screenshot {}: the run ended before frame {} was rendered",
            path.display(),
            app.args.screenshot_frame
        );
    }
    Ok(())
}

impl App {
    fn new(
        args: Args,
        instance: wgpu::Instance,
        entries: Vec<PresetEntry>,
        audio: AudioFeed,
    ) -> Self {
        let now = Instant::now();
        let mut texture_dirs = args.texture_dir.clone();
        for d in FsTextureProvider::default_dirs() {
            if !texture_dirs.contains(&d) {
                texture_dirs.push(d);
            }
        }
        let deterministic = args.deterministic;
        let fps_cap = args.fps.or(if deterministic { Some(60) } else { None });
        let period = fps_cap.map(|f| Duration::from_secs_f64(1.0 / f.max(1) as f64));
        let browser_seed = presets::browser_seed(
            args.seed,
            deterministic,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        if args.seed.is_none() && !deterministic {
            log::info!("preset order seed {browser_seed} (--seed {browser_seed} reproduces it)");
        }
        let mut presets = PresetBrowser::new(
            entries,
            args.shuffle,
            Duration::from_secs_f64(args.duration.max(0.0)),
            u64::from(browser_seed),
        );
        if args.compat.md3() {
            presets.set_md3_time(
                Duration::from_secs_f64(args.transition_duration.max(0.0)),
                args.duration_rand,
            );
        }
        let static_status = audio.static_status();
        Self {
            explicit_first: args.preset.is_some(),
            fullscreen: args.fullscreen,
            args,
            instance,
            run: None,
            audio,
            monitor: AudioMonitor::new(MonitorConfig::default(), now),
            static_status,
            presets,
            texture_dirs,
            start: now,
            frame: 0,
            pcm: Vec::new(),
            not_before: now,
            period,
            last_title: String::new(),
            last_title_at: now,
            fps_window: (now, 0),
            fps: 0.0,
            last_click: None,
            cursor: PhysicalPosition::new(0.0, 0.0),
            cursor_inside: false,
            left_down: false,
            pending_size: None,
            error: None,
            gpu_rebuilds: 0,
            wav_end_logged: false,
            script: VecDeque::new(),
            last_audio_stats: now,
            refresh_mhz: None,
            refresh_checked: None,
            pending_auto_cut: false,
            screenshot_saved: false,
        }
    }

    /// Timer-based frame cap, or `None` when vsync already paces at (about) the cap. Sleeping
    /// to a deadline that is within a few percent of the refresh interval beats against the
    /// display and halves the frame rate, so caps at or above the refresh rate are left to
    /// vsync.
    fn effective_period(&mut self, window: &Window, now: Instant) -> Option<Duration> {
        let p = self.period?;
        if self.args.no_vsync {
            return Some(p);
        }
        if self
            .refresh_checked
            .is_none_or(|t| now.duration_since(t) > Duration::from_secs(2))
        {
            self.refresh_mhz = window
                .current_monitor()
                .and_then(|m| m.refresh_rate_millihertz());
            self.refresh_checked = Some(now);
        }
        match self.refresh_mhz {
            Some(mhz) if 1.0 / p.as_secs_f64() >= mhz as f64 / 1000.0 * 0.97 => None,
            _ => Some(p),
        }
    }

    // ---- GPU / visualizer ---------------------------------------------------------------

    fn make_visualizer(&self, gfx: &Gfx) -> Result<Box<dyn Visualizer>, String> {
        let (w, h) = gfx.size();
        match self.args.visualizer {
            VisualizerKind::Scope => Ok(Box::new(ScopeVisualizer::new(
                &gfx.device,
                &gfx.queue,
                gfx.format(),
                w,
                h,
            ))),
            #[cfg(feature = "engine")]
            VisualizerKind::Engine => {
                let textures = FsTextureProvider::new(self.texture_dirs.clone());
                log::info!(
                    "textures: {} file(s) in {} dir(s): {:?}",
                    textures.len(),
                    textures.dirs().len(),
                    textures.dirs()
                );
                crate::engine_vis::EngineVisualizer::new(
                    &gfx.device,
                    &gfx.queue,
                    gfx.format(),
                    (w, h),
                    self.audio.sample_rate(),
                    textures,
                    &crate::engine_vis::EngineOptions {
                        seed: self.args.engine_seed(),
                        fps: self.args.fps.unwrap_or(60),
                        preset_duration: self.args.duration,
                        compat: self.args.compat,
                        mesh: self.args.mesh,
                        auto_hard_cut_mode: self.args.auto_hard_cut,
                    },
                )
                .map(|v| Box::new(v) as Box<dyn Visualizer>)
            }
        }
    }

    fn create_run(
        &self,
        el: &ActiveEventLoop,
        window: Option<Arc<Window>>,
    ) -> Result<Running, String> {
        let window = match window {
            Some(w) => w,
            None => {
                let (w, h) = (self.args.size.0, self.args.size.1);
                let mut attrs = Window::default_attributes().with_title("lactea");
                attrs = if self.args.pixels {
                    attrs.with_inner_size(PhysicalSize::new(w, h))
                } else {
                    attrs.with_inner_size(LogicalSize::new(w as f64, h as f64))
                };
                if self.fullscreen {
                    attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
                }
                #[cfg(target_os = "linux")]
                {
                    use winit::platform::wayland::WindowAttributesExtWayland;
                    attrs = attrs.with_name("lactea", "lactea");
                }
                Arc::new(
                    el.create_window(attrs)
                        .map_err(|e| format!("cannot create window: {e}"))?,
                )
            }
        };
        let gfx = Gfx::new(
            self.instance.clone(),
            window.clone(),
            !self.args.no_vsync,
            self.args.screenshot.is_some(),
        )?;
        let viz = self.make_visualizer(&gfx)?;
        Ok(Running { window, gfx, viz })
    }

    /// The GPU device was lost: rebuild device, surface and visualizer, then reload the preset.
    fn rebuild_gpu(&mut self, el: &ActiveEventLoop, old: Running) -> Option<Running> {
        self.gpu_rebuilds += 1;
        if self.gpu_rebuilds > 3 {
            self.fail(el, "GPU device lost repeatedly; giving up".into());
            return None;
        }
        log::warn!(
            "rebuilding GPU state after device loss (attempt {})",
            self.gpu_rebuilds
        );
        let auto_mode = old.viz.auto_hard_cut_mode();
        let window = old.window.clone();
        drop(old);
        match self.create_run(el, Some(window)) {
            Ok(mut run) => {
                if let Some(mode) = auto_mode {
                    run.viz.set_auto_hard_cut_mode(mode);
                }
                if let Some(idx) = self.presets.current() {
                    let entry = self.presets.entry(idx).clone();
                    if let Err(e) = Self::load_entry(&mut run, &entry, Change::Cut) {
                        log::warn!("reloading '{}' failed: {e}", entry.name);
                    }
                }
                Some(run)
            }
            Err(e) => {
                self.fail(el, format!("cannot recover from GPU device loss: {e}"));
                None
            }
        }
    }

    fn fail(&mut self, el: &ActiveEventLoop, msg: String) {
        log::error!("{msg}");
        self.error = Some(msg);
        el.exit();
    }

    // ---- presets --------------------------------------------------------------------------

    /// Run `f` on the visualizer, containing GPU validation errors and panics.
    fn guarded(
        run: &mut Running,
        f: impl FnOnce(&mut dyn Visualizer) -> Result<(), String>,
    ) -> Result<(), String> {
        let scope = run
            .gfx
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let result = catch_unwind(AssertUnwindSafe(|| f(run.viz.as_mut())));
        let gpu_err = pollster::block_on(scope.pop());
        match result {
            Err(_) => Err("engine panicked while loading the preset".into()),
            Ok(Err(e)) => Err(e),
            Ok(Ok(())) => match gpu_err {
                Some(e) => Err(format!("GPU validation error: {e}")),
                None => Ok(()),
            },
        }
    }

    /// Read + load one preset, containing GPU validation errors and panics.
    fn load_entry(run: &mut Running, entry: &PresetEntry, change: Change) -> Result<(), String> {
        let bytes =
            std::fs::read(&entry.path).map_err(|e| format!("{}: {e}", entry.path.display()))?;
        Self::guarded(run, |viz| viz.load_preset(&bytes, &entry.name, change))
    }

    /// Nothing could be loaded: show the visualizer's idle preset rather than black.
    fn show_idle(run: &mut Running) {
        if let Err(e) = Self::guarded(run, |viz| viz.load_idle()) {
            log::error!("{e}");
        }
    }

    /// Navigate to the preset `nav` selects, skipping presets that fail to load.
    fn switch(&mut self, run: &mut Running, nav: Nav, now: Instant) {
        self.switch_with(run, nav, now, false);
    }

    /// Make everything compat-dependent in the host follow the compat mode the last loaded preset runs
    /// under: under `--compat auto` the MD3 display time applies to MilkDrop 3 presets only (fixed modes
    /// are set once in `new`). `None` (no compat modes) changes nothing.
    fn follow_compat(&mut self, resolved: Option<CompatArg>) {
        if self.args.compat != CompatArg::Auto {
            return;
        }
        match resolved {
            Some(c) if c.md3() => self.presets.set_md3_time(
                Duration::from_secs_f64(self.args.transition_duration.max(0.0)),
                self.args.duration_rand,
            ),
            Some(_) => self.presets.clear_md3_time(),
            None => {}
        }
    }

    /// [`Self::switch`]; `force_cut` loads with a hard cut whatever `--transition-duration` says (MD3
    /// auto hard cuts).
    fn switch_with(&mut self, run: &mut Running, nav: Nav, now: Instant, force_cut: bool) {
        log::debug!("preset navigation: {nav:?}");
        let first = self.presets.current().is_none();
        let start = if first && self.explicit_first {
            Some(0)
        } else {
            self.presets.pick(nav)
        };
        let Some(mut idx) = start else {
            // an empty list (nothing found to play)
            self.presets.stall();
            Self::show_idle(run);
            self.update_title(run, true);
            return;
        };
        // the very first preset starts at once; every other change blends (`--transition-duration`)
        let change = if first || force_cut || self.args.transition_duration <= 0.0 {
            Change::Cut
        } else {
            Change::Soft {
                seconds: self.args.transition_duration,
            }
        };
        let mut tried = 1;
        loop {
            let entry = self.presets.entry(idx).clone();
            let t0 = Instant::now();
            match Self::load_entry(run, &entry, change) {
                Ok(()) => {
                    self.follow_compat(run.viz.current_compat());
                    self.presets.commit(idx, nav, now);
                    log::info!(
                        "preset [{}/{}]{}: {} ({:.0} ms)",
                        idx + 1,
                        self.presets.len(),
                        if self.presets.locked() {
                            " (locked)"
                        } else {
                            ""
                        },
                        entry.name,
                        t0.elapsed().as_secs_f64() * 1000.0
                    );
                    if let Some(c) = run.viz.current_compat() {
                        log::info!("preset {}: compat {}", entry.name, c.label());
                    }
                    break;
                }
                Err(e) => {
                    log::warn!("preset '{}' skipped: {e}", entry.name);
                    match self.presets.after_failure(idx, nav, tried) {
                        Some(next) => {
                            idx = next;
                            tried += 1;
                        }
                        None => {
                            log::error!(
                                "none of the {} preset(s) could be loaded",
                                self.presets.len()
                            );
                            // no retry until the user navigates; with nothing on screen show the
                            // idle preset instead of black
                            self.presets.stall();
                            if self.presets.current().is_none() {
                                Self::show_idle(run);
                            }
                            break;
                        }
                    }
                }
            }
        }
        self.update_title(run, true);
    }

    // ---- input ----------------------------------------------------------------------------

    fn toggle_fullscreen(&mut self, window: &Window) {
        self.fullscreen = window.fullscreen().is_none();
        window.set_fullscreen(self.fullscreen.then_some(Fullscreen::Borderless(None)));
        window.set_cursor_visible(!self.fullscreen);
    }

    fn on_key(&mut self, el: &ActiveEventLoop, ev: KeyEvent) {
        if ev.state != ElementState::Pressed || ev.repeat {
            return;
        }
        if let Some(action) = action_for_key(&ev.logical_key) {
            self.perform(el, action);
        }
    }

    fn perform(&mut self, el: &ActiveEventLoop, action: Action) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        let now = Instant::now();
        match action {
            Action::Quit => el.exit(),
            Action::Navigate(nav) => {
                self.presets.resume();
                self.switch(&mut run, nav, now);
            }
            Action::CycleAutoHardCut => match run.viz.auto_hard_cut_mode() {
                None => log::info!(
                    "auto hard cut needs a MilkDrop 3 preset (--compat milkdrop3, or auto on one)"
                ),
                Some(mode) => {
                    let next = next_auto_hard_cut(mode);
                    run.viz.set_auto_hard_cut_mode(next);
                    if next == 0 {
                        self.pending_auto_cut = false;
                    }
                    log::info!("auto hard cut: {}", auto_hard_cut_label(next));
                    self.update_title(&run, true);
                }
            },
            Action::ToggleFullscreen => self.toggle_fullscreen(&run.window),
            Action::ToggleLock => {
                let locked = self.presets.toggle_lock();
                log::info!("preset {}", if locked { "locked" } else { "unlocked" });
                self.update_title(&run, true);
            }
            Action::DumpStages => {
                let stages = run.viz.dump_stages();
                if stages.is_empty() {
                    log::warn!("this visualizer has no pipeline stages to dump");
                }
                let dir = debug_dump_dir();
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    log::warn!("debug dump: cannot create {}: {e}", dir.display());
                } else {
                    for (name, w, h, rgba) in stages {
                        let path = dir.join(format!("lactea-debug-{:05}-{name}.png", self.frame));
                        match crate::capture::save_png(&path, w, h, rgba) {
                            Ok(()) => log::info!("wrote {}", path.display()),
                            Err(e) => log::warn!("debug dump: {e}"),
                        }
                    }
                }
            }
        }
        self.run = Some(run);
    }

    fn on_click(&mut self, window: &Window) {
        let now = Instant::now();
        let p = self.cursor;
        let double = self.last_click.is_some_and(|(t, q)| {
            now.duration_since(t) <= DOUBLE_CLICK
                && (p.x - q.x).abs() < 8.0
                && (p.y - q.y).abs() < 8.0
        });
        if double {
            self.last_click = None;
            self.toggle_fullscreen(window);
        } else {
            self.last_click = Some((now, p));
        }
    }

    // ---- title ----------------------------------------------------------------------------

    fn audio_text(&self) -> String {
        if let Some(s) = &self.static_status {
            return s.title();
        }
        match &self.audio {
            AudioFeed::Deterministic(w) => {
                format!("deterministic frame {} @ {} fps", self.frame, w.fps())
            }
            AudioFeed::Silent => "audio: off".into(),
            _ => match self.audio.position_secs(Instant::now()) {
                Some(p) => format!(
                    "{} [{} {}:{:02}]",
                    self.monitor.status().title(),
                    self.audio.tag(),
                    p as u64 / 60,
                    p as u64 % 60
                ),
                None => format!("{} [{}]", self.monitor.status().title(), self.audio.tag()),
            },
        }
    }

    fn update_title(&mut self, run: &Running, force: bool) {
        let now = Instant::now();
        if !force && now.duration_since(self.last_title_at) < Duration::from_millis(250) {
            return;
        }
        self.last_title_at = now;
        let compat = match (self.args.compat, run.viz.current_compat()) {
            (CompatArg::Auto, Some(c)) if self.presets.current().is_some() => {
                format!(" [{}]", c.label())
            }
            _ => String::new(),
        };
        let preset = match (self.presets.current(), self.presets.current_entry()) {
            (Some(i), Some(e)) => format!("{}{compat} [{}/{}]", e.name, i + 1, self.presets.len()),
            _ if self.presets.stalled() => "idle preset".to_string(),
            _ => "no preset".to_string(),
        };
        let hardcut = match run.viz.auto_hard_cut_mode() {
            Some(mode) if mode > 0 => format!(" | {}", auto_hard_cut_label(mode)),
            _ => String::new(),
        };
        let title = format!(
            "lactea | {preset}{}{hardcut} | {} | {:.0} fps",
            if self.presets.locked() {
                " | LOCKED"
            } else {
                ""
            },
            self.audio_text(),
            self.fps
        );
        if title != self.last_title {
            log::debug!(target: "lactea::title", "{title}");
            run.window.set_title(&title);
            self.last_title = title;
        }
    }

    fn observe_audio(&mut self, now: Instant) {
        if !self.audio.is_live() {
            return;
        }
        if log::log_enabled!(target: "lactea::level", log::Level::Trace) {
            // `RUST_LOG=lactea::level=trace`: wall-clock epoch ms + peak of the PCM handed to
            // the visualizer this frame. Comparing two instances (one playing, one capturing the
            // monitor) measures audible-vs-fed sync.
            let epoch_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            let peak = self.pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            log::trace!(target: "lactea::level", "{epoch_ms:.1} {peak:.4} {}", self.pcm.len() / 2);
        }
        let (frames, ended, failure) = match self.audio.shared() {
            Some(s) => (
                s.frames_in.load(std::sync::atomic::Ordering::Relaxed),
                s.ended.load(std::sync::atomic::Ordering::Acquire),
                s.failure(),
            ),
            None => return,
        };
        if now.duration_since(self.last_audio_stats) >= Duration::from_secs(2) {
            self.last_audio_stats = now;
            let dropped = self
                .audio
                .shared()
                .map(|s| s.dropped_frames.load(std::sync::atomic::Ordering::Relaxed))
                .unwrap_or(0);
            log::debug!(
                "audio: {:?}, peak {:.1} dBFS, {frames} frames received, {dropped} dropped",
                self.monitor.status(),
                self.monitor.peak_db()
            );
        }
        if let Some(status) = self.monitor.update(now, frames, ended, failure, &self.pcm) {
            let src = self.audio.describe();
            let detail = self.audio.shared().map(|s| s.detail()).unwrap_or_default();
            match status {
                AudioStatus::Active { .. } => log::info!("audio: signal detected on {src}"),
                AudioStatus::Silent { secs } => log::warn!(
                    "audio: SILENCE for {secs}s on {src} (frames are arriving, so the capture works; nothing is playing, or the wrong sink/device is being listened to)"
                ),
                AudioStatus::NoData { secs } => log::error!(
                    "audio: NO DATA for {secs}s from {src} ({detail}); the capture is stalled or not linked"
                ),
                AudioStatus::Failed(e) => log::error!("audio: FAILED on {src}: {e}"),
                AudioStatus::Ended => log::info!("audio: end of file"),
                AudioStatus::Starting => {}
            }
        }
    }

    // ---- frame ----------------------------------------------------------------------------

    fn redraw(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        if self
            .args
            .exit_after
            .is_some_and(|s| now.duration_since(self.start).as_secs_f64() >= s)
            || self.args.frames.is_some_and(|n| self.frame >= n)
        {
            el.exit();
            return;
        }
        // Scripted key presses (testing): run them before this frame's work.
        while self.script.front().is_some_and(|(f, _)| *f <= self.frame) {
            if let Some((_, action)) = self.script.pop_front() {
                log::debug!("scripted key at frame {}: {action:?}", self.frame);
                self.perform(el, action);
            }
        }
        let Some(mut run) = self.run.take() else {
            return;
        };

        if run.gfx.device_lost() {
            match self.rebuild_gpu(el, run) {
                Some(r) => run = r,
                None => return,
            }
        }

        // Size: coalesce resize events into one reconfigure per frame.
        let size = self
            .pending_size
            .take()
            .unwrap_or_else(|| run.window.inner_size());
        if size.width == 0 || size.height == 0 {
            // Minimised: nothing to draw into.
            self.not_before = now + IDLE_REDRAW;
            self.run = Some(run);
            return;
        }
        if run.gfx.resize(size.width, size.height) {
            run.viz.resize(size.width, size.height);
            log::debug!("resized to {}x{}", size.width, size.height);
        }

        // Audio for this frame.
        self.pcm.clear();
        self.audio.poll(now, self.frame, &mut self.pcm);
        self.observe_audio(now);
        if !self.pcm.is_empty() {
            run.viz.push_pcm(&self.pcm, crate::audio::CHANNELS);
        }
        if let AudioFeed::Deterministic(w) = &self.audio
            && !self.wav_end_logged
            && self.frame >= w.video_frames()
        {
            self.wav_end_logged = true;
            log::info!(
                "deterministic audio ended at frame {} (continuing with silence)",
                self.frame
            );
        }

        // Preset: first load, then an MD3 auto hard cut (a hard cut, nothing to do for mode 7 and
        // while locked or with a single preset), then timer.
        let auto_cut = std::mem::take(&mut self.pending_auto_cut)
            && !self.presets.locked()
            && self.presets.len() > 1;
        if self.presets.first_load_due() {
            self.switch(&mut run, Nav::Next, now);
        } else if auto_cut {
            log::debug!("preset switch requested: auto hard cut");
            self.switch_with(&mut run, Nav::Auto, now, true);
        } else if self.presets.auto_due(now) {
            log::debug!("preset switch requested: display time is up");
            self.switch(&mut run, Nav::Auto, now);
        }

        let time = match &self.audio {
            AudioFeed::Deterministic(w) => self.frame as f64 / w.fps() as f64,
            _ => now.duration_since(self.start).as_secs_f64(),
        };

        match run.gfx.acquire() {
            Acquired::Frame(tex) => {
                let view = tex
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let mut enc =
                    run.gfx
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("frame"),
                        });
                let size = run.window.inner_size();
                let inside = self.cursor_inside
                    && (0.0..f64::from(size.width)).contains(&self.cursor.x)
                    && (0.0..f64::from(size.height)).contains(&self.cursor.y);
                run.viz.set_mouse(
                    inside.then(|| {
                        [
                            (self.cursor.x / f64::from(size.width)) as f32,
                            (self.cursor.y / f64::from(size.height)) as f32,
                        ]
                    }),
                    self.left_down,
                );
                run.viz.render(&mut enc, &view, time, self.frame as u32);
                if run.viz.take_auto_cuts() > 0 && !self.presets.locked() {
                    self.pending_auto_cut = true;
                }
                let shot = match &self.args.screenshot {
                    Some(_) if self.frame == self.args.screenshot_frame => {
                        match crate::capture::encode_copy(&run.gfx.device, &mut enc, &tex.texture) {
                            Ok(rb) => Some(rb),
                            Err(e) => {
                                self.fail(el, format!("--screenshot: {e}"));
                                None
                            }
                        }
                    }
                    _ => None,
                };
                run.gfx.queue.submit(Some(enc.finish()));
                if let (Some(rb), Some(path)) = (shot, self.args.screenshot.clone()) {
                    let saved = rb.into_rgba(&run.gfx.device).and_then(|(w, h, rgba)| {
                        crate::capture::save_png(&path, w, h, rgba).map(|_| (w, h))
                    });
                    match saved {
                        Ok((w, h)) => {
                            self.screenshot_saved = true;
                            log::info!(
                                "screenshot of frame {} ({w}x{h}) saved to {}",
                                self.frame,
                                path.display()
                            );
                            el.exit();
                        }
                        Err(e) => self.fail(el, format!("--screenshot: {e}")),
                    }
                }
                run.window.pre_present_notify();
                run.gfx.queue.present(tex);
                self.frame += 1;
                let period = self.effective_period(&run.window, now);
                self.not_before = match period {
                    Some(p) => (self.not_before + p).max(now),
                    None => now,
                };
            }
            Acquired::Skip => {
                self.not_before = now + IDLE_REDRAW;
            }
        }

        // FPS over ~1 s windows.
        let since = now.duration_since(self.fps_window.0);
        if since >= Duration::from_secs(1) {
            self.fps = (self.frame - self.fps_window.1) as f32 / since.as_secs_f32();
            self.fps_window = (now, self.frame);
            log::debug!("{:.1} fps", self.fps);
        }
        self.update_title(&run, false);
        self.run = Some(run);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.run.is_some() {
            return;
        }
        match self.create_run(el, None) {
            Ok(run) => {
                if self.fullscreen {
                    run.window.set_cursor_visible(false);
                }
                run.window.request_redraw();
                self.run = Some(run);
            }
            Err(e) => self.fail(el, e),
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => self.pending_size = Some(size),
            WindowEvent::RedrawRequested => self.redraw(el),
            WindowEvent::KeyboardInput { event, .. } => self.on_key(el, event),
            WindowEvent::CursorMoved { position, .. } => self.cursor = position,
            WindowEvent::CursorEntered { .. } => self.cursor_inside = true,
            WindowEvent::CursorLeft { .. } => self.cursor_inside = false,
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.left_down = state == ElementState::Pressed;
                if self.left_down
                    && let Some(run) = self.run.take()
                {
                    self.on_click(&run.window);
                    self.run = Some(run);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let Some(run) = &self.run else { return };
        let now = Instant::now();
        if now >= self.not_before {
            el.set_control_flow(ControlFlow::Wait);
            run.window.request_redraw();
        } else {
            el.set_control_flow(ControlFlow::WaitUntil(self.not_before));
        }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // Stop audio threads/streams before the window goes away.
        self.audio = AudioFeed::Silent;
        self.run = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f8_cycles_off_through_seven_and_back() {
        let mut seen = vec![0];
        let mut m = 0;
        for _ in 0..8 {
            m = next_auto_hard_cut(m);
            seen.push(m);
        }
        assert_eq!(seen, [0, 1, 2, 3, 4, 5, 6, 7, 0]);
        // out of range (the engine clamps, so it never reports it) restarts at off
        assert_eq!(next_auto_hard_cut(200), 0);
    }

    fn parse(args: &[&str]) -> Args {
        use clap::Parser;
        Args::try_parse_from(std::iter::once("lactea").chain(args.iter().copied())).unwrap()
    }

    /// A `--preset` that does not exist used to open a black window that retried every frame and
    /// exited 0; it is now rejected before a window exists.
    #[test]
    fn missing_preset_or_dir_is_a_usage_error() {
        let e = check_inputs(&parse(&["--preset", "/nonexistent/x.milk"])).unwrap_err();
        assert_eq!(e, "--preset /nonexistent/x.milk: no such file");
        let e = check_inputs(&parse(&["--dir", "/nonexistent"])).unwrap_err();
        assert_eq!(e, "--dir /nonexistent: no such directory");
        let here = std::env::temp_dir();
        let here = here.to_str().unwrap();
        assert!(check_inputs(&parse(&["--dir", here])).is_ok());
        assert!(check_inputs(&parse(&[])).is_ok());
    }

    /// `--deterministic` promises a run that is a pure function of its inputs; an unreadable WAV
    /// must not degrade to wall-clock time.
    #[test]
    fn unreadable_audio_wav_or_file_is_an_error() {
        let wav = parse(&["--deterministic", "--audio-wav", "/nonexistent/a.wav"]);
        let e = AudioFeed::open(&wav).err().expect("must fail");
        assert!(e.starts_with("--audio-wav: "), "{e}");
        let file = parse(&["--audio-file", "/nonexistent/a.mp3"]);
        let e = AudioFeed::open(&file).err().expect("must fail");
        assert!(e.starts_with("--audio-file: "), "{e}");
        assert!(matches!(
            AudioFeed::open(&parse(&["--no-audio"])),
            Ok(AudioFeed::Silent)
        ));
    }
}
