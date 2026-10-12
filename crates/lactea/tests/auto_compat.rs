//! `EngineConfig::auto_compat`: every load resolves the compatibility mode from the preset's bytes. A playlist
//! alternating projectM and MilkDrop 3 presets (a `q40` preset, a sprite preset, a `.milk2` double preset) through one
//! engine, the transitions between the modes, a failed load, determinism and the fixed-mode contrast. Time-stepped
//! (60 fps clock). Skipped (with a note) when there is no GPU adapter.

use lactea::{Compat, Engine, EngineConfig, FrameClock, Transition, compat_for_preset};

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
fn flat(r: f32, g: f32, b: f32, extra: &str) -> String {
    format!(
        "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=2\nPSVERSION_COMP=0\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n\
         warp_1=`shader_body\nwarp_2=`{{\nwarp_3=`  ret = float3({r}, {g}, {b});\nwarp_4=`}}\n{extra}"
    )
}

fn plain(r: f32, g: f32, b: f32) -> String {
    flat(r, g, b, "")
}

/// MilkDrop 3 by its expression variable `q40`.
fn q40(r: f32, g: f32, b: f32) -> String {
    flat(r, g, b, "per_frame_1=q40 = 1;\n")
}

/// MilkDrop 3 by a sprite block (the image does not exist: a warning, the preset still loads).
fn sprite(r: f32, g: f32, b: f32) -> String {
    flat(
        r,
        g,
        b,
        "[SPRITE1_BEGIN]\nSpriteName=sprites\\none.png\nSpriteLayer=1\n[SPRITE1_END]\n",
    )
}

/// A hand-made `.milk2`: PRESET1 left, PRESET2 right.
fn milk2(p1: &str, p2: &str) -> String {
    format!(
        "Created with MILKDROP 3.37 on 05/01/2026 at 12:34:56\nwww.milkdrop3.com\n\n\
         blending_pattern=side\nblending_progress=0.5\nblending_direction=1\nrandom_1=0.000000\n\
         random_2=0.500000\nrandom_3=0.5\nrandom_4=0.5\nrandom_5=0.5\n\
         \n[PRESET1_BEGIN]\nNAME=first.milk\n{p1}[PRESET1_END]\n\n[PRESET2_BEGIN]\nNAME=second\n{p2}[PRESET2_END]\n"
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

fn cfg(compat: Compat, auto_compat: bool) -> EngineConfig {
    EngineConfig {
        width: W,
        height: H,
        mesh: [16, 12],
        seed: 7,
        compat,
        auto_compat,
        blend_time_user: 0.1,
        ..Default::default()
    }
}

impl Rig {
    fn new(config: EngineConfig) -> Option<Rig> {
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

    /// Render `n` frames; returns the last one.
    fn run(&mut self, n: u32) -> Vec<u8> {
        let mut last = Vec::new();
        for _ in 0..n {
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
            last = self.read();
        }
        last
    }

    fn load(&mut self, text: &str, name: &str, t: Transition) -> lactea::PresetReport {
        self.engine
            .load_preset(text.as_bytes(), name, t)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
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

fn px(img: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

/// The flat colour of a preset: projectM's composite leaves a full channel at ~182, MilkDrop 3's at 255, so a
/// channel counts as lit above 100 and dark below 10.
fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    let ok = a
        .iter()
        .zip(b)
        .all(|(&x, y)| if y == 255 { x > 100 } else { x < 10 });
    if !ok {
        eprintln!("pixel {a:?}, expected {b:?}");
    }
    ok
}

macro_rules! rig {
    ($cfg:expr) => {
        match Rig::new($cfg) {
            Some(r) => r,
            None => {
                eprintln!("skipping: no wgpu adapter");
                return;
            }
        }
    };
}

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];

#[test]
fn compat_for_preset_picks_milkdrop3_only_on_evidence() {
    assert_eq!(
        compat_for_preset(plain(1.0, 0.0, 0.0).as_bytes()),
        Compat::ProjectM
    );
    assert_eq!(
        compat_for_preset(q40(1.0, 0.0, 0.0).as_bytes()),
        Compat::MilkDrop3
    );
    assert_eq!(
        compat_for_preset(sprite(1.0, 0.0, 0.0).as_bytes()),
        Compat::MilkDrop3
    );
    let pair = milk2(&plain(1.0, 0.0, 0.0), &plain(0.0, 0.0, 1.0));
    assert_eq!(compat_for_preset(pair.as_bytes()), Compat::MilkDrop3);
    assert_eq!(compat_for_preset(b""), Compat::ProjectM);
}

#[test]
fn a_playlist_of_mixed_presets_resolves_and_renders_each() {
    let mut rig = rig!(cfg(Compat::ProjectM, true));
    let (device, _) = GPU.clone().unwrap();
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    assert_eq!(rig.engine.current_compat(), Compat::ProjectM);

    // projectM -> projectM: no switch
    let r = rig.load(&plain(0.0, 1.0, 0.0), "a.milk", Transition::Cut);
    assert_eq!(r.compat, Compat::ProjectM);
    assert!(near(px(&rig.run(5), 32, 24), GREEN));

    // projectM -> MilkDrop 3 (q40): the mode changes, a soft cut request becomes a hard one
    let r = rig.load(
        &q40(1.0, 0.0, 0.0),
        "b.milk",
        Transition::Soft { seconds: 1.0 },
    );
    assert_eq!(r.compat, Compat::MilkDrop3);
    assert_eq!(rig.engine.current_compat(), Compat::MilkDrop3);
    assert!(rig.engine.config().compat.rules().md3_ns_eel2);
    assert!(rig.engine.transition_info().is_none());
    assert!(rig.engine.incoming_renderer().is_none());
    assert!(rig.engine.transition_progress().is_none());
    assert_eq!(rig.engine.renderer().compat(), Compat::MilkDrop3);
    assert!(
        near(px(&rig.run(1), 32, 24), RED),
        "first frame is the new preset alone"
    );
    rig.run(4);

    // MilkDrop 3 -> MilkDrop 3 sprite preset: same mode, an ordinary MD2-style blend
    let r = rig.load(
        &sprite(0.0, 0.0, 1.0),
        "c.milk",
        Transition::Soft { seconds: 0.5 },
    );
    assert_eq!(r.compat, Compat::MilkDrop3);
    assert!(rig.engine.transition_progress().is_some());
    rig.run(40);
    assert!(near(px(&rig.run(1), 32, 24), BLUE));

    // MilkDrop 3 -> .milk2 (MilkDrop 3): the double preset plays, left half PRESET1, right half PRESET2
    let pair = milk2(&plain(1.0, 0.0, 0.0), &plain(0.0, 1.0, 0.0));
    let r = rig.load(&pair, "d.milk2", Transition::Soft { seconds: 1.0 });
    assert_eq!(r.compat, Compat::MilkDrop3);
    let img = rig.run(60);
    assert!(rig.engine.double_preset_info().is_some());
    assert!(near(px(&img, 3, 24), RED), "{:?}", px(&img, 3, 24));
    assert!(
        near(px(&img, W - 4, 24), GREEN),
        "{:?}",
        px(&img, W - 4, 24)
    );

    // .milk2 -> projectM: a hard cut that also ends the double preset
    let r = rig.load(
        &plain(0.0, 0.0, 1.0),
        "e.milk",
        Transition::Soft { seconds: 1.0 },
    );
    assert_eq!(r.compat, Compat::ProjectM);
    assert_eq!(rig.engine.current_compat(), Compat::ProjectM);
    assert!(rig.engine.double_preset_info().is_none());
    assert!(rig.engine.transition_info().is_none());
    assert_eq!(rig.engine.renderer().compat(), Compat::ProjectM);
    assert!(near(px(&rig.run(1), 32, 24), BLUE));
    rig.run(4);

    // projectM -> projectM keeps the image-space soft cut
    let r = rig.load(
        &plain(0.0, 1.0, 0.0),
        "f.milk",
        Transition::Soft { seconds: 1.0 },
    );
    assert_eq!(r.compat, Compat::ProjectM);
    assert!(rig.engine.transition_info().is_some());
    assert!(rig.engine.incoming_renderer().is_some());
    rig.run(90);
    assert!(near(px(&rig.run(1), 32, 24), GREEN));

    if let Some(err) = pollster::block_on(scope.pop()) {
        panic!("wgpu validation error: {err}");
    }
}

#[test]
fn a_failed_load_keeps_the_running_mode() {
    let mut rig = rig!(cfg(Compat::ProjectM, true));
    rig.load(&plain(0.0, 1.0, 0.0), "a.milk", Transition::Cut);
    rig.run(3);
    // a `.milk2` whose header number does not parse fails in MilkDrop 3; the switch must be undone
    let bad = milk2(&plain(1.0, 0.0, 0.0), &plain(0.0, 0.0, 1.0))
        .replace("blending_progress=0.5", "blending_progress=oops");
    assert_eq!(compat_for_preset(bad.as_bytes()), Compat::MilkDrop3);
    assert!(
        rig.engine
            .load_preset(bad.as_bytes(), "bad.milk2", Transition::Cut)
            .is_err()
    );
    assert_eq!(rig.engine.current_compat(), Compat::ProjectM);
    assert_eq!(rig.engine.active_preset_name(), Some("a.milk"));
    assert_eq!(rig.engine.renderer().compat(), Compat::ProjectM);
    assert!(near(px(&rig.run(2), 32, 24), GREEN));
    // and the engine still switches afterwards
    let r = rig.load(&q40(1.0, 0.0, 0.0), "b.milk", Transition::Cut);
    assert_eq!(r.compat, Compat::MilkDrop3);
    assert!(near(px(&rig.run(2), 32, 24), RED));
}

#[test]
fn the_configured_mode_only_matters_before_the_first_load() {
    // an auto engine configured as MilkDrop 2 resolves the first preset like any other
    let mut rig = rig!(cfg(Compat::MilkDrop2, true));
    assert_eq!(rig.engine.current_compat(), Compat::MilkDrop2);
    assert_eq!(rig.engine.renderer().compat(), Compat::MilkDrop2);
    let r = rig.load(&plain(0.0, 1.0, 0.0), "a.milk", Transition::Cut);
    assert_eq!(r.compat, Compat::ProjectM);
    assert_eq!(rig.engine.renderer().compat(), Compat::ProjectM);
    assert!(near(px(&rig.run(3), 32, 24), GREEN));
}

#[test]
fn a_fixed_engine_never_switches() {
    let mut rig = rig!(cfg(Compat::ProjectM, false));
    let r = rig.load(&q40(1.0, 0.0, 0.0), "b.milk", Transition::Cut);
    assert_eq!(r.compat, Compat::ProjectM);
    assert_eq!(rig.engine.current_compat(), Compat::ProjectM);
    let mut rig = rig!(cfg(Compat::MilkDrop3, false));
    let r = rig.load(&plain(1.0, 0.0, 0.0), "a.milk", Transition::Cut);
    assert_eq!(r.compat, Compat::MilkDrop3);
    assert_eq!(rig.engine.current_compat(), Compat::MilkDrop3);
}

#[test]
fn the_same_inputs_give_the_same_frames() {
    let sequence = |rig: &mut Rig| {
        let mut frames = Vec::new();
        for (text, name, t) in [
            (plain(0.0, 1.0, 0.0), "a.milk", Transition::Cut),
            (
                q40(1.0, 0.0, 0.0),
                "b.milk",
                Transition::Soft { seconds: 1.0 },
            ),
            (
                milk2(&plain(1.0, 0.0, 0.0), &plain(0.0, 1.0, 0.0)),
                "d.milk2",
                Transition::Soft { seconds: 1.0 },
            ),
            (
                plain(0.0, 0.0, 1.0),
                "e.milk",
                Transition::Soft { seconds: 1.0 },
            ),
            (
                plain(0.0, 1.0, 0.0),
                "f.milk",
                Transition::Soft { seconds: 0.5 },
            ),
        ] {
            rig.load(&text, name, t);
            frames.push(rig.run(1));
            frames.push(rig.run(20));
        }
        frames
    };
    let mut a = rig!(cfg(Compat::ProjectM, true));
    let mut b = rig!(cfg(Compat::ProjectM, true));
    assert!(sequence(&mut a) == sequence(&mut b));
}

#[test]
fn after_a_switch_a_preset_renders_as_in_a_fixed_engine() {
    // a flat preset ignores history and audio, so the frames after the switch must equal a fixed MilkDrop 3 engine's
    let mut auto = rig!(cfg(Compat::ProjectM, true));
    auto.load(&plain(0.0, 1.0, 0.0), "a.milk", Transition::Cut);
    auto.run(5);
    auto.load(&q40(1.0, 0.0, 0.0), "b.milk", Transition::Cut);
    let mut fixed = rig!(cfg(Compat::MilkDrop3, false));
    fixed.load(&plain(0.0, 1.0, 0.0), "a.milk", Transition::Cut);
    fixed.run(5);
    fixed.load(&q40(1.0, 0.0, 0.0), "b.milk", Transition::Cut);
    for _ in 0..10 {
        assert!(auto.run(1) == fixed.run(1));
    }
}

/// A flat preset whose warp shader swizzles a parenthesised constructor: projectM's hlslparser takes `(float3(` as
/// the start of a cast and cannot parse the `.xyz` after the closing parenthesis, MilkDrop 2's translator can.
fn swizzled_constructor(r: f32, g: f32, b: f32) -> String {
    format!(
        "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=2\nPSVERSION_COMP=0\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n\
         warp_1=`shader_body\nwarp_2=`{{\nwarp_3=`  ret = (float3({r}, {g}, {b})).xyz;\nwarp_4=`}}\n"
    )
}

#[test]
fn a_projectm_shader_translation_failure_falls_back_to_milkdrop2() {
    let text = swizzled_constructor(0.0, 1.0, 0.0);
    assert_eq!(compat_for_preset(text.as_bytes()), Compat::ProjectM);

    // fixed modes are unchanged: projectM replaces the shader, MilkDrop 2 translates it
    let mut fixed = rig!(cfg(Compat::ProjectM, false));
    let r = fixed.load(&text, "s.milk", Transition::Cut);
    assert_eq!(r.compat, Compat::ProjectM);
    assert_eq!(r.shader_fallbacks.len(), 1);
    let mut fixed2 = rig!(cfg(Compat::MilkDrop2, false));
    assert!(
        fixed2
            .load(&text, "s.milk", Transition::Cut)
            .shader_fallbacks
            .is_empty()
    );

    let mut rig = rig!(cfg(Compat::ProjectM, true));
    rig.load(&q40(1.0, 0.0, 0.0), "q.milk", Transition::Cut);
    rig.run(3);
    let r = rig.load(&text, "s.milk", Transition::Cut);
    assert_eq!(r.compat, Compat::MilkDrop2);
    assert_eq!(rig.engine.current_compat(), Compat::MilkDrop2);
    assert!(r.shader_fallbacks.is_empty());
    assert!(
        r.warnings
            .iter()
            .any(|w| w.starts_with("fallback: projectM shader translation failed:")),
        "{:?}",
        r.warnings
    );
    assert!(near(px(&rig.run(2), 32, 24), GREEN));
}

/// A flat preset whose per-frame code splits the identifier `is_beat` over two code lines. MilkDrop 2 glues the
/// lines together (`is_` + `beat`), projectM keeps them apart and rejects the code.
fn split_identifier(r: f32, g: f32, b: f32) -> String {
    flat(
        r,
        g,
        b,
        "per_frame_1=is_beat = 1;\nper_frame_2=k1 =  is_\nper_frame_3=beat*0;\n",
    )
}

#[test]
fn a_projectm_compile_error_falls_back_to_milkdrop2() {
    let text = split_identifier(0.0, 1.0, 0.0);
    assert_eq!(compat_for_preset(text.as_bytes()), Compat::ProjectM);

    // fixed modes are unchanged: projectM rejects it, MilkDrop 2 loads it
    let mut fixed = rig!(cfg(Compat::ProjectM, false));
    assert!(
        fixed
            .engine
            .load_preset(text.as_bytes(), "s.milk", Transition::Cut)
            .is_err()
    );
    let mut fixed2 = rig!(cfg(Compat::MilkDrop2, false));
    fixed2.load(&text, "s.milk", Transition::Cut);

    let mut rig = rig!(cfg(Compat::ProjectM, true));
    rig.load(&q40(1.0, 0.0, 0.0), "q.milk", Transition::Cut);
    rig.run(3);
    let r = rig.load(&text, "s.milk", Transition::Soft { seconds: 1.0 });
    assert_eq!(r.compat, Compat::MilkDrop2);
    assert_eq!(rig.engine.current_compat(), Compat::MilkDrop2);
    assert_eq!(rig.engine.renderer().compat(), Compat::MilkDrop2);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.starts_with("fallback: projectM rejected: per-frame code")),
        "{:?}",
        r.warnings
    );
    // a hard cut (the mode changed from MilkDrop 3), and the preset renders
    assert!(rig.engine.transition_info().is_none());
    assert!(near(px(&rig.run(2), 32, 24), GREEN));

    // the next plain preset goes back to projectM
    let r = rig.load(&plain(0.0, 0.0, 1.0), "p.milk", Transition::Cut);
    assert_eq!(r.compat, Compat::ProjectM);
    assert!(r.warnings.is_empty());
    assert_eq!(rig.engine.current_compat(), Compat::ProjectM);
}

#[test]
fn a_preset_both_modes_reject_reports_the_projectm_error_and_keeps_the_mode() {
    let mut rig = rig!(cfg(Compat::ProjectM, true));
    rig.load(&plain(0.0, 1.0, 0.0), "a.milk", Transition::Cut);
    rig.run(3);
    let bad = flat(1.0, 0.0, 0.0, "per_frame_1=x = = 1;\n");
    let err = rig
        .engine
        .load_preset(bad.as_bytes(), "bad.milk", Transition::Cut)
        .unwrap_err()
        .to_string();
    assert!(err.starts_with("per-frame code"), "{err}");
    assert_eq!(rig.engine.current_compat(), Compat::ProjectM);
    assert_eq!(rig.engine.active_preset_name(), Some("a.milk"));
    assert_eq!(rig.engine.renderer().compat(), Compat::ProjectM);
    assert!(near(px(&rig.run(2), 32, 24), GREEN));
}
