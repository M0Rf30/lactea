//! MilkDrop 3 core render deltas through the engine (`docs/md3.md`, "Main waveform and render deltas"): the 17 main
//! waveform modes, the 500-side shapes, the border gate of preset blends. Skipped (with a note) when there is no GPU
//! adapter.

use lactea::{Compat, Engine, EngineConfig, FrameClock, Transition};
use lactea_testutil::{gpu, read_rgba8};

const W: u32 = 128;
const H: u32 = 96;

static GPU: std::sync::LazyLock<Option<(wgpu::Device, wgpu::Queue)>> =
    std::sync::LazyLock::new(gpu);

/// A v1 preset: no shaders, no decay, gamma 1, so a finished frame shows the main image unscaled.
fn preset(wave_alpha: &str, extra: &str) -> String {
    format!("MILKDROP_PRESET_VERSION=100\nfDecay=1\nfWaveAlpha={wave_alpha}\nfGammaAdj=1\n{extra}")
}

fn wave_preset(mode: i32, extra: &str) -> String {
    preset(
        "1",
        &format!("nWaveMode={mode}\nwave_r=1\nwave_g=1\nwave_b=1\nfWaveScale=1\n{extra}"),
    )
}

/// A static full-frame colour: the outer border (size 1) covers everything.
fn solid(r: f32, g: f32, b: f32) -> String {
    preset(
        "0",
        &format!("ob_size=1\nob_a=1\nob_r={r}\nob_g={g}\nob_b={b}\n"),
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
    fn new(compat: Compat, disable_box: bool) -> Option<Rig> {
        let (device, queue) = GPU.clone()?;
        let engine = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: W,
                height: H,
                mesh: [16, 12],
                compat,
                start_clean: true,
                md3_disable_box: disable_box,
                ..Default::default()
            },
        )
        .unwrap();
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

    /// Render the next frame (60 fps clock) after feeding one block of audio, return the target pixels.
    fn step(&mut self, pcm: &[f32]) -> Vec<u8> {
        if !pcm.is_empty() {
            self.engine.push_pcm_f32(pcm, 2);
        }
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

    fn run_to(&mut self, frame: u32, pcm: &[f32]) -> Vec<u8> {
        let mut last = Vec::new();
        while self.frame <= frame {
            last = self.step(pcm);
        }
        last
    }

    fn read(&self) -> Vec<u8> {
        read_rgba8(&self.device, &self.queue, &self.target, W, H)
    }
}

/// Loud stereo input with different content per channel.
fn pcm() -> Vec<f32> {
    (0..2048 * 2)
        .map(|i| {
            let s = (i / 2) as f32;
            if i % 2 == 0 {
                (s * 0.1).sin() * 0.8
            } else {
                (s * 0.037).sin() * 0.6
            }
        })
        .collect()
}

fn lit(img: &[u8]) -> usize {
    img.chunks(4)
        .filter(|p| p[0] > 30 || p[1] > 30 || p[2] > 30)
        .count()
}

fn render_wave(compat: Compat, mode: i32, extra: &str) -> Option<Vec<u8>> {
    let mut rig = Rig::new(compat, true)?;
    rig.engine
        .load_preset(wave_preset(mode, extra).as_bytes(), "wave", Transition::Cut)
        .unwrap();
    let pcm = pcm();
    Some(rig.run_to(12, &pcm))
}

/// The modes MD2 does not have (9..=16) draw under MD3 and nothing under MD2.
#[test]
fn modes_9_to_16_draw_under_md3_only() {
    for mode in [9, 10, 11, 12, 13, 14, 15, 16] {
        let Some(md3) = render_wave(Compat::MilkDrop3, mode, "") else {
            eprintln!("SKIP: no wgpu adapter");
            return;
        };
        let md2 = render_wave(Compat::MilkDrop2, mode, "").unwrap();
        assert!(lit(&md3) > 0, "MD3 mode {mode} draws a wave");
        assert_eq!(lit(&md2), 0, "MD2 draws nothing for mode {mode}");
    }
}

/// `nWaveMode = 17` is mode 0 under MD3 (`% 17`), the same pixels as 0; and `wave_mode` from the per-frame code goes
/// through the same wrap.
#[test]
fn mode_17_is_the_circle_and_negative_modes_are_nothing() {
    let Some(zero) = render_wave(Compat::MilkDrop3, 0, "") else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    assert!(lit(&zero) > 0);
    let seventeen = render_wave(Compat::MilkDrop3, 17, "").unwrap();
    assert_eq!(zero, seventeen, "17 % 17 = 0");
    let by_code = render_wave(Compat::MilkDrop3, 3, "per_frame_1=wave_mode=34;\n").unwrap();
    assert_eq!(zero, by_code, "wave_mode = 34 -> 0");
    // a negative remainder is not a mode: 480 zero vertices, invisible as lines
    let neg = render_wave(Compat::MilkDrop3, 3, "per_frame_1=wave_mode=-1;\n").unwrap();
    assert_eq!(lit(&neg), 0, "wave_mode -1 draws nothing visible");
    // ... but dots land at the screen centre
    let dots = render_wave(
        Compat::MilkDrop3,
        3,
        "bWaveDots=1\nper_frame_1=wave_mode=-1;\n",
    )
    .unwrap();
    let c = ((H / 2) * W + W / 2) as usize * 4;
    let near_centre = (-2i32..=2).any(|dy| {
        (-2i32..=2).any(|dx| {
            let i = ((H as i32 / 2 + dy) * W as i32 + W as i32 / 2 + dx) as usize * 4;
            dots[i] > 30
        })
    });
    assert!(near_centre, "zero-vertex dots at the centre ({})", dots[c]);
}

/// Shape `sides` = 500 renders (buffers hold the 502-vertex fan) and is a filled disc; under MD2 the same shape stops
/// at 100 sides (also a disc, so only the absence of errors is common).
#[test]
fn shape_with_500_sides_renders_without_errors() {
    let shape = "shapecode_0_enabled=1\nshapecode_0_sides=500\nshapecode_0_rad=0.4\nshapecode_0_x=0.5\nshapecode_0_y=0.5\n\
                 shapecode_0_r=1\nshapecode_0_g=1\nshapecode_0_b=1\nshapecode_0_a=1\nshapecode_0_r2=1\nshapecode_0_g2=1\nshapecode_0_b2=1\nshapecode_0_a2=1\nshapecode_0_border_a=0\n";
    let Some(mut rig) = Rig::new(Compat::MilkDrop3, true) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let scope = rig.device.push_error_scope(wgpu::ErrorFilter::Validation);
    rig.engine
        .load_preset(preset("0", shape).as_bytes(), "shape", Transition::Cut)
        .unwrap();
    let img = rig.run_to(3, &[]);
    // disc of radius 0.4 of the half-height, x scaled by the aspect: about pi * r^2 of the pixels in y units
    let r_px = 0.4 * (H as f32 / 2.0);
    let expect = std::f32::consts::PI * r_px * r_px;
    let n = lit(&img) as f32;
    assert!(
        (n - expect).abs() < expect * 0.15,
        "disc area {n}, expected about {expect}"
    );
    assert!(
        pollster::block_on(scope.pop()).is_none(),
        "validation errors"
    );
}

/// `DisableBox` (MD3 default on): no outer / inner border while a preset blend runs; afterwards they are back.
/// MilkDrop 2 and MD3 with `DisableBox` off draw them during the blend.
#[test]
fn borders_are_hidden_during_a_blend_with_disable_box() {
    let blend = |compat: Compat, disable_box: bool| -> Option<([i32; 3], [i32; 3])> {
        let mut rig = Rig::new(compat, disable_box)?;
        rig.engine
            .load_preset(solid(1.0, 0.0, 0.0).as_bytes(), "red", Transition::Cut)
            .unwrap();
        rig.run_to(9, &[]);
        rig.engine
            .load_preset(
                solid(0.0, 0.0, 1.0).as_bytes(),
                "blue",
                Transition::Soft { seconds: 1.0 },
            )
            .unwrap();
        let centre = |img: &[u8]| {
            let i = (((H / 2) * W + W / 2) * 4) as usize;
            [
                i32::from(img[i]),
                i32::from(img[i + 1]),
                i32::from(img[i + 2]),
            ]
        };
        let mid = centre(&rig.run_to(40, &[]));
        let end = centre(&rig.run_to(100, &[]));
        Some((mid, end))
    };
    let Some((mid, end)) = blend(Compat::MilkDrop3, true) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    assert!(
        mid[0] > 240 && mid[2] < 15,
        "MD3 + DisableBox: still the old image mid-blend: {mid:?}"
    );
    assert!(
        end[2] > 240 && end[0] < 15,
        "borders are back after the blend: {end:?}"
    );

    let (mid_off, _) = blend(Compat::MilkDrop3, false).unwrap();
    assert!(
        mid_off[2] > 15,
        "DisableBox off: the new border shows through the blend: {mid_off:?}"
    );
    let (mid_md2, _) = blend(Compat::MilkDrop2, true).unwrap();
    assert!(
        mid_md2[2] > 15,
        "MilkDrop 2 draws borders during blends: {mid_md2:?}"
    );
}
