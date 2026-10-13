// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Renderer-level tests of MilkDrop 3 sprites (`FrameInput::sprites`): the 11 blend modes on the GPU against the
//! CPU model of the table, and where each layer lands in the frame pipeline. Skipped (with a note) when there is no
//! GPU adapter.

use lactea_abi::dump::Stage;
use lactea_render::{
    Compat, CustomWave, FrameInput, RenderConfig, Renderer, SPRITE_MODES, ShapeInstance,
    SpriteDraw, SpriteVertex, WavePoint, sprite_blend_reference,
};
use lactea_testutil::{gpu_or_skip, quiet_input, read_rgba8};

const W: u32 = 64;
const H: u32 = 48;

fn renderer(device: &wgpu::Device, queue: &wgpu::Queue) -> Renderer {
    Renderer::new(
        device,
        queue,
        RenderConfig {
            width: W,
            height: H,
            mesh: [16, 12],
            compat: Compat::MilkDrop3,
            ..RenderConfig::default()
        },
    )
    .expect("renderer")
}

/// A quad covering `[-half, half]^2` of the target with the whole image on it.
fn quad(half: f32) -> [SpriteVertex; 4] {
    let v = |x: f32, y: f32, u: f32, w: f32| SpriteVertex {
        pos: [x, y],
        uv: [u, w],
    };
    [
        v(-half, -half, 0.0, 0.0),
        v(half, -half, 1.0, 0.0),
        v(-half, half, 0.0, 1.0),
        v(half, half, 1.0, 1.0),
    ]
}

fn draw(layer: u8, image: u32, blend: i32, rgba: [f32; 4], half: f32) -> SpriteDraw {
    SpriteDraw {
        layer,
        image,
        frame: 0,
        blend,
        rgba,
        quad: quad(half),
        burn: false,
    }
}

/// One sprite of every mode over a flat grey, against the CPU model of the blend table.
#[test]
fn blend_modes_match_the_cpu_reference() {
    let (device, queue) = gpu_or_skip!();
    let mut r = renderer(&device, &queue);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
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
    let texel_opaque = [204u8, 102, 51, 255];
    let texel_keyed = [204u8, 102, 51, 0];
    r.set_sprite_image(0, 1, 1, &[&texel_opaque], true).unwrap();
    r.set_sprite_image(1, 1, 1, &[&texel_keyed], true).unwrap();
    let dst = [128u8; 3];
    for (image, texel) in [(0u32, texel_opaque), (1, texel_keyed)] {
        for rgba in [[1.0, 1.0, 1.0, 0.4], [1.0, 0.6, 0.2, 0.4]] {
            for mode in 0..SPRITE_MODES {
                let mut enc = device.create_command_encoder(&Default::default());
                drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: f64::from(dst[0]) / 255.0,
                                g: f64::from(dst[1]) / 255.0,
                                b: f64::from(dst[2]) / 255.0,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                }));
                r.draw_sprites(&mut enc, &view, &[draw(1, image, mode as i32, rgba, 2.0)]);
                queue.submit(Some(enc.finish()));
                let px = read_rgba8(&device, &queue, &target, 4, 4);
                let got = [px[0], px[1], px[2]];
                let want = sprite_blend_reference(mode, rgba, texel, dst);
                for c in 0..3 {
                    assert!(
                        (f32::from(got[c]) - want[c]).abs() <= 2.0,
                        "mode {mode} image {image} rgba {rgba:?}: gpu {got:?} vs reference {want:?}"
                    );
                }
            }
        }
    }
}

fn render(device: &wgpu::Device, queue: &wgpu::Queue, r: &mut Renderer, f: &FrameInput) {
    let mut enc = device.create_command_encoder(&Default::default());
    r.render_frame_to_output(&mut enc, f).expect("frame");
    queue.submit(Some(enc.finish()));
}

fn pixel(img: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

fn stage(r: &Renderer, s: Stage) -> Vec<u8> {
    r.read_stage(s).expect("read stage").2
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const WHITE: [u8; 4] = [255; 4];

fn is_near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 3)
}

/// Layers 1..4 draw into the feedback image: the sprite goes through the composite and is warped into the next
/// frame. Layer 0 draws on the composited image: no composite, and only burn feeds it back.
#[test]
fn feedback_layers_are_composited_and_remembered_top_layer_is_not() {
    let (device, queue) = gpu_or_skip!();
    let centre = (W / 2, H / 2);
    let run = |layer: u8, burn: bool| {
        let mut r = renderer(&device, &queue);
        r.set_sprite_image(0, 1, 1, &[&RED], true).unwrap();
        let mut f = quiet_input();
        // the composite inverts: a sprite that goes through it shows cyan
        f.post.invert = true;
        let mut d = draw(layer, 0, 4, [1.0; 4], 0.5);
        d.burn = burn;
        f.sprites = vec![d];
        render(&device, &queue, &mut r, &f);
        let first = pixel(&stage(&r, Stage::Final), centre.0, centre.1);
        let mut g = quiet_input();
        g.post.invert = false;
        render(&device, &queue, &mut r, &g);
        let second = pixel(&stage(&r, Stage::Final), centre.0, centre.1);
        (first, second)
    };
    // layer 1: composited (inverted red = cyan), kept for the next frame (shown un-inverted there)
    let (first, second) = run(1, false);
    assert!(is_near(first, [0, 255, 255]), "layer 1 frame 0: {first:?}");
    assert!(is_near(second, [255, 0, 0]), "layer 1 frame 1: {second:?}");
    // layer 0 without burn: plain red on the output, gone the next frame
    let (first, second) = run(0, false);
    assert!(is_near(first, [255, 0, 0]), "layer 0 frame 0: {first:?}");
    assert!(is_near(second, [0, 0, 0]), "layer 0 frame 1: {second:?}");
    // layer 0 with burn: also written into the feedback image after the composite read it
    let (first, second) = run(0, true);
    assert!(is_near(first, [255, 0, 0]), "burn frame 0: {first:?}");
    assert!(is_near(second, [255, 0, 0]), "burn frame 1: {second:?}");
}

/// MD3 draws a burning sprite of layers 1..4 twice into the feedback image (measured under Wine): an additive
/// sprite is twice as strong with burn as without; MilkDrop 2 rules draw it once.
#[test]
fn burn_doubles_additive_sprites_in_feedback_layers_under_md3() {
    let (device, queue) = gpu_or_skip!();
    let centre_value = |compat: Compat, layer: u8, burn: bool| {
        let mut r = Renderer::new(
            &device,
            &queue,
            RenderConfig {
                width: W,
                height: H,
                mesh: [16, 12],
                compat,
                ..RenderConfig::default()
            },
        )
        .unwrap();
        r.set_sprite_image(0, 1, 1, &[&[128, 128, 128, 255]], true)
            .unwrap();
        let mut f = quiet_input();
        let mut d = draw(layer, 0, 2, [1.0, 1.0, 1.0, 0.2], 2.0);
        d.burn = burn;
        f.sprites = vec![d];
        render(&device, &queue, &mut r, &f);
        pixel(&stage(&r, Stage::Final), W / 2, H / 2)[0]
    };
    let (once, twice) = (
        centre_value(Compat::MilkDrop3, 1, false),
        centre_value(Compat::MilkDrop3, 1, true),
    );
    assert!((24..=28).contains(&once), "one additive draw: {once}");
    assert!(
        twice.abs_diff(2 * once) <= 2,
        "burn draws twice: {once} vs {twice}"
    );
    assert_eq!(
        centre_value(Compat::MilkDrop2, 1, true),
        once,
        "MD2 rules draw once"
    );
    // layer 0 burns into the feedback image *after* the composite: the output itself shows one draw
    assert_eq!(centre_value(Compat::MilkDrop3, 0, true), once);
}

/// Layers 1..4 sit between the warp, the shapes, the custom waves and the main wave + borders.
#[test]
fn feedback_layers_interleave_with_shapes_waves_and_borders() {
    let (device, queue) = gpu_or_skip!();
    let at = |layer: u8, image: [u8; 4], half: f32| {
        let mut r = renderer(&device, &queue);
        r.set_sprite_image(0, 1, 1, &[&image], true).unwrap();
        let mut f = quiet_input();
        // an opaque red square in the middle
        f.shapes = vec![ShapeInstance {
            rad: 0.4,
            rgba: [1.0, 0.0, 0.0, 1.0],
            rgba2: [1.0, 0.0, 0.0, 1.0],
            border_rgba: [0.0; 4],
            ..ShapeInstance::default()
        }];
        // a thick blue line through the middle
        f.waves = vec![CustomWave {
            points: vec![
                WavePoint {
                    x: 0.05,
                    y: 0.5,
                    rgba: [0.0, 0.0, 1.0, 1.0],
                },
                WavePoint {
                    x: 0.95,
                    y: 0.5,
                    rgba: [0.0, 0.0, 1.0, 1.0],
                },
            ],
            thick: true,
            ..CustomWave::default()
        }];
        // a green outer border
        f.borders.ob_rgba = [0.0, 1.0, 0.0, 1.0];
        f.borders.ob_size = 0.2;
        f.sprites = vec![draw(layer, 0, 4, [1.0; 4], half)];
        render(&device, &queue, &mut r, &f);
        stage(&r, Stage::Final)
    };
    let blue_line = |img: &[u8]| {
        (H / 2 - 3..H / 2 + 3).any(|y| {
            let p = pixel(img, W / 2, y);
            p[2] > 200 && p[0] < 60 && p[1] < 60
        })
    };
    // a small green sprite over the shape, away from the line: shape (red) vs sprite (green) at the corner of the
    // shape (outside the line, inside the shape)
    let probe = (W / 2 - 4, H / 2 - 5);
    let small = 0.25;
    let layer1 = at(1, GREEN, small);
    let layer2 = at(2, GREEN, small);
    assert!(
        is_near(pixel(&layer1, probe.0, probe.1), [255, 0, 0]),
        "layer 1 is below the shape: {:?}",
        pixel(&layer1, probe.0, probe.1)
    );
    assert!(
        is_near(pixel(&layer2, probe.0, probe.1), [0, 255, 0]),
        "layer 2 is above the shape: {:?}",
        pixel(&layer2, probe.0, probe.1)
    );
    // a white sprite covering everything: the line shows under layer 2 and is covered by layers 3 and 4
    assert!(blue_line(&at(2, WHITE, 2.0)), "layer 2 is below the waves");
    assert!(!blue_line(&at(3, WHITE, 2.0)), "layer 3 is above the waves");
    // the border: drawn after layer 3, covered by layer 4
    let corner = |img: &[u8]| pixel(img, 1, 1);
    let b3 = at(3, WHITE, 2.0);
    assert!(
        is_near(corner(&b3), [0, 255, 0]),
        "the border is drawn after layer 3: {:?}",
        corner(&b3)
    );
    let b4 = at(4, WHITE, 2.0);
    assert!(
        is_near(corner(&b4), [255, 255, 255]),
        "layer 4 covers the border: {:?}",
        corner(&b4)
    );
}

/// A sprite or texture larger than the device's 2D texture limit is an error, not a wgpu validation panic.
#[test]
fn images_beyond_the_device_texture_limit_are_rejected() {
    let (device, queue) = gpu_or_skip!();
    let mut r = renderer(&device, &queue);
    let side = device.limits().max_texture_dimension_2d + 1;
    // the limit is checked before any pixel is read, so the data never matters
    assert!(r.set_sprite_image(0, side, 1, &[&[0u8; 4]], true).is_err());
    assert!(r.set_user_texture("big", 1, side, &[0u8; 4]).is_err());
    assert!(
        r.set_user_animation("anim", side, 1, &[&[0u8; 4]], &[0.1])
            .is_err()
    );
    r.set_sprite_image(0, 1, 1, &[&[0u8; 4]], true).unwrap();
}
