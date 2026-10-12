// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! MilkDrop 3 preset sprites: the quad draw with the 11 blend modes (MD3 3.37, docs/md3.md "Sprites").
//!
//! MD3 draws a sprite with the fixed-function D3D9 pipeline: vertex colour, one texture stage and a blend
//! state. The stage arithmetic is reproduced by one fragment entry point per mode, the blend factors by the
//! pipeline's blend state, so each mode is exactly `out = C * SRC + D * DEST` of the table in the docs.
//! Everything that decides *what* is drawn (sprite state, geometry, GIF frame) is the engine's; this module
//! only turns a [`SpriteDraw`] into pixels.

use std::collections::HashMap;
use wgpu::util::DeviceExt;

/// Number of blend modes (`0..=10`).
pub const SPRITE_MODES: usize = 11;

/// One corner of a sprite quad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpriteVertex {
    /// Position in MD3's sprite space: `±1` is the target's edge, **y points down** (the D3D 2D projection of
    /// MilkDrop flips y), z is 0.
    pub pos: [f32; 2],
    /// Texture coordinate (row 0 = top of the image); the sampler wraps in both directions.
    pub uv: [f32; 2],
}

/// One sprite draw handed to the renderer in [`FrameInput::sprites`](crate::FrameInput::sprites).
#[derive(Clone, Debug)]
pub struct SpriteDraw {
    /// Where it is drawn: `1..=4` into the feedback image (between warp / shapes / waves / main wave, so it
    /// goes through the composite and is warped next frame), `0` onto the composited output. Other values are
    /// not drawn.
    pub layer: u8,
    /// Image id given to [`Renderer::set_sprite_image`](crate::Renderer::set_sprite_image).
    pub image: u32,
    /// Frame of that image (animated GIFs); out of range draws nothing.
    pub frame: usize,
    /// Blend mode `0..=10`; any other value behaves as mode 0.
    pub blend: i32,
    /// `r, g, b` clamped to `0..1`; `a` is the per-frame alpha times the preset's `SpriteAlpha`, **not**
    /// clamped (the vertex colour byte wraps like `trunc(255 a) & 0xFF`).
    pub rgba: [f32; 4],
    /// Triangle-strip order.
    pub quad: [SpriteVertex; 4],
    /// Layer 0 only: the sprite is also drawn into the feedback image after the composite has read it.
    pub burn: bool,
}

/// The mode a `blend` value selects (`0` for anything outside `0..=10`).
pub fn sprite_mode(blend: i32) -> usize {
    if (0..SPRITE_MODES as i32).contains(&blend) {
        blend as usize
    } else {
        0
    }
}

/// `trunc(255 x) & 0xFF`: how D3D packs a float vertex colour channel.
fn q8(x: f32) -> u8 {
    ((x * 255.0) as i32 & 0xFF) as u8
}

/// The vertex colour `(r, g, b, a)` of a sprite in `mode`, after the byte quantisation of `D3DCOLOR`.
///
/// Modes 1 and 2 premultiply the colour by `a` and force alpha 1, mode 3 is plain white, mode 7 has a white
/// colour (its colour comes from the texture and `TFACTOR`), mode 8 folds the colour `c` to `c > 0.5 ? 1 - c : c`.
pub fn sprite_vertex_color(mode: usize, rgba: [f32; 4]) -> [u8; 4] {
    let [r, g, b, a] = rgba;
    match mode {
        1 | 2 => [q8(r * a), q8(g * a), q8(b * a), 255],
        3 => [255; 4],
        7 => [255, 255, 255, q8(a)],
        8 => {
            let f = |c: f32| if c > 0.5 { 1.0 - c } else { c };
            [q8(f(r)), q8(f(g)), q8(f(b)), q8(a)]
        }
        _ => [q8(r), q8(g), q8(b), q8(a)],
    }
}

/// CPU model of the table: result of drawing a texel `texel` (straight alpha) over the destination colour `dst`
/// with a sprite of colour `rgba` in `mode`, in `0..=255` units (not rounded). It follows the table, not the
/// shader: stage colour `C` / alpha `A`, then `out = C * SRC + D * DEST` with the mode's factors.
pub fn sprite_blend_reference(
    mode: usize,
    rgba: [f32; 4],
    texel: [u8; 4],
    dst: [u8; 3],
) -> [f32; 3] {
    let v = sprite_vertex_color(mode, rgba).map(|c| f32::from(c) / 255.0);
    let t = texel.map(|c| f32::from(c) / 255.0);
    let d = dst.map(|c| f32::from(c) / 255.0);
    let sat = |x: f32| x.clamp(0.0, 1.0);
    // stage-0 alpha: MODULATE(diffuse, texture) for 4, 7, 8, the diffuse alpha otherwise
    let a = match mode {
        4 | 7 | 8 => v[3] * t[3],
        _ => v[3],
    };
    let mut c = [0.0f32; 3];
    for i in 0..3 {
        c[i] = match mode {
            3 => t[i],
            7 => sat(1.0 - t[i]),
            9 => sat(t[i] + v[i] - 0.5),
            10 => sat(2.0 * t[i] * v[i]),
            _ => v[i] * t[i],
        };
    }
    let mut out = [0.0f32; 3];
    for i in 0..3 {
        // (SRC factor, DEST factor)
        let (s, dd) = match mode {
            1 => (1.0, 0.0), // blend off
            2 | 10 => (1.0, 1.0),
            3 => (c[i], 1.0 - c[i]),
            5 => (d[i], 0.0),
            6 => (0.0, 1.0 - c[i]),
            9 => (0.0, c[i]),
            _ => (a, 1.0 - a),
        };
        out[i] = sat(c[i] * s + d[i] * dd) * 255.0;
    }
    out
}

/// GIF frame choice (MD3 3.37): `t = fmod(time, sum(delays))`, the first frame whose cumulative delay is
/// strictly greater than `t`, else the last. One frame, no delays or a non-positive total always give frame 0.
/// `delays` are in seconds and already follow the "`<= 0` means 0.1 s" rule.
pub fn gif_frame_index(delays: &[f64], time: f64) -> usize {
    let total: f64 = delays.iter().sum();
    if delays.len() < 2 || total.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return 0;
    }
    let t = time % total;
    let mut acc = 0.0;
    for (i, d) in delays.iter().enumerate() {
        acc += d;
        if acc > t {
            return i;
        }
    }
    delays.len() - 1
}

/// An image registered with [`Renderer::set_sprite_image`](crate::Renderer::set_sprite_image).
pub(crate) struct SpriteImage {
    /// One view per animation frame (a view keeps its texture alive).
    pub(crate) frames: Vec<wgpu::TextureView>,
}

/// GPU vertex of a sprite quad.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

const ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];

/// Vertex stage (sprite space has y down) and one fragment entry point per mode, each producing the stage-0
/// colour and alpha of the table.
const SRC: &str = r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> VOut {
    var o: VOut;
    o.position = vec4<f32>(pos.x, -pos.y, 0.0, 1.0);
    o.uv = uv;
    o.color = color;
    return o;
}

fn texel(in: VOut) -> vec4<f32> { return textureSample(tex, samp, in.uv); }

// 0 Blend: MODULATE(diffuse, texture) colour, diffuse alpha
@fragment fn fs0(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(in.color.rgb * t.rgb, in.color.a);
}
// 1 Decal, 2 Additive: colour only, alpha 1 (decal: blending off)
@fragment fn fs1(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(in.color.rgb * t.rgb, 1.0);
}
@fragment fn fs2(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(in.color.rgb * t.rgb, 1.0);
}
// 3 Srccolor: the texture alone
@fragment fn fs3(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(t.rgb, 1.0);
}
// 4 Colorkey: alpha = diffuse * texture alpha
@fragment fn fs4(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(in.color.rgb * t.rgb, in.color.a * t.a);
}
// 5 Multiplicative, 6 Subtractive: colour only
@fragment fn fs5(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(in.color.rgb * t.rgb, in.color.a);
}
@fragment fn fs6(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(in.color.rgb * t.rgb, in.color.a);
}
// 7 Invert: SUBTRACT(TFACTOR = white, texture)
@fragment fn fs7(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(clamp(vec3<f32>(1.0) - t.rgb, vec3<f32>(0.0), vec3<f32>(1.0)), in.color.a * t.a);
}
// 8 Cut-off: MODULATE(texture, diffuse), alpha = diffuse * texture alpha
@fragment fn fs8(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(t.rgb * in.color.rgb, in.color.a * t.a);
}
// 9 Darken: ADDSIGNED(texture, diffuse)
@fragment fn fs9(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(clamp(t.rgb + in.color.rgb - vec3<f32>(0.5), vec3<f32>(0.0), vec3<f32>(1.0)), in.color.a);
}
// 10 Vivid: MODULATE2X(texture, diffuse)
@fragment fn fs10(in: VOut) -> @location(0) vec4<f32> {
    let t = texel(in);
    return vec4<f32>(clamp(2.0 * t.rgb * in.color.rgb, vec3<f32>(0.0), vec3<f32>(1.0)), in.color.a);
}
"#;

/// `(SRCBLEND, DESTBLEND)` of a mode; `None` = alpha blending off (decal).
fn factors(mode: usize) -> Option<(wgpu::BlendFactor, wgpu::BlendFactor)> {
    use wgpu::BlendFactor as F;
    Some(match mode {
        1 => return None,
        2 | 10 => (F::One, F::One),
        3 => (F::Src, F::OneMinusSrc),
        5 => (F::Dst, F::Zero),
        6 => (F::Zero, F::OneMinusSrc),
        9 => (F::Zero, F::Src),
        _ => (F::SrcAlpha, F::OneMinusSrcAlpha),
    })
}

/// The 11 sprite pipelines, built on first use.
pub(crate) struct SpritePipes {
    pipelines: Vec<wgpu::RenderPipeline>,
}

impl SpritePipes {
    pub(crate) fn new(
        device: &wgpu::Device,
        bgl_tex: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprite"),
            source: wgpu::ShaderSource::Wgsl(SRC.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite"),
            bind_group_layouts: &[Some(bgl_tex)],
            immediate_size: 0,
        });
        let pipelines = (0..SPRITE_MODES)
            .map(|mode| {
                let entry = format!("fs{mode}");
                // the destination alpha is never touched: it carries no meaning in the MilkDrop 2 family
                let keep_alpha = wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Zero,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                };
                let blend = factors(mode).map(|(s, d)| wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: s,
                        dst_factor: d,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: keep_alpha,
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("sprite"),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("vs_main"),
                        compilation_options: Default::default(),
                        buffers: &[Some(wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<GpuVertex>() as u64,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &ATTRS,
                        })],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleStrip,
                        ..Default::default()
                    },
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some(&entry),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .collect();
        Self { pipelines }
    }

    /// Draw `draws` in order onto `target` (loaded, not cleared).
    pub(crate) fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        bgl_tex: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        images: &HashMap<u32, SpriteImage>,
        draws: &[&SpriteDraw],
    ) {
        let mut verts: Vec<GpuVertex> = Vec::with_capacity(draws.len() * 4);
        let mut groups: Vec<(usize, wgpu::BindGroup)> = Vec::with_capacity(draws.len());
        for d in draws {
            let Some(view) = images.get(&d.image).and_then(|i| i.frames.get(d.frame)) else {
                continue;
            };
            let mode = sprite_mode(d.blend);
            let color = sprite_vertex_color(mode, d.rgba).map(|c| f32::from(c) / 255.0);
            verts.extend(d.quad.iter().map(|v| GpuVertex {
                pos: v.pos,
                uv: v.uv,
                color,
            }));
            groups.push((
                mode,
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("sprite"),
                    layout: bgl_tex,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                    ],
                }),
            ));
        }
        if groups.is_empty() {
            return;
        }
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sprite verts"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sprites"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_vertex_buffer(0, buf.slice(..));
        for (i, (mode, bg)) in groups.iter().enumerate() {
            pass.set_pipeline(&self.pipelines[*mode]);
            pass.set_bind_group(0, bg, &[]);
            let s = (i * 4) as u32;
            pass.draw(s..s + 4, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `T = (204, 102, 51, 255)`, `D = 128` of the blend table.
    const T: [u8; 4] = [204, 102, 51, 255];
    const D: [u8; 3] = [128; 3];

    fn rounded(mode: usize, rgb: [f32; 3]) -> [i32; 3] {
        sprite_blend_reference(mode, [rgb[0], rgb[1], rgb[2], 0.4], T, D).map(|c| c.round() as i32)
    }

    fn near(a: [i32; 3], b: [i32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1)
    }

    /// The reference reproduces the worked example of the specification (+-1 for hardware rounding).
    #[test]
    fn reference_matches_the_documented_example() {
        let white = [
            [158, 118, 97],
            [82, 41, 20],
            [210, 169, 148],
            [189, 118, 113],
            [158, 118, 97],
            [102, 51, 26],
            [26, 77, 102],
            [97, 138, 158],
            [77, 77, 77],
            [128, 115, 90],
            [255, 255, 230],
        ];
        let tint = [
            [158, 101, 81],
            [82, 24, 4],
            [210, 152, 132],
            [189, 118, 113],
            [158, 101, 81],
            [102, 31, 5],
            [26, 97, 123],
            [97, 138, 158],
            [77, 93, 81],
            [128, 64, 0],
            [255, 250, 148],
        ];
        for mode in 0..SPRITE_MODES {
            let got = rounded(mode, [1.0, 1.0, 1.0]);
            assert!(near(got, white[mode]), "mode {mode} white: {got:?}");
            let got = rounded(mode, [1.0, 0.6, 0.2]);
            assert!(near(got, tint[mode]), "mode {mode} tint: {got:?}");
        }
    }

    #[test]
    fn keyed_texels_leave_the_destination_for_alpha_textured_modes() {
        let keyed = [204, 102, 51, 0];
        for mode in [4, 7, 8] {
            let out = sprite_blend_reference(mode, [1.0, 1.0, 1.0, 0.4], keyed, D);
            assert_eq!(out.map(|c| c.round() as i32), [128; 3], "mode {mode}");
        }
        // the others ignore the texel alpha
        for mode in [0, 1, 2, 3, 5, 6, 9, 10] {
            let a = sprite_blend_reference(mode, [1.0, 0.6, 0.2, 0.4], keyed, D);
            let b = sprite_blend_reference(mode, [1.0, 0.6, 0.2, 0.4], T, D);
            assert_eq!(a, b, "mode {mode}");
        }
    }

    #[test]
    fn vertex_colours_follow_the_table() {
        let rgba = [1.0, 0.6, 0.2, 0.4];
        assert_eq!(sprite_vertex_color(0, rgba), [255, 153, 51, 102]);
        assert_eq!(sprite_vertex_color(1, rgba), [102, 61, 20, 255]);
        assert_eq!(sprite_vertex_color(3, rgba), [255; 4]);
        assert_eq!(sprite_vertex_color(7, rgba), [255, 255, 255, 102]);
        // mode 8 folds the colour: 0.6 -> 0.4
        let v8 = sprite_vertex_color(8, rgba);
        assert_eq!((v8[0], v8[2], v8[3]), (0, 51, 102));
        assert!((101..=102).contains(&v8[1]), "{v8:?}");
        // the alpha byte wraps like `& 0xFF`
        assert_eq!(sprite_vertex_color(0, [0.0, 0.0, 0.0, 2.0])[3], 254);
    }

    #[test]
    fn out_of_range_blend_is_mode_zero() {
        assert_eq!(sprite_mode(-1), 0);
        assert_eq!(sprite_mode(11), 0);
        assert_eq!(sprite_mode(10), 10);
    }

    #[test]
    fn gif_frame_selection_follows_the_delay_rule() {
        // delays [10, 20, 5] hundredths
        let d = [0.1, 0.2, 0.05];
        assert_eq!(gif_frame_index(&d, 0.0), 0);
        assert_eq!(gif_frame_index(&d, 0.12), 1);
        // cumulative 0.1, 0.3, 0.35: "0.3 > t" is false at exactly 0.3 and t = 0.31 is frame 2
        assert_eq!(gif_frame_index(&d, 0.31), 2);
        assert_eq!(gif_frame_index(&d, 0.36), 0, "0.36 mod 0.35 = 0.01");
        assert_eq!(gif_frame_index(&d, 0.35 * 7.0 + 0.12), 1);
        // single frames and degenerate delays stay on frame 0
        assert_eq!(gif_frame_index(&[0.5], 3.0), 0);
        assert_eq!(gif_frame_index(&[], 3.0), 0);
        assert_eq!(gif_frame_index(&[0.0, 0.0], 3.0), 0);
    }
}
