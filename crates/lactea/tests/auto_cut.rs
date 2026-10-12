//! MD3 `AutoHardCutMode` through the engine: cuts reach the host under `Compat::MilkDrop3` only, and mode 7's
//! composite-shader effects change the picture the way their HLSL says. Skipped (with a note) without a GPU adapter.

use lactea::auto_cut::{Effect, MODE7_EFFECTS};
use lactea::{Compat, Engine, EngineConfig, FrameClock, PresetControl, Transition};

const W: u32 = 64;
const H: u32 = 36;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    engine: Engine,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    frame: u32,
    dt: f64,
}

impl Rig {
    fn new(compat: Compat, mode: u8, dt: f64) -> Option<Self> {
        let (device, queue) = gpu()?;
        let engine = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: W,
                height: H,
                mesh: [16, 12],
                compat,
                auto_hard_cut_mode: mode,
                ..EngineConfig::default()
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
        Some(Self {
            device,
            queue,
            engine,
            target,
            view,
            frame: 0,
            dt,
        })
    }

    fn load(&mut self, text: &str) {
        self.engine
            .load_preset(text.as_bytes(), "t", Transition::Cut)
            .unwrap();
    }

    /// Render the next frame at `frame * dt`; returns the cuts the engine asked for.
    fn step(&mut self) -> u32 {
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.engine.render(
            &mut enc,
            &self.view,
            FrameClock {
                time: f64::from(self.frame) * self.dt,
                frame: self.frame,
            },
        );
        self.queue.submit(Some(enc.finish()));
        self.frame += 1;
        self.engine.take_auto_cuts()
    }

    fn pixels(&self) -> Vec<u8> {
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
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        rx.recv().unwrap().unwrap();
        let data = slice.get_mapped_range().unwrap();
        let mut out = Vec::new();
        for y in 0..H {
            let s = (y * bpr) as usize;
            out.extend_from_slice(&data[s..s + (W * 4) as usize]);
        }
        out
    }
}

/// A v1 preset whose per-frame code pins the levels the auto hard cut reads.
const LOUD: &str = "MILKDROP_PRESET_VERSION=100\nper_frame_1=bass=2;treb=3.5;\n";

/// A comp shader that paints `(uv.x, uv.y, 0.25)`: every effect is a pointwise function of the base picture.
const UV_COMP: &str = "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=2\nPSVERSION_COMP=2\n\
    [preset00]\nfDecay=0.5\n\
    comp_1=`shader_body\ncomp_2=`{\ncomp_3=`  ret = float3(uv.x, uv.y, 0.25);\ncomp_4=`}\n";

fn run_cuts(compat: Compat, mode: u8) -> Option<Vec<u32>> {
    let mut rig = Rig::new(compat, mode, 0.07)?;
    rig.load(LOUD);
    Some((0..20).map(|_| rig.step()).collect())
}

#[test]
fn mode_1_cuts_under_milkdrop_3_with_the_minimum_delay() {
    let Some(cuts) = run_cuts(Compat::MilkDrop3, 1) else {
        return eprintln!("skipping: no wgpu adapter");
    };
    // frame 0 sees no levels yet; from frame 1 bass = 2; t = 0.07 * frame, cut once t >= 0.2 s
    let frames: Vec<usize> = (0..cuts.len()).filter(|&f| cuts[f] > 0).collect();
    assert_eq!(frames, [3, 6, 9, 12, 15, 18]);
    assert!(cuts.iter().all(|&c| c <= 1));
}

#[test]
fn nothing_cuts_under_projectm_and_milkdrop_2_or_with_the_mode_off() {
    for (compat, mode) in [
        (Compat::ProjectM, 1),
        (Compat::MilkDrop2, 1),
        (Compat::ProjectM, 6),
        (Compat::MilkDrop3, 0),
    ] {
        let Some(cuts) = run_cuts(compat, mode) else {
            return eprintln!("skipping: no wgpu adapter");
        };
        assert!(cuts.iter().all(|&c| c == 0), "{compat:?} mode {mode}");
    }
}

#[test]
fn the_runtime_setter_switches_the_mode_and_is_gated_by_compat() {
    let Some(mut rig) = Rig::new(Compat::MilkDrop3, 0, 0.07) else {
        return eprintln!("skipping: no wgpu adapter");
    };
    rig.load(LOUD);
    assert!(!rig.engine.auto_hard_cut_active());
    assert!((0..10).all(|_| rig.step() == 0));
    rig.engine.set_auto_hard_cut_mode(6);
    assert_eq!(rig.engine.auto_hard_cut_mode(), 6);
    assert!(rig.engine.auto_hard_cut_active());
    // mode 6: no delay, every frame with bass > 1.5
    assert!((0..5).all(|_| rig.step() == 1));
    rig.engine.set_auto_hard_cut_mode(99);
    assert_eq!(rig.engine.auto_hard_cut_mode(), 7);

    // the setting is stored but inert under projectM
    let Some(mut pm) = Rig::new(Compat::ProjectM, 0, 0.07) else {
        return;
    };
    pm.engine.set_auto_hard_cut_mode(6);
    assert_eq!(pm.engine.auto_hard_cut_mode(), 6);
    assert!(!pm.engine.auto_hard_cut_active());
}

#[test]
fn an_auto_cut_becomes_a_hard_cut_switch_request() {
    let Some(mut rig) = Rig::new(Compat::MilkDrop3, 6, 0.07) else {
        return eprintln!("skipping: no wgpu adapter");
    };
    rig.load(LOUD);
    let mut control = PresetControl::new(1);
    control.set_preset_duration(1e6);
    control.start_preset(0.0);
    let mut requests = Vec::new();
    for f in 0..4u32 {
        let cuts = rig.step();
        let t = f64::from(f) * 0.07;
        if let Some(r) = control.update_with_auto_cuts(t, 0.0, cuts) {
            requests.push((f, r.hard_cut));
            // the host loads the next preset with a cut
            rig.load(LOUD);
            control.start_preset(t);
        }
    }
    // frame 0 has no levels; frame 1 cuts; the reloaded preset has run no per-frame code yet, so frame 2 sees
    // no levels; frame 3 cuts again
    assert_eq!(requests, [(1, true), (3, true)]);
}

#[test]
fn every_mode_7_effect_changes_the_picture_as_its_hlsl_says() {
    let Some(mut rig) = Rig::new(Compat::MilkDrop3, 0, 1.0 / 60.0) else {
        return eprintln!("skipping: no wgpu adapter");
    };
    rig.load(UV_COMP);
    rig.step();
    let base = rig.pixels();
    // the shader really paints (uv.x, uv.y, 0.25): a ramp in both directions
    let px = |v: &[u8], i: usize| [v[i * 4], v[i * 4 + 1], v[i * 4 + 2]];
    assert!(px(&base, 5)[0] < px(&base, 60)[0]);
    assert!(px(&base, 0)[1] < px(&base, (W * (H - 1)) as usize)[1]);
    assert!((i32::from(px(&base, 100)[2]) - 64).abs() <= 2);

    let tol = 3.0;
    for effect in MODE7_EFFECTS {
        assert!(rig.engine.inject_comp_effect(effect), "{effect:?}");
        rig.step();
        let out = rig.pixels();
        for i in 0..(W * H) as usize {
            let [r, g, b] = px(&base, i).map(|v| f32::from(v) / 255.0);
            let [or, og, ob] = px(&out, i).map(|v| f32::from(v) / 255.0);
            // expected output of the shader, written from the effect's HLSL
            let (er, eg, eb) = match effect {
                Effect::Solarize => (
                    r * (1.0 - r) * 4.0,
                    g * (1.0 - g) * 4.0,
                    b * (1.0 - b) * 4.0,
                ),
                Effect::Invert => (1.0 - r, 1.0 - g, 1.0 - b),
                Effect::Flip => (1.0 - r, 1.0 - g, b),
                Effect::Rotate => (g, r, b),
                Effect::Split => ((r - 0.5).abs(), g, b),
                Effect::Multiplicate => {
                    let m = |u: f32| 1.0 - (((u - 0.5) * 0.75).rem_euclid(1.0) * 2.0 - 1.0).abs();
                    (m(r), m(g), b)
                }
            };
            for (a, e, what) in [(or, er, "r"), (og, eg, "g"), (ob, eb, "b")] {
                assert!(
                    (a * 255.0 - e * 255.0).abs() <= tol,
                    "{effect:?} px {i} {what}: got {} want {}",
                    a * 255.0,
                    e * 255.0
                );
            }
        }
        // a second injection removes it again
        assert!(rig.engine.inject_comp_effect(effect));
        rig.step();
        let back = rig.pixels();
        assert!(
            base.iter().zip(&back).all(|(a, b)| a.abs_diff(*b) <= 1),
            "{effect:?} did not toggle off"
        );
    }

    // effects accumulate: two at once apply in source order (coordinate effects first, colour last)
    assert!(rig.engine.inject_comp_effect(Effect::Rotate));
    assert!(rig.engine.inject_comp_effect(Effect::Invert));
    rig.step();
    let both = rig.pixels();
    for i in 0..(W * H) as usize {
        let [r, g, _] = px(&base, i);
        let [or, og, _] = px(&both, i);
        assert!(i32::from(or).abs_diff(255 - i32::from(g)) <= 3, "px {i}");
        assert!(i32::from(og).abs_diff(255 - i32::from(r)) <= 3, "px {i}");
    }
}

#[test]
fn mode_7_injects_an_effect_into_the_running_preset_without_a_cut() {
    let preset = format!("{UV_COMP}per_frame_1=treb=3.5;\n");
    let Some(mut rig) = Rig::new(Compat::MilkDrop3, 7, 0.2) else {
        return eprintln!("skipping: no wgpu adapter");
    };
    rig.load(&preset);
    // frame 0: no levels; frame 1: treb = 3.5 but t = 0.2 < 0.5; frame 3 (t = 0.6): first injection
    let mut frames = Vec::new();
    let mut pics = Vec::new();
    for f in 0..4 {
        assert_eq!(rig.step(), 0, "mode 7 never cuts");
        pics.push(rig.pixels());
        frames.push(f);
    }
    assert_eq!(pics[0], pics[1]);
    assert_eq!(pics[1], pics[2]);
    assert_ne!(pics[2], pics[3], "an effect was injected at frame 3");
}

#[test]
fn a_preset_without_a_composite_shader_gets_the_default_one_before_injection() {
    // v100 preset: no shaders at all, black picture (no wave, no echo, no gamma)
    let text = "MILKDROP_PRESET_VERSION=100\nfGammaAdj=1\nfWaveAlpha=0\nfDecay=1\nfVideoEchoAlpha=0\nfShader=0\nbMotionVectorsOn=0\n";
    let Some(mut rig) = Rig::new(Compat::MilkDrop3, 0, 1.0 / 60.0) else {
        return eprintln!("skipping: no wgpu adapter");
    };
    rig.load(text);
    rig.step();
    let base = rig.pixels();
    assert!(base.chunks(4).all(|p| p[0] < 8 && p[1] < 8 && p[2] < 8));
    assert!(rig.engine.inject_comp_effect(Effect::Invert));
    rig.step();
    let out = rig.pixels();
    assert!(
        out.chunks(4)
            .all(|p| p[0] > 247 && p[1] > 247 && p[2] > 247),
        "inverted black is white"
    );
}

#[test]
fn injection_is_a_milkdrop_2_family_feature() {
    let Some(mut rig) = Rig::new(Compat::ProjectM, 0, 1.0 / 60.0) else {
        return eprintln!("skipping: no wgpu adapter");
    };
    rig.load(UV_COMP);
    assert!(!rig.engine.inject_comp_effect(Effect::Invert));
}

struct CountingProvider(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl lactea::TextureProvider for CountingProvider {
    fn load(&mut self, name: &str) -> Option<lactea::Image> {
        assert_eq!(name, "tex");
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(lactea::Image {
            width: 2,
            height: 2,
            rgba8: vec![255; 16],
        })
    }

    fn random_pool(&self) -> Vec<String> {
        Vec::new()
    }
}

/// An effect injection runs inside `render`: it must not ask the host for the preset's textures again.
#[test]
fn injecting_an_effect_does_not_reload_the_presets_textures() {
    let preset = "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=2\nPSVERSION_COMP=2\n\
        [preset00]\nfDecay=0.5\ncomp_1=`shader_body\ncomp_2=`{\n\
        comp_3=`  ret = tex2D(sampler_tex, uv).rgb;\ncomp_4=`}\n";
    let Some(mut rig) = Rig::new(Compat::MilkDrop3, 0, 1.0 / 60.0) else {
        return eprintln!("skipping: no wgpu adapter");
    };
    let loads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    rig.engine
        .set_texture_provider(Box::new(CountingProvider(loads.clone())));
    rig.load(preset);
    let at_load = loads.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(at_load, 1, "the preset's own load asks once");
    assert!(rig.engine.inject_comp_effect(Effect::Invert));
    rig.step();
    assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), at_load);
}
