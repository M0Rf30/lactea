//! `Compat::MilkDrop3` expression semantics against pixels measured on the real MilkDrop 3.37.
//!
//! Each fixture under `fixtures/md3/eel/` evaluates a handful of expressions per frame and writes
//! the results into the outer / inner border colours (`ob_*`, `ib_*`, 10 % wide, `decay = 0`, so the
//! frame is the two solid bands on black; the default `gamma = 2` doubles every channel and clamps
//! at 255, which is why the probes keep their values in 0..0.5). The expected bytes are the
//! screenshots of the MD3 3.37 build run under Wine (`md3shot.sh`, 800x600) sampled in the middle
//! of the outer (x = 20) and inner (x = 60) band; a fixture whose per-frame part does not compile
//! in MD3 shows no border at all (black) there. Skipped (with a note) when there is no GPU adapter.

use lactea::{Compat, Engine, EngineConfig, FrameClock, Transition};
use std::path::Path;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let bpr = (w * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(bpr * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let data = slice.get_mapped_range().unwrap();
    let mut out = Vec::new();
    for y in 0..h {
        let s = (y * bpr) as usize;
        out.extend_from_slice(&data[s..s + (w * 4) as usize]);
    }
    out
}

/// (fixture, outer band RGB, inner band RGB) as shot by MD3 3.37.
const MD3_SHOTS: [(&str, [u8; 3], [u8; 3]); 14] = [
    // constants 0.2 / 0.4 / 0.6 (x2 by gamma, clamped) and the colour of `0.5 + 0.1 * v`
    ("colors", [102, 204, 255], [204, 255, 254]),
    // round-to-nearest-even: `3.7|0`, `10%3.5`, `loop(3.5)`, `megabuf(1.6)`, `megabuf(-1)`, `gmegabuf(-1)`
    ("rounding", [255, 255, 255], [255, 255, 255]),
    // ties go to even: `2.5|0 = 2`, `3.5|0 = 4`, `loop(2.5)` runs 2 times, `0.5|0 = 0`, `10%2.5 = 0`, `loop(0.5)` runs 0 times
    ("rounding_ties", [255, 255, 255], [255, 255, 255]),
    // `-7%3` is -1 (sign after `%`), `a%3` of a negative a is +1, `-2^2`, `2*3^2`, `2^3^2`
    ("mod_pow_sign", [122, 132, 132], [106, 218, 158]),
    ("prec_mod_pow", [146, 158, 158], [188, 192, 152]),
    ("prec_bitwise", [172, 126, 158], [142, 126, 132]),
    // `.5`, `$PI`, `$E`, `int()`, `$X..`, `$'A'`
    ("literals", [152, 142, 140], [122, 142, 160]),
    // `5.`, `c ? x` without else, sign, min/max, sqr/sqrt
    ("ternary_funcs", [158, 162, 126], [126, 162, 192]),
    ("mod_zero_loops", [152, 132, 136], [180, 180, 136]),
    ("truthiness", [228, 152, 132], [132, 132, 126]),
    ("prec_unary", [132, 126, 152], [106, 122, 44]),
    // `mouse.x` does not compile in MD3: no border at all
    ("mouse_dot_x", [0, 0, 0], [0, 0, 0]),
    ("underscore_name", [142, 132, 136], [142, 146, 152]),
    ("mod_negative", [204, 255, 254], [255, 254, 255]),
];

#[test]
fn milkdrop3_expressions_match_the_md3_screenshots() {
    let Some((device, queue)) = gpu() else {
        eprintln!("skipping: no wgpu adapter");
        return;
    };
    let (w, h) = (400u32, 300u32);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
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
    });
    let view = target.create_view(&Default::default());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/md3/eel");
    let mut bad = Vec::new();
    for (name, ob, ib) in MD3_SHOTS {
        let bytes = std::fs::read(dir.join(format!("{name}.milk"))).unwrap();
        let mut engine = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: w,
                height: h,
                compat: Compat::MilkDrop3,
                ..EngineConfig::default()
            },
        );
        engine
            .load_preset(&bytes, name, Transition::Cut)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        for frame in 0..10u32 {
            let mut enc = device.create_command_encoder(&Default::default());
            engine.render(
                &mut enc,
                &view,
                FrameClock {
                    time: f64::from(frame) / 60.0,
                    frame,
                },
            );
            queue.submit(Some(enc.finish()));
        }
        let px = read_back(&device, &queue, &target, w, h);
        let at = |x: u32| {
            let i = ((h / 2 * w + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2]]
        };
        let (got_ob, got_ib) = (at(w / 40), at(w * 3 / 40));
        if (got_ob, got_ib) != (ob, ib) {
            bad.push(format!(
                "{name}: got {got_ob:?} {got_ib:?}, MD3 {ob:?} {ib:?}"
            ));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}
