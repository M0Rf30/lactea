//! Runtime parameters of an [`Engine`]: the `projectm_set_*` / `projectm_get_*` and `projectm_pcm_*`
//! functions that are about the engine itself (as opposed to preset switching, see
//! [`PresetControl`](crate::PresetControl)).
//!
//! This module is a child of `engine` so it can reach the engine's private state; the public API is
//! all that matters.

use super::Engine;
use crate::EngineError;
use lactea_audio::MAX_SAMPLES;

/// projectM's mesh clamp (`ProjectM::SetMeshSize`): an odd size is rounded up to the next even one
/// (a centre row/column would otherwise be missing from the warp mesh), then each axis is limited to
/// `8..=300`.
pub fn clamp_mesh_size(width: u32, height: u32) -> [u32; 2] {
    let f = |v: u32| v.saturating_add(v & 1).clamp(8, 300);
    [f(width), f(height)]
}

impl Engine {
    /// `projectm_pcm_get_max_samples`: sample frames per channel the engine keeps; longer pushes
    /// keep only the newest ones.
    pub const fn pcm_max_samples() -> usize {
        MAX_SAMPLES
    }

    /// `projectm_pcm_add_int16`: interleaved signed 16-bit PCM (full scale 32768), mono or
    /// stereo (more channels: only the first two are analysed).
    pub fn push_pcm_i16(&mut self, interleaved: &[i16], channels: usize) {
        self.analyzer.push_pcm_i16(interleaved, channels);
    }

    /// `projectm_pcm_add_uint8`: interleaved unsigned 8-bit PCM (silence = 128).
    pub fn push_pcm_u8(&mut self, interleaved: &[u8], channels: usize) {
        self.analyzer.push_pcm_u8(interleaved, channels);
    }

    /// `projectm_get_last_frame_time`: the `time` of the last rendered frame (0 before the first).
    pub fn last_frame_time(&self) -> f64 {
        self.last_time.unwrap_or(0.0)
    }

    /// `projectm_get_window_size`: the render size (see [`Engine::resize`]).
    pub fn window_size(&self) -> (u32, u32) {
        (self.cfg.width, self.cfg.height)
    }

    /// `projectm_get_fps`: the frame rate presets see as `fps`.
    pub fn fps(&self) -> u32 {
        self.cfg.fps
    }

    /// `projectm_set_fps`: the frame rate reported to presets (projectM never measures it; this is
    /// the application's target or averaged value). Takes effect on the next frame.
    pub fn set_fps(&mut self, fps: u32) {
        if fps > 0 {
            self.cfg.fps = fps;
        }
    }

    /// The `progress` denominator (see `EngineConfig::preset_duration`).
    pub fn preset_duration(&self) -> f64 {
        self.cfg.preset_duration
    }

    /// `projectm_get_preset_start_clean` (see [`EngineConfig::start_clean`](crate::EngineConfig::start_clean)).
    pub fn start_clean(&self) -> bool {
        self.cfg.start_clean
    }

    /// `projectm_set_preset_start_clean`: applies to presets loaded afterwards.
    pub fn set_start_clean(&mut self, enabled: bool) {
        self.cfg.start_clean = enabled;
    }

    /// `projectm_set_preset_duration` as far as the engine is concerned: the preset variable
    /// `progress = elapsed / duration`. Takes effect on the next frame, for the shown and the incoming preset.
    pub fn set_preset_duration(&mut self, seconds: f64) {
        self.cfg.preset_duration = seconds;
        self.clock.preset_duration = seconds;
        self.clock.duration_a = seconds;
        self.clock.duration_b = seconds;
    }

    /// The display time of the preset loaded last, when it differs from [`Engine::preset_duration`]: projectM's
    /// `TimeKeeper` samples one per preset (`sampledPresetDuration`, e.g.
    /// [`PresetControl::preset_duration_sampled`](crate::PresetControl::preset_duration_sampled)) and the preset's
    /// `progress` runs over it. Call it right after the load; during a soft cut it applies to the incoming preset
    /// and becomes the shown preset's duration when the cut ends. A non-positive or non-finite value is ignored.
    pub fn set_loaded_preset_duration(&mut self, seconds: f64) {
        if !(seconds.is_finite() && seconds > 0.0) {
            return;
        }
        if self.clock.smoothing {
            self.clock.duration_b = seconds;
        } else {
            self.clock.duration_a = seconds;
        }
    }

    /// `projectm_get_mesh_size`: warp-mesh cells per axis.
    pub fn mesh_size(&self) -> [u32; 2] {
        self.cfg.mesh
    }

    /// `projectm_set_mesh_size`: clamped like projectM; returns the size that
    /// is now in use. The warp grid of the previous frame is forgotten (the images are kept), the
    /// preset's `meshx`/`meshy` variables follow.
    pub fn set_mesh_size(&mut self, width: u32, height: u32) -> Result<[u32; 2], EngineError> {
        let mesh = clamp_mesh_size(width, height);
        let old = self.cfg.mesh;
        let result = self
            .all_renderers_mut()
            .into_iter()
            .try_for_each(|r| r.set_mesh_size(mesh));
        if let Err(e) = result {
            for r in self.all_renderers_mut() {
                // the old mesh was accepted before
                let _ = r.set_mesh_size(old);
            }
            return Err(e.into());
        }
        self.cfg.mesh = mesh;
        self.md2_follow_mesh(old);
        Ok(mesh)
    }

    /// `projectm_get_texel_offset`.
    pub fn texel_offset(&self) -> [f32; 2] {
        self.cfg.texel_offset
    }

    /// `projectm_set_texel_offset`: sub-texel offset (in texels) added to warp UVs; takes effect on
    /// the next frame.
    pub fn set_texel_offset(&mut self, offset: [f32; 2]) {
        self.cfg.texel_offset = offset;
        for r in self.all_renderers_mut() {
            r.set_texel_offset(offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineConfig, FrameClock};
    use lactea_testutil::gpu;

    #[test]
    fn mesh_clamp_follows_projectm() {
        assert_eq!(clamp_mesh_size(32, 24), [32, 24]);
        assert_eq!(clamp_mesh_size(33, 25), [34, 26]); // odd -> next even
        assert_eq!(clamp_mesh_size(0, 1), [8, 8]); // lower bound 8
        assert_eq!(clamp_mesh_size(7, 9), [8, 10]);
        assert_eq!(clamp_mesh_size(299, 301), [300, 300]); // 299 -> 300, 301 -> 302 -> 300
        assert_eq!(clamp_mesh_size(10_000, u32::MAX), [300, 300]); // no overflow
        assert_eq!(Engine::pcm_max_samples(), 576);
    }

    /// An unusable config is rounded the way the setters do it, so the blend pattern (sized from the stored mesh) and
    /// the renderer agree: a MilkDrop 2 soft cut on a `[3, 5]` mesh used to index past the pattern.
    #[test]
    fn new_sanitises_the_config_and_a_md2_soft_cut_survives_a_tiny_mesh() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let cfg = EngineConfig {
            width: 0,
            height: 0,
            mesh: [3, 5],
            fps: 0,
            compat: crate::Compat::MilkDrop2,
            ..Default::default()
        };
        let mut e = Engine::new(&device, &queue, cfg).unwrap();
        assert_eq!(e.mesh_size(), [8, 8]);
        assert_eq!((e.window_size(), e.fps()), ((1, 1), 1));
        e.set_fps(0);
        assert_eq!(e.fps(), 1, "a zero rate is ignored");
        let milk = "[preset00]\nper_frame_1=q1 = 1;\n";
        e.load_preset(milk.as_bytes(), "a", crate::Transition::Cut)
            .unwrap();
        e.load_preset(
            milk.as_bytes(),
            "b",
            crate::Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let mut enc = device.create_command_encoder(&Default::default());
        e.render(
            &mut enc,
            &view,
            FrameClock {
                time: 0.1,
                frame: 1,
            },
        );
        queue.submit(Some(enc.finish()));
    }

    #[test]
    fn runtime_parameters_round_trip_and_take_effect() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let cfg = EngineConfig {
            width: 64,
            height: 36,
            mesh: [16, 12],
            ..Default::default()
        };
        let mut e = Engine::new(&device, &queue, cfg).unwrap();
        assert_eq!(
            (
                e.fps(),
                e.preset_duration(),
                e.mesh_size(),
                e.texel_offset()
            ),
            (60, 1e9, [16, 12], [0.0, 0.0])
        );
        assert_eq!(e.window_size(), (64, 36));

        e.set_fps(35);
        e.set_preset_duration(30.0);
        e.set_texel_offset([0.5, 0.25]);
        assert_eq!(e.set_mesh_size(33, 5).unwrap(), [34, 8]);
        assert_eq!(
            (
                e.fps(),
                e.preset_duration(),
                e.texel_offset(),
                e.mesh_size()
            ),
            (35, 30.0, [0.5, 0.25], [34, 8])
        );
        // the renderer follows: geometry of the new mesh, texel offset
        assert_eq!(
            (
                e.renderer().mesh_geometry().gx(),
                e.renderer().mesh_geometry().gy()
            ),
            (34, 8)
        );
        assert_eq!(e.renderer().config().texel_offset, [0.5, 0.25]);

        // the preset sees them: `fps`, `meshx/meshy`, `progress` (frame 0 starts the preset clock)
        let milk = "[preset00]\nper_frame_1=q1 = fps; q2 = meshx; q3 = meshy; q4 = progress;\n";
        e.load_preset(milk.as_bytes(), "params", crate::Transition::Cut)
            .unwrap();
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 64,
                height: 36,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        for (frame, time) in [(0u32, 10.0f64), (1, 25.0)] {
            let mut enc = device.create_command_encoder(&Default::default());
            e.render(&mut enc, &view, FrameClock { time, frame });
            queue.submit(Some(enc.finish()));
        }
        assert_eq!(e.last_frame_time(), 25.0);
        let vars: std::collections::HashMap<String, f64> = e.frame_vars().into_iter().collect();
        assert_eq!(vars["q1"], 35.0);
        assert_eq!((vars["q2"], vars["q3"]), (34.0, 8.0));
        // projectM's `PresetProgressA`: (now - clock time at the load) / duration; the load happened before any frame (time 0)
        assert!(
            (vars["q4"] - 25.0 / 30.0).abs() < 1e-6,
            "progress {}",
            vars["q4"]
        );

        // PCM: the integer entry points feed the same analyser as the float one
        e.push_pcm_i16(&[i16::MAX; 64], 2);
        e.push_pcm_u8(&[255; 64], 1);
    }

    #[test]
    fn idle_preset_loads_and_renders_something() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let cfg = EngineConfig {
            width: 96,
            height: 54,
            ..Default::default()
        };
        let mut e = Engine::new(&device, &queue, cfg).unwrap();
        assert_eq!(e.preset_name(), None);
        let report = e.load_idle_preset().expect("idle preset loads");
        assert!(
            report.warnings.is_empty() && report.unresolved_textures.is_empty(),
            "{report:?}"
        );
        assert_eq!(e.preset_name(), Some(crate::idle::IDLE_PRESET_NAME));
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 96,
                height: 54,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let pcm: Vec<f32> = (0..1470).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        for frame in 0..30u32 {
            e.push_pcm_f32(&pcm, 2);
            let mut enc = device.create_command_encoder(&Default::default());
            e.render(
                &mut enc,
                &view,
                FrameClock {
                    time: f64::from(frame) / 60.0,
                    frame,
                },
            );
            queue.submit(Some(enc.finish()));
        }
        let (_, _, rgba) = e
            .renderer()
            .read_stage(lactea_abi::dump::Stage::Final)
            .expect("read back");
        let lit = rgba
            .chunks(4)
            .filter(|p| p[0] > 8 || p[1] > 8 || p[2] > 8)
            .count();
        assert!(lit > 50, "idle preset drew only {lit} lit pixels");
    }
}
