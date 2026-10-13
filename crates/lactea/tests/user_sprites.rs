//! projectM user sprites (`Engine::sprite_*`) through the engine: created from `imgNN` text, drawn over the finished
//! image, `done` removes them after their last frame and `burn` keeps them in the feedback image. Skipped (with a
//! note) without a GPU adapter.

use lactea::{Compat, Engine, EngineConfig, FrameClock, Image, TextureProvider, Transition};

const SIZE: (u32, u32) = (64, 36);

macro_rules! rig_or_skip {
    () => {
        match Rig::new() {
            Some(r) => r,
            None => {
                assert!(
                    std::env::var_os("LACTEA_REQUIRE_GPU").is_none(),
                    "no wgpu adapter (LACTEA_REQUIRE_GPU is set)"
                );
                eprintln!("SKIP: no wgpu adapter");
                return;
            }
        }
    };
}

/// One 8x8 solid image called `red`.
struct Provider;

impl TextureProvider for Provider {
    fn load(&mut self, name: &str) -> Option<Image> {
        (name == "red").then(|| Image {
            width: 8,
            height: 8,
            rgba8: [255, 0, 0, 255].repeat(64),
        })
    }
    fn random_pool(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A preset that draws nothing and keeps its feedback image as it is.
const PRESET: &str =
    "MILKDROP_PRESET_VERSION=300\n[preset00]\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n";

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    tex: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Rig {
    fn new() -> Option<Self> {
        let instance = wgpu::Instance::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: SIZE.0,
                height: SIZE.1,
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
        Some(Self {
            device,
            queue,
            tex,
            view,
        })
    }

    fn engine(&self, compat: Compat) -> Engine {
        let mut e = Engine::new(
            &self.device,
            &self.queue,
            EngineConfig {
                width: SIZE.0,
                height: SIZE.1,
                compat,
                mesh: [16, 12],
                start_clean: true,
                ..Default::default()
            },
        )
        .unwrap();
        e.set_texture_provider(Box::new(Provider));
        e.load_preset(PRESET.as_bytes(), "p.milk", Transition::Cut)
            .unwrap();
        e
    }

    /// Render frame `frame` and return the centre pixel of the composite.
    fn centre(&self, e: &mut Engine, frame: u32) -> [u8; 3] {
        let mut enc = self.device.create_command_encoder(&Default::default());
        e.render(
            &mut enc,
            &self.view,
            FrameClock {
                time: f64::from(frame) / 60.0,
                frame,
            },
        );
        self.queue.submit(Some(enc.finish()));
        let (w, h, rgba) = e
            .renderer()
            .read_stage(lactea::Stage::Final)
            .expect("read back");
        let i = ((h / 2 * w + w / 2) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    }

    /// RGBA8 pixels of the render target.
    fn read_target(&self) -> Vec<u8> {
        let (w, h) = SIZE;
        let bpr = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(bpr * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(enc.finish()));
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let data = buf.slice(..).get_mapped_range().unwrap();
        let mut out = Vec::new();
        for y in 0..h {
            out.extend_from_slice(&data[(y * bpr) as usize..(y * bpr + w * 4) as usize]);
        }
        out
    }
}

const RED: [u8; 3] = [255, 0, 0];
const BLACK: [u8; 3] = [0, 0, 0];

/// `img=red` covering the screen (`sx = sy = 1`), with the given code.
fn section(code: &str) -> Vec<u8> {
    let mut s = String::from("[img00]\nimg=red\ninit_1=sx=1;sy=1;\n");
    for (i, l) in code.lines().enumerate() {
        s.push_str(&format!("code_{}={l}\n", i + 1));
    }
    s.into_bytes()
}

#[test]
fn a_sprite_is_drawn_over_the_image_in_every_mode_and_done_removes_it_after_its_last_frame() {
    let rig = rig_or_skip!();
    for compat in [Compat::ProjectM, Compat::MilkDrop2, Compat::MilkDrop3] {
        let mut e = rig.engine(compat);
        assert_eq!(rig.centre(&mut e, 0), BLACK, "{compat:?}");
        let id = e.sprite_create("MilkDrop", &section("burn=0;done=above(time,0.02);"));
        assert_eq!(id, 1, "{compat:?}");
        assert_eq!(e.sprite_count(), 1);
        assert_eq!(rig.centre(&mut e, 1), RED, "{compat:?}: drawn while alive");
        assert_eq!(
            rig.centre(&mut e, 2),
            RED,
            "{compat:?}: done frame still draws"
        );
        assert_eq!(
            e.sprite_count(),
            0,
            "{compat:?}: removed after the done frame"
        );
        assert_eq!(
            rig.centre(&mut e, 3),
            BLACK,
            "{compat:?}: not burned, so the image is clean again"
        );
    }
}

#[test]
fn burn_keeps_the_sprite_in_the_feedback_image_after_it_is_gone() {
    let rig = rig_or_skip!();
    let mut e = rig.engine(Compat::ProjectM);
    rig.centre(&mut e, 0);
    e.sprite_create("milkdrop", &section("burn=1;done=1;"));
    assert_eq!(rig.centre(&mut e, 1), RED);
    assert_eq!(e.sprite_count(), 0);
    assert_eq!(
        rig.centre(&mut e, 2),
        RED,
        "the preset keeps decaying the burned image (decay 1)"
    );
}

/// projectM draws user sprites after the transition, on the output: during a Warp soft cut the sprite is still an
/// undistorted, axis-aligned rectangle, which it would not be if it were blended in with the presets' images.
#[test]
fn a_sprite_over_a_warp_soft_cut_is_not_distorted_by_the_blend() {
    let rig = rig_or_skip!();
    let solid = |g: f32, b: f32| {
        format!(
            "MILKDROP_PRESET_VERSION=100\nfDecay=1\nfWaveAlpha=0\nob_size=1\nob_a=1\nob_g={g}\nob_b={b}\n"
        )
    };
    let mut e = rig.engine(Compat::ProjectM);
    e.load_preset(solid(1.0, 0.0).as_bytes(), "green", Transition::Cut)
        .unwrap();
    rig.centre(&mut e, 0);
    e.force_next_transition(Some(lactea::TransitionKind::Warp), Some([250, 0, 0, 0]));
    e.load_preset(
        solid(0.0, 1.0).as_bytes(),
        "blue",
        Transition::Soft { seconds: 1.0 },
    )
    .unwrap();
    assert_eq!(
        e.sprite_create("milkdrop", &section("sx=0.4;sy=0.4;burn=0;")),
        1
    );
    // frame 1: the blend is under way and the sprite is on top of it
    let mut enc = rig.device.create_command_encoder(&Default::default());
    e.render(
        &mut enc,
        &rig.view,
        FrameClock {
            time: 0.5,
            frame: 1,
        },
    );
    rig.queue.submit(Some(enc.finish()));
    assert!(e.transition_progress().is_some(), "a soft cut is running");
    let (w, h) = SIZE;
    let px = rig.read_target();
    let (mut n, mut x0, mut x1, mut y0, mut y1) = (0u32, w, 0, h, 0);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if px[i] > 240 && px[i + 1] < 20 && px[i + 2] < 20 {
                n += 1;
                (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
            }
        }
    }
    assert!(n > 100, "the sprite shows ({n} red pixels)");
    assert_eq!(
        n,
        (x1 - x0 + 1) * (y1 - y0 + 1),
        "the red pixels form a full rectangle"
    );
}

#[test]
fn a_sprite_whose_code_does_not_compile_is_refused() {
    let rig = rig_or_skip!();
    let mut e = rig.engine(Compat::ProjectM);
    assert_eq!(e.sprite_create("milkdrop", &section("x = (;")), 0);
    assert_eq!(e.sprite_create("png", &section("")), 0);
    assert_eq!(e.sprite_count(), 0);
}

#[test]
fn sprite_variables_can_be_read_and_driven_by_the_host() {
    let rig = rig_or_skip!();
    let mut e = rig.engine(Compat::ProjectM);
    let id = e.sprite_create("milkdrop", &section("burn=0;a=alpha;"));
    assert_ne!(id, 0);
    e.set_sprite_var(id, "alpha", 0.0);
    assert_eq!(rig.centre(&mut e, 1), BLACK, "alpha 0 draws nothing");
    assert_eq!(e.sprite_var(id, "a"), 0.0);
    e.set_sprite_var(id, "alpha", 1.0);
    assert_eq!(rig.centre(&mut e, 2), RED);
    assert_eq!(e.sprite_var(id, "a"), 1.0);
    assert_eq!(e.sprite_var(id, "nothing"), 0.0);
}
