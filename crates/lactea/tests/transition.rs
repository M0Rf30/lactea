//! Soft cuts through the engine: window timing, both presets running, projectM's seeded shader
//! choice, `start_clean`. Skipped (with a note) when there is no GPU adapter.

use lactea::{Engine, EngineConfig, FrameClock, Transition, TransitionKind};

const W: u32 = 64;
const H: u32 = 48;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// A static full-frame colour: the outer border (size 1) covers everything, no waves, no motion.
fn solid(r: f32, g: f32, b: f32) -> String {
    format!(
        "MILKDROP_PRESET_VERSION=100\nfDecay=1\nfWaveAlpha=0\nob_size=1\nob_a=1\nob_r={r}\nob_g={g}\nob_b={b}\n"
    )
}

/// No content at all: whatever the first warp samples stays (decay 1).
const EMPTY: &str = "MILKDROP_PRESET_VERSION=100\nfDecay=1\nfWaveAlpha=0\n";

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    engine: Engine,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    frame: u32,
}

impl Rig {
    fn new(start_clean: bool, seed: u64) -> Option<Rig> {
        let (device, queue) = gpu()?;
        let engine = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: W,
                height: H,
                mesh: [16, 12],
                seed,
                start_clean,
                ..Default::default()
            },
        );
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

    /// Render the next frame (60 fps clock) and return the target pixels.
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

fn centre(img: &[u8]) -> [u8; 3] {
    let i = (((H / 2) * W + W / 2) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

/// Frame time `n / 60`: a 1 s soft cut loaded before frame 10 starts at the time of frame 9
/// (projectM starts at the previous frame's time) and is drawn on frames 10..=69 (`69/60 - 9/60`
/// is just below 1 in doubles), frame 70 is the first normal frame of the new preset.
#[test]
fn soft_cut_window_and_both_presets_keep_running() {
    let Some(mut rig) = Rig::new(true, 1) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    rig.engine
        .load_preset(solid(1.0, 0.0, 0.0).as_bytes(), "red", Transition::Cut)
        .unwrap();
    let mut red_alone = Vec::new();
    for n in 0..10 {
        let img = rig.step();
        if n == 9 {
            red_alone = img;
        }
    }
    assert!(rig.engine.transition_progress().is_none());
    rig.engine
        .force_next_transition(Some(TransitionKind::SimpleBlend), None);
    rig.engine
        .load_preset(
            solid(0.0, 0.0, 1.0).as_bytes(),
            "blue",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    assert_eq!(rig.engine.preset_name(), Some("blue"));
    assert_eq!(rig.engine.active_preset_name(), Some("red"));
    let info = rig
        .engine
        .transition_info()
        .expect("a soft cut was started");
    assert_eq!(info.kind, TransitionKind::SimpleBlend);
    assert_eq!(info.duration, 1.0);

    let mut first = Vec::new();
    let mut mid = Vec::new();
    for n in 10..=69 {
        let img = rig.step();
        let p = rig
            .engine
            .transition_progress()
            .unwrap_or_else(|| panic!("frame {n}: no soft cut"));
        assert!(rig.engine.incoming_renderer().is_some());
        assert!(
            (p - f64::from(n - 9) / 60.0).abs() < 1e-9,
            "frame {n}: progress {p}"
        );
        if n == 10 {
            first = img;
        } else if n == 39 {
            mid = img;
        }
    }
    // frame 10: 1/60 of the way, cosine progress ~ 0.0007: still the red preset
    let (c0, red) = (centre(&first), centre(&red_alone));
    assert!(
        i32::from(c0[0]) + 3 >= i32::from(red[0]) && c0[2] <= 3,
        "start: {c0:?} vs red alone {red:?}"
    );
    // frame 39: linear 30/60 = half way: cosine 0.5, an even mix of both presets
    let m = centre(&mid);
    assert!(
        m[0] > 40 && m[2] > 40,
        "mid-transition should show both presets: {m:?}"
    );
    let sum = i32::from(m[0]) + i32::from(m[2]);
    assert!(
        (sum - i32::from(red[0])).abs() <= 6,
        "even mix of red {red:?} and blue: {m:?}"
    );

    // frame 70: the soft cut is over, the incoming preset is the active one
    let last = rig.step();
    assert!(rig.engine.transition_progress().is_none());
    assert!(rig.engine.incoming_renderer().is_none());
    assert_eq!(rig.engine.active_preset_name(), Some("blue"));
    let c = centre(&last);
    assert!(c[0] <= 3 && c[2] >= 40, "blue only after the window: {c:?}");
}

/// projectM draws `progress = 1` for both presets while smoothing and `elapsed / preset_duration`
/// (from the start of the soft cut) afterwards.
#[test]
fn progress_variable_is_one_while_smoothing() {
    let Some(mut rig) = Rig::new(true, 1) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    // `q1 = progress` shows in the per-frame vars
    let preset =
        "MILKDROP_PRESET_VERSION=100\nfDecay=1\nfWaveAlpha=0\nper_frame_1=q1 = progress;\n";
    rig.engine
        .load_preset(preset.as_bytes(), "a", Transition::Cut)
        .unwrap();
    for _ in 0..3 {
        rig.step();
    }
    let q1 = |e: &Engine| {
        e.frame_vars()
            .into_iter()
            .find(|(n, _)| n == "q1")
            .map(|(_, v)| v)
            .unwrap()
    };
    assert!(
        q1(&rig.engine) < 1e-6,
        "no smoothing: progress {}",
        q1(&rig.engine)
    );
    rig.engine
        .load_preset(preset.as_bytes(), "b", Transition::Soft { seconds: 0.5 })
        .unwrap();
    rig.step();
    assert_eq!(q1(&rig.engine), 1.0);
    let inc = rig
        .engine
        .incoming_frame_vars()
        .into_iter()
        .find(|(n, _)| n == "q1")
        .map(|(_, v)| v);
    assert_eq!(inc, Some(1.0));
}

/// The shader choice and the four static integers come from the documented seeded streams; these
/// values are what the instrumented projectM draws for the same seeds (oracle/README.md).
#[test]
fn seeded_transition_matches_the_oracle_streams() {
    let expected: [(u64, TransitionKind, [i32; 4]); 4] = [
        (
            1,
            TransitionKind::Plasma,
            [373681287, -1050250163, -1780155473, 1561790212],
        ),
        (
            2,
            TransitionKind::ZoomBlur,
            [498705158, 56118026, -607435896, -2081302658],
        ),
        (
            3,
            TransitionKind::Warp,
            [676665606, 186979840, -657669255, 1605857494],
        ),
        (
            8,
            TransitionKind::SimpleBlend,
            [-806856679, -1041133122, -1157823268, 170814210],
        ),
    ];
    for (seed, kind, rand_static) in expected {
        let Some(mut rig) = Rig::new(true, seed) else {
            eprintln!("SKIP: no wgpu adapter");
            return;
        };
        rig.engine
            .load_preset(EMPTY.as_bytes(), "a", Transition::Cut)
            .unwrap();
        rig.step();
        rig.engine
            .load_preset(EMPTY.as_bytes(), "b", Transition::Soft { seconds: 1.0 })
            .unwrap();
        let t = rig.engine.transition_info().unwrap();
        assert_eq!((t.kind, t.rand_static), (kind, rand_static), "seed {seed}");
    }
}

/// A second soft cut continues the streams (the oracle's two statics are process-wide).
#[test]
fn consecutive_soft_cuts_draw_new_values() {
    let Some(mut rig) = Rig::new(true, 1) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    rig.engine
        .load_preset(EMPTY.as_bytes(), "a", Transition::Cut)
        .unwrap();
    rig.step();
    rig.engine
        .load_preset(EMPTY.as_bytes(), "b", Transition::Soft { seconds: 1.0 })
        .unwrap();
    let first = rig.engine.transition_info().unwrap();
    // loading again while blending completes the first soft cut at once and starts another
    rig.step();
    rig.engine
        .load_preset(EMPTY.as_bytes(), "c", Transition::Soft { seconds: 1.0 })
        .unwrap();
    let second = rig.engine.transition_info().unwrap();
    assert_ne!(first.rand_static, second.rand_static);
    assert_eq!(rig.engine.active_preset_name(), Some("b"));
    assert_eq!(rig.engine.preset_name(), Some("c"));
}

/// projectM's default (`start_clean` off): the new preset's first frame warps from the old
/// preset's finished image; with `start_clean` it starts black.
#[test]
fn new_preset_starts_from_the_old_image_unless_start_clean() {
    for clean in [false, true] {
        let Some(mut rig) = Rig::new(clean, 1) else {
            eprintln!("SKIP: no wgpu adapter");
            return;
        };
        rig.engine
            .load_preset(solid(1.0, 0.0, 0.0).as_bytes(), "red", Transition::Cut)
            .unwrap();
        for _ in 0..3 {
            rig.step();
        }
        let old = centre(&rig.step());
        assert!(old[0] > 40);
        rig.engine
            .load_preset(EMPTY.as_bytes(), "empty", Transition::Cut)
            .unwrap();
        let first = centre(&rig.step());
        if clean {
            assert_eq!(first, [0, 0, 0], "start_clean: black");
        } else {
            assert!(
                (i32::from(first[0]) - i32::from(old[0])).abs() <= 3
                    && first[1] <= 3
                    && first[2] <= 3,
                "old image carried over: {first:?} vs {old:?}"
            );
        }
    }
}

/// A failed load must not disturb a running soft cut.
#[test]
fn failed_load_keeps_the_running_transition() {
    let Some(mut rig) = Rig::new(true, 1) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    rig.engine
        .load_preset(solid(1.0, 0.0, 0.0).as_bytes(), "red", Transition::Cut)
        .unwrap();
    rig.step();
    rig.engine
        .load_preset(
            solid(0.0, 0.0, 1.0).as_bytes(),
            "blue",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    rig.step();
    let bad = b"MILKDROP_PRESET_VERSION=100\nper_frame_1=zoom = = 3;\n";
    assert!(
        rig.engine
            .load_preset(bad, "bad", Transition::Soft { seconds: 1.0 })
            .is_err()
    );
    assert_eq!(rig.engine.active_preset_name(), Some("red"));
    assert_eq!(rig.engine.preset_name(), Some("blue"));
    assert!(rig.engine.transition_progress().is_some());
    rig.step();
}
