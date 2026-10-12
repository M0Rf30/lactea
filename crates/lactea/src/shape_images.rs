//! Textures of custom shapes' `image=` key (`shapecode_N_image`, a projectM addition: a *textured*
//! shape samples that texture instead of the main image).
//!
//! A child module of `engine` (see the `#[path]` declaration there) so it can reach the provider and
//! the renderer; the preset parsing is `lactea-preset`'s `CustomShape::image`, the drawing
//! `lactea-render`'s `ShapeInstance::image`.

use super::Engine;
use crate::PresetReport;
use crate::textures::{Image, shape_image};
use lactea_preset::Preset;
use std::collections::HashSet;

impl Engine {
    /// Hand the images of the preset's enabled shapes to the renderer (projectM loads them lazily at
    /// the first draw; whether a shape is textured can depend on its per-frame code, so every enabled
    /// shape with an `image=` counts). Names the provider cannot supply are reported in
    /// `unresolved_textures`; the renderer then draws a 1x1 black placeholder like projectM.
    /// The MilkDrop 2 family has no such key (`CompatRules::custom_shape_image`; the preset parser leaves it in `extra`).
    pub(super) fn resolve_shape_images(
        &mut self,
        renderer: &mut lactea_render::Renderer,
        preset: &Preset,
        report: &mut PresetReport,
    ) {
        let rules = self.cfg.compat.rules();
        if !rules.custom_shape_image {
            return;
        }
        let slots = rules.custom_slots;
        let mut seen: HashSet<String> = HashSet::new();
        for shape in preset.shapes.iter().take(slots).filter(|s| s.enabled != 0) {
            let Some(im) = shape_image(&shape.image) else {
                continue;
            };
            // projectM's own noise textures are found before any directory is scanned
            if lactea_render::noise::builtin_image_noise(&im.name).is_some() {
                continue;
            }
            if !seen.insert(im.name.clone()) {
                continue;
            }
            let img: Option<Image> = self.provider.as_mut().and_then(|p| p.load(&im.name));
            match img {
                Some(img)
                    if img.width > 0
                        && img.height > 0
                        && img.rgba8.len() == (img.width * img.height * 4) as usize =>
                {
                    match renderer.set_user_texture(&im.name, img.width, img.height, &img.rgba8) {
                        Ok(()) => report.resolved_textures.push((im.name.clone(), im.name)),
                        Err(e) => {
                            report
                                .warnings
                                .push(format!("shape image {}: {e}", im.name));
                            report.unresolved_textures.push(im.name);
                        }
                    }
                }
                _ => report.unresolved_textures.push(im.name),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Engine, EngineConfig, FrameClock, Image, TextureProvider};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Red(Rc<RefCell<Vec<String>>>);
    impl TextureProvider for Red {
        fn load(&mut self, name: &str) -> Option<Image> {
            self.0.borrow_mut().push(name.to_string());
            (name == "redtex").then(|| Image {
                width: 2,
                height: 2,
                rgba8: [255, 0, 0, 255].repeat(4),
            })
        }
        fn random_pool(&self) -> Vec<String> {
            vec!["redtex".into()]
        }
    }

    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    fn shape_preset(image_line: &str) -> String {
        format!(
            "[preset00]\nfDecay=0.9\nfWaveAlpha=0\nshapecode_0_enabled=1\nshapecode_0_sides=4\nshapecode_0_textured=1\n\
             {image_line}shapecode_0_x=0.5\nshapecode_0_y=0.5\nshapecode_0_rad=0.4\nshapecode_0_r=1\nshapecode_0_g=1\n\
             shapecode_0_b=1\nshapecode_0_a=1\nshapecode_0_r2=1\nshapecode_0_g2=1\nshapecode_0_b2=1\nshapecode_0_a2=1\n\
             shapecode_0_border_a=0\n"
        )
    }

    /// Centre pixel of the composite after a few frames.
    fn centre(
        e: &mut Engine,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: (u32, u32),
    ) -> [u8; 4] {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
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
        for frame in 0..3u32 {
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
        let (w, h, rgba) = e
            .renderer()
            .read_stage(lactea_abi::dump::Stage::Final)
            .expect("read back");
        let i = ((h / 2 * w + w / 2) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    /// `shapecode_N_image` (projectM `CustomShape.cpp:154-175`): a textured shape with an `image=` shows that texture
    /// (here solid red, looked up by the name without its `pc_` quality prefix); without the key it shows the main
    /// image (black on the first frames); an image the host cannot supply is reported and drawn as a 1x1 placeholder.
    #[test]
    fn textured_shape_samples_its_image() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let size = (64u32, 36u32);
        let cfg = EngineConfig {
            width: size.0,
            height: size.1,
            ..Default::default()
        };

        let asked = Rc::new(RefCell::new(Vec::new()));
        let mut e = Engine::new(&device, &queue, cfg.clone());
        e.set_texture_provider(Box::new(Red(asked.clone())));
        let report = e
            .load_preset(
                shape_preset("shapecode_0_image=pc_RedTex\n").as_bytes(),
                "img",
                crate::Transition::Cut,
            )
            .unwrap();
        assert_eq!(
            *asked.borrow(),
            vec!["redtex"],
            "prefix and case are stripped before the provider is asked"
        );
        assert_eq!(
            report.resolved_textures,
            vec![("redtex".to_string(), "redtex".to_string())]
        );
        assert!(report.unresolved_textures.is_empty());
        let c = centre(&mut e, &device, &queue, size);
        assert!(
            c[0] > 200 && c[1] < 30 && c[2] < 30,
            "textured with the red image, got {c:?}"
        );

        // no image key: the shape samples the (still black) main image
        let mut e = Engine::new(&device, &queue, cfg.clone());
        e.set_texture_provider(Box::new(Red(Default::default())));
        e.load_preset(shape_preset("").as_bytes(), "noimg", crate::Transition::Cut)
            .unwrap();
        let c = centre(&mut e, &device, &queue, size);
        assert!(
            c[0] < 30 && c[1] < 30 && c[2] < 30,
            "main-image texture on a black frame, got {c:?}"
        );

        // an image nobody can supply: reported, placeholder drawn (never the red one)
        let mut e = Engine::new(&device, &queue, cfg);
        e.set_texture_provider(Box::new(Red(Default::default())));
        let report = e
            .load_preset(
                shape_preset("shapecode_0_image=missing\n").as_bytes(),
                "gone",
                crate::Transition::Cut,
            )
            .unwrap();
        assert_eq!(report.unresolved_textures, vec!["missing".to_string()]);
        let c = centre(&mut e, &device, &queue, size);
        assert!(c[0] < 30, "placeholder, got {c:?}");
    }

    /// projectM registers its noise textures before it scans a directory: `image=noise_lq` never reaches the provider,
    /// is not an unresolved texture, and is drawn with the noise (not the black placeholder).
    #[test]
    fn builtin_noise_texture_as_shape_image() {
        let Some((device, queue)) = gpu() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let size = (64u32, 36u32);
        let cfg = EngineConfig {
            width: size.0,
            height: size.1,
            ..Default::default()
        };
        let asked = Rc::new(RefCell::new(Vec::new()));
        let mut e = Engine::new(&device, &queue, cfg);
        e.set_texture_provider(Box::new(Red(asked.clone())));
        let report = e
            .load_preset(
                shape_preset("shapecode_0_image=pw_noise_lq\n").as_bytes(),
                "noise",
                crate::Transition::Cut,
            )
            .unwrap();
        assert!(asked.borrow().is_empty(), "asked {:?}", asked.borrow());
        assert!(
            report.unresolved_textures.is_empty(),
            "{:?}",
            report.unresolved_textures
        );
        let c = centre(&mut e, &device, &queue, size);
        assert!(
            c[0] > 0 || c[1] > 0 || c[2] > 0,
            "noise texture expected, got {c:?}"
        );
    }
}
