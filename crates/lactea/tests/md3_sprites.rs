//! MilkDrop 3 preset sprites through the engine: parsing, loading through the provider, drawing, lifetime and
//! the compat gating; animated GIFs as sprites and as shader textures. Skipped (with a note) without a GPU adapter.

use lactea::{
    AnimFrame, AnimatedImage, Compat, Engine, EngineConfig, FrameClock, Image, Media,
    TextureProvider, Transition,
};
use std::cell::RefCell;
use std::rc::Rc;

use lactea_testutil::gpu_or_skip;
macro_rules! rig_or_skip {
    () => {{
        let (device, queue) = gpu_or_skip!();
        Rig::new(device, queue)
    }};
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
    fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
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
        Self {
            device,
            queue,
            view,
        }
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
        )
        .unwrap();
        e.set_texture_provider(Box::new(p));
        e
    }

    fn engine_with(&self, mut config: EngineConfig, p: Provider) -> Engine {
        config.width = SIZE.0;
        config.height = SIZE.1;
        config.mesh = [16, 12];
        config.start_clean = true;
        let mut e = Engine::new(&self.device, &self.queue, config).unwrap();
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

/// A sprite and a shader texture are decoded once per provider: a reload (here with another colour key, which must
/// not leak into the shared decode) does not ask again, and `set_texture_provider` starts from scratch.
#[test]
fn reloading_a_sprite_preset_does_not_decode_it_again() {
    let rig = rig_or_skip!();
    let (p, asked) = provider(Some(red()), Some(red()));
    let mut e = rig.engine(Compat::MilkDrop3, p);
    let load = |e: &mut Engine, key: &str| {
        e.load_preset(
            preset(&format!("SpriteColorKey={key}\nSpriteBurn=0\n"), "").as_bytes(),
            "k",
            Transition::Cut,
        )
        .unwrap()
    };
    load(&mut e, "0xFF0000");
    assert!(near(rig.centre_at(&mut e, 0.0, 0), [0, 0, 0]), "keyed out");
    let report = load(&mut e, "0x000000");
    assert_eq!(report.sprite.as_deref(), Some("sprites\\red.png"));
    assert!(
        near(rig.centre_at(&mut e, 0.0, 0), [255, 0, 0]),
        "the first load's key did not change the shared decode"
    );
    assert_eq!(*asked.borrow(), vec!["sprite sprites\\red.png"]);

    let (p2, asked2) = provider(Some(red()), None);
    e.set_texture_provider(Box::new(p2));
    load(&mut e, "0x000000");
    assert_eq!(*asked2.borrow(), vec!["sprite sprites\\red.png"]);
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

/// Sprite code runs on the engine's MD3 dialect (ns-eel2: `k % 3` of `k = -7` is 1, MilkDrop 2 gives -1), and code
/// that does not compile is dropped while the sprite keeps drawing (MD3 is silent about it).
#[test]
fn sprite_code_runs_on_ns_eel2_and_a_failing_part_is_dropped() {
    let rig = rig_or_skip!();
    let (p, _) = provider(Some(red()), None);
    let mut e = rig.engine(Compat::MilkDrop3, p);
    e.load_preset(
        preset("SpriteBurn=0\n", "k = -7;\ndone = k % 3 == 1;").as_bytes(),
        "ns",
        Transition::Cut,
    )
    .unwrap();
    assert!(near(rig.centre_at(&mut e, 0.0, 0), [255, 0, 0]));
    assert!(
        near(rig.centre_at(&mut e, 0.1, 1), [0, 0, 0]),
        "done = 1 killed the sprite: ns-eel2 `%`"
    );

    let (p, _) = provider(Some(red()), None);
    let mut e = rig.engine(Compat::MilkDrop3, p);
    let report = e
        .load_preset(
            preset("SpriteBurn=0\n", "done = (;").as_bytes(),
            "bad",
            Transition::Cut,
        )
        .unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.starts_with("sprite code dropped: ")),
        "{:?}",
        report.warnings
    );
    assert!(near(rig.centre_at(&mut e, 0.0, 0), [255, 0, 0]));
    assert!(
        near(rig.centre_at(&mut e, 0.5, 1), [255, 0, 0]),
        "the sprite stays"
    );
}

/// A hand-made `.milk2` whose halves draw nothing. `sprite_header` is the optional `sprite=1` line, `half1` / `half2`
/// extra lines of the halves, `block` the sprite block after `[PRESET2_END]`.
fn milk2(sprite_header: &str, half1: &str, half2: &str, block: &str) -> String {
    let half = "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=0\nPSVERSION_COMP=0\n[preset00]\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n";
    format!(
        "Created with MILKDROP 3.37 on 05/01/2026 at 12:34:56\nwww.example.org\n\n{sprite_header}blending_pattern=zoom\nblending_progress=1.00\nblending_direction=1\nrandom_1=0.1\nrandom_2=0.2\nrandom_3=0.3\nrandom_4=0.4\nrandom_5=0.5\n\
         [PRESET1_BEGIN]\nNAME=first\n{half}{half1}[PRESET1_END]\n\n[PRESET2_BEGIN]\nNAME=second\n{half}{half2}[PRESET2_END]\n{block}"
    )
}

fn block(name: &str) -> String {
    format!(
        "\n[SPRITE1_BEGIN]\nSpriteName=sprites\\{name}\nSpriteColorKey=0x000000\nSpriteLayer=0\nSpriteBlend=4\nSpriteAlpha=1.000000\nSpriteBurn=0\nSpriteX=0.000000\nSpriteY=0.000000\nSpriteSX=-0.500000\nSpriteSY=-0.500000\nSpriteRot=0.000000\nSpriteSpeed=0.000000\nSpriteRepeatX=1.000000\nSpriteRepeatY=1.000000\n[SPRITE1_END]\n"
    )
}

/// The sprite block after `[PRESET2_END]` belongs to PRESET2 and is honoured only with the header's `sprite=1`; a
/// sprite block lying inside either half is never read.
#[test]
fn milk2_sprite_belongs_to_preset2_and_needs_the_header_flag() {
    let rig = rig_or_skip!();
    let stale = block("stale.png");
    // PRESET1 carries a stale block, PRESET2 has none of its own before the real one after `[PRESET2_END]`
    let flagged = milk2("sprite=1\n", &stale, "", &block("red.png"));
    let (p, asked) = provider(Some(red()), None);
    let mut e = rig.engine(Compat::MilkDrop3, p);
    let report = e
        .load_preset(flagged.as_bytes(), "pair.milk2", Transition::Cut)
        .unwrap();
    assert_eq!(report.sprite.as_deref(), Some("sprites\\red.png"));
    assert_eq!(*asked.borrow(), vec!["sprite sprites\\red.png"]);
    assert!(near(rig.centre_at(&mut e, 0.0, 0), [255, 0, 0]));

    // without `sprite=1` the block after `[PRESET2_END]` is outside both halves, a block inside PRESET2 is ignored
    for text in [
        milk2("", "", "", &block("red.png")),
        milk2("", "", &block("red.png"), ""),
    ] {
        let (p, asked) = provider(Some(red()), None);
        let mut e = rig.engine(Compat::MilkDrop3, p);
        let report = e
            .load_preset(text.as_bytes(), "pair.milk2", Transition::Cut)
            .unwrap();
        assert_eq!(report.sprite, None);
        assert!(asked.borrow().is_empty(), "asked {:?}", asked.borrow());
        assert!(near(rig.centre_at(&mut e, 0.0, 0), [0, 0, 0]));
    }
}

/// A soft transition into a `.milk2` imports PRESET2 and its sprite when the first leg completes, not before.
#[test]
fn milk2_sprite_arrives_with_preset2_after_the_first_leg() {
    let rig = rig_or_skip!();
    let (p, asked) = provider(Some(red()), None);
    let mut e = rig.engine_with(
        EngineConfig {
            compat: Compat::MilkDrop3,
            blend_time_user: 1.0,
            ..Default::default()
        },
        p,
    );
    let plain = "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=0\nPSVERSION_COMP=0\n[preset00]\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n";
    e.load_preset(plain.as_bytes(), "plain", Transition::Cut)
        .unwrap();
    let mut frame = 0;
    let mut step = |e: &mut Engine| {
        let c = rig.centre_at(e, f64::from(frame) / 60.0, frame);
        frame += 1;
        c
    };
    step(&mut e);
    let text = milk2("sprite=1\n", "", "", &block("red.png"));
    e.load_preset(
        text.as_bytes(),
        "pair.milk2",
        Transition::Soft { seconds: 2.7 },
    )
    .unwrap();
    // leg A takes `blend_time_user` = 1 s = 60 frames
    let mut c = [0; 3];
    for _ in 0..40 {
        c = step(&mut e);
    }
    assert!(asked.borrow().is_empty(), "PRESET2 is not imported yet");
    assert!(near(c, [0, 0, 0]));
    for _ in 0..40 {
        c = step(&mut e);
    }
    assert_eq!(*asked.borrow(), vec!["sprite sprites\\red.png"]);
    assert!(near(c, [255, 0, 0]), "the sprite is drawn: {c:?}");
}
