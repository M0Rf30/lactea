//! Encrypted-shader override (`docs/md3.md`, "Encrypted shaders"): a user HLSL registered for an `MD31` handle replaces the
//! preset's stub warp shader. Skipped (with a note) when there is no GPU adapter.

use lactea::{Compat, Engine, EngineConfig, FrameClock, ShaderOverride, Transition};
use lactea_testutil::{gpu_or_skip, read_rgba8};

const W: u32 = 64;
const H: u32 = 48;
const HANDLE: u64 = 0x1234_5678_9abc_def0;

/// A preset with encrypted shaders: `MD31` header, a stub warp shader painting solid red.
fn encrypted_preset() -> String {
    format!(
        "MD31={HANDLE}\nMILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=5\nPSVERSION_COMP=0\n[preset00]\n\
         fDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n\
         warp_1=`shader_body\nwarp_2=`{{\nwarp_3=`  // This shader works only with MD3 :)\nwarp_4=`  ret = float3(1, 0, 0);\nwarp_5=`}}\n"
    )
}

fn render_centre(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    engine: &mut Engine,
    view: &wgpu::TextureView,
    target: &wgpu::Texture,
) -> [u8; 4] {
    for frame in 0..3 {
        let mut enc = device.create_command_encoder(&Default::default());
        engine.render(
            &mut enc,
            view,
            FrameClock {
                time: f64::from(frame) / 60.0,
                frame,
            },
        );
        queue.submit(Some(enc.finish()));
    }
    let data = read_rgba8(device, queue, target, W, H);
    let o = (((H / 2) * W + (W / 2)) * 4) as usize;
    [data[o], data[o + 1], data[o + 2], data[o + 3]]
}

#[test]
fn override_replaces_encrypted_stub() {
    let (device, queue) = gpu_or_skip!();
    let mut engine = Engine::new(
        &device,
        &queue,
        EngineConfig {
            width: W,
            height: H,
            mesh: [16, 12],
            compat: Compat::MilkDrop3,
            start_clean: true,
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
    let text = encrypted_preset();

    engine
        .load_preset(text.as_bytes(), "locked", Transition::Cut)
        .unwrap();
    let stub = render_centre(&device, &queue, &mut engine, &view, &target);
    assert!(
        stub[0] > 200 && stub[2] < 50,
        "stub should paint red: {stub:?}"
    );

    engine.set_shader_override(
        HANDLE,
        ShaderOverride {
            warp: Some("shader_body\n{\n  ret = float3(0, 0, 1);\n}".into()),
            ..Default::default()
        },
    );
    engine
        .load_preset(text.as_bytes(), "locked", Transition::Cut)
        .unwrap();
    let over = render_centre(&device, &queue, &mut engine, &view, &target);
    assert!(
        over[2] > 200 && over[0] < 50,
        "override should paint blue: {over:?}"
    );
    assert_ne!(stub, over);
}
