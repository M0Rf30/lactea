//! Remaining `projectm_*` API surface of an [`Engine`]: touch (`touch.h`), the switch-failed
//! callback (`callbacks.h`) and debug image export (`debug.h`).
//!
//! A child of `engine` like `params.rs`, for access to the engine's private state.

use super::Engine;
use crate::callbacks::SwitchFailure;
use crate::{EngineError, PresetReport};
use lactea_abi::dump::Stage;
use lactea_render::RenderError;

/// `projectm_touch_type` (`types.h:68-79`): the waveform a touch asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum TouchType {
    /// Random waveform (the default).
    #[default]
    Random = 0,
    /// Circle.
    Circle = 1,
    /// Radial blob.
    RadialBlob = 2,
    /// Blob variant 2.
    Blob2 = 3,
    /// Blob variant 3.
    Blob3 = 4,
    /// Derivative line.
    DerivativeLine = 5,
    /// Blob variant 5.
    Blob5 = 6,
    /// Single line.
    Line = 7,
    /// Double line.
    DoubleLine = 8,
}

impl TouchType {
    /// The C enum value to a type (`None` outside `0..=8`).
    pub fn from_i32(v: i32) -> Option<Self> {
        Some(match v {
            0 => Self::Random,
            1 => Self::Circle,
            2 => Self::RadialBlob,
            3 => Self::Blob2,
            4 => Self::Blob3,
            5 => Self::DerivativeLine,
            6 => Self::Blob5,
            7 => Self::Line,
            8 => Self::DoubleLine,
            _ => return None,
        })
    }
}

/// A rendered stage read back from the GPU: RGBA8, row 0 = top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugImage {
    /// Pipeline stage the pixels were read from.
    pub stage: Stage,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, RGBA8.
    pub rgba: Vec<u8>,
}

impl Engine {
    /// `projectm_touch`. **Intentionally draws nothing**: in projectM 4.2 the four touch functions are
    /// `// UNIMPLEMENTED` empty bodies (`ProjectM.cpp:595-613`), so a host that calls them gets the
    /// output of a host that does not, which was verified frame by frame against projectM. The
    /// signatures exist so hosts written against projectM link unchanged, and so a future upstream
    /// implementation has a place to land. `pressure` is an `int` in the C header.
    pub fn touch(&mut self, _x: f32, _y: f32, _pressure: i32, _kind: TouchType) {}

    /// `projectm_touch_drag` (no-op, see [`Engine::touch`]).
    pub fn touch_drag(&mut self, _x: f32, _y: f32, _pressure: i32) {}

    /// `projectm_touch_destroy` (no-op, see [`Engine::touch`]).
    pub fn touch_destroy(&mut self, _x: f32, _y: f32) {}

    /// `projectm_touch_destroy_all` (no-op, see [`Engine::touch`]).
    pub fn touch_destroy_all(&mut self) {}

    /// `projectm_set_preset_switch_failed_event_callback`: called from [`Engine::load_preset`] (and
    /// the idle-preset loader) with the preset name and the error text each time a load fails; the
    /// call still returns the `Err`. `None` removes the callback.
    pub fn set_preset_switch_failed_callback(
        &mut self,
        f: Option<impl Fn(&SwitchFailure) + Send + Sync + 'static>,
    ) {
        self.on_switch_failed.set(f);
    }

    /// `projectm_write_debug_image_on_next_frame`, as a pull API: the library never touches the
    /// filesystem, so instead of a file name the host gets the pixels of any pipeline stage of the
    /// last rendered frame (call after the frame's command buffer was submitted) and writes them
    /// wherever it wants. Unlike projectM (an empty stub upstream) every stage is available:
    /// `Final`, `Main` (what the next frame warps), `Warped`, `Blur1..3`. The blur stages hold data
    /// only when the preset samples blur (or, with the `diagnostics` feature, after
    /// `renderer_mut().set_min_blur_levels(3)`).
    ///
    /// While a soft cut runs this is the outgoing preset; see [`Engine::debug_image_incoming`].
    pub fn debug_image(&self, stage: Stage) -> Result<DebugImage, RenderError> {
        image_of(self.renderer(), stage)
    }

    /// [`Engine::debug_image`] of the incoming preset during a soft cut (`None` outside one).
    pub fn debug_image_incoming(&self, stage: Stage) -> Option<Result<DebugImage, RenderError>> {
        self.incoming_renderer().map(|r| image_of(r, stage))
    }

    /// Every stage in [`Stage::ALL`] order. Fails on the first read error.
    pub fn debug_images(&self) -> Result<Vec<DebugImage>, RenderError> {
        Stage::ALL
            .into_iter()
            .map(|s| self.debug_image(s))
            .collect()
    }
}

impl Engine {
    /// `projectm_burn_texture` (`ProjectM::BurnInTexture`): draws `rgba8` (`width x height` pixels, row 0 = top) as
    /// one quad over the image the next frame's warp reads, in the active preset and, during a soft cut, in the
    /// incoming one. `rect = [left, top, width, height]` in output pixels, placed with projectM's arithmetic (see
    /// [`Renderer::burn_texture`](lactea_render::Renderer::burn_texture)). Does nothing before the first preset
    /// load, like projectM without an active preset. projectM takes an OpenGL texture id; here the pixels are copied.
    pub fn burn_texture(
        &mut self,
        rgba8: &[u8],
        width: u32,
        height: u32,
        rect: [i32; 4],
    ) -> Result<(), EngineError> {
        for slot in self.active.iter_mut().chain(self.incoming.iter_mut()) {
            slot.renderer.burn_texture(rgba8, width, height, rect)?;
        }
        Ok(())
    }

    /// `projectm_reset_textures`: forgets every texture the provider decoded and resolves the textures of the running
    /// preset(s) again from the current provider (a host that changed the files behind it sees the new pixels).
    /// Returns the texture names that stay unresolved. The preset's shader interfaces and shape images are kept, so
    /// nothing is recompiled.
    pub fn reload_textures(&mut self) -> Vec<String> {
        self.media_cache.clear();
        let mut report = PresetReport::default();
        for which in 0..2 {
            let slot = if which == 0 {
                self.active.take()
            } else {
                self.incoming.take()
            };
            let Some(mut slot) = slot else { continue };
            slot.renderer.clear_user_textures();
            let ifaces = std::mem::take(&mut slot.loaded.ifaces);
            self.resolve_textures(&mut slot.renderer, &ifaces, &mut report);
            slot.loaded.ifaces = ifaces;
            self.resolve_shape_images(&mut slot.renderer, &slot.loaded.preset, &mut report);
            if which == 0 {
                self.active = Some(slot);
            } else {
                self.incoming = Some(slot);
            }
        }
        report.unresolved_textures
    }
}

fn image_of(r: &lactea_render::Renderer, stage: Stage) -> Result<DebugImage, RenderError> {
    let (width, height, rgba) = r.read_stage(stage)?;
    Ok(DebugImage {
        stage,
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineConfig, FrameClock, Transition};
    use lactea_testutil::gpu;
    use std::sync::mpsc;

    fn target(device: &wgpu::Device, w: u32, h: u32) -> wgpu::TextureView {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
            .create_view(&Default::default())
    }

    const MILK: &str =
        "[preset00]\nwave_r=1\nwave_g=0.5\nwave_a=1\nper_frame_1=wave_x=0.5+0.2*sin(time);\n";

    fn run(touch: bool) -> Vec<DebugImage> {
        let Some((device, queue)) = gpu() else {
            return Vec::new();
        };
        let cfg = EngineConfig {
            width: 64,
            height: 36,
            mesh: [16, 12],
            ..Default::default()
        };
        let mut e = Engine::new(&device, &queue, cfg).unwrap();
        e.load_preset(MILK.as_bytes(), "t", Transition::Cut)
            .unwrap();
        let view = target(&device, 64, 36);
        let pcm: Vec<f32> = (0..1470).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        for frame in 0..8u32 {
            if touch {
                e.touch(0.5, 0.5, 1, TouchType::Circle);
                e.touch_drag(0.6, 0.4, 1);
                if frame == 4 {
                    e.touch_destroy(0.6, 0.4);
                    e.touch_destroy_all();
                }
            }
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
        e.debug_images().unwrap()
    }

    #[test]
    fn touch_is_projectms_no_op_and_debug_images_cover_every_stage() {
        let plain = run(false);
        if plain.is_empty() {
            eprintln!("skipping: no wgpu adapter");
            return;
        }
        assert_eq!(plain.len(), Stage::ALL.len());
        assert_eq!(
            (plain[0].stage, plain[0].width, plain[0].height),
            (Stage::Final, 64, 36)
        );
        for d in &plain {
            assert_eq!(
                d.rgba.len(),
                (d.width * d.height * 4) as usize,
                "{:?}",
                d.stage
            );
        }
        assert!(
            plain[0].rgba.chunks(4).any(|p| p[0] > 8),
            "the wave must be visible"
        );
        assert_eq!(
            plain,
            run(true),
            "touch calls must not change a single pixel"
        );
        assert_eq!(TouchType::from_i32(8), Some(TouchType::DoubleLine));
        assert_eq!(TouchType::from_i32(9), None);
    }

    #[test]
    fn failed_loads_raise_the_callback_and_keep_the_previous_preset() {
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
        let (tx, rx) = mpsc::channel::<SwitchFailure>();
        e.set_preset_switch_failed_callback(Some(move |f: &SwitchFailure| {
            let _ = tx.send(f.clone());
        }));
        e.load_preset(MILK.as_bytes(), "good", Transition::Cut)
            .unwrap();
        assert!(rx.try_recv().is_err());
        let bad = "[preset00]\nper_frame_1=x = (;\n";
        assert!(
            e.load_preset(bad.as_bytes(), "bad", Transition::Cut)
                .is_err()
        );
        let got = rx.try_recv().expect("callback fired");
        assert_eq!(got.name, "bad");
        assert!(!got.message.is_empty());
        assert_eq!(e.preset_name(), Some("good"));
        e.set_preset_switch_failed_callback(None::<fn(&SwitchFailure)>);
        assert!(
            e.load_preset(bad.as_bytes(), "bad2", Transition::Cut)
                .is_err()
        );
        assert!(rx.try_recv().is_err());
    }

    /// `projectm_reset_textures`: a texture whose file changed behind the provider shows the new pixels after
    /// `reload_textures`, and not before.
    #[test]
    fn reload_textures_picks_up_changed_pixels() {
        use crate::{Image, TextureProvider};
        use std::cell::Cell;
        use std::rc::Rc;

        struct Solid(Rc<Cell<[u8; 4]>>);
        impl TextureProvider for Solid {
            fn load(&mut self, name: &str) -> Option<Image> {
                (name == "solid").then(|| Image {
                    width: 2,
                    height: 2,
                    rgba8: self.0.get().repeat(4),
                })
            }
            fn random_pool(&self) -> Vec<String> {
                vec!["solid".into()]
            }
        }

        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let colour = Rc::new(Cell::new([255, 0, 0, 255]));
        let mut e = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: 64,
                height: 36,
                mesh: [16, 12],
                ..Default::default()
            },
        )
        .unwrap();
        e.set_texture_provider(Box::new(Solid(colour.clone())));
        let preset = "[preset00]\nfDecay=0.9\nfWaveAlpha=0\nshapecode_0_enabled=1\nshapecode_0_sides=4\n\
            shapecode_0_textured=1\nshapecode_0_image=solid\nshapecode_0_x=0.5\nshapecode_0_y=0.5\n\
            shapecode_0_rad=0.4\nshapecode_0_r=1\nshapecode_0_g=1\nshapecode_0_b=1\nshapecode_0_a=1\n\
            shapecode_0_r2=1\nshapecode_0_g2=1\nshapecode_0_b2=1\nshapecode_0_a2=1\nshapecode_0_border_a=0\n";
        e.load_preset(preset.as_bytes(), "t", Transition::Cut)
            .unwrap();
        let view = target(&device, 64, 36);
        let mut frame = 0u32;
        let mut centre = |e: &mut Engine| {
            for _ in 0..3 {
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
                frame += 1;
            }
            let d = e.debug_image(Stage::Final).unwrap();
            let i = ((d.height / 2 * d.width + d.width / 2) * 4) as usize;
            [d.rgba[i], d.rgba[i + 1], d.rgba[i + 2]]
        };
        let before = centre(&mut e);
        assert!(before[0] > 200 && before[1] < 40, "{before:?}");
        colour.set([0, 255, 0, 255]);
        assert!(
            centre(&mut e)[0] > 200,
            "the decoded texture is kept until reset"
        );
        assert!(e.reload_textures().is_empty());
        let after = centre(&mut e);
        assert!(after[1] > 200 && after[0] < 40, "{after:?}");
    }
}
