//! MilkDrop 2's own preset blend (`Compat::MilkDrop2`, `m_bBlending`) through the engine: one renderer, both
//! states' equations running, the blended variables / meshes / shapes, window length, loading during a blend, the
//! warp / composite pass pairs for every shader combination. Skipped (with a note) when there is no GPU adapter.

use lactea::{Compat, Engine, EngineConfig, FrameClock, Transition};

const W: u32 = 64;
const H: u32 = 48;

/// One device for the whole test binary (error scopes are per thread, so tests do not disturb each other).
static GPU: std::sync::LazyLock<Option<(wgpu::Device, wgpu::Queue)>> =
    std::sync::LazyLock::new(|| {
        let instance = wgpu::Instance::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    });

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    GPU.clone()
}

/// A v1 preset: no shaders, no decay, no main wave, gamma 1 (so a finished frame shows the main image unscaled).
fn preset(extra: &str) -> String {
    preset_full("1", "0", extra)
}

/// [`preset`] with explicit `fDecay` / `fWaveAlpha` (the parser takes the first copy of a duplicated key).
fn preset_full(decay: &str, wave_alpha: &str, extra: &str) -> String {
    format!(
        "MILKDROP_PRESET_VERSION=100\nfDecay={decay}\nfWaveAlpha={wave_alpha}\nfGammaAdj=1\n{extra}"
    )
}

/// A static full-frame colour: the outer border (size 1) covers everything.
fn solid(r: f32, g: f32, b: f32) -> String {
    preset(&format!(
        "ob_size=1\nob_a=1\nob_r={r}\nob_g={g}\nob_b={b}\n"
    ))
}

/// Like [`solid`] with explicit pixel-shader versions (v2.01 file: PSVERSION_WARP / PSVERSION_COMP).
fn solid_ps(r: f32, g: f32, b: f32, warp: bool, comp: bool) -> String {
    format!(
        "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP={}\nPSVERSION_COMP={}\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\nob_size=1\nob_a=1\nob_r={r}\nob_g={g}\nob_b={b}\n",
        if warp { 2 } else { 0 },
        if comp { 2 } else { 0 }
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

impl Rig {
    fn new(seed: u64) -> Option<Rig> {
        Rig::with(seed, 1e9)
    }

    fn with(seed: u64, preset_duration: f64) -> Option<Rig> {
        let (device, queue) = gpu()?;
        let engine = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: W,
                height: H,
                mesh: [16, 12],
                seed,
                compat: Compat::MilkDrop2,
                start_clean: true,
                preset_duration,
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

    fn run_to(&mut self, frame: u32) -> Vec<u8> {
        let mut last = Vec::new();
        while self.frame <= frame {
            last = self.step();
        }
        last
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
    ($seed:expr) => {
        match Rig::new($seed) {
            Some(r) => r,
            None => {
                eprintln!("SKIP: no wgpu adapter");
                return;
            }
        }
    };
}

fn centre(img: &[u8]) -> [i32; 3] {
    let i = (((H / 2) * W + W / 2) * 4) as usize;
    [
        i32::from(img[i]),
        i32::from(img[i + 1]),
        i32::from(img[i + 2]),
    ]
}

fn max_abs_diff(a: &[u8], b: &[u8]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs())
        .max()
        .unwrap_or(0)
}

fn state_var(e: &Engine, name: &str) -> f64 {
    e.frame_vars()
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no variable {name}"))
}

/// `CosineInterp` (`0.5 - 0.5 cos(pi x)`).
#[allow(clippy::approx_constant, clippy::excessive_precision)] // MD2's own rounded pi
fn cosine(p: f32) -> f32 {
    0.5 - 0.5 * (p * 3.1415926535898f32).cos()
}

/// `((int)(x * 255)) & 0xFF`: how MD2 quantises a vertex colour.
fn quant(x: f32) -> i32 {
    ((x * 255.0) as i32) & 0xFF
}

/// Blend progress of frame `frame` of a blend loaded after `loaded_after` rendered frames: MD2's float
/// `(GetTime() - start) / duration` with `start` the time of the last rendered frame.
fn progress(frame: u32, loaded_after: u32, dur: f32) -> f32 {
    let start = (f64::from(loaded_after - 1) / 60.0) as f32;
    let t = (f64::from(frame) / 60.0) as f32;
    (t - start) / dur
}

/// The value MD2 shows for a border colour component that blends from `old` to `new`: the new state's own
/// `CBlendableFloat` runs linearly from the old value, then the two states' values are mixed by `CosineInterp`
/// (`old + cosine(p) * p * (new - old)`), then the vertex colour is truncated to 8 bit.
fn blended_border(old: f32, new: f32, p: f32) -> i32 {
    let cm = f64::from(cosine(p));
    let new_eval = old * (1.0 - p) + new * p;
    let v = cm * f64::from(new_eval) + (1.0 - cm) * f64::from(old);
    quant(v as f32)
}

/// The window: a 1 s blend loaded before frame 10 starts at the time of frame 9 and is drawn on frames 10..=69
/// (`progress > 1.0f` ends it, frame 70 is the first normal frame); one renderer, no incoming preset, no
/// image-space transition info.
#[test]
fn soft_cut_runs_in_one_renderer_and_the_window_ends_above_one() {
    let mut rig = rig!(1);
    rig.engine
        .load_preset(solid(1.0, 0.0, 0.0).as_bytes(), "red", Transition::Cut)
        .unwrap();
    rig.run_to(9);
    assert!(rig.engine.transition_progress().is_none());
    rig.engine
        .load_preset(
            solid(0.0, 0.0, 1.0).as_bytes(),
            "blue",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    // MD2: the new preset is `m_pState`; no second renderer, no transition shader
    assert_eq!(rig.engine.preset_name(), Some("blue"));
    assert_eq!(rig.engine.active_preset_name(), Some("blue"));
    assert!(rig.engine.incoming_renderer().is_none());
    assert!(rig.engine.transition_info().is_none());
    assert_eq!(rig.engine.transition_progress(), Some(0.0));
    assert!(rig.engine.md2_blend_pattern().is_some());

    for n in 10..=69 {
        rig.step();
        let p = rig
            .engine
            .transition_progress()
            .unwrap_or_else(|| panic!("frame {n}: no blend"));
        assert!(
            (p - f64::from(progress(n, 10, 1.0))).abs() < 1e-6,
            "frame {n}: progress {p}"
        );
        assert!(rig.engine.incoming_renderer().is_none());
    }
    rig.step();
    assert!(
        rig.engine.transition_progress().is_none(),
        "frame 70 is the first normal frame"
    );
    assert!(rig.engine.md2_blend_pattern().is_none());
    assert_eq!(centre(&rig.step()), [0, 0, 255]);
}

/// Hand-computed variable blend: the border colour of the blended state is
/// `old + cosine(p) * p * (new - old)` (the new state's own float blend, then the cosine mix of the two states).
#[test]
fn blended_variables_follow_the_double_blend_formula() {
    let mut rig = rig!(1);
    rig.engine
        .load_preset(solid(1.0, 0.0, 0.0).as_bytes(), "red", Transition::Cut)
        .unwrap();
    rig.run_to(9);
    rig.engine
        .load_preset(
            solid(0.0, 0.0, 1.0).as_bytes(),
            "blue",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    let mut checked = 0;
    for n in 10..=69 {
        let img = rig.step();
        if n % 7 != 3 {
            continue;
        }
        let p = progress(n, 10, 1.0);
        let c = centre(&img);
        let want = [blended_border(1.0, 0.0, p), 0, blended_border(0.0, 1.0, p)];
        for k in 0..3 {
            assert!(
                (c[k] - want[k]).abs() <= 2,
                "frame {n} p={p}: {c:?} vs {want:?}"
            );
        }
        checked += 1;
    }
    assert!(checked >= 8);
    // mid-blend both colours are present
}

/// At the first blend frame (progress ~ 0) the picture is the old preset alone: per-frame variables, per-vertex
/// motion, custom shapes. Compared with an engine that never switched.
#[test]
fn first_blend_frame_is_the_old_preset_alone() {
    let old = preset_full(
        "0.97",
        "0",
        "zoom=1.03\nrot=0.05\ndx=0.01\n\
         shapecode_0_enabled=1\nshapecode_0_sides=4\nshapecode_0_x=0.3\nshapecode_0_y=0.55\nshapecode_0_rad=0.18\n\
         shapecode_0_ang=0.4\nshapecode_0_r=1\nshapecode_0_g=0.5\nshapecode_0_b=0.2\nshapecode_0_a=1\n\
         shapecode_0_r2=0.1\nshapecode_0_g2=0.9\nshapecode_0_b2=0.8\nshapecode_0_a2=1\n",
    );
    let new = preset_full(
        "0.9",
        "0",
        "zoom=0.9\nrot=-0.3\ndx=-0.05\nwarp=0\nob_size=0.2\nob_a=1\nob_g=1\n",
    );
    let mut alone = rig!(5);
    alone
        .engine
        .load_preset(old.as_bytes(), "old", Transition::Cut)
        .unwrap();
    let mut blended = rig!(5);
    blended
        .engine
        .load_preset(old.as_bytes(), "old", Transition::Cut)
        .unwrap();
    alone.run_to(19);
    blended.run_to(19);
    // 600 s blend: progress at frame 20 is 2.8e-5 (the vertex alphas are all 0: only the old shader / state is seen)
    blended
        .engine
        .load_preset(new.as_bytes(), "new", Transition::Soft { seconds: 600.0 })
        .unwrap();
    let a = alone.run_to(20);
    let b = blended.run_to(20);
    assert!(blended.engine.transition_progress().unwrap() < 1e-4);
    let d = max_abs_diff(&a, &b);
    // the shape's alpha_mult 1 - 2.8e-5 truncates 255 to 254: one level at most, plus UV rounding
    assert!(d <= 3, "max abs diff {d}");
    // sanity: the picture is not empty, and the test is sensitive: a hard cut to the new preset at the same frame
    // looks quite different
    assert!(a.iter().any(|v| *v > 60));
    let mut cut = rig!(5);
    cut.engine
        .load_preset(old.as_bytes(), "old", Transition::Cut)
        .unwrap();
    cut.run_to(19);
    cut.engine
        .load_preset(new.as_bytes(), "new", Transition::Cut)
        .unwrap();
    let c = cut.run_to(20);
    assert!(
        max_abs_diff(&a, &c) > 20,
        "a hard cut differs from the old preset alone"
    );
}

/// The last blend frames show the new preset; the first frame after the window is the new preset alone.
#[test]
fn blend_ends_in_the_new_preset() {
    let mut rig = rig!(1);
    rig.engine
        .load_preset(solid(1.0, 0.0, 0.0).as_bytes(), "red", Transition::Cut)
        .unwrap();
    rig.run_to(9);
    rig.engine
        .load_preset(
            solid(0.0, 0.0, 1.0).as_bytes(),
            "blue",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    let last = rig.run_to(69);
    let c = centre(&last);
    assert!(c[0] <= 3 && c[2] >= 250, "last blend frame: {c:?}");
    assert_eq!(centre(&rig.step()), [0, 0, 255]);
}

/// Pure per-vertex UV blend: with the uniform pattern a mesh vertex's UV is `new * mix2 + old * (1 - mix2)` with
/// `mix2 = progress`, so one blend frame moves the feedback image by `progress * dx` (the new preset translates by
/// `dx`, the old one does not). The red square drawn on frame 0 only (shape code) is followed by its centroid.
#[test]
fn uv_blend_moves_the_feedback_by_mix2_times_the_new_motion() {
    let seed = preset(
        "shapecode_0_enabled=1\nshapecode_0_sides=4\nshapecode_0_x=0.25\nshapecode_0_y=0.5\nshapecode_0_rad=0.15\n\
         shapecode_0_r=1\nshapecode_0_g=0\nshapecode_0_b=0\nshapecode_0_a=1\n\
         shapecode_0_r2=1\nshapecode_0_g2=0\nshapecode_0_b2=0\nshapecode_0_a2=1\nshapecode_0_border_a=0\n\
         shape_0_per_frame1=a = below(frame, 1); a2 = a;\nwarp=0\n",
    );
    let mover = preset("warp=0\ndx=0.3\n");
    let centroid_x = |img: &[u8]| {
        let (mut sum, mut mass) = (0.0f64, 0.0f64);
        for y in 0..H {
            for x in 0..W {
                let r = f64::from(img[((y * W + x) * 4) as usize]);
                sum += (f64::from(x) + 0.5) * r;
                mass += r;
            }
        }
        sum / mass
    };
    let mut rig = rig!(1);
    rig.engine.set_md2_blend_pattern(Some(0));
    rig.engine
        .load_preset(seed.as_bytes(), "seed", Transition::Cut)
        .unwrap();
    let before = rig.run_to(9);
    let x0 = centroid_x(&before);
    assert!(
        (x0 - 0.25 * f64::from(W)).abs() < 1.0,
        "the square is at x = 0.25: {x0}"
    );
    // 0.1 s blend: the first blend frame (10) is at progress 1/60 / 0.1
    rig.engine
        .load_preset(mover.as_bytes(), "mover", Transition::Soft { seconds: 0.1 })
        .unwrap();
    assert_eq!(rig.engine.md2_blend_pattern(), Some(0));
    let after = rig.run_to(10);
    let p = f64::from(progress(10, 10, 0.1));
    let x1 = centroid_x(&after);
    let want = p * 0.3 * f64::from(W);
    assert!(
        ((x1 - x0) - want).abs() < 0.3,
        "centroid moved {} px, expected {want} (progress {p})",
        x1 - x0
    );
}

/// While blending, the old state's shapes are drawn with `alpha_mult = 1 - progress`, the new state's with
/// `progress`, new first. With decay 0 every frame starts black, so the centre pixel is the plain alpha mix.
#[test]
fn shapes_of_both_states_are_drawn_with_alpha_mult() {
    let shape = |r: f32, g: f32, b: f32| {
        preset_full(
            "0",
            "0",
            &format!(
                "shapecode_0_enabled=1\nshapecode_0_sides=4\nshapecode_0_x=0.5\nshapecode_0_y=0.5\n\
             shapecode_0_rad=0.9\nshapecode_0_r={r}\nshapecode_0_g={g}\nshapecode_0_b={b}\nshapecode_0_a=1\n\
             shapecode_0_r2={r}\nshapecode_0_g2={g}\nshapecode_0_b2={b}\nshapecode_0_a2=1\nshapecode_0_border_a=0\n"
            ),
        )
    };
    let mut rig = rig!(2);
    rig.engine
        .load_preset(
            shape(1.0, 0.0, 0.0).as_bytes(),
            "red shape",
            Transition::Cut,
        )
        .unwrap();
    rig.run_to(9);
    // red shape alone: opaque red
    assert_eq!(centre(&rig.step()), [255, 0, 0]);
    rig.engine
        .load_preset(
            shape(0.0, 0.0, 1.0).as_bytes(),
            "blue shape",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    for n in [20u32, 35, 50, 60] {
        let img = rig.run_to(n);
        let p = progress(n, 11, 1.0);
        let a_new = ((255.0f32) * p) as i32 & 0xFF; // (int)(a * 255 * alpha_mult), new state
        let a_old = (255.0f32 * (1.0 - p)) as i32 & 0xFF; // old state
        // new (blue) first onto black, then old (red) over it
        let blue = a_new as f32;
        let red = a_old as f32;
        let want = [red, 0.0, blue * (1.0 - red / 255.0)];
        let c = centre(&img);
        for k in 0..3 {
            assert!(
                (c[k] as f32 - want[k]).abs() <= 3.0,
                "frame {n} p={p}: {c:?} vs {want:?}"
            );
        }
        assert!(c[0] > 0 && c[2] > 0, "both shapes show: {c:?}");
    }
}

/// `progress` of the preset variable during a blend: `(time - presetStart) / (nextPresetTime - presetStart)` with
/// `nextPresetTime` fixed on the first frame after the load; afterwards the engine's usual elapsed fraction.
#[test]
fn progress_variable_follows_md2_during_a_blend() {
    let Some(mut rig) = Rig::with(1, 10.0) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let p = preset("per_frame_1=q1 = progress;\n");
    rig.engine
        .load_preset(p.as_bytes(), "a", Transition::Cut)
        .unwrap();
    rig.run_to(5);
    rig.engine
        .load_preset(p.as_bytes(), "b", Transition::Soft { seconds: 0.5 })
        .unwrap();
    // loaded after frame 5: start = 5/60; the first blend frame (6) fixes next = 6/60 + 10
    for n in 6..=20u32 {
        rig.step();
        let start = (5.0f64 / 60.0) as f32;
        let next = (f64::from(6u32) / 60.0 + 10.0) as f32;
        let t = (f64::from(n) / 60.0) as f32;
        let want = (t - start) / (next - start);
        let q1 = state_var(&rig.engine, "q1");
        if rig.engine.transition_progress().is_some() {
            assert!(
                (q1 - f64::from(want)).abs() < 1e-6,
                "frame {n}: {q1} vs {want}"
            );
        } else {
            // blend over (0.5 s window = frames 6..=35): the usual progress
            assert!(q1 >= 0.0);
        }
    }
    assert!(
        rig.engine.transition_progress().is_some(),
        "still blending at frame 20"
    );
    rig.run_to(40);
    assert!(rig.engine.transition_progress().is_none());
    let q1 = state_var(&rig.engine, "q1");
    assert!(
        (q1 - (40.0 / 60.0 - 5.0 / 60.0) / 10.0).abs() < 1e-6,
        "after the blend: elapsed / duration {q1}"
    );
}

/// Loading a preset during a blend makes the half-blended state the old one and drops the previous old state: the
/// new blend starts again at progress 0.
#[test]
fn loading_during_a_blend_blends_from_the_current_state() {
    let mut rig = rig!(1);
    rig.engine
        .load_preset(solid(1.0, 0.0, 0.0).as_bytes(), "red", Transition::Cut)
        .unwrap();
    rig.run_to(9);
    rig.engine
        .load_preset(
            solid(0.0, 1.0, 0.0).as_bytes(),
            "green",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    let mid = rig.run_to(39);
    let c_mid = centre(&mid);
    assert!(
        c_mid[0] > 20 && c_mid[1] > 20,
        "red/green mix at 0.5: {c_mid:?}"
    );
    let kind1 = rig.engine.md2_blend_pattern().unwrap();
    rig.engine
        .load_preset(
            solid(0.0, 0.0, 1.0).as_bytes(),
            "blue",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    assert_eq!(rig.engine.preset_name(), Some("blue"));
    assert_eq!(rig.engine.transition_progress(), Some(0.0));
    assert!(rig.engine.md2_blend_pattern().is_some());
    let _ = kind1;
    // window of the second blend: loaded after frame 39 -> start 39/60, drawn 40..=99, over at 100
    for n in 40..=99 {
        rig.step();
        let p = rig
            .engine
            .transition_progress()
            .unwrap_or_else(|| panic!("frame {n}: no blend"));
        assert!(
            (p - f64::from(progress(n, 40, 1.0))).abs() < 1e-6,
            "frame {n}: progress {p}"
        );
    }
    let c = centre(&rig.step());
    assert!(rig.engine.transition_progress().is_none());
    assert_eq!(c, [0, 0, 255]);
}

/// A hard cut ends a running blend at once (`Default()` clears `m_bBlending`); a non-positive soft duration is
/// MD2's `fBlendTime == 0` hard cut; with nothing loaded a soft cut is a cut.
#[test]
fn cuts_end_a_blend_and_zero_duration_is_a_cut() {
    let mut rig = rig!(1);
    rig.engine
        .load_preset(
            solid(1.0, 0.0, 0.0).as_bytes(),
            "red",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    assert!(
        rig.engine.transition_progress().is_none(),
        "nothing to blend from: a cut"
    );
    rig.run_to(9);
    rig.engine
        .load_preset(
            solid(0.0, 1.0, 0.0).as_bytes(),
            "green",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    rig.run_to(20);
    assert!(rig.engine.transition_progress().is_some());
    rig.engine
        .load_preset(solid(0.0, 0.0, 1.0).as_bytes(), "blue", Transition::Cut)
        .unwrap();
    assert!(rig.engine.transition_progress().is_none());
    assert!(rig.engine.md2_blend_pattern().is_none());
    assert_eq!(centre(&rig.step()), [0, 0, 255]);
    rig.engine
        .load_preset(
            solid(1.0, 0.0, 0.0).as_bytes(),
            "red",
            Transition::Soft { seconds: 0.0 },
        )
        .unwrap();
    assert!(rig.engine.transition_progress().is_none());
    assert_eq!(centre(&rig.step()), [255, 0, 0]);
}

/// A failed load must not disturb a running blend (the old shaders and state are put back).
#[test]
fn failed_load_keeps_the_running_blend() {
    let mut rig = rig!(1);
    rig.engine
        .load_preset(solid(1.0, 0.0, 0.0).as_bytes(), "red", Transition::Cut)
        .unwrap();
    rig.run_to(5);
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
    assert_eq!(rig.engine.preset_name(), Some("blue"));
    assert!(rig.engine.transition_progress().is_some());
    let kind = rig.engine.md2_blend_pattern();
    rig.step();
    assert_eq!(rig.engine.md2_blend_pattern(), kind);
    // and it still ends in the new preset
    rig.run_to(70);
    assert_eq!(centre(&rig.step()), [0, 0, 255]);
}

/// Same seed, same frames; the pattern comes from the documented seeded stream and can be forced
/// (`m_nTransitionBlendPattern`).
#[test]
fn blend_pattern_is_seeded_and_forceable() {
    let run = |seed: u64, forced: Option<u32>| {
        let mut rig = Rig::new(seed)?;
        rig.engine.set_md2_blend_pattern(forced);
        rig.engine
            .load_preset(solid(1.0, 0.0, 0.0).as_bytes(), "a", Transition::Cut)
            .unwrap();
        rig.run_to(3);
        rig.engine
            .load_preset(
                solid(0.0, 0.0, 1.0).as_bytes(),
                "b",
                Transition::Soft { seconds: 0.5 },
            )
            .unwrap();
        let kind = rig.engine.md2_blend_pattern().unwrap();
        let img = rig.run_to(20);
        Some((kind, img))
    };
    let Some((k1, i1)) = run(7, None) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let (k2, i2) = run(7, None).unwrap();
    assert_eq!((k1, &i1), (k2, &i2), "same seed, same pattern and frames");
    assert!(k1 <= 18);
    let kinds: std::collections::BTreeSet<u32> =
        (1..=8u64).map(|s| run(s, None).unwrap().0).collect();
    assert!(kinds.len() >= 2, "the seed selects the pattern: {kinds:?}");
    for forced in [0u32, 2, 18] {
        assert_eq!(run(7, Some(forced)).unwrap().0, forced);
    }
}

/// Every combination of warp / composite shaders in the old and the new preset goes through MD2's pass tables
/// (`WarpedBlit_*`, `ShowToUser_*`, tile culling, flipped alpha for the "funky reversal"): no validation errors, the
/// first blend frame shows the old preset, the last the new one.
#[test]
fn every_shader_combination_blends_cleanly() {
    let Some((device, _)) = gpu() else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut n_runs = 0;
    for bits in 0..16u32 {
        let (ow, oc, nw, nc) = (bits & 8 != 0, bits & 4 != 0, bits & 2 != 0, bits & 1 != 0);
        let Some(mut r) = Rig::new(u64::from(bits) + 1) else {
            return;
        };
        r.engine
            .load_preset(
                solid_ps(1.0, 0.0, 0.0, ow, oc).as_bytes(),
                "old",
                Transition::Cut,
            )
            .unwrap();
        r.run_to(4);
        r.engine
            .load_preset(
                solid_ps(0.0, 0.0, 1.0, nw, nc).as_bytes(),
                "new",
                Transition::Soft { seconds: 600.0 },
            )
            .unwrap();
        let first = r.run_to(5);
        let c = centre(&first);
        assert!(
            c[0] >= 250 && c[1] <= 4 && c[2] <= 4,
            "old/new shaders {ow}{oc}/{nw}{nc}: first blend frame {c:?}"
        );
        // second run: short blend, ends in the new preset
        let Some(mut r2) = Rig::new(u64::from(bits) + 100) else {
            return;
        };
        r2.engine
            .load_preset(
                solid_ps(1.0, 0.0, 0.0, ow, oc).as_bytes(),
                "old",
                Transition::Cut,
            )
            .unwrap();
        r2.run_to(4);
        r2.engine
            .load_preset(
                solid_ps(0.0, 0.0, 1.0, nw, nc).as_bytes(),
                "new",
                Transition::Soft { seconds: 0.25 },
            )
            .unwrap();
        let mut mid = Vec::new();
        for n in 5..=9u32 {
            let img = r2.step();
            if n == 9 {
                mid = img;
            }
        }
        let cm = centre(&mid);
        assert!(
            cm[0] + cm[2] > 40,
            "old/new shaders {ow}{oc}/{nw}{nc}: mid-blend {cm:?}"
        );
        let end = r2.run_to(30);
        assert!(r2.engine.transition_progress().is_none());
        let c = centre(&end);
        assert!(
            c[0] <= 4 && c[1] <= 4 && c[2] >= 250,
            "old/new shaders {ow}{oc}/{nw}{nc}: end {c:?}"
        );
        n_runs += 1;
        drop((r, r2));
    }
    assert_eq!(n_runs, 16);
    assert!(
        pollster::block_on(scope.pop()).is_none(),
        "validation errors"
    );
}

/// A v2.01 preset whose warp / composite shader text (if any) is `ret = <expr>;`-style body code.
fn shader_preset(border: [f32; 4], warp: Option<&str>, comp: Option<&str>) -> String {
    let mut s = format!(
        "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP={}\nPSVERSION_COMP={}\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n\
         ob_size=1\nob_a={}\nob_r={}\nob_g={}\nob_b={}\n",
        if warp.is_some() { 2 } else { 0 },
        if comp.is_some() { 2 } else { 0 },
        border[3],
        border[0],
        border[1],
        border[2],
    );
    if let Some(w) = warp {
        s += &format!("warp_1=`shader_body\nwarp_2=`{{\nwarp_3=`  {w}\nwarp_4=`}}\n");
    }
    if let Some(c) = comp {
        s += &format!("comp_1=`shader_body\ncomp_2=`{{\ncomp_3=`  {c}\ncomp_4=`}}\n");
    }
    s
}

/// Counts of pixels that are (almost) `a` and (almost) `b`.
fn count_pure(img: &[u8], a: [u8; 3], b: [u8; 3]) -> (usize, usize) {
    let near =
        |p: &[u8], c: [u8; 3]| (0..3).all(|k| (i32::from(p[k]) - i32::from(c[k])).abs() <= 12);
    let (mut na, mut nb) = (0, 0);
    for p in img.chunks(4) {
        na += usize::from(near(p, a));
        nb += usize::from(near(p, b));
    }
    (na, nb)
}

/// A directional wipe cuts the picture into an old and a new region (per-vertex alpha, tile culling, alpha-blended
/// second pass), here between two *warp shaders* that paint pure red / pure blue. The uniform pattern gives the
/// same mix everywhere instead.
#[test]
fn wipe_pattern_splits_the_picture_between_two_warp_shaders() {
    let old = shader_preset([0.0; 4], Some("ret = float3(1, 0, 0);"), None);
    let new = shader_preset([0.0; 4], Some("ret = float3(0, 0, 1);"), None);
    let run = |pattern: u32| {
        let mut rig = Rig::new(1)?;
        rig.engine.set_md2_blend_pattern(Some(pattern));
        rig.engine
            .load_preset(old.as_bytes(), "old", Transition::Cut)
            .unwrap();
        rig.run_to(9);
        rig.engine
            .load_preset(new.as_bytes(), "new", Transition::Soft { seconds: 1.0 })
            .unwrap();
        Some((rig.run_to(10), rig.run_to(40), rig.run_to(69)))
    };
    let Some((first, mid, last)) = run(1) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let n = (W * H) as usize;
    let (red, blue) = ([255u8, 0, 0], [0u8, 0, 255]);
    let (r0, _) = count_pure(&first, red, blue);
    assert!(r0 * 10 > n * 9, "start: almost all old ({r0} of {n})");
    let (r, b) = count_pure(&mid, red, blue);
    assert!(
        r * 20 > n && b * 20 > n,
        "wipe front: old {r} and new {b} regions of {n}"
    );
    assert!(
        r + b > n / 4,
        "most of the picture is still pure old / pure new: {r} + {b} of {n}"
    );
    let (_, b1) = count_pure(&last, red, blue);
    assert!(b1 * 10 > n * 9, "end: almost all new ({b1} of {n})");
    // uniform: every pixel the same mix, no pure region mid-blend
    let (_, mid_uniform, _) = run(0).unwrap();
    let (r, b) = count_pure(&mid_uniform, red, blue);
    assert!(r + b < n / 20, "uniform blend has no pure region: {r} {b}");
}

/// The composite alpha comes from a lookup in the warp mesh's vertex alphas: a wipe between two *composite shaders*
/// (old red, new blue; the main image is white) and the "funky reversal" (old composite shader, new fixed function:
/// the old shader is drawn over the fixed-function quad with flipped alpha).
#[test]
#[allow(clippy::type_complexity)]
fn wipe_pattern_splits_the_composite_between_two_comp_shaders() {
    let white = [1.0f32, 1.0, 1.0, 1.0];
    let red_comp = Some("ret = tex2D(sampler_main, uv).xyz * float3(1, 0, 0);");
    let blue_comp = Some("ret = tex2D(sampler_main, uv).xyz * float3(0, 0, 1);");
    let cases: [(Option<&str>, Option<&str>, [u8; 3], [u8; 3]); 2] = [
        (red_comp, blue_comp, [255, 0, 0], [0, 0, 255]),
        // old comp shader (red) vs fixed function (white)
        (red_comp, None, [255, 0, 0], [255, 255, 255]),
    ];
    for (i, (old_comp, new_comp, old_c, new_c)) in cases.into_iter().enumerate() {
        let old = shader_preset(white, None, old_comp);
        let new = shader_preset(white, None, new_comp);
        let Some(mut rig) = Rig::new(2) else {
            eprintln!("SKIP: no wgpu adapter");
            return;
        };
        let scope = rig.device.push_error_scope(wgpu::ErrorFilter::Validation);
        rig.engine.set_md2_blend_pattern(Some(1));
        rig.engine
            .load_preset(old.as_bytes(), "old", Transition::Cut)
            .unwrap();
        rig.run_to(9);
        rig.engine
            .load_preset(new.as_bytes(), "new", Transition::Soft { seconds: 1.0 })
            .unwrap();
        let first = rig.run_to(10);
        let mid = rig.run_to(40);
        let last = rig.run_to(69);
        let n = (W * H) as usize;
        let (o0, _) = count_pure(&first, old_c, new_c);
        assert!(
            o0 * 10 > n * 9,
            "case {i}: start is the old composite ({o0} of {n})"
        );
        let (o, nw) = count_pure(&mid, old_c, new_c);
        assert!(
            o * 20 > n && nw * 20 > n,
            "case {i}: both regions mid-blend: {o} {nw} of {n}"
        );
        let (_, n1) = count_pure(&last, old_c, new_c);
        assert!(
            n1 * 10 > n * 9,
            "case {i}: end is the new composite ({n1} of {n})"
        );
        assert!(
            pollster::block_on(scope.pop()).is_none(),
            "case {i}: validation errors"
        );
    }
}

/// A main-wave mode change blends two waves (`its == 2`): it draws something in between and does not crash.
#[test]
fn wave_mode_change_blends_two_waves() {
    let wave = |mode: i32| {
        preset_full(
            "1",
            "1",
            &format!("nWaveMode={mode}\nwave_r=1\nwave_g=1\nwave_b=1\nfWaveScale=1\n"),
        )
    };
    let Some(mut rig) = Rig::new(1) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let scope = rig.device.push_error_scope(wgpu::ErrorFilter::Validation);
    // a loud input so that the waves have an amplitude
    let pcm: Vec<f32> = (0..4096 * 2)
        .map(|i| ((i / 2) as f32 * 0.1).sin() * 0.8)
        .collect();
    rig.engine.push_pcm_f32(&pcm, 2);
    rig.engine
        .load_preset(wave(0).as_bytes(), "circle", Transition::Cut)
        .unwrap();
    rig.run_to(9);
    rig.engine
        .load_preset(
            wave(2).as_bytes(),
            "spiro",
            Transition::Soft { seconds: 1.0 },
        )
        .unwrap();
    let mut lit = 0;
    for n in 10..=69 {
        rig.engine.push_pcm_f32(&pcm, 2);
        let img = rig.step();
        if n % 10 == 0 {
            lit += img
                .chunks(4)
                .filter(|p| p[0] > 30 || p[1] > 30 || p[2] > 30)
                .count();
        }
    }
    assert!(lit > 0, "the blended wave is drawn");
    assert!(
        pollster::block_on(scope.pop()).is_none(),
        "validation errors"
    );
}

/// Corpus presets (v1 and v2 with warp / comp shaders, per-vertex code, custom waves / shapes) blend without
/// validation errors; skipped without the projectM preset pack.
#[test]
fn corpus_presets_blend_without_validation_errors() {
    use std::path::Path;
    let corpus = Path::new("/usr/share/projectM/presets");
    let find = |name: &str| -> Option<Vec<u8>> {
        fn walk(dir: &Path, name: &str, depth: u32) -> Option<std::path::PathBuf> {
            for e in std::fs::read_dir(dir).ok()?.flatten() {
                let p = e.path();
                if p.is_dir() && depth < 3 {
                    if let Some(f) = walk(&p, name, depth + 1) {
                        return Some(f);
                    }
                } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
                    return Some(p);
                }
            }
            None
        }
        std::fs::read(walk(corpus, name, 0)?).ok()
    };
    let names = [
        "Aderrasi - Antidote.milk",
        "Geiss - Skin Dots 3.milk",
        "EoS + Phat - CAT Scan.milk",
        "Aderrasi - Songflower (Hybrid Plant).milk",
        "Geiss - Diffuser (Gold Mix).milk",
        "ORB - Acid Sunrise.milk",
    ];
    let presets: Vec<(&str, Vec<u8>)> = names
        .iter()
        .filter_map(|n| find(n).map(|b| (*n, b)))
        .collect();
    if presets.len() < 2 {
        eprintln!("SKIP: no preset corpus");
        return;
    }
    let Some(mut rig) = Rig::new(4) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let scope = rig.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pcm: Vec<f32> = (0..2048 * 2)
        .map(|i| ((i / 2) as f32 * 0.05).sin() * 0.5)
        .collect();
    for (i, (name, bytes)) in presets.iter().enumerate() {
        let t = if i == 0 {
            Transition::Cut
        } else {
            Transition::Soft { seconds: 0.4 }
        };
        if let Err(e) = rig.engine.load_preset(bytes, name, t) {
            eprintln!("skipping {name}: {e}");
            continue;
        }
        for _ in 0..30 {
            rig.engine.push_pcm_f32(&pcm, 2);
            let img = rig.step();
            assert!(img.len() == (W * H * 4) as usize);
        }
    }
    assert!(
        pollster::block_on(scope.pop()).is_none(),
        "validation errors"
    );
}
