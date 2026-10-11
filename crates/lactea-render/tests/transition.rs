// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Headless GPU tests of the projectM-style transition blender. They skip (with a message) when no
//! adapter is available, or fail if `LACTEA_REQUIRE_GPU` is set.

#![allow(clippy::manual_div_ceil)]
use lactea_render::{TransitionBlender, TransitionKind, TransitionParams};
use wgpu::util::DeviceExt;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// No adapter: skip with a message, unless `LACTEA_REQUIRE_GPU` is set (CI), which makes it a failure.
macro_rules! need_gpu {
    () => {
        match gpu() {
            Some(g) => g,
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

const W: u32 = 64;
const H: u32 = 48;

fn image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    f: impl Fn(u32, u32) -> [u8; 4],
) -> wgpu::TextureView {
    let mut data = Vec::new();
    for y in 0..H {
        for x in 0..W {
            data.extend_from_slice(&f(x, y));
        }
    }
    device
        .create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("in"),
                size: wgpu::Extent3d {
                    width: W,
                    height: H,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &data,
        )
        .create_view(&Default::default())
}

fn blend(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    old: &wgpu::TextureView,
    new: &wgpu::TextureView,
    p: &TransitionParams,
) -> Vec<u8> {
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
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let blender = TransitionBlender::new(device, format);
    let mut enc = device.create_command_encoder(&Default::default());
    blender.draw(&mut enc, old, new, &view, (W, H), p);
    let bpr = (W * 4 + 255) / 256 * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(bpr * H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
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
    queue.submit(Some(enc.finish()));
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buf.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::new();
    for y in 0..H {
        out.extend_from_slice(&data[(y * bpr) as usize..(y * bpr + W * 4) as usize]);
    }
    out
}

fn px(img: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

fn params(kind: TransitionKind, secs: f64, dur: f64, rs: [i32; 4]) -> TransitionParams {
    TransitionParams::new(kind, rs, dur, secs, 0.0)
}

#[test]
fn simple_blend_is_the_cosine_mix_in_image_orientation() {
    let (device, queue) = need_gpu!();
    // old: red ramp downwards (row 0 = top); new: blue ramp to the right
    let old = image(&device, &queue, |_, y| {
        [(y * 255 / (H - 1)) as u8, 0, 0, 255]
    });
    let new = image(&device, &queue, |x, _| {
        [0, 0, (x * 255 / (W - 1)) as u8, 255]
    });
    for secs in [0.0, 0.3, 0.5, 0.8, 1.0] {
        let p = params(TransitionKind::SimpleBlend, secs, 1.0, [0; 4]);
        let out = blend(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &old,
            &new,
            &p,
        );
        let c = p.cosine as f32;
        for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1), (20, 30)] {
            let o = (y * 255 / (H - 1)) as f32;
            let n = (x * 255 / (W - 1)) as f32;
            let got = px(&out, x, y);
            let exp = [(o * (1.0 - c)).round(), 0.0, (n * c).round()];
            assert!(
                (f32::from(got[0]) - exp[0]).abs() <= 2.0
                    && (f32::from(got[2]) - exp[2]).abs() <= 2.0,
                "t={secs} ({x},{y}): got {got:?} want {exp:?}"
            );
            assert_eq!(got[3], 255);
        }
    }
}

#[test]
fn every_shader_starts_on_the_old_image_and_ends_on_the_new_one() {
    let (device, queue) = need_gpu!();
    let old = image(&device, &queue, |_, _| [200, 20, 20, 255]);
    let new = image(&device, &queue, |_, _| [20, 200, 20, 255]);
    for kind in TransitionKind::ALL {
        for (secs, want) in [(0.0, [200.0, 20.0]), (1.0, [20.0, 200.0])] {
            let p = params(kind, secs, 1.0, [1234, 5678, 9, 10]);
            let out = blend(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8Unorm,
                &old,
                &new,
                &p,
            );
            let (mut worst, mut sum) = (0.0f32, 0.0f32);
            for y in 0..H {
                for x in 0..W {
                    let g = px(&out, x, y);
                    let e = (f32::from(g[0]) - want[0])
                        .abs()
                        .max((f32::from(g[1]) - want[1]).abs());
                    worst = worst.max(e);
                    sum += e;
                }
            }
            if matches!(kind, TransitionKind::Plasma | TransitionKind::Circle) {
                // plasma noise crosses the threshold in a few places even at the extremes, and
                // projectM's circle leaves its soft edge around the centre at progress 0
                let mean = sum / (W * H) as f32;
                assert!(mean <= 25.0, "{kind:?} t={secs}: mean channel error {mean}");
            } else {
                assert!(
                    worst <= 3.0,
                    "{kind:?} t={secs}: worst channel error {worst}"
                );
            }
        }
    }
}

/// projectM's warp transition with direction 2 (`mod(rand.x * .01, 4) in [2, 3)`) sweeps along
/// `uv.y`, which runs *upwards* in GL: at half progress the top of the picture already shows the
/// new image and the bottom still the old one.
#[test]
fn warp_transition_runs_bottom_up_like_gl() {
    let (device, queue) = need_gpu!();
    let old = image(&device, &queue, |_, _| [255, 0, 0, 255]);
    let new = image(&device, &queue, |_, _| [0, 255, 0, 255]);
    let p = params(TransitionKind::Warp, 0.5, 1.0, [250, 0, 0, 0]);
    let out = blend(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        &old,
        &new,
        &p,
    );
    let top = px(&out, W / 2, 0);
    let bottom = px(&out, W / 2, H - 1);
    assert!(
        top[1] > 240 && top[0] < 15,
        "top row should be the new image: {top:?}"
    );
    assert!(
        bottom[0] > 240 && bottom[1] < 15,
        "bottom row should be the old image: {bottom:?}"
    );
}

/// Circle transition, direction `inOrOut < 1`: the *new* image grows from the centre.
#[test]
fn circle_transition_opens_from_the_centre() {
    let (device, queue) = need_gpu!();
    let old = image(&device, &queue, |_, _| [255, 0, 0, 255]);
    let new = image(&device, &queue, |_, _| [0, 255, 0, 255]);
    // rand.x = 0 -> inOrOut = 0 (inner = old, outer = new... the circle edge moves outwards)
    let p = params(TransitionKind::Circle, 0.5, 1.0, [0, 0, 0, 0]);
    let out = blend(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        &old,
        &new,
        &p,
    );
    let centre = px(&out, W / 2, H / 2);
    let corner = px(&out, 0, 0);
    assert!(
        centre[1] > 240,
        "centre shows the outer (new) image first: {centre:?}"
    );
    assert!(
        corner[0] > 240,
        "corner still the inner (old) image: {corner:?}"
    );
}

#[test]
fn srgb_targets_are_compensated() {
    let (device, queue) = need_gpu!();
    let old = image(&device, &queue, |_, _| [100, 150, 200, 255]);
    let new = image(&device, &queue, |_, _| [100, 150, 200, 255]);
    let p = params(TransitionKind::SimpleBlend, 0.5, 1.0, [0; 4]);
    let out = blend(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &old,
        &new,
        &p,
    );
    let g = px(&out, 3, 3);
    for (a, b) in g.iter().zip([100u8, 150, 200, 255]) {
        assert!((i32::from(*a) - i32::from(b)).abs() <= 1, "{g:?}");
    }
}
