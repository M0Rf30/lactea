// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Headless demo: hand-built `FrameInput` sequence (zoom + rotation + decay, main wave modes 0..7
//! from a synthetic sine, one shape, one custom wave) rendered to PNGs in `out/render-example/`.
//!
//! `cargo run -p lactea-render --example headless`

// the synthetic waveform uses a short 2*pi literal and the frame is filled in field by field
#![allow(clippy::approx_constant, clippy::field_reassign_with_default)]
use lactea_abi::dump::Stage;
use lactea_render::warp_mesh::{self, VertexVars, WarpAnim};
use lactea_render::{CustomWave, FrameInput, RenderConfig, Renderer, ShapeInstance, WavePoint};

const W: u32 = 640;
const H: u32 = 360;
const FPS: f32 = 60.0;
const FRAMES_PER_MODE: u32 = 90;

fn main() {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("no wgpu adapter");
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device");
    println!("adapter: {}", adapter.get_info().name);

    let cfg = RenderConfig {
        width: W,
        height: H,
        ..RenderConfig::default()
    };
    let mut r = Renderer::new(&device, &queue, cfg.clone()).unwrap();
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
        format: cfg.output_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());

    let out_dir = std::path::Path::new("out/render-example");
    std::fs::create_dir_all(out_dir).unwrap();
    let geom = r.mesh_geometry().clone();

    let mut frame = 0u32;
    for mode in 0..8 {
        for k in 0..FRAMES_PER_MODE {
            let t = frame as f32 / FPS;
            let mut f = FrameInput::default();
            f.frame = frame;
            f.time = frame as f64 / FPS as f64;
            f.fps = FPS;

            // motion: slow zoom + rotation + a little warp, decay .96
            let anim = WarpAnim::new(t, 1.0, 1.0);
            let v = VertexVars {
                zoom: 1.02,
                rot: 0.01 * (t * 0.7).sin(),
                warp: 0.5,
                ..Default::default()
            };
            f.warp_uv = Some(warp_mesh::warp_mesh_uniform(&geom, &v, &anim, [0.0, 0.0]));
            f.post.decay = 0.96;
            f.post.gamma = 2.0;
            f.post.echo_alpha = 0.25;
            f.post.echo_zoom = 1.1;
            f.post.echo_orient = 1;
            f.mv.rgba[3] = 0.0;
            f.borders.ob_rgba = [0.0, 0.0, 0.0, 0.0];
            f.borders.ib_rgba = [0.3, 0.3, 0.8, 0.5];
            f.bass = 1.0;
            f.mid = 1.0;
            f.treb = 1.0;
            f.imm_abs = [1.0, 1.0, 1.0];

            // synthetic stereo sine, MD2 units (about +-128 full scale)
            f.main_wave.mode = mode;
            f.main_wave.rgba = [0.3 + 0.7 * (t * 0.5).sin().abs(), 0.8, 1.0, 0.9];
            f.main_wave.additive = mode % 2 == 1;
            f.main_wave.scale = 1.0;
            for i in 0..576 {
                let ph = i as f32 / 576.0;
                let a =
                    70.0 * (ph * 6.2832 * 3.0 + t * 4.0).sin() + 25.0 * (ph * 6.2832 * 11.0).sin();
                let b =
                    70.0 * (ph * 6.2832 * 4.0 - t * 3.0).cos() + 20.0 * (ph * 6.2832 * 7.0).sin();
                f.main_wave.wave[0][i] = a;
                f.main_wave.wave[1][i] = b;
            }
            for i in 0..512 {
                let s = (-(i as f32) / 80.0).exp() * 40.0 + 1.0;
                f.main_wave.spectrum[0][i] = s;
                f.main_wave.spectrum[1][i] = s;
            }

            // one shape
            f.shapes.push(ShapeInstance {
                sides: 6,
                x: f64::from(0.5 + 0.25 * (t * 0.9).cos()),
                y: f64::from(0.5 + 0.25 * (t * 0.9).sin()),
                rad: 0.12,
                ang: t,
                rgba: [1.0, 0.4, 0.1, 0.7],
                rgba2: [0.1, 0.1, 1.0, 0.2],
                border_rgba: [1.0, 1.0, 1.0, 0.8],
                thick: true,
                ..Default::default()
            });

            // one custom wave (a sine across the screen)
            f.waves.push(CustomWave {
                points: (0..128)
                    .map(|i| {
                        let u = i as f32 / 127.0;
                        WavePoint {
                            x: f64::from(u),
                            y: f64::from(0.25 + 0.1 * (u * 18.0 + t * 5.0).sin()),
                            rgba: [0.2, 1.0, 0.3, 0.9],
                        }
                    })
                    .collect(),
                thick: true,
                ..Default::default()
            });

            let mut enc = device.create_command_encoder(&Default::default());
            r.render_frame(&mut enc, &f, &view).unwrap();
            queue.submit(Some(enc.finish()));
            frame += 1;

            if k == FRAMES_PER_MODE - 1 {
                for (stage, name) in [(Stage::Final, "final"), (Stage::Main, "main")] {
                    let (w, h, data) = r.read_stage(stage).unwrap();
                    let path = out_dir.join(format!("wave_mode_{mode}_{name}.png"));
                    image::save_buffer(&path, &data, w, h, image::ColorType::Rgba8).unwrap();
                    println!("wrote {}", path.display());
                }
            }
        }
    }
}
