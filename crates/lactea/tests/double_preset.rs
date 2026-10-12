//! MilkDrop 3 `.milk2` double presets (`Compat::MilkDrop3`) through the engine: the frozen mask between two presets,
//! the soft-in legs, the soft-out ramp, hard cuts, `BlendMilk2`, borders, the MD3 patterns of ordinary soft cuts.
//! Time-stepped (60 fps clock) and deterministic. Skipped (with a note) when there is no GPU adapter.

use lactea::{
    Compat, DoubleMode, Engine, EngineConfig, EngineError, FrameClock, Md3Pattern, Transition,
};

const W: u32 = 64;
const H: u32 = 48;

static GPU: std::sync::LazyLock<Option<(wgpu::Device, wgpu::Queue)>> =
    std::sync::LazyLock::new(|| {
        let instance = wgpu::Instance::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    });

/// A v2.01 preset whose warp shader paints one flat colour (nothing else is drawn, no borders).
fn flat(r: f32, g: f32, b: f32) -> String {
    format!(
        "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=2\nPSVERSION_COMP=0\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n\
         warp_1=`shader_body\nwarp_2=`{{\nwarp_3=`  ret = float3({r}, {g}, {b});\nwarp_4=`}}\n"
    )
}

const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const GREEN: [u8; 3] = [0, 255, 0];

fn red() -> String {
    flat(1.0, 0.0, 0.0)
}
fn blue() -> String {
    flat(0.0, 0.0, 1.0)
}
fn green() -> String {
    flat(0.0, 1.0, 0.0)
}

/// A hand-made `.milk2` (LF endings, the saver's layout).
fn milk2(p1: &str, p2: &str, header: &str) -> String {
    format!(
        "Created with MILKDROP 3.37 on 05/01/2026 at 12:34:56\nwww.milkdrop3.com\n\n{header}\n[PRESET1_BEGIN]\nNAME=first.milk\n{p1}[PRESET1_END]\n\n[PRESET2_BEGIN]\nNAME=second\n{p2}[PRESET2_END]\n"
    )
}

/// `side`, `random_1 = 0` (angle 0: PRESET1 on the left, PRESET2 on the right), `random_2 = 0.5` (band 0.2).
fn side_header(progress: &str) -> String {
    format!(
        "blending_pattern=side\nblending_progress={progress}\nblending_direction=1\nrandom_1=0.000000\nrandom_2=0.500000\nrandom_3=0.5\nrandom_4=0.5\nrandom_5=0.5\n"
    )
}

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    engine: Engine,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    frame: u32,
}

fn cfg() -> EngineConfig {
    EngineConfig {
        width: W,
        height: H,
        mesh: [16, 12],
        seed: 7,
        compat: Compat::MilkDrop3,
        start_clean: true,
        ..Default::default()
    }
}

impl Rig {
    fn new(compat: Compat) -> Option<Rig> {
        Rig::with(EngineConfig { compat, ..cfg() })
    }

    fn with(config: EngineConfig) -> Option<Rig> {
        let (device, queue) = GPU.clone()?;
        let engine = Engine::new(&device, &queue, config);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("target"),
            size: wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        Some(Rig {
            device,
            queue,
            engine,
            target,
            view,
            frame: 0,
        })
    }

    fn step(&mut self) -> Vec<u8> {
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.engine.render(
            &mut enc,
            &self.view,
            FrameClock {
                time: f64::from(self.frame) / 60.0,
                frame: self.frame,
            },
        );
        self.frame += 1;
        self.queue.submit(Some(enc.finish()));
        self.read()
    }

    /// Render up to and including frame `frame`; returns that frame.
    fn run_to(&mut self, frame: u32) -> Vec<u8> {
        let mut last = Vec::new();
        while self.frame <= frame {
            last = self.step();
        }
        last
    }

    fn load(&mut self, text: &str, name: &str, t: Transition) -> Result<(), EngineError> {
        self.engine
            .load_preset(text.as_bytes(), name, t)
            .map(|_| ())
    }

    fn mode(&self) -> DoubleMode {
        self.engine
            .double_preset_info()
            .expect("a double preset")
            .mode
    }

    fn blend_progress(&self) -> f32 {
        self.engine
            .double_preset_info()
            .expect("a double preset")
            .blend_progress
    }

    fn read(&self) -> Vec<u8> {
        let bpr = (W * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(bpr * H),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(H),
                },
            },
            wgpu::Extent3d {
                width: W,
                height: H,
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
        for y in 0..H {
            out.extend_from_slice(&data[(y * bpr) as usize..(y * bpr + W * 4) as usize]);
        }
        out
    }
}

macro_rules! rig {
    (@ $make:expr) => {
        match $make {
            Some(r) => r,
            None => {
                eprintln!("SKIP: no wgpu adapter");
                return;
            }
        }
    };
    ($compat:expr) => {
        rig!(@ Rig::new($compat))
    };
}

/// `(pixels (almost) `c`, pixels)` in the columns `x0..x1`.
fn pure_in(img: &[u8], c: [u8; 3], x0: u32, x1: u32) -> (usize, usize) {
    let mut n = 0;
    let mut total = 0;
    for y in 0..H {
        for x in x0..x1 {
            let i = ((y * W + x) * 4) as usize;
            total += 1;
            n += usize::from((0..3).all(|k| (i32::from(img[i + k]) - i32::from(c[k])).abs() <= 12));
        }
    }
    (n, total)
}

fn mostly(img: &[u8], c: [u8; 3], x0: u32, x1: u32) -> bool {
    let (n, total) = pure_in(img, c, x0, x1);
    n * 100 >= total * 95
}

fn max_abs_diff(a: &[u8], b: &[u8]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs())
        .max()
        .unwrap_or(0)
}

/// The frozen mask of `blending_pattern=side`, `blending_progress=0.5`, angle 0, band 0.2: PRESET1 (the old state)
/// on the left, PRESET2 on the right, the blend between. It is on screen from the first frame and does not move.
#[test]
fn side_at_half_splits_the_picture_and_stays_frozen() {
    let mut rig = rig!(Compat::MilkDrop3);
    let text = milk2(&red(), &blue(), &side_header("0.5"));
    rig.load(&text, "pair.milk2", Transition::Cut).unwrap();
    let first = rig.run_to(0);
    // mix = clamp(5 t - 2), t = (X - 0.5) / sqrt(2) + 0.5: pure PRESET1 up to X ~ 0.36, pure PRESET2 from X ~ 0.64
    assert!(mostly(&first, RED, 0, 19), "left: PRESET1");
    assert!(mostly(&first, BLUE, 46, 64), "right: PRESET2");
    let (r, total) = pure_in(&first, RED, 24, 40);
    let (b, _) = pure_in(&first, BLUE, 24, 40);
    assert!(
        r + b < total / 2,
        "the middle is a blend, not pure: {r} + {b} of {total}"
    );
    assert!(pure_in(&first, RED, 0, 64).0 > 200 && pure_in(&first, BLUE, 0, 64).0 > 200);
    // many frames later: the same picture
    let later = rig.run_to(300);
    assert!(
        max_abs_diff(&first, &later) <= 2,
        "frozen: {}",
        max_abs_diff(&first, &later)
    );
    let info = rig.engine.double_preset_info().expect("a double preset");
    assert_eq!(info.pattern, "side");
    assert_eq!(info.known_pattern, Some(Md3Pattern::Side));
    assert_eq!((info.progress, info.direction), (0.5, 1));
    assert_eq!(info.random, [0.0, 0.5, 0.5, 0.5, 0.5]);
    assert_eq!(info.name1.as_deref(), Some("first"));
    assert_eq!(info.name2.as_deref(), Some("second"));
    assert_eq!(info.blend_progress, 0.5);
    assert_eq!(info.mode, DoubleMode::Steady);
    assert_eq!(rig.engine.preset_name(), Some("pair.milk2"));
}

/// `blending_progress` is the new state's weight and is not clamped: 0 shows PRESET1 only, 1 PRESET2 only.
#[test]
fn progress_selects_the_roles() {
    for (progress, want) in [("0", RED), ("1", BLUE)] {
        let mut rig = rig!(Compat::MilkDrop3);
        let text = milk2(&red(), &blue(), &side_header(progress));
        rig.load(&text, "pair.milk2", Transition::Cut).unwrap();
        let img = rig.run_to(5);
        assert!(mostly(&img, want, 0, 64), "progress {progress}");
    }
}

/// Both halves of a double preset run their equations on ns-eel2, the engine's MD3 dialect: `a % 3` of `a = -7` is
/// `1` there (MilkDrop 2 gives `-1`), which the warp shader shows as red through `q1`.
#[test]
fn both_halves_run_on_ns_eel2() {
    let half = "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=2\nPSVERSION_COMP=0\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n\
                per_frame_1=a = -7;\nper_frame_2=q1 = a % 3 == 1;\n\
                warp_1=`shader_body\nwarp_2=`{\nwarp_3=`  ret = float3(q1, 0, 0);\nwarp_4=`}\n";
    for progress in ["0", "1"] {
        let mut rig = rig!(Compat::MilkDrop3);
        let text = milk2(half, half, &side_header(progress));
        rig.load(&text, "pair.milk2", Transition::Cut).unwrap();
        let img = rig.run_to(5);
        assert!(
            mostly(&img, RED, 0, 64),
            "progress {progress}: the half shown ran on ns-eel2"
        );
    }
    // the same preset on MilkDrop 2's expression engine stays black
    let mut rig = rig!(Compat::MilkDrop2);
    rig.load(half, "plain.milk", Transition::Cut).unwrap();
    let img = rig.run_to(5);
    assert!(mostly(&img, [0, 0, 0], 0, 64));
}

/// Only MilkDrop 3 plays a double preset; the other modes refuse it. A file that is not a double preset loads as the
/// ordinary `.milk` it is, malformed header numbers and unusable halves are errors, and a failed load leaves the
/// previous preset alone.
#[test]
fn only_milk_drop_3_plays_double_presets() {
    let text = milk2(&red(), &blue(), &side_header("0.5"));
    for compat in [Compat::ProjectM, Compat::MilkDrop2] {
        let mut rig = rig!(compat);
        let err = rig
            .load(&text, "pair.milk2", Transition::Cut)
            .expect_err("a .milk2 needs MilkDrop 3");
        assert!(matches!(err, EngineError::Unsupported(_)), "{err}");
        assert!(err.to_string().contains("MilkDrop3"), "{err}");
        assert!(rig.engine.preset_name().is_none(), "nothing was loaded");
    }
    // a missing marker: not a double preset, loaded as a plain `.milk` (PRESET1's keys come first)
    let truncated = text.replace("[PRESET2_END]\n", "");
    let mut rig = rig!(Compat::MilkDrop3);
    rig.load(&truncated, "broken.milk2", Transition::Cut)
        .unwrap();
    assert!(rig.engine.double_preset_info().is_none());
    rig.load(&green(), "g.milk", Transition::Cut).unwrap();
    let bad = text.replace("blending_progress=0.5", "blending_progress=abc");
    let err = rig.load(&bad, "bad.milk2", Transition::Cut).unwrap_err();
    assert!(matches!(err, EngineError::Milk2(_)), "{err}");
    assert_eq!(rig.engine.preset_name(), Some("g.milk"));
    let img = rig.run_to(3);
    assert!(mostly(&img, GREEN, 0, 64));
    // an empty PRESET2 cannot be played
    let empty2 = text.replace(&format!("NAME=second\n{}", blue()), "");
    assert!(rig.load(&empty2, "e.milk2", Transition::Cut).is_err());
    assert_eq!(rig.engine.preset_name(), Some("g.milk"));
}

/// Borders (`ob_*` / `ib_*`) are never drawn while a `.milk2` is loaded, even with `DisableBox` off (which only
/// gates ordinary blends); the same presets as plain presets draw them.
#[test]
fn borders_are_never_drawn_for_a_double_preset() {
    let bordered = |r: f32| {
        format!(
            "MILKDROP_PRESET_VERSION=100\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\nob_size=1\nob_a=1\nob_r={r}\nob_g=0\nob_b=0\nib_size=0.2\nib_a=1\nib_r=1\nib_g=1\nib_b=1\n"
        )
    };
    let mut rig = rig!(Compat::MilkDrop3);
    rig.load(&bordered(1.0), "plain.milk", Transition::Cut)
        .unwrap();
    let plain = rig.run_to(3);
    assert!(
        pure_in(&plain, RED, 0, 64).0 > 1000,
        "the plain preset draws its outer border"
    );
    let mut rig = rig!(@ Rig::with(EngineConfig { md3_disable_box: false, ..cfg() }));
    let text = milk2(&bordered(1.0), &bordered(0.5), &side_header("0.5"));
    rig.load(&text, "pair.milk2", Transition::Cut).unwrap();
    let img = rig.run_to(10);
    assert!(
        img.chunks(4).all(|p| p[0] <= 2 && p[1] <= 2 && p[2] <= 2),
        "nothing is drawn"
    );
}

/// The soft transition into a double preset: leg A (the running preset blends to PRESET1 with the file's pattern in
/// `BlendTimeUser`), leg B (PRESET2 rises to `blending_progress` at the rate `1 / BlendTimeUser`), then the hold.
#[test]
fn soft_in_runs_two_legs_then_holds() {
    let mut rig = rig!(@ Rig::with(EngineConfig { blend_time_user: 1.0, ..cfg() }));
    rig.load(&green(), "g.milk", Transition::Cut).unwrap();
    rig.run_to(9);
    // the request's own time (2.7 s) is irrelevant: both legs take BlendTimeUser
    let text = milk2(&red(), &blue(), &side_header("0.5"));
    rig.load(&text, "pair.milk2", Transition::Soft { seconds: 2.7 })
        .unwrap();
    assert_eq!(rig.mode(), DoubleMode::SoftInPreset1);
    assert_eq!(rig.engine.preset_name(), Some("pair.milk2"));

    // leg A: progress (t - start) / 1.0 with start = the time of frame 9
    let a = rig.run_to(39);
    assert_eq!(rig.mode(), DoubleMode::SoftInPreset1);
    assert!(
        (rig.blend_progress() - 0.5).abs() < 1e-3,
        "frame 39: {}",
        rig.blend_progress()
    );
    // the wipe of the file's own pattern between the running preset (green) and PRESET1 (red)
    assert!(
        pure_in(&a, GREEN, 0, 64).0 > 100,
        "green is still on screen"
    );
    assert!(pure_in(&a, RED, 0, 64).0 > 100, "red has come in");
    // an ordinary blend weights the *new* state (PRESET1 here) by the mask: it comes in where the mask is large
    assert!(
        mostly(&a, GREEN, 0, 12),
        "the running preset is left behind on the left"
    );
    assert!(mostly(&a, RED, 52, 64), "PRESET1 enters on the right");
    assert_eq!(pure_in(&a, BLUE, 0, 64).0, 0, "PRESET2 is not in yet");
    let late = rig.run_to(66);
    assert!(
        pure_in(&late, RED, 0, 64).0 * 100 > 85 * (W * H) as usize,
        "end of leg A: PRESET1 alone"
    );
    // frame 70 (t = 1.1667 > start + 1): PRESET2 loads on top, progress restarts at 0
    rig.run_to(70);
    assert_eq!(rig.mode(), DoubleMode::SoftInPreset2);
    assert!(rig.blend_progress() < 0.02, "{}", rig.blend_progress());
    // leg B: min((t - start) / 1.0, 0.5): frame 85 is 15 frames after the load
    rig.run_to(85);
    assert!(
        (rig.blend_progress() - 0.25).abs() < 1e-3,
        "{}",
        rig.blend_progress()
    );
    // it reaches 0.5 after 30 frames and holds, indefinitely
    let held = rig.run_to(110);
    assert_eq!(rig.mode(), DoubleMode::Steady);
    assert_eq!(rig.blend_progress(), 0.5);
    assert!(mostly(&held, RED, 0, 19) && mostly(&held, BLUE, 46, 64));
    let much_later = rig.run_to(400);
    assert!(max_abs_diff(&held, &much_later) <= 2);
    assert_eq!(rig.blend_progress(), 0.5);
}

/// Leaving a double preset (`progressMode` 5): the request is deferred, the ramp goes on from `P` to 1 (PRESET2 alone),
/// then the saved preset loads as an ordinary soft cut with the saved time.
#[test]
fn leaving_ramps_to_the_second_preset_then_soft_cuts() {
    let mut rig = rig!(Compat::MilkDrop3);
    let text = milk2(&red(), &blue(), &side_header("0.5"));
    rig.load(&text, "pair.milk2", Transition::Cut).unwrap();
    rig.run_to(20);
    let report = rig
        .engine
        .load_preset(
            green().as_bytes(),
            "g.milk",
            Transition::Soft { seconds: 2.0 },
        )
        .expect("the request is accepted");
    assert_eq!(report.name, "g.milk");
    assert_eq!(rig.mode(), DoubleMode::SoftOut);
    assert_eq!(
        rig.engine.preset_name(),
        Some("pair.milk2"),
        "still the double preset"
    );
    // a hard load has a 1 s base duration: `start := now - P * dur`, so the next frame is at P + one frame
    rig.run_to(21);
    let p = rig.blend_progress();
    assert!((p - (0.5 + 1.0 / 60.0)).abs() < 1e-3, "{p}");
    // a second request while leaving is ignored
    rig.engine
        .load_preset(
            red().as_bytes(),
            "r.milk",
            Transition::Soft { seconds: 2.0 },
        )
        .unwrap();
    // the ramp reaches 1 at (1 - 0.5) / 1 s = 30 frames after the request (frame 50)
    let mid = rig.run_to(40);
    assert!(
        pure_in(&mid, BLUE, 0, 64).0 * 2 > (W * H) as usize,
        "the second preset takes over"
    );
    rig.run_to(52);
    assert!(
        rig.engine.double_preset_info().is_none(),
        "the double preset is over"
    );
    assert_eq!(
        rig.engine.preset_name(),
        Some("g.milk"),
        "the saved request loaded, not the ignored one"
    );
    assert!(
        rig.engine.transition_progress().is_some(),
        "an ordinary soft cut P2 -> green runs"
    );
    // and it is the saved 2 s: well after, green is alone
    let end = rig.run_to(52 + 130);
    assert!(rig.engine.transition_progress().is_none());
    assert!(mostly(&end, GREEN, 0, 64));
}

/// A request during the first soft-in leg changes nothing there: the leg runs on, PRESET2 is never shown, and the saved
/// request loads when the leg ends.
#[test]
fn leaving_during_the_first_leg_never_shows_the_second_preset() {
    let mut rig = rig!(@ Rig::with(EngineConfig { blend_time_user: 1.0, ..cfg() }));
    rig.load(&green(), "g.milk", Transition::Cut).unwrap();
    rig.run_to(9);
    let text = milk2(&red(), &blue(), &side_header("0.5"));
    rig.load(&text, "pair.milk2", Transition::Soft { seconds: 2.0 })
        .unwrap();
    rig.run_to(30);
    // the next preset (a different colour from everything in the pair)
    let other = flat(1.0, 1.0, 0.0);
    rig.load(&other, "y.milk", Transition::Soft { seconds: 2.0 })
        .unwrap();
    assert_eq!(rig.mode(), DoubleMode::SoftOut);
    let before = rig.blend_progress();
    for f in 31..=68 {
        let img = rig.run_to(f);
        assert_eq!(
            pure_in(&img, BLUE, 0, 64).0,
            0,
            "frame {f}: PRESET2 never shows"
        );
    }
    // the leg's ramp was not touched: progress keeps following (t - start) / 1.0
    let p = rig.blend_progress();
    assert!(
        (p - (before + 38.0 / 60.0)).abs() < 2e-3,
        "{p} after {before}"
    );
    // frame 69 reaches 1: the double preset is over, the saved request loads
    rig.run_to(72);
    assert!(rig.engine.double_preset_info().is_none());
    assert_eq!(rig.engine.preset_name(), Some("y.milk"));
    let img = rig.run_to(72 + 150);
    assert!(mostly(&img, [255, 255, 0], 0, 64));
}

/// `BlendMilk2 = 0`: a `.milk2` is always a hard load, and leaving one is a hard cut.
#[test]
fn blend_milk2_off_makes_every_transition_hard() {
    let mut rig = rig!(@ Rig::with(EngineConfig { blend_milk2: false, ..cfg() }));
    rig.load(&green(), "g.milk", Transition::Cut).unwrap();
    rig.run_to(5);
    let text = milk2(&red(), &blue(), &side_header("0.5"));
    rig.load(&text, "pair.milk2", Transition::Soft { seconds: 2.0 })
        .unwrap();
    assert_eq!(rig.mode(), DoubleMode::Steady, "no soft-in legs");
    let img = rig.run_to(6);
    assert!(mostly(&img, RED, 0, 19) && mostly(&img, BLUE, 46, 64));
    rig.load(&green(), "g2.milk", Transition::Soft { seconds: 2.0 })
        .unwrap();
    assert!(
        rig.engine.double_preset_info().is_none(),
        "no soft-out ramp"
    );
    assert_eq!(rig.engine.preset_name(), Some("g2.milk"));
    let img = rig.run_to(8);
    assert!(mostly(&img, GREEN, 0, 64));
    assert!(rig.engine.transition_progress().is_none());
}

/// A hard cut (`Transition::Cut`, or a zero soft time) replaces a double preset at once, also during a soft-in, and a
/// `.milk2` after a `.milk2` with a cut is a plain replacement.
#[test]
fn hard_cuts_replace_the_double_preset_at_once() {
    let mut rig = rig!(Compat::MilkDrop3);
    let text = milk2(&red(), &blue(), &side_header("0.5"));
    rig.load(&green(), "g.milk", Transition::Cut).unwrap();
    rig.run_to(5);
    rig.load(&text, "pair.milk2", Transition::Soft { seconds: 1.0 })
        .unwrap();
    rig.run_to(20);
    assert!(rig.engine.double_preset_info().is_some());
    rig.load(&green(), "g.milk", Transition::Cut).unwrap();
    assert!(rig.engine.double_preset_info().is_none());
    let img = rig.run_to(25);
    assert!(mostly(&img, GREEN, 0, 64));
    assert!(rig.engine.transition_progress().is_none());
    // double -> double with a cut
    rig.load(&text, "pair.milk2", Transition::Cut).unwrap();
    let other = milk2(&green(), &red(), &side_header("1"));
    rig.load(&other, "other.milk2", Transition::Cut).unwrap();
    let img = rig.run_to(30);
    assert!(mostly(&img, RED, 0, 64), "progress 1: PRESET2 alone");
    assert_eq!(rig.mode(), DoubleMode::Steady);
    // a non-positive soft time is a cut as well
    rig.load(&green(), "g.milk", Transition::Soft { seconds: 0.0 })
        .unwrap();
    assert!(rig.engine.double_preset_info().is_none());
}

/// A missing header key keeps the value the previous load or ordinary soft cut left behind: the engine models MD3's
/// process-global values as explicit carry-over state.
#[test]
fn missing_header_keys_keep_the_last_soft_cuts_values() {
    let mut rig = rig!(Compat::MilkDrop3);
    // process start: no pattern (the mesh stays uniform), progress 1, direction 0
    let bare = milk2(&red(), &blue(), "blending_progress=0.5\n");
    rig.load(&bare, "bare.milk2", Transition::Cut).unwrap();
    let info = rig.engine.double_preset_info().unwrap();
    assert_eq!(info.pattern, "");
    assert_eq!(info.known_pattern, None);
    assert_eq!((info.direction, info.random), (0, [0.0; 5]));
    // an unknown pattern name leaves the mesh uniform: mix = clamp(P) = 0.5 everywhere, no pure region
    let img = rig.run_to(5);
    assert_eq!(
        pure_in(&img, RED, 0, 64).0 + pure_in(&img, BLUE, 0, 64).0,
        0
    );
    // an ordinary soft cut draws a pattern, a direction and five randoms
    rig.load(&green(), "g.milk", Transition::Cut).unwrap();
    rig.run_to(10);
    rig.load(&red(), "r.milk", Transition::Soft { seconds: 1.0 })
        .unwrap();
    let kind = rig.engine.md2_blend_pattern().expect("blend running");
    let want = Md3Pattern::from_index(kind as usize).unwrap();
    rig.run_to(12);
    rig.load(&bare, "bare.milk2", Transition::Cut).unwrap();
    let info = rig.engine.double_preset_info().unwrap();
    assert_eq!(info.known_pattern, Some(want), "the soft cut's pattern");
    assert!(info.direction == 1 || info.direction == -1);
    assert!(info.random.iter().all(|r| (0.0..=1.0).contains(r)) && info.random != [0.0; 5]);
    assert_eq!(info.progress, 0.5, "the key the file does have");
}

/// Ordinary soft cuts under MD3 pick from the 30 MD3 patterns (the enabled list); the choice is seeded, can be forced,
/// the list can be changed; MD2 keeps BeatDrop's 19.
#[test]
fn ordinary_soft_cuts_use_md3_patterns() {
    let kinds = |compat: Compat, enabled: Option<&[Md3Pattern]>| {
        let mut rig = Rig::new(compat)?;
        if let Some(e) = enabled {
            rig.engine.set_md3_transition_patterns(e);
        }
        rig.load(&green(), "g.milk", Transition::Cut).unwrap();
        rig.run_to(2);
        let mut out = Vec::new();
        for i in 0..24u32 {
            let colour = if i % 2 == 0 { red() } else { blue() };
            rig.load(&colour, "x.milk", Transition::Soft { seconds: 1.0 })
                .unwrap();
            out.push(rig.engine.md2_blend_pattern());
            rig.run_to(rig.frame);
        }
        Some(out)
    };
    let Some(default) = kinds(Compat::MilkDrop3, None) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let enabled: Vec<u32> = lactea::default_enabled_patterns()
        .iter()
        .map(|p| p.index() as u32)
        .collect();
    assert!(
        default.iter().all(|k| enabled.contains(&k.unwrap())),
        "{default:?} not in {enabled:?}"
    );
    assert!(
        default.iter().any(|k| k.unwrap() > 3),
        "more than BeatDrop's first patterns appear: {default:?}"
    );
    assert_eq!(kinds(Compat::MilkDrop3, None).unwrap(), default, "seeded");
    // the list is configurable (an empty one means zoom only)
    let only = kinds(
        Compat::MilkDrop3,
        Some(&[Md3Pattern::Cross2, Md3Pattern::Wave]),
    )
    .unwrap();
    assert!(
        only.iter().all(|k| [22, 29].contains(&k.unwrap())),
        "{only:?}"
    );
    let zoom = kinds(Compat::MilkDrop3, Some(&[])).unwrap();
    assert!(zoom.iter().all(|k| *k == Some(0)));
    // MD2 draws BeatDrop's 0..=18
    let md2 = kinds(Compat::MilkDrop2, None).unwrap();
    assert!(md2.iter().all(|k| k.unwrap() <= 18));
    // forcing a pattern: any of the 30 under MD3
    let mut rig = rig!(Compat::MilkDrop3);
    rig.engine.set_md2_blend_pattern(Some(29));
    rig.load(&green(), "g.milk", Transition::Cut).unwrap();
    rig.run_to(2);
    rig.load(&red(), "r.milk", Transition::Soft { seconds: 1.0 })
        .unwrap();
    assert_eq!(rig.engine.md2_blend_pattern(), Some(29));
}

/// A resize regenerates the frozen pattern for the new aspect; the picture stays valid and frozen.
#[test]
fn resize_keeps_the_double_preset_frozen() {
    let mut rig = rig!(Compat::MilkDrop3);
    let text = milk2(&red(), &blue(), &side_header("0.5"));
    rig.load(&text, "pair.milk2", Transition::Cut).unwrap();
    rig.run_to(3);
    rig.engine.resize(W, H);
    let img = rig.run_to(6);
    assert!(mostly(&img, RED, 0, 19) && mostly(&img, BLUE, 46, 64));
}
