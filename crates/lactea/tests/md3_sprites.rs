//! MilkDrop 3 preset sprites through the engine: parsing, loading through the provider, drawing, lifetime and
//! the compat gating; animated GIFs as sprites and as shader textures. Skipped (with a note) without a GPU adapter.

use lactea::{
    AnimFrame, AnimatedImage, Compat, Engine, EngineConfig, FrameClock, Image, Media,
    TextureProvider, Transition,
};
use std::cell::RefCell;
use std::rc::Rc;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

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

const SIZE: (u32, u32) = (64, 36);

fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.repeat((w * h) as usize)
}

fn gif(colors: &[[u8; 4]], delays: &[f64]) -> Media {
    Media::Animated(AnimatedImage {
        width: 4,
        height: 4,
        frames: colors
            .iter()
            .zip(delays)
            .map(|(c, d)| AnimFrame {
                rgba8: solid(4, 4, *c),
                delay: *d,
            })
            .collect(),
    })
}

/// A provider with one sprite image and one shader texture; remembers what it was asked.
struct Provider {
    sprite: Option<Media>,
    texture: Option<Media>,
    asked: Rc<RefCell<Vec<String>>>,
}

impl TextureProvider for Provider {
    fn load(&mut self, name: &str) -> Option<Image> {
        self.asked.borrow_mut().push(format!("load {name}"));
        None
    }
    fn random_pool(&self) -> Vec<String> {
        Vec::new()
    }
    fn load_md3_texture(&mut self, name: &str) -> Option<Media> {
        self.asked.borrow_mut().push(format!("md3 {name}"));
        self.texture.clone()
    }
    fn load_sprite(&mut self, path: &str) -> Option<Media> {
        self.asked.borrow_mut().push(format!("sprite {path}"));
        self.sprite.clone()
    }
}

fn provider(sprite: Option<Media>, texture: Option<Media>) -> (Provider, Rc<RefCell<Vec<String>>>) {
    let asked = Rc::new(RefCell::new(Vec::new()));
    (
        Provider {
            sprite,
            texture,
            asked: asked.clone(),
        },
        asked,
    )
}

fn red() -> Media {
    Media::Still(Image {
        width: 8,
        height: 8,
        rgba8: solid(8, 8, [255, 0, 0, 255]),
    })
}

/// A preset that draws nothing of its own plus a sprite block (the MD3 writer's layout).
fn preset(sprite_keys: &str, code: &str) -> String {
    let mut s = String::from(
        "MILKDROP_PRESET_VERSION=300\n[preset00]\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n\n[SPRITE1_BEGIN]\n\
         SpriteName=sprites\\red.png\n",
    );
    s.push_str(sprite_keys);
    for (i, l) in code.lines().enumerate() {
        s.push_str(&format!("code_{}={l}\n", i + 1));
    }
    s.push_str("[SPRITE1_END]\n");
    s
}

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    view: wgpu::TextureView,
}

impl Rig {
    fn new() -> Option<Self> {
        let (device, queue) = gpu()?;
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
            view,
        })
    }

    fn engine(&self, compat: Compat, p: Provider) -> Engine {
        let mut e = Engine::new(
            &self.device,
            &self.queue,
            EngineConfig {
                width: SIZE.0,
                height: SIZE.1,
                compat,
                mesh: [16, 12],
                // a new preset starts black, not from the old finished image (which holds the old sprite)
                start_clean: true,
                ..Default::default()
            },
        );
        e.set_texture_provider(Box::new(p));
        e
    }

    /// Render frame `frame` at `time` and return the centre pixel of the composite.
    fn centre_at(&self, e: &mut Engine, time: f64, frame: u32) -> [u8; 3] {
        let mut enc = self.device.create_command_encoder(&Default::default());
        e.render(&mut enc, &self.view, FrameClock { time, frame });
        self.queue.submit(Some(enc.finish()));
        let (w, h, rgba) = e
            .renderer()
            .read_stage(lactea::Stage::Final)
            .expect("read back");
        let i = ((h / 2 * w + w / 2) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    }
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 6)
}

/// The sprite of an MD3 preset is asked from the provider by its path, drawn at the centre, reported; projectM
/// and MilkDrop 2 ignore the block and never ask.
#[test]
fn md3_draws_the_preset_sprite_and_the_other_modes_ignore_it() {
    let rig = rig_or_skip!();
    let keys = "SpriteLayer=0\nSpriteBlend=4\nSpriteBurn=0\n";
    let (p, asked) = provider(Some(red()), None);
    let mut e = rig.engine(Compat::MilkDrop3, p);
    let report = e
        .load_preset(preset(keys, "").as_bytes(), "s", Transition::Cut)
        .unwrap();
    assert_eq!(report.sprite.as_deref(), Some("sprites\\red.png"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(*asked.borrow(), vec!["sprite sprites\\red.png"]);
    assert!(near(rig.centre_at(&mut e, 0.0, 0), [255, 0, 0]));

    for compat in [Compat::ProjectM, Compat::MilkDrop2] {
        let (p, asked) = provider(Some(red()), None);
        let mut e = rig.engine(compat, p);
        let report = e
            .load_preset(preset(keys, "").as_bytes(), "s", Transition::Cut)
            .unwrap();
        assert_eq!(report.sprite, None, "{compat:?}");
        assert!(
            asked.borrow().is_empty(),
            "{compat:?} asked {:?}",
            asked.borrow()
        );
        assert!(near(rig.centre_at(&mut e, 0.0, 0), [0, 0, 0]), "{compat:?}");
    }
}

#[test]
fn missing_image_is_a_warning_and_the_next_preset_drops_the_sprite() {
    let rig = rig_or_skip!();
    let (p, _) = provider(None, None);
    let mut e = rig.engine(Compat::MilkDrop3, p);
    let report = e
        .load_preset(preset("", "").as_bytes(), "gone", Transition::Cut)
        .unwrap();
    assert_eq!(report.sprite, None);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("image not found")),
        "{:?}",
        report.warnings
    );

    // a sprite, then a preset without one: the sprite is killed at the load
    let (p, _) = provider(Some(red()), None);
    let mut e = rig.engine(Compat::MilkDrop3, p);
    e.load_preset(
        preset("SpriteBurn=0\n", "").as_bytes(),
        "a",
        Transition::Cut,
    )
    .unwrap();
    assert!(near(rig.centre_at(&mut e, 0.0, 0), [255, 0, 0]));
    e.load_preset(
        b"MILKDROP_PRESET_VERSION=300\n[preset00]\nfDecay=1\nfWaveAlpha=0\n",
        "b",
        Transition::Cut,
    )
    .unwrap();
    assert!(near(rig.centre_at(&mut e, 0.1, 1), [0, 0, 0]));
    // also through a MilkDrop 2 style soft cut
    e.load_preset(
        preset("SpriteBurn=0\n", "").as_bytes(),
        "c",
        Transition::Cut,
    )
    .unwrap();
    e.load_preset(
        b"MILKDROP_PRESET_VERSION=300\n[preset00]\nfDecay=1\nfWaveAlpha=0\n",
        "d",
        Transition::Soft { seconds: 1.0 },
    )
    .unwrap();
    assert!(near(rig.centre_at(&mut e, 0.2, 2), [0, 0, 0]));
}

/// The colour key of a static image: texels of exactly that colour are transparent for the keyed blend modes.
#[test]
fn color_key_hides_matching_texels_only_for_static_images() {
    let rig = rig_or_skip!();
    let draws = |key: &str| {
        let (p, _) = provider(Some(red()), None);
        let mut e = rig.engine(Compat::MilkDrop3, p);
        e.load_preset(
            preset(&format!("SpriteColorKey={key}\nSpriteBurn=0\n"), "").as_bytes(),
            "k",
            Transition::Cut,
        )
        .unwrap();
        rig.centre_at(&mut e, 0.0, 0)
    };
    assert!(near(draws("0x000000"), [255, 0, 0]), "key does not match");
    assert!(
        near(draws("0xFF0000"), [0, 0, 0]),
        "key matches: transparent"
    );

    // an animated image is never keyed
    let (p, _) = provider(Some(gif(&[[255, 0, 0, 255]], &[0.1])), None);
    let mut e = rig.engine(Compat::MilkDrop3, p);
    e.load_preset(
        preset("SpriteColorKey=0xFF0000\nSpriteBurn=0\n", "").as_bytes(),
        "g",
        Transition::Cut,
    )
    .unwrap();
    assert!(near(rig.centre_at(&mut e, 0.0, 0), [255, 0, 0]));
}

/// GIF sprite: the frame follows `fmod(time since the sprite started, total)` with delays 0.1 / 0.2 / 0.05.
#[test]
fn gif_sprite_frames_follow_the_delays_and_loop() {
    let rig = rig_or_skip!();
    let colors = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]];
    let (p, _) = provider(Some(gif(&colors, &[0.1, 0.2, 0.05])), None);
    let mut e = rig.engine(Compat::MilkDrop3, p);
    e.load_preset(
        preset("SpriteBurn=0\n", "").as_bytes(),
        "g",
        Transition::Cut,
    )
    .unwrap();
    let want = [
        (0.0, [255, 0, 0]),
        (0.12, [0, 255, 0]),
        (0.31, [0, 0, 255]),
        (0.36, [255, 0, 0]),
    ];
    for (i, (t, c)) in want.into_iter().enumerate() {
        let got = rig.centre_at(&mut e, t, i as u32);
        assert!(near(got, c), "t = {t}: {got:?} vs {c:?}");
    }
}

/// An animated GIF named by a shader (`sampler_<name>`) shows the frame picked by `fmod(time, total)`.
#[test]
fn gif_shader_texture_is_animated_under_md3_only() {
    let rig = rig_or_skip!();
    let text = "MILKDROP_PRESET_VERSION=201\nPSVERSION_WARP=2\nPSVERSION_COMP=2\n[preset00]\nfDecay=1\nfWaveAlpha=0\n\
                comp_1=`shader_body\ncomp_2=`{\ncomp_3=`  ret = tex2D(sampler_blink, uv).xyz;\ncomp_4=`}\n";
    let tex = || gif(&[[255, 0, 0, 255], [0, 255, 0, 255]], &[0.5, 0.5]);
    let (p, asked) = provider(None, Some(tex()));
    let mut e = rig.engine(Compat::MilkDrop3, p);
    let report = e
        .load_preset(text.as_bytes(), "t", Transition::Cut)
        .unwrap();
    assert_eq!(
        report.resolved_textures,
        vec![("blink".to_string(), "blink".to_string())],
        "{report:?}"
    );
    assert_eq!(*asked.borrow(), vec!["md3 blink"]);
    assert!(near(rig.centre_at(&mut e, 0.1, 0), [255, 0, 0]));
    assert!(near(rig.centre_at(&mut e, 0.6, 1), [0, 255, 0]));
    assert!(near(rig.centre_at(&mut e, 1.1, 2), [255, 0, 0]), "loops");

    // MilkDrop 2 asks the plain `load` (no GIFs, no MD3 folders): unresolved
    let (p, asked) = provider(None, Some(tex()));
    let mut e = rig.engine(Compat::MilkDrop2, p);
    let report = e
        .load_preset(text.as_bytes(), "t", Transition::Cut)
        .unwrap();
    assert_eq!(report.unresolved_textures, vec!["blink".to_string()]);
    assert_eq!(*asked.borrow(), vec!["load blink"]);
}

/// Sprite code drives the draw: `done` kills it after the frame that sets it.
#[test]
fn sprite_code_runs_and_done_kills() {
    let rig = rig_or_skip!();
    let (p, _) = provider(Some(red()), None);
    let mut e = rig.engine(Compat::MilkDrop3, p);
    e.load_preset(
        preset("SpriteBurn=0\n", "done = time > 0.5;").as_bytes(),
        "d",
        Transition::Cut,
    )
    .unwrap();
    assert!(near(rig.centre_at(&mut e, 0.0, 0), [255, 0, 0]));
    assert!(
        near(rig.centre_at(&mut e, 0.6, 1), [255, 0, 0]),
        "the frame that sets done is drawn"
    );
    assert!(
        near(rig.centre_at(&mut e, 0.7, 2), [0, 0, 0]),
        "then it is gone"
    );
}
