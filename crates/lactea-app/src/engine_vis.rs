//! Adapter from the MilkDrop engine (crate `lactea`) to the shell's [`Visualizer`] trait,
//! and from [`FsTextureProvider`] to the engine's `TextureProvider`.

use lactea::{
    Compat, Engine, EngineConfig, FrameClock, Image, PresetReport, ShaderOutcome, TextureProvider,
    Transition,
};

use crate::cli::CompatArg;
use crate::textures::FsTextureProvider;
use crate::visualizer::{Change, Visualizer};

/// Run-level engine settings that come from the command line.
pub struct EngineOptions {
    pub seed: u32,
    pub fps: u32,
    /// Auto-advance period in seconds (`progress` = elapsed / this); 0 = never rotates.
    pub preset_duration: f64,
    pub compat: CompatArg,
    pub mesh: (u32, u32),
}

struct FsProvider(FsTextureProvider);

impl TextureProvider for FsProvider {
    fn load(&mut self, name: &str) -> Option<Image> {
        self.0.load(name).map(|t| Image {
            width: t.width,
            height: t.height,
            rgba8: t.rgba8,
        })
    }

    fn random_pool(&self) -> Vec<String> {
        self.0.random_pool()
    }
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
        textures: FsTextureProvider,
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
                CompatArg::Projectm => Compat::ProjectM,
                CompatArg::Milkdrop2 => Compat::MilkDrop2,
            },
            output_format: format,
            sample_rate,
            fps: opts.fps,
            // 0 = never rotates: the engine's default (1e9 s) behaves like a locked preset.
            preset_duration: if opts.preset_duration > 0.0 {
                opts.preset_duration
            } else {
                1e9
            },
            ..Default::default()
        };
        let mut engine = Engine::new(device, queue, cfg);
        engine.set_texture_provider(Box::new(FsProvider(textures)));
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
                Ok(())
            }
            Err(e) => Err(e.to_string()),
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
        self.engine.resize(w, h);
    }
}
