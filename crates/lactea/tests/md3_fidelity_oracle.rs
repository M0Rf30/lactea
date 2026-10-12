//! Fidelity probes of `Compat::MilkDrop3` against MD3 3.37 measured under Wine (`md3shot.sh`, silence, 800x600 or
//! 800x450). Each test renders hand-made fixtures of `fixtures/md3/{wave,sprites,autocut}` (and generated flat-colour
//! `.milk2` pairs) through the engine and pins what the real program showed; `docs/md3.md` lists the measurements.
//! Skipped (with a note) when there is no GPU adapter.

use lactea::{
    Compat, Engine, EngineConfig, FrameClock, Image, Media, Stage, TextureProvider, Transition,
    decode_gif,
};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/md3")
}

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// Sprites of `fixtures/md3/sprites`, asked as `sprites\name`.
struct Sprites;

impl TextureProvider for Sprites {
    fn load(&mut self, _name: &str) -> Option<Image> {
        None
    }
    fn random_pool(&self) -> Vec<String> {
        Vec::new()
    }
    fn load_sprite(&mut self, path: &str) -> Option<Media> {
        let file = path.rsplit(['\\', '/']).next()?;
        let bytes = std::fs::read(root().join("sprites").join(file)).ok()?;
        if file.ends_with(".gif") {
            return decode_gif(&bytes).ok().map(Media::Animated);
        }
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        Some(Media::Still(Image {
            width: img.width(),
            height: img.height(),
            rgba8: img.into_raw(),
        }))
    }
}

struct Shot {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}

impl Shot {
    fn px(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.w + x) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
    }
    /// Pixels whose brightest channel exceeds 8, as (x, y).
    fn lit(&self) -> Vec<(usize, usize)> {
        (0..self.h)
            .flat_map(|y| (0..self.w).map(move |x| (x, y)))
            .filter(|&(x, y)| self.px(x, y).into_iter().max().unwrap_or(0) > 8)
            .collect()
    }
}

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Rig {
    fn new() -> Option<Self> {
        let (device, queue) = gpu()?;
        Some(Self { device, queue })
    }

    fn engine(&self, config: EngineConfig, name: &str, text: &[u8]) -> Engine {
        let mut e = Engine::new(
            &self.device,
            &self.queue,
            EngineConfig {
                compat: Compat::MilkDrop3,
                ..config
            },
        );
        e.set_texture_provider(Box::new(Sprites));
        e.load_preset(text, name, Transition::Cut)
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        e
    }

    fn target(&self, w: u32, h: u32) -> wgpu::TextureView {
        self.device
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

    fn step(&self, e: &mut Engine, view: &wgpu::TextureView, frame: u32) {
        let mut enc = self.device.create_command_encoder(&Default::default());
        e.render(
            &mut enc,
            view,
            FrameClock {
                time: f64::from(frame) / 60.0,
                frame,
            },
        );
        self.queue.submit(Some(enc.finish()));
    }

    /// `frames` frames of the preset (silence, 60 fps clock), then the composite.
    fn run(&self, cfg: EngineConfig, name: &str, text: &[u8], frames: u32) -> Shot {
        let view = self.target(cfg.width, cfg.height);
        let mut e = self.engine(cfg, name, text);
        for f in 0..frames {
            self.step(&mut e, &view, f);
        }
        shot(&e)
    }
}

fn shot(e: &Engine) -> Shot {
    let (w, h, rgba) = e.renderer().read_stage(Stage::Final).expect("read back");
    Shot {
        w: w as usize,
        h: h as usize,
        rgba,
    }
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

fn cfg(w: u32, h: u32, mesh: [u32; 2]) -> EngineConfig {
    EngineConfig {
        width: w,
        height: h,
        mesh,
        start_clean: true,
        ..Default::default()
    }
}

fn fixture(dir: &str, name: &str) -> Vec<u8> {
    std::fs::read(root().join(dir).join(format!("{name}.milk")))
        .unwrap_or_else(|e| panic!("{dir}/{name}: {e}"))
}

/// What the lit pixels of `s` outside the band of rows `rows` look like: their bounding box, `None` when there are none.
fn off_band(s: &Shot, rows: std::ops::RangeInclusive<usize>) -> Option<[usize; 4]> {
    let mut bb: Option<[usize; 4]> = None;
    for (x, y) in s.lit().into_iter().filter(|(_, y)| !rows.contains(y)) {
        bb = Some(match bb {
            None => [x, y, x, y],
            Some([a, b, c, d]) => [a.min(x), b.min(y), c.max(x), d.max(y)],
        });
    }
    bb
}

fn rows(s: &Shot) -> Vec<usize> {
    let mut r: Vec<usize> = s.lit().into_iter().map(|(_, y)| y).collect();
    r.dedup();
    r
}

/// Silence, 800x600. MD3 (Wine) at `wave_x = 0.75`, `wave_a = 1`, no feedback: mode 9 is a horizontal line on rows 148
/// and 149 (half intensity each); with dots the unwritten second strip adds a cluster of points at the screen centre
/// (MD3: a 3x3 splat at x 400..402, y 300..302), with thick lines it adds nothing; mode 6 with dots has no cluster.
#[test]
fn wave_mode_9_dots_leave_the_second_strip_at_the_centre() {
    let rig = rig_or_skip!();
    let at = |name: &str| rig.run(cfg(800, 600, [48, 36]), name, &fixture("wave", name), 3);
    let line = at("m09_line");
    assert_eq!(rows(&line), [148, 149]);
    assert_eq!(line.lit().len(), 2 * 800);
    assert_eq!(off_band(&line, 148..=149), None);
    for name in ["m09_dots", "m09_thick", "m06_dots"] {
        let s = at(name);
        assert_eq!(&rows(&s)[..3], [148, 149, 150], "{name}");
        let bb = off_band(&s, 148..=150);
        match name {
            "m09_dots" => {
                let [x0, y0, x1, y1] = bb.expect("centre cluster of the dots");
                assert!(
                    x0 >= 396 && x1 <= 404 && y0 >= 296 && y1 <= 304,
                    "{name}: {bb:?}"
                );
            }
            _ => assert_eq!(bb, None, "{name}"),
        }
    }
}

/// A mode whose `% 17` remainder is negative draws 480 zero vertices with the plain `wave_a` alpha: invisible lines, so
/// dots show a centre cluster even when the volume-alpha window would hide every valid mode, and an alpha byte
/// that wraps (`wave_a = 1.0045` -> 256) leaves nothing (a valid mode clamps the alpha and stays visible). Both as
/// measured on MD3.
#[test]
fn wave_mode_outside_0_16_has_the_plain_unclamped_alpha() {
    let rig = rig_or_skip!();
    let at = |name: &str| rig.run(cfg(800, 600, [48, 36]), name, &fixture("wave", name), 3);
    for name in ["inv_dots", "inv_dots_vol"] {
        let bb = off_band(&at(name), 0..=0).unwrap_or_else(|| panic!("{name}: nothing drawn"));
        assert!(
            bb[0] >= 396 && bb[2] <= 404 && bb[1] >= 296 && bb[3] <= 304,
            "{name}: {bb:?}"
        );
    }
    assert!(
        at("m6_dots_vol").lit().is_empty(),
        "valid mode, volume alpha"
    );
    assert!(
        at("inv_wrap").lit().is_empty(),
        "invalid mode, alpha byte 0"
    );
    assert!(at("m6_wrap").lit().len() >= 800, "valid mode clamps alpha");
}

/// Additive grey sprite (alpha 0.04, decay 0.9): layers 1..4 settle at 45/255 without burn and 94/255 with it (MD3,
/// 800x600); the burn is drawn twice in every layer.
#[test]
fn sprite_burn_is_drawn_twice_in_layers_1_to_4() {
    let rig = rig_or_skip!();
    for layer in 1..=4 {
        for (burn, want) in [(0, 45u8), (1, 94)] {
            let name = format!("burn_L{layer}_b{burn}");
            let s = rig.run(
                cfg(320, 240, [32, 24]),
                &name,
                &fixture("sprites", &name),
                200,
            );
            let got = s.px(160, 120);
            assert!(
                got.iter().all(|c| c.abs_diff(want) <= 1),
                "{name}: {got:?}, MD3 {want}"
            );
        }
    }
}

/// `fid_disp.gif` (red full-canvas keep, green square dispose-to-background, blue square dispose-to-previous, yellow
/// checker with transparent cells keep; 1 s each) on a grey background: MD3 shows exactly the GIF89a composition
/// at every step (800x600; the points are the hole of the green square, the blue square and the two checker cells).
#[test]
fn gif_sprite_frames_are_composed_the_gif89a_way() {
    let rig = rig_or_skip!();
    let points = [(280usize, 180usize), (520, 420), (503, 83), (542, 83)];
    let want: [[[u8; 3]; 4]; 4] = [
        [[255, 0, 0], [255, 0, 0], [255, 0, 0], [255, 0, 0]],
        [[0, 255, 0], [255, 0, 0], [255, 0, 0], [255, 0, 0]],
        [[76, 76, 76], [0, 0, 255], [255, 0, 0], [255, 0, 0]],
        [[76, 76, 76], [255, 0, 0], [255, 0, 0], [255, 255, 0]],
    ];
    let view = rig.target(800, 600);
    let mut e = rig.engine(
        cfg(800, 600, [32, 24]),
        "gif_disp",
        &fixture("sprites", "gif_disp"),
    );
    for frame in 0..240u32 {
        rig.step(&mut e, &view, frame);
        // the middle of each second
        if frame % 60 == 30 {
            let s = shot(&e);
            let state = (frame / 60) as usize;
            for (p, (x, y)) in points.iter().enumerate() {
                let got = s.px(*x, *y);
                assert!(
                    got.iter()
                        .zip(want[state][p])
                        .all(|(a, b)| a.abs_diff(b) <= 6),
                    "gif frame {state} point {p}: {got:?}, MD3 {:?}",
                    want[state][p]
                );
            }
        }
    }
}

/// `bass` / `treb` assigned by the per-frame code drive the auto hard cut (MD3 loaded another preset within a second
/// for `bass = 2` with modes 1 and 6, `treb = 5` with mode 2; nothing for the control, `bass_att`, or the wrong
/// level; thresholds: mode 6 fires at `bass = 1.6` not 1.4, mode 2 at `treb = 3.0` not 2.8).
#[test]
fn auto_hard_cut_follows_the_levels_the_per_frame_code_writes() {
    let rig = rig_or_skip!();
    for (mode, name, fires) in [
        (6u8, "bass2", true),
        (6, "control", false),
        (6, "treb5", false),
        (6, "bass_att2", false),
        (1, "bass2", true),
        (2, "treb5", true),
        (2, "bass2", false),
        (6, "bass1_4", false),
        (6, "bass1_6", true),
        (2, "treb2_8", false),
        (2, "treb3_0", true),
    ] {
        let view = rig.target(64, 48);
        let mut e = rig.engine(
            EngineConfig {
                auto_hard_cut_mode: mode,
                ..cfg(64, 48, [16, 12])
            },
            name,
            &fixture("autocut", name),
        );
        let mut cuts = 0;
        for f in 0..120 {
            rig.step(&mut e, &view, f);
            cuts += e.take_auto_cuts();
        }
        assert_eq!(cuts > 0, fires, "mode {mode} {name}: {cuts} cuts");
    }
}

fn blend_half(r: u8, b: u8) -> String {
    format!(
        "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=2\nPSVERSION_COMP=2\n[preset00]\nfGammaAdj=1.000\nfDecay=0.900\n\
         fWaveAlpha=0.000\nper_frame_1=wave_a=0;\ncomp_1=`shader_body\ncomp_2=`{{\ncomp_3=`  ret = float3({r},0,{b});\ncomp_4=`}}\n"
    )
}

fn blend_pair(pattern: &str, progress: &str) -> String {
    format!(
        "Created with test\nwww.example.org\n\nblending_pattern={pattern}\nblending_progress={progress}\nblending_direction=1\n\
         random_1=0.300000\nrandom_2=0.550000\nrandom_3=0.650000\nrandom_4=0.350000\nrandom_5=0.450000\n\n\
         [PRESET1_BEGIN]\nNAME=red\n{}[PRESET1_END]\n\n[PRESET2_BEGIN]\nNAME=blue\n{}[PRESET2_END]\n",
        blend_half(1, 0),
        blend_half(0, 1)
    )
}

/// Mean red channel (percent) in a 6x4 grid of the frozen mix, 800x450, red = old / PRESET1: the ten patterns that
/// the shipped pack does not exercise (`progress = 0.5`) and `cross2` at 0.85, as MD3 drew them through `--via-ui`.
/// MD3's mesh is 64x48; the tolerance covers the rasteriser (the largest difference measured was 10 points).
const BLEND_GRIDS: [(&str, [u8; 24]); 11] = [
    (
        "checkerboard",
        [
            38, 0, 0, 45, 16, 0, 55, 0, 0, 62, 27, 0, 0, 25, 17, 0, 0, 32, 0, 56, 41, 0, 3, 68,
        ],
    ),
    (
        "bubbles",
        [
            28, 0, 2, 62, 15, 0, 1, 38, 16, 0, 8, 47, 2, 63, 28, 0, 14, 77, 13, 1, 1, 32, 8, 1,
        ],
    ),
    (
        "stars2",
        [
            38, 18, 31, 39, 35, 13, 28, 40, 35, 36, 41, 51, 44, 38, 36, 29, 35, 19, 21, 33, 41, 31,
            23, 47,
        ],
    ),
    (
        "clock",
        [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 12, 63, 58, 10, 0, 37, 84, 100, 100, 81, 36,
        ],
    ),
    (
        "nuclear",
        [
            0, 0, 53, 45, 0, 0, 0, 0, 22, 14, 0, 0, 45, 64, 47, 53, 63, 44, 100, 82, 8, 10, 87, 100,
        ],
    ),
    (
        "cisor",
        [
            0, 0, 0, 0, 47, 95, 5, 0, 0, 0, 15, 92, 89, 56, 20, 3, 0, 73, 100, 100, 99, 80, 48, 48,
        ],
    ),
    (
        "wave",
        [
            17, 97, 37, 0, 0, 0, 63, 78, 1, 0, 8, 44, 72, 41, 0, 2, 78, 96, 82, 26, 0, 22, 96, 21,
        ],
    ),
    (
        "lineshorizontal",
        [
            70, 70, 70, 70, 70, 70, 0, 0, 0, 0, 0, 0, 68, 68, 68, 68, 68, 68, 7, 7, 7, 7, 7, 7,
        ],
    ),
    (
        "cross",
        [
            67, 81, 100, 100, 77, 67, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100,
            70, 84, 100, 100, 80, 70,
        ],
    ),
    (
        "cross2",
        [
            100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100, 100,
            100, 100, 100, 100, 100, 100, 100,
        ],
    ),
    (
        "cross2p",
        [
            99, 94, 30, 41, 99, 99, 30, 29, 37, 34, 30, 30, 35, 35, 42, 39, 35, 35, 95, 90, 30, 40,
            95, 95,
        ],
    ),
];

#[test]
fn unshipped_blend_patterns_match_the_md3_masks() {
    let rig = rig_or_skip!();
    let mut bad = Vec::new();
    for (name, want) in BLEND_GRIDS {
        let (pattern, progress) = match name {
            "cross2p" => ("cross2", "0.85"),
            n => (n, "0.5"),
        };
        let s = rig.run(
            cfg(800, 450, [64, 48]),
            "pair.milk2",
            blend_pair(pattern, progress).as_bytes(),
            4,
        );
        for (cell, &w) in want.iter().enumerate() {
            let (cx, cy) = (cell % 6, cell / 6);
            let (x0, x1, y0, y1) = (
                cx * s.w / 6,
                (cx + 1) * s.w / 6,
                cy * s.h / 4,
                (cy + 1) * s.h / 4,
            );
            let mut sum = 0u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += u64::from(s.px(x, y)[0]);
                }
            }
            let got = (sum as f64 / ((x1 - x0) * (y1 - y0)) as f64 / 2.55).round();
            if (got - f64::from(w)).abs() > 12.0 {
                bad.push(format!("{name} cell {cell}: {got}, MD3 {w}"));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}
