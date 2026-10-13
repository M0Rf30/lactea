//! Adapter from the MilkDrop engine (crate `lactea`) to the shell's [`Visualizer`] trait.

use lactea::{Compat, Engine, EngineConfig, FrameClock, PresetReport, ShaderOutcome, Transition};
use lactea_host::DirTextures;

use crate::cli::CompatArg;
use crate::visualizer::{Change, Visualizer};

/// Run-level engine settings that come from the command line.
pub struct EngineOptions {
    pub seed: u32,
    pub fps: u32,
    /// Auto-advance period in seconds (`progress` = elapsed / this); 0 = never rotates.
    pub preset_duration: f64,
    pub compat: CompatArg,
    pub mesh: (u32, u32),
    /// Initial MD3 `AutoHardCutMode` (0..=7; acts only with `--compat milkdrop3`).
    pub auto_hard_cut_mode: u8,
    pub shader_overrides: std::collections::HashMap<u64, lactea::ShaderOverride>,
}

pub struct EngineVisualizer {
    engine: Engine,
}

impl EngineVisualizer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        size: (u32, u32),
        sample_rate: u32,
        textures: DirTextures,
        opts: &EngineOptions,
    ) -> Result<Self, String> {
        if !opts.mesh.0.is_multiple_of(2)
            || !opts.mesh.1.is_multiple_of(2)
            || opts.mesh.0 < 2
            || opts.mesh.1 < 2
        {
            return Err(format!(
                "--mesh {}x{}: both dimensions must be even and >= 2",
                opts.mesh.0, opts.mesh.1
            ));
        }
        let cfg = EngineConfig {
            width: size.0,
            height: size.1,
            mesh: [opts.mesh.0, opts.mesh.1],
            seed: opts.seed as u64,
            compat: match opts.compat {
                // the mode before the first load; every load then resolves it from the preset
                CompatArg::Auto | CompatArg::Projectm => Compat::ProjectM,
                CompatArg::Milkdrop2 => Compat::MilkDrop2,
                CompatArg::Milkdrop3 => Compat::MilkDrop3,
            },
            auto_compat: opts.compat == CompatArg::Auto,
            output_format: format,
            sample_rate,
            fps: opts.fps,
            // 0 = never rotates: the engine's default (1e9 s) behaves like a locked preset.
            preset_duration: if opts.preset_duration > 0.0 {
                opts.preset_duration
            } else {
                1e9
            },
            auto_hard_cut_mode: opts.auto_hard_cut_mode,
            shader_overrides: opts.shader_overrides.clone(),
            ..Default::default()
        };
        let mut engine = Engine::new(device, queue, cfg).map_err(|e| e.to_string())?;
        engine.set_texture_provider(Box::new(textures));
        engine.set_preset_switch_failed_callback(Some(|f: &lactea::SwitchFailure| {
            log::debug!(
                "engine: preset switch failed for '{}': {}",
                f.name,
                f.message
            );
        }));
        Ok(Self { engine })
    }
}

fn log_report(r: &PresetReport) {
    let outcome = |o: ShaderOutcome| format!("{o:?}");
    log::debug!(
        "preset '{}': psversion {}, warp {}, comp {}",
        r.name,
        r.preset_version,
        outcome(r.warp),
        outcome(r.comp)
    );
    for w in &r.warnings {
        log::debug!("preset '{}': {w}", r.name);
    }
    for f in &r.shader_fallbacks {
        log::warn!(
            "preset '{}': {:?} shader replaced by {} ({})",
            r.name,
            f.pass,
            f.fallback,
            f.reason
        );
    }
    if !r.unresolved_textures.is_empty() {
        log::warn!(
            "preset '{}': missing textures: {}",
            r.name,
            r.unresolved_textures.join(", ")
        );
    }
}

impl Visualizer for EngineVisualizer {
    fn load_preset(&mut self, bytes: &[u8], name: &str, change: Change) -> Result<(), String> {
        let transition = match change {
            Change::Cut => Transition::Cut,
            Change::Soft { seconds } => Transition::Soft { seconds },
        };
        match self.engine.load_preset(bytes, name, transition) {
            Ok(report) => {
                log_report(&report);
                if let Some(d) = self.engine.double_preset_info() {
                    log::debug!(
                        "double preset '{}': {} (direction {}) held at {}, {} + {}",
                        d.name,
                        d.pattern,
                        d.direction,
                        d.progress,
                        d.name1.as_deref().unwrap_or("?"),
                        d.name2.as_deref().unwrap_or("?")
                    );
                }
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn load_idle(&mut self) -> Result<(), String> {
        match self.engine.load_idle_preset() {
            Ok(report) => {
                log::info!("no preset could be loaded: showing the built-in idle preset");
                log_report(&report);
                Ok(())
            }
            Err(e) => Err(format!("idle preset: {e}")),
        }
    }

    fn push_pcm(&mut self, interleaved: &[f32], channels: usize) {
        self.engine.push_pcm_f32(interleaved, channels);
    }

    fn dump_stages(&mut self) -> Vec<(String, u32, u32, Vec<u8>)> {
        match self.engine.debug_images() {
            Ok(images) => images
                .into_iter()
                .map(|i| (i.stage.dir().to_string(), i.width, i.height, i.rgba))
                .collect(),
            Err(e) => {
                log::warn!("debug stage read failed: {e}");
                Vec::new()
            }
        }
    }

    fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        time: f64,
        frame: u32,
    ) {
        self.engine
            .render(encoder, view, FrameClock { time, frame });
    }

    fn resize(&mut self, w: u32, h: u32) {
        if let Err(e) = self.engine.resize(w, h) {
            log::warn!("engine resize to {w}x{h} failed: {e}");
        }
    }

    fn take_auto_cuts(&mut self) -> u32 {
        self.engine.take_auto_cuts()
    }

    fn set_auto_hard_cut_mode(&mut self, mode: u8) {
        self.engine.set_auto_hard_cut_mode(mode);
    }

    fn auto_hard_cut_mode(&self) -> Option<u8> {
        // Unsupported compat modes store the mode but never act on it; under auto the mode is the
        // resolved one (`config().compat` is not).
        self.engine
            .current_compat()
            .rules()
            .md3_auto_hard_cut
            .then(|| self.engine.auto_hard_cut_mode())
    }

    fn current_compat(&self) -> Option<CompatArg> {
        Some(match self.engine.current_compat() {
            Compat::ProjectM => CompatArg::Projectm,
            Compat::MilkDrop2 => CompatArg::Milkdrop2,
            Compat::MilkDrop3 => CompatArg::Milkdrop3,
        })
    }

    fn set_loaded_preset_duration(&mut self, seconds: f64) {
        self.engine.set_loaded_preset_duration(seconds);
    }

    fn set_mouse(&mut self, position: Option<[f32; 2]>, left_down: bool) {
        self.engine.set_mouse(lactea::MouseState {
            position,
            left_down,
        });
    }
}
