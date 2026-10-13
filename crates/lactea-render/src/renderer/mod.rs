// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! GPU side: resources, pipelines and the per-frame pass sequence.

use crate::blur;
use crate::config::{Compat, CompatRules, CompiledShader, RenderConfig, RenderError, StrokeRaster};
use crate::frame::{FrameInput, ShapeImage};
use crate::geom::{self, WarpVertex, WfVertex};
use crate::motion::MotionVectorState;
use crate::noise;
use crate::rng::Rng;
use crate::shader_ext::ShaderExt;
use crate::shaders;
use crate::sprite::{SpriteDraw, SpriteImage, SpritePipes};
use crate::stroke::{self, LineRule};
use crate::uniforms::{self, MilkUniformInputs, RotParams};
use crate::warp_mesh::{self, MeshGeometry, VertexVars, WarpAnim};
use crate::wavegen;
use crate::wavegen_md3;
use lactea_abi::dump::Stage;
use lactea_abi::{
    Address, BINDING_MILK_EXT, FFT_BINS, FRAGMENT_ENTRY, Filter, MeshVertex, MilkUniforms,
    NoiseTex, PassUniforms, ShaderInterface, TexDim, TexSource,
};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

mod burn;
mod fixed;
mod orchestrate;
mod passes;
mod pipelines;
mod uv_readback;

use pipelines::*;
use uv_readback::UvReadback;

const INTERNAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

// ---------------------------------------------------------------------------------------
// small GPU helpers
// ---------------------------------------------------------------------------------------

#[derive(Clone)]
struct Tex {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

fn create_target(device: &wgpu::Device, w: u32, h: u32, label: &str) -> Tex {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: INTERNAL_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Tex {
        texture,
        view,
        w,
        h,
    }
}

/// `texCoords` of projectM's warp shader: `RG16F`.
const WARP_SINK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

fn create_sink(device: &wgpu::Device, w: u32, h: u32) -> Tex {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("warp_sink"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: WARP_SINK_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Tex {
        texture,
        view,
        w,
        h,
    }
}

/// An empty `R32Float` texture, as the shader FFT and BeatDrop's wave textures are: zero until the first upload.
/// `R32Float` is not filterable without `FLOAT32_FILTERABLE`; the translated shaders `textureLoad` it and blend the
/// neighbouring texels by hand.
fn create_r32f_tex(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    w: usize,
    h: usize,
) -> Tex {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w as u32,
            height: h as u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &vec![0u8; w * h * 4],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w as u32 * 4),
            rows_per_image: Some(h as u32),
        },
        wgpu::Extent3d {
            width: w as u32,
            height: h as u32,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Tex {
        texture,
        view,
        w: w as u32,
        h: h as u32,
    }
}

/// CPU box-filter mip chain (D3D `AUTOGENMIPMAP` equivalent), level 0 included.
fn mip_chain(data: &[u8], w: u32, h: u32) -> Vec<(Vec<u8>, u32, u32)> {
    let mut levels = vec![(data.to_vec(), w, h)];
    while levels.last().map(|l| l.1 > 1 || l.2 > 1).unwrap_or(false) {
        let (src, sw, sh) = levels.last().unwrap();
        let (dw, dh) = ((*sw / 2).max(1), (*sh / 2).max(1));
        let mut dst = vec![0u8; (dw * dh * 4) as usize];
        for y in 0..dh {
            for x in 0..dw {
                let x0 = (x * 2).min(sw - 1);
                let x1 = (x * 2 + 1).min(sw - 1);
                let y0 = (y * 2).min(sh - 1);
                let y1 = (y * 2 + 1).min(sh - 1);
                for c in 0..4 {
                    let s = |xx: u32, yy: u32| src[((yy * sw + xx) * 4 + c) as usize] as u32;
                    dst[((y * dw + x) * 4 + c) as usize] =
                        ((s(x0, y0) + s(x1, y0) + s(x0, y1) + s(x1, y1) + 2) / 4) as u8;
                }
            }
        }
        levels.push((dst, dw, dh));
    }
    levels
}

fn upload_2d(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    w: u32,
    h: u32,
    rgba: &[u8],
    mips: bool,
    label: &str,
) -> Tex {
    let levels = if mips {
        mip_chain(rgba, w, h)
    } else {
        vec![(rgba.to_vec(), w, h)]
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: INTERNAL_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (i, (data, lw, lh)) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: i as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(lw * 4),
                rows_per_image: Some(*lh),
            },
            wgpu::Extent3d {
                width: *lw,
                height: *lh,
                depth_or_array_layers: 1,
            },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Tex {
        texture,
        view,
        w,
        h,
    }
}

fn upload_3d(device: &wgpu::Device, queue: &wgpu::Queue, n: u32, rgba: &[u8], label: &str) -> Tex {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: n,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: INTERNAL_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(n * 4),
            rows_per_image: Some(n),
        },
        wgpu::Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: n,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D3),
        ..Default::default()
    });
    Tex {
        texture,
        view,
        w: n,
        h: n,
    }
}

/// `frames` must be a non-empty list of `w * h * 4` byte images of a non-empty size that fits the device's
/// largest 2D texture (`max_side`).
fn check_frames(
    what: &str,
    max_side: u32,
    w: u32,
    h: u32,
    frames: &[&[u8]],
) -> Result<(), RenderError> {
    if w > max_side || h > max_side {
        return Err(RenderError::Invalid(format!(
            "{what}: {w}x{h} exceeds the device texture limit of {max_side}"
        )));
    }
    let need = w as usize * h as usize * 4;
    if w == 0 || h == 0 || frames.is_empty() || frames.iter().any(|f| f.len() != need) {
        return Err(RenderError::Invalid(format!(
            "{what}: {} frame(s) of {w}x{h} need {need} bytes each",
            frames.len()
        )));
    }
    Ok(())
}

fn buffer_init<T: bytemuck::Pod>(
    device: &wgpu::Device,
    label: &str,
    data: &[T],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let bytes: &[u8] = bytemuck::cast_slice(data);
    if bytes.is_empty() {
        // zero-sized buffers are awkward; hand out a 16-byte dummy
        return device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: &[0u8; 16],
            usage,
        });
    }
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytes,
        usage,
    })
}

// ---------------------------------------------------------------------------------------
// blend modes / pipeline set
// ---------------------------------------------------------------------------------------

use crate::blend::{self, Which};
/// A blur pass's uniform values and the bind group built from them.
type BlurBindGroup = ([[f32; 4]; 6], wgpu::BindGroup);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Blend {
    /// D3DRS_ALPHABLENDENABLE = FALSE (or ONE/ZERO)
    Replace,
    /// ONE / ONE
    Add,
    /// SRCALPHA / INVSRCALPHA
    SrcAlpha,
    /// INVSRCALPHA / SRCALPHA (`bFlipAlpha` of MD2's preset blend)
    InvSrcAlpha,
    /// SRCALPHA / ONE
    SrcAlphaAdd,
    /// INVDESTCOLOR / ZERO
    InvDstZero,
    /// ZERO / DESTCOLOR
    ZeroDst,
    /// ZERO / INVDESTCOLOR
    ZeroInvDst,
    /// DESTCOLOR / ONE
    DstOne,
}

impl Blend {
    /// Every variant: the pipeline maps are keyed over this, so a new mode gets its pipelines
    /// without a second list. `blend_state` is the exhaustive match that forces a new arm.
    const ALL: [Blend; 9] = [
        Blend::Replace,
        Blend::Add,
        Blend::SrcAlpha,
        Blend::InvSrcAlpha,
        Blend::SrcAlphaAdd,
        Blend::InvDstZero,
        Blend::ZeroDst,
        Blend::ZeroInvDst,
        Blend::DstOne,
    ];
}

fn blend_state(b: Blend) -> Option<wgpu::BlendState> {
    use wgpu::BlendFactor as F;
    let (s, d) = match b {
        Blend::Replace => return None,
        Blend::Add => (F::One, F::One),
        Blend::SrcAlpha => (F::SrcAlpha, F::OneMinusSrcAlpha),
        Blend::InvSrcAlpha => (F::OneMinusSrcAlpha, F::SrcAlpha),
        Blend::SrcAlphaAdd => (F::SrcAlpha, F::One),
        Blend::InvDstZero => (F::OneMinusDst, F::Zero),
        Blend::ZeroDst => (F::Zero, F::Dst),
        Blend::ZeroInvDst => (F::Zero, F::OneMinusDst),
        Blend::DstOne => (F::Dst, F::One),
    };
    let c = wgpu::BlendComponent {
        src_factor: s,
        dst_factor: d,
        operation: wgpu::BlendOperation::Add,
    };
    Some(wgpu::BlendState { color: c, alpha: c })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Flat,
    Tex,
    /// A textured draw that samples the main image with the leftover blur sampler (linear, clamp), see
    /// `Renderer::build_overlay`.
    TexLeak,
    /// A textured draw that samples a shape's `image=` user texture (index into `Batch::images`).
    TexImage(u32),
}

/// Primitive topology of a flat-colour draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Prim {
    /// `TriangleList`
    Tri,
    /// `LineList` (pairs of vertices, vertex colours interpolate along the line)
    Line,
    /// `PointList` (1 px)
    Point,
}

#[derive(Default)]
struct Batch {
    wf: Vec<WfVertex>,
    mesh: Vec<MeshVertex>,
    cmds: Vec<(Kind, Blend, Prim, u32, u32)>,
    /// Textures of [`Kind::TexImage`] commands (one entry per command).
    images: Vec<ShapeImage>,
    /// Command indices after the custom shapes / after the custom waves: where sprite layers 2 and 3 go.
    marks: [usize; 2],
    /// Commands before this index never merge with later ones (a sprite layer may be drawn in between).
    barrier: usize,
}

impl Batch {
    fn flat(&mut self, blend: Blend, verts: &[WfVertex]) {
        self.flat_prim(Prim::Tri, blend, verts);
    }
    fn flat_prim(&mut self, prim: Prim, blend: Blend, verts: &[WfVertex]) {
        if verts.is_empty() {
            return;
        }
        let start = self.wf.len() as u32;
        self.wf.extend_from_slice(verts);
        if self.cmds.len() > self.barrier
            && let Some(last) = self.cmds.last_mut()
            && last.0 == Kind::Flat
            && last.1 == blend
            && last.2 == prim
            && last.3 + last.4 == start
        {
            last.4 += verts.len() as u32;
            return;
        }
        self.cmds
            .push((Kind::Flat, blend, prim, start, verts.len() as u32));
    }
    fn tex(&mut self, blend: Blend, verts: &[MeshVertex]) {
        if verts.is_empty() {
            return;
        }
        let start = self.mesh.len() as u32;
        self.mesh.extend_from_slice(verts);
        if self.cmds.len() > self.barrier
            && let Some(last) = self.cmds.last_mut()
            && last.0 == Kind::Tex
            && last.1 == blend
            && last.3 + last.4 == start
        {
            last.4 += verts.len() as u32;
            return;
        }
        self.cmds
            .push((Kind::Tex, blend, Prim::Tri, start, verts.len() as u32));
    }
    fn tex_leak(&mut self, blend: Blend, verts: &[MeshVertex]) {
        if verts.is_empty() {
            return;
        }
        let start = self.mesh.len() as u32;
        self.mesh.extend_from_slice(verts);
        self.cmds
            .push((Kind::TexLeak, blend, Prim::Tri, start, verts.len() as u32));
    }
    /// A textured fill that samples the user texture of a shape's `image=` key.
    fn tex_image(&mut self, blend: Blend, verts: &[MeshVertex], image: &ShapeImage) {
        if verts.is_empty() {
            return;
        }
        let start = self.mesh.len() as u32;
        self.mesh.extend_from_slice(verts);
        let id = self.images.len() as u32;
        self.images.push(ShapeImage {
            name: image.name.to_ascii_lowercase(),
            ..image.clone()
        });
        self.cmds.push((
            Kind::TexImage(id),
            blend,
            Prim::Tri,
            start,
            verts.len() as u32,
        ));
    }
    /// Let `fill` append vertices of `prim` to the flat-vertex buffer and record them as one command (merged with the
    /// previous one when it is compatible), without an intermediate buffer.
    fn flat_extend(&mut self, prim: Prim, blend: Blend, fill: impl FnOnce(&mut Vec<WfVertex>)) {
        let start = self.wf.len() as u32;
        fill(&mut self.wf);
        let count = self.wf.len() as u32 - start;
        if count == 0 {
            return;
        }
        if self.cmds.len() > self.barrier
            && let Some(last) = self.cmds.last_mut()
            && last.0 == Kind::Flat
            && last.1 == blend
            && last.2 == prim
            && last.3 + last.4 == start
        {
            last.4 += count;
            return;
        }
        self.cmds.push((Kind::Flat, blend, prim, start, count));
    }
    /// 1-pixel line segments: `segs` holds pairs of vertices (clip space, per-vertex colour); `rule` picks the
    /// GL line rule (see `stroke.rs`).
    fn segments(
        &mut self,
        mode: StrokeRaster,
        rule: LineRule,
        blend: Blend,
        w: f32,
        h: f32,
        segs: &[WfVertex],
    ) {
        match mode {
            StrokeRaster::Native => self.flat_prim(Prim::Line, blend, segs),
            StrokeRaster::Emulated => self.flat_extend(Prim::Point, blend, |out| {
                stroke::raster_segments(segs, w as u32, h as u32, rule, out);
            }),
            StrokeRaster::Quads => self.flat_extend(Prim::Tri, blend, |out| {
                for s in segs.as_chunks::<2>().0 {
                    geom::push_line(out, s[0].pos, s[1].pos, w, h, s[0].color);
                }
            }),
        }
    }
    /// Points of `size` pixels (`size == 1` is a 1-px GL point; larger sizes are D3D9 point sprites = quads).
    fn points(
        &mut self,
        mode: StrokeRaster,
        blend: Blend,
        w: f32,
        h: f32,
        pts: &[WfVertex],
        size: f32,
    ) {
        match mode {
            StrokeRaster::Native if size <= 1.0 => self.flat_prim(Prim::Point, blend, pts),
            StrokeRaster::Emulated if size <= 1.0 => self.flat_extend(Prim::Point, blend, |out| {
                stroke::raster_points(pts, w as u32, h as u32, out);
            }),
            _ => self.flat_extend(Prim::Tri, blend, |out| {
                for p in pts {
                    geom::push_point(out, p.pos, size, w, h, p.color);
                }
            }),
        }
    }
    /// Remember the current end of the command list as sprite split `which` (0 = after the custom shapes, 1 = after
    /// the custom waves).
    fn mark(&mut self, which: usize) {
        self.marks[which] = self.cmds.len();
        self.barrier = self.cmds.len();
    }
    fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }
}

struct Pipelines {
    bgl_tex: wgpu::BindGroupLayout,
    bgl_warp_vs: wgpu::BindGroupLayout,
    /// projectM warp vertex stage + fixed-function fragment, keyed by `main_alpha`
    warp_tex: HashMap<bool, wgpu::RenderPipeline>,
    warp_uv_readback: wgpu::RenderPipeline,
    bgl_blur: wgpu::BindGroupLayout,
    bgl_blit: wgpu::BindGroupLayout,
    bgl_mu: wgpu::BindGroupLayout,
    /// keyed by (blend, alpha): `alpha` = projectM RGBA semantics (alpha written, `tex.a` modulates)
    tex: HashMap<(Blend, bool), wgpu::RenderPipeline>,
    flat: HashMap<(Blend, Prim, bool), wgpu::RenderPipeline>,
    blur_h: wgpu::RenderPipeline,
    blur_v: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    mesh_vs: wgpu::ShaderModule,
    /// `[linear wrap, linear clamp, point wrap, point clamp]` with mips, then the same four limited to level 0
    samplers: [wgpu::Sampler; 8],
    /// MD3 sprite pipelines, built when the first sprite image is registered.
    sprite: std::sync::OnceLock<SpritePipes>,
    /// The `burn_texture` copy pipeline, built on first use.
    burn: std::sync::OnceLock<burn::BurnPipe>,
    /// Sprite pipelines for the host's output format when it is not the internal one.
    sprite_out: std::sync::OnceLock<SpritePipes>,
}

/// Uniforms of [`shaders::warp_vs`] (projectM's warp vertex shader).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct WarpVsUniforms {
    /// `aspectX, aspectY, invAspectX, invAspectY`
    aspect: [f32; 4],
    /// `warpTime, warpScaleInverse, texelOffset.x, texelOffset.y`
    timing: [f32; 4],
    /// `warpFactors`
    factors: [f32; 4],
    /// `frag_COLOR` (`decay`, or 1)
    diffuse: [f32; 4],
}

struct ShaderPass {
    pipeline: wgpu::RenderPipeline,
    /// Warp shaders only: the same fragment stage behind projectM's warp vertex shader ([`shaders::warp_vs`]).
    warp_pipeline: Option<wgpu::RenderPipeline>,
    pass_bgl: wgpu::BindGroupLayout,
    interface: ShaderInterface,
    /// Warp shaders: the fragment stage returns the interpolated UV as a second output ([`with_uv_output`]).
    uv_sink: bool,
    /// `CompatRules::md2_preset_blend` only: the same fragment stage with MD2's blend-pass alpha blending, keyed
    /// `[SRCALPHA / INVSRCALPHA, INVSRCALPHA / SRCALPHA]` (`bFlipAlpha`), mesh vertices.
    alpha_pipelines: Option<[wgpu::RenderPipeline; 2]>,
}

/// Vertex data of a user-shader pass: the generic mesh vertex, or projectM's warp mesh vertex (GPU UV
/// evaluation) with the uniforms of its vertex stage.
enum PassVerts<'a> {
    Mesh(&'a [MeshVertex]),
    Warp(&'a [WarpVertex], &'a WarpVsUniforms),
}

struct UserTex {
    /// The texture (the first frame of an animation).
    tex: Tex,
    anim: Option<UserAnim>,
}

/// An animated GIF used as a shader texture (MD3): the frame shown is picked by the time of the frame.
struct UserAnim {
    /// Never empty.
    frames: Vec<Tex>,
    /// Seconds per frame, same length as `frames`.
    delays: Vec<f64>,
}

impl UserTex {
    fn view_at(&self, time: f64) -> &wgpu::TextureView {
        match &self.anim {
            Some(a) => &a.frames[crate::sprite::gif_frame_index(&a.delays, time)].view,
            None => &self.tex.view,
        }
    }
}

/// The shader-side state of one preset: its warp / composite pass and the user textures their samplers read (MD2's
/// `m_shaders`, and `m_OldShaders` during a blend).
struct ShaderSet {
    warp: Option<ShaderPass>,
    comp: Option<ShaderPass>,
    user_tex: HashMap<String, UserTex>,
}

/// A preset's installed shaders and textures, detached from the renderer ([`Renderer::take_shader_state`]).
pub struct ShaderState(ShaderSet);

/// How a user-shader pass writes its target ([`Renderer::run_shader_pass`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PassBlend {
    /// Clear the target, opaque draw.
    Replace,
    /// Keep the target, `SRCALPHA / INVSRCALPHA`.
    SrcAlpha,
    /// Keep the target, `INVSRCALPHA / SRCALPHA`.
    InvSrcAlpha,
}

/// Row pitch of a buffer copy of `row_bytes` bytes per row (wgpu requires multiples of 256).
fn padded_bytes_per_row(row_bytes: u32) -> u32 {
    row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

/// Whether a GPU buffer can be mapped and waited for inside a call. WebGPU maps buffers only once control returns
/// to the browser's event loop, so such a wait never completes there: the web build reads nothing back and the
/// motion vector pass uses the CPU port of the warp formula (`cpu_warp_uv`) instead of the GPU's own UVs.
pub(crate) const SYNC_READBACK: bool = cfg!(not(target_arch = "wasm32"));

/// Read an `Rgba8Unorm`-sized (4 bytes per texel) `w` x `h` texture back as tightly packed RGBA8 (row 0 = top).
/// Submits its own copy and waits for it, so the frame's command buffers must have been submitted. Always an error
/// on the web ([`SYNC_READBACK`]).
pub fn readback_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Result<Vec<u8>, RenderError> {
    if !SYNC_READBACK {
        return Err(RenderError::Readback(
            "synchronous GPU readback is not available on the web".to_owned(),
        ));
    }
    let bpr = padded_bytes_per_row(w * 4);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(bpr) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback"),
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
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
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| RenderError::Readback(format!("{e:?}")))?;
    rx.recv()
        .map_err(|e| RenderError::Readback(e.to_string()))?
        .map_err(|e| RenderError::Readback(e.to_string()))?;
    let data = slice
        .get_mapped_range()
        .map_err(|e| RenderError::Readback(e.to_string()))?;
    let row = w as usize * 4;
    let mut out = Vec::with_capacity(row * h as usize);
    for y in 0..h {
        let s = y as usize * bpr as usize;
        out.extend_from_slice(&data[s..s + row]);
    }
    drop(data);
    buf.unmap();
    Ok(out)
}

/// Index buffer of the GPU warp mesh ([`geom::warp_gpu_indices`]) and its index count; only projectM's rules draw it.
/// Constant for a mesh size, so built when the mesh changes rather than every frame.
fn warp_index_buffer(
    device: &wgpu::Device,
    geom: &MeshGeometry,
    rules: &CompatRules,
) -> Option<(wgpu::Buffer, u32)> {
    if !rules.warp_uv_on_gpu {
        return None;
    }
    let idx = geom::warp_gpu_indices(geom, rules.warp_diag_flipped);
    let buf = buffer_init(device, "warp indices", &idx, wgpu::BufferUsages::INDEX);
    Some((buf, idx.len() as u32))
}

/// Check a render size against zero and the device's 2D texture limit.
fn check_size(device: &wgpu::Device, width: u32, height: u32) -> Result<(), RenderError> {
    if width == 0 || height == 0 {
        return Err(RenderError::Invalid(format!(
            "render size must be non-zero, got {width}x{height}"
        )));
    }
    let max = device.limits().max_texture_dimension_2d;
    if width > max || height > max {
        return Err(RenderError::Invalid(format!(
            "render size {width}x{height} exceeds the device texture limit of {max}"
        )));
    }
    Ok(())
}

/// The mesh geometry of `cfg`, after checking that its size and mesh are usable.
fn checked_geometry(
    device: &wgpu::Device,
    cfg: &RenderConfig,
) -> Result<MeshGeometry, RenderError> {
    check_size(device, cfg.width, cfg.height)?;
    MeshGeometry::new(cfg.mesh[0], cfg.mesh[1], cfg.width, cfg.height)
}

// ---------------------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------------------

/// The MilkDrop 2 frame renderer. See the crate docs for the pass order.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    cfg: RenderConfig,
    rules: CompatRules,
    geom: MeshGeometry,
    /// Triangles of the GPU warp mesh over the unique vertices of [`geom::build_warp_gpu_vertices`].
    warp_indices: Option<(wgpu::Buffer, u32)>,
    /// See [`Renderer::blit`].
    blit_bg: std::cell::RefCell<Option<wgpu::BindGroup>>,
    /// Bind groups of blur passes 1.. keyed by their uniform values (see `Renderer::run_blur`); dropped on `resize`.
    blur_bg: std::cell::RefCell<Vec<Option<BlurBindGroup>>>,
    rot: RotParams,
    pipes: std::sync::Arc<Pipelines>,
    /// `m_lpVS[0..2]` ping-pong; `main[main_idx]` holds the latest `main` image.
    main: [Tex; 2],
    main_idx: usize,
    warped: Tex,
    final_tex: Tex,
    blur: Vec<Tex>,
    noise: Vec<Tex>,
    user_tex: HashMap<String, UserTex>,
    /// MD3 sprite images by id ([`Renderer::set_sprite_image`]).
    sprite_images: HashMap<u32, SpriteImage>,
    /// Time of the frame being recorded: animated textures pick their frame from it.
    anim_time: f64,
    dummy_2d: Tex,
    dummy_3d: Tex,
    /// `sampler_fft` ([`TexSource::Fft`]): the smoothed MilkDrop 3 shader FFT (512 x 1), or under
    /// `CompatRules::md2_shader_inputs` BeatDrop's `m_lpFFTTexture` (1024 x 2: spectrum, peak hold). Owned per renderer
    /// (a [`Renderer::sibling`] has its own) and replaced whenever a frame carries `FrameInput::ext.fft` /
    /// `FrameInput::ext.beatdrop`.
    fft: Tex,
    /// BeatDrop's `m_lpWaveTexture` (480 x 2: left, right), only under `CompatRules::md2_shader_inputs`. The
    /// translator names it by the user texture `"wave"` (the frozen ABI has no source for it): see
    /// [`Renderer::engine_wave`].
    wave: Option<Tex>,
    /// Scratch attachment for the user warp shader's second output ([`with_uv_output`]); never read.
    warp_sink: Tex,
    warp_shader: Option<ShaderPass>,
    comp_shader: Option<ShaderPass>,
    /// MD2's `m_OldShaders` (+ the old preset's textures): the old preset's shaders while a preset blend runs.
    old_shaders: Option<ShaderSet>,
    /// Warp UV grid of the previous frame (motion vector reverse propagation).
    prev_warp_uv: Option<Vec<[f32; 2]>>,
    /// The previous frame's warp vertex-shader inputs, to evaluate its UVs on the GPU (motion vector lookups).
    prev_warp_gpu: Option<crate::frame::WarpGpu>,
    /// Persistent readback of the GPU's warp UVs ([`Renderer::gpu_warp_vertex_uv`]).
    uv_readback: Option<UvReadback>,
    /// projectM's motion vector mesh (stale vertices, index count).
    mv_state: MotionVectorState,
    frames_since_resize: u32,
    /// projectM's persistent main-wave buffer tail (see [`wavegen::PmWaveTail`]); zero at preset load.
    main_wave_tail: wavegen::PmWaveTail,
    /// Working buffers of the per-frame geometry builders.
    scratch: FrameScratch,
    /// Highest blur level used by the last rendered frame's shaders (for tests / diagnostics).
    last_blur_levels: u8,
    /// Minimum blur level computed every frame ([`Renderer::set_min_blur_levels`]).
    #[cfg(feature = "diagnostics")]
    min_blur_levels: u8,
    /// One-shot override of the composite `sampler_main` ([`Renderer::inject_comp_source`]).
    #[cfg(feature = "diagnostics")]
    comp_source_override: Option<Tex>,
}

/// Hooks for comparing against a reference render (feature `diagnostics`): replacing internal images, forcing
/// per-frame state and reading intermediate images back.
#[cfg(feature = "diagnostics")]
impl Renderer {
    /// Replace a built-in noise texture (e.g. with a reference render's noise). 2D: `size*size*4` bytes,
    /// row 0 first; volumes: `size^3*4`, slice-major. Sizes are MD2's (see [`crate::noise::noise_spec`]).
    pub fn inject_noise(&mut self, which: NoiseTex, data: &[u8]) -> Result<(), RenderError> {
        let (size, _, vol) = noise::noise_spec(which);
        let expect = if vol {
            (size * size * size * 4) as usize
        } else {
            (size * size * 4) as usize
        };
        if data.len() != expect {
            return Err(RenderError::Invalid(format!(
                "{which:?}: expected {expect} bytes, got {}",
                data.len()
            )));
        }
        self.noise[which.index()] = if vol {
            upload_3d(&self.device, &self.queue, size, data, "noise_vol")
        } else {
            upload_2d(&self.device, &self.queue, size, size, data, true, "noise")
        };
        Ok(())
    }

    /// Replace the previous-frame image (`main`, what the next warp samples).
    pub fn inject_main(&mut self, rgba8: &[u8]) -> Result<(), RenderError> {
        let t = &self.main[self.main_idx];
        self.write_rgba8(t, rgba8, "inject_main")
    }

    /// Use `rgba8` as the composite source (a comp shader's `sampler_main`, or the no-shader composite's
    /// texture) for the next rendered frame only. Does not touch the stored `main` image, so the `main` stage
    /// read-back stays the renderer's own.
    pub fn inject_comp_source(&mut self, rgba8: &[u8]) -> Result<(), RenderError> {
        let (w, h) = (self.cfg.width, self.cfg.height);
        let t = create_target(&self.device, w, h, "comp source override");
        self.write_rgba8(&t, rgba8, "inject_comp_source")?;
        self.comp_source_override = Some(t);
        Ok(())
    }

    /// Make the next frame's motion-vector pass use this warp UV grid (the previous frame's UVs).
    pub fn inject_prev_warp_uv(&mut self, uv: Option<Vec<[f32; 2]>>) {
        self.prev_warp_uv = uv;
    }

    /// The image the last frame's warp pass sampled (previous `main` + this frame's motion vectors).
    /// Valid until the next `render_frame`.
    pub fn read_warp_source(&self) -> Result<(u32, u32, Vec<u8>), RenderError> {
        self.read_texture(&self.main[1 - self.main_idx])
    }

    /// Compute at least `levels` (0..=3) blur levels every frame even if no shader samples them, so
    /// `Stage::Blur1..3` can be read back.
    pub fn set_min_blur_levels(&mut self, levels: u8) {
        self.min_blur_levels = levels.min(3);
    }

    /// Per-preset random parameters (rotation matrices, hue offsets).
    pub fn rot_params(&self) -> &RotParams {
        &self.rot
    }

    /// Replace the per-preset random parameters (they otherwise stay as drawn from the seed).
    pub fn set_rot_params(&mut self, p: RotParams) {
        self.rot = p;
    }

    /// Upload a full-size RGBA8 image (row 0 = top) into `t`.
    fn write_rgba8(&self, t: &Tex, rgba8: &[u8], what: &str) -> Result<(), RenderError> {
        let need = t.w as usize * t.h as usize * 4;
        if rgba8.len() != need {
            return Err(RenderError::Invalid(format!(
                "{what}: need {need} bytes, got {}",
                rgba8.len()
            )));
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &t.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba8,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(t.w * 4),
                rows_per_image: Some(t.h),
            },
            wgpu::Extent3d {
                width: t.w,
                height: t.h,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }
}

impl Renderer {
    /// Create a renderer. Textures are cleared to black; noise textures are generated from
    /// `cfg.seed` (MD2 algorithm). `device`/`queue` are cloned handles (cheap).
    ///
    /// # Errors
    ///
    /// [`RenderError::Invalid`] if the size is zero or the mesh is not an even grid of at least 2x2 cells.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cfg: RenderConfig,
    ) -> Result<Self, RenderError> {
        let geom = checked_geometry(device, &cfg)?;
        let pipes = std::sync::Arc::new(Pipelines::new(device, cfg.output_format));
        Ok(Self::assemble(device, queue, cfg, geom, pipes, None))
    }

    /// A second renderer with the same configuration that shares this one's pipelines and
    /// (immutable) noise textures but has its own images, shaders and user textures: what the
    /// engine needs to run two presets at once while they transition. Cheaper than
    /// [`Renderer::new`] (no pipeline compilation, no noise generation), but it still allocates the render targets.
    pub fn sibling(&self) -> Self {
        self.sibling_with_compat(self.cfg.compat)
    }

    /// [`Renderer::sibling`] that follows another compatibility mode: same size, mesh, stroke setting, shared
    /// pipelines and noise (neither depends on the mode), but its own images, shaders and textures, and the rules of
    /// `compat`. What the engine builds into when an automatically selected mode changes.
    pub fn sibling_with_compat(&self, compat: Compat) -> Self {
        let mut cfg = self.cfg.clone();
        cfg.compat = compat;
        let mut r = Self::assemble(
            &self.device,
            &self.queue,
            cfg,
            self.geom.clone(),
            std::sync::Arc::clone(&self.pipes),
            Some(self.noise.clone()),
        );
        #[cfg(feature = "diagnostics")]
        {
            r.min_blur_levels = self.min_blur_levels;
        }
        r.rot = self.rot.clone();
        r
    }

    fn assemble(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cfg: RenderConfig,
        geom: MeshGeometry,
        pipes: std::sync::Arc<Pipelines>,
        shared_noise: Option<Vec<Tex>>,
    ) -> Self {
        let rules = cfg.compat.rules();

        let (w, h) = (cfg.width, cfg.height);
        let main = [
            create_target(device, w, h, "main0"),
            create_target(device, w, h, "main1"),
        ];
        let warped = create_target(device, w, h, "warped");
        let final_tex = create_target(device, w, h, "final");
        let blur = blur::blur_tex_sizes(w, h)
            .iter()
            .enumerate()
            .map(|(i, &(bw, bh))| create_target(device, bw, bh, &format!("blur{i}")))
            .collect();

        let rng = Rng::new(cfg.seed);
        let noise_tex = match shared_noise {
            Some(n) => n,
            None => {
                let mut noise_rng = rng.fork(0x6E6F_6973);
                let mut noise_tex = Vec::new();
                for t in noise::ALL_NOISE {
                    let data = noise::generate(t, &mut noise_rng, &rules);
                    let (size, _, vol) = noise::noise_spec(t);
                    noise_tex.push(if vol {
                        upload_3d(device, queue, size, &data, "noise_vol")
                    } else {
                        upload_2d(device, queue, size, size, &data, true, "noise")
                    });
                }
                noise_tex
            }
        };
        let rot = RotParams::randomize(&mut rng.fork(0x7072_6573));
        let dummy_2d = upload_2d(device, queue, 1, 1, &[0, 0, 0, 255], false, "dummy2d");
        let dummy_3d = upload_3d(device, queue, 1, &[0, 0, 0, 255], "dummy3d");
        let (fft_w, fft_h) = if rules.md2_shader_inputs {
            (crate::BEATDROP_FFT_BINS, 2)
        } else {
            (FFT_BINS, 1)
        };
        let fft = create_r32f_tex(device, queue, "fft", fft_w, fft_h);
        let wave = rules
            .md2_shader_inputs
            .then(|| create_r32f_tex(device, queue, "wave", crate::BEATDROP_WAVE_SAMPLES, 2));

        let warp_indices = warp_index_buffer(device, &geom, &rules);
        let r = Renderer {
            device: device.clone(),
            queue: queue.clone(),
            cfg,
            rules,
            geom,
            warp_indices,
            blit_bg: Default::default(),
            blur_bg: Default::default(),
            rot,
            pipes,
            main,
            main_idx: 0,
            warped,
            final_tex,
            blur,
            noise: noise_tex,
            user_tex: HashMap::new(),
            sprite_images: HashMap::new(),
            anim_time: 0.0,
            dummy_2d,
            dummy_3d,
            fft,
            wave,
            warp_sink: create_sink(device, w, h),
            warp_shader: None,
            comp_shader: None,
            old_shaders: None,
            prev_warp_uv: None,
            prev_warp_gpu: None,
            uv_readback: None,
            mv_state: MotionVectorState::default(),
            frames_since_resize: 0,
            main_wave_tail: Default::default(),
            scratch: FrameScratch::default(),
            last_blur_levels: 0,
            #[cfg(feature = "diagnostics")]
            min_blur_levels: 0,
            #[cfg(feature = "diagnostics")]
            comp_source_override: None,
        };
        r.clear_all();
        r
    }

    // ---- configuration ----------------------------------------------------------------

    /// The configuration in use (size and mesh follow [`Renderer::resize`] / [`Renderer::set_mesh_size`]).
    pub fn config(&self) -> &RenderConfig {
        &self.cfg
    }

    /// The warp-mesh geometry (vertex positions, `rad`, `ang`) — what the engine needs to run per-pixel code.
    pub fn mesh_geometry(&self) -> &MeshGeometry {
        &self.geom
    }

    /// The reference behaviour followed where MilkDrop 2 and projectM differ.
    pub fn compat(&self) -> Compat {
        self.cfg.compat
    }

    /// The rasteriser actually used for lines and points: the configured one, except that
    /// `Compat::MilkDrop2` / `Compat::MilkDrop3` always draw D3D9-style thin quads.
    pub fn stroke_raster(&self) -> StrokeRaster {
        self.stroke_mode()
    }

    fn stroke_mode(&self) -> StrokeRaster {
        if self.rules.gl_strokes {
            self.cfg.stroke
        } else {
            StrokeRaster::Quads
        }
    }

    /// Switch the line / point rasteriser (see [`StrokeRaster`]); takes effect on the next frame.
    pub fn set_stroke_raster(&mut self, s: StrokeRaster) {
        self.cfg.stroke = s;
    }

    /// Resize the internal image (and output). Clears every texture and forgets the previous
    /// frame (`m_nFramesSinceResize = 0`). Noise and user textures are kept.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        check_size(&self.device, width, height)?;
        self.geom = MeshGeometry::new(self.cfg.mesh[0], self.cfg.mesh[1], width, height)?;
        self.cfg.width = width;
        self.cfg.height = height;
        let d = &self.device;
        self.main = [
            create_target(d, width, height, "main0"),
            create_target(d, width, height, "main1"),
        ];
        self.warped = create_target(d, width, height, "warped");
        self.warp_sink = create_sink(d, width, height);
        self.final_tex = create_target(d, width, height, "final");
        *self.blit_bg.borrow_mut() = None;
        self.blur_bg.borrow_mut().clear();
        self.blur = blur::blur_tex_sizes(width, height)
            .iter()
            .enumerate()
            .map(|(i, &(bw, bh))| create_target(d, bw, bh, &format!("blur{i}")))
            .collect();
        self.main_idx = 0;
        self.forget_prev_warp();
        self.mv_state.reset();
        self.frames_since_resize = 0;
        self.clear_all();
        Ok(())
    }

    /// Change the warp-mesh resolution (cells per axis; the caller applies projectM's clamping, see
    /// `lactea::Engine::set_mesh_size`). The previous frame's warp grid and the motion-vector state
    /// belong to the old mesh and are forgotten; the images are kept.
    pub fn set_mesh_size(&mut self, mesh: [u32; 2]) -> Result<(), RenderError> {
        if self.cfg.mesh == mesh {
            return Ok(());
        }
        self.geom = MeshGeometry::new(mesh[0], mesh[1], self.cfg.width, self.cfg.height)?;
        self.cfg.mesh = mesh;
        self.warp_indices = warp_index_buffer(&self.device, &self.geom, &self.rules);
        self.forget_prev_warp();
        self.mv_state.reset();
        Ok(())
    }

    /// Change the sub-texel offset added to warp UVs (`projectm_set_texel_offset`); takes effect on
    /// the next frame.
    pub fn set_texel_offset(&mut self, offset: [f32; 2]) {
        self.cfg.texel_offset = offset;
    }

    /// Start a preset from a black frame (projectM `start_clean`): clears every image and
    /// forgets the previous frame's warp grid; drops all user textures (the next preset
    /// registers its own). Noise textures are kept.
    pub fn reset_images(&mut self) {
        self.main_idx = 0;
        self.forget_prev_warp();
        self.mv_state.reset();
        self.main_wave_tail = Default::default();
        self.frames_since_resize = 0;
        self.user_tex.clear();
        self.sprite_images.clear();
        self.old_shaders = None;
        self.clear_all();
    }

    fn clear_all(&self) {
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("clear"),
            });
        let mut all: Vec<&Tex> = vec![&self.main[0], &self.main[1], &self.warped, &self.final_tex];
        all.extend(self.blur.iter());
        for t in all {
            let _ = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: if self.rules.main_alpha { 0.0 } else { 1.0 },
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        self.queue.submit(Some(enc.finish()));
    }

    // ---- shaders ----------------------------------------------------------------------

    /// Install (or remove, `None` = MD2 `WarpedBlit_NoShaders`) the warp shader.
    pub fn set_warp_shader(&mut self, shader: Option<CompiledShader>) -> Result<(), RenderError> {
        self.warp_shader = match shader {
            Some(s) => Some(self.build_shader_pass(&s, "warp")?),
            None => None,
        };
        Ok(())
    }

    /// Install (or remove, `None` = MD2 `ShowToUser_NoShaders`) the composite shader.
    pub fn set_comp_shader(&mut self, shader: Option<CompiledShader>) -> Result<(), RenderError> {
        self.comp_shader = match shader {
            Some(s) => Some(self.build_shader_pass(&s, "comp")?),
            None => None,
        };
        Ok(())
    }

    /// Detach the installed warp / composite shaders and the user textures (MD2's `m_shaders`), leaving the renderer
    /// without any. The engine uses this to build the next preset's shaders next to the running ones during a preset
    /// blend ([`Renderer::set_old_shader_state`]) and to put the old ones back when that load fails.
    pub fn take_shader_state(&mut self) -> ShaderState {
        ShaderState(ShaderSet {
            warp: self.warp_shader.take(),
            comp: self.comp_shader.take(),
            user_tex: std::mem::take(&mut self.user_tex),
        })
    }

    /// Reinstall a state taken with [`Renderer::take_shader_state`] (replacing whatever is installed).
    pub fn restore_shader_state(&mut self, state: ShaderState) {
        let ShaderSet {
            warp,
            comp,
            user_tex,
        } = state.0;
        self.warp_shader = warp;
        self.comp_shader = comp;
        self.user_tex = user_tex;
    }

    /// MD2's `m_OldShaders`: keep `state` (the outgoing preset's shaders and textures) alive for the old half of a
    /// preset blend; `None` releases them (`m_OldShaders.*.Clear()` when the blend ends).
    pub fn set_old_shader_state(&mut self, state: Option<ShaderState>) {
        self.old_shaders = state.map(|s| s.0);
    }

    /// Whether old shaders are being kept for a preset blend.
    pub fn has_old_shaders(&self) -> bool {
        self.old_shaders.is_some()
    }

    /// `(warp, composite)`: whether the installed shaders are user / default pixel shaders (`m_nWarpPSVersion > 0`
    /// style flags of the *new* preset and of the old one, if one is kept).
    pub fn shader_flags(&self) -> ((bool, bool), (bool, bool)) {
        let old = self
            .old_shaders
            .as_ref()
            .map_or((false, false), |o| (o.warp.is_some(), o.comp.is_some()));
        (
            (self.warp_shader.is_some(), self.comp_shader.is_some()),
            old,
        )
    }

    fn build_shader_pass(
        &self,
        s: &CompiledShader,
        label: &str,
    ) -> Result<ShaderPass, RenderError> {
        if s.interface.textures.len() > lactea_abi::MAX_PASS_TEXTURES {
            return Err(RenderError::Invalid(format!(
                "{} textures > {}",
                s.interface.textures.len(),
                lactea_abi::MAX_PASS_TEXTURES
            )));
        }
        let device = &self.device;
        // group(1) layout from the interface
        // the fold functions read `pu` in the vertex stage
        let pass_ubo_stages = if s.folds.is_empty() {
            wgpu::ShaderStages::FRAGMENT
        } else {
            wgpu::ShaderStages::VERTEX_FRAGMENT
        };
        let mut entries = vec![ubo_entry(
            lactea_abi::BINDING_PASS_UNIFORMS,
            pass_ubo_stages,
        )];
        for (i, t) in s.interface.textures.iter().enumerate() {
            let dim = match t.dim {
                TexDim::D2 => wgpu::TextureViewDimension::D2,
                TexDim::D3 => wgpu::TextureViewDimension::D3,
            };
            // R32Float is not filterable: the translated shader only `textureLoad`s the FFT and BeatDrop's wave
            // textures (the sampler entry stays `Filtering` and is never paired with it in a `textureSample`)
            let filterable =
                !matches!(t.source, TexSource::Fft) && self.engine_wave(&t.source).is_none();
            entries.push(tex_entry(
                lactea_abi::texture_binding(i as u32),
                dim,
                filterable,
            ));
            entries.push(samp_entry(lactea_abi::sampler_binding(i as u32)));
        }

        #[cfg(not(target_arch = "wasm32"))]
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pass_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &entries,
        });
        // projectM's user warp shader also writes the interpolated warp UV to a second attachment (`texCoords`,
        // read back by the motion-vector pass). That second use of `uv` keeps Mesa's NIR from folding constants
        // into the varying's plane equation (`uv - 0.5` becomes `plane(uv) - 0.5` instead of the folded
        // `plane(uv - 0.5)`, which rounds differently); projectM's driver compiles the shader unfolded, so ours must too.
        let (wgsl, uv_sink) = match (label == "warp").then(|| with_uv_output(&s.wgsl)).flatten() {
            Some(w) => (w, true),
            None => (s.wgsl.clone(), false), // not a recognisable entry: let the shader validation report it
        };
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(wgsl.as_str().into()),
        });
        let fold_vs = (!s.folds.is_empty()).then(|| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(shaders::mesh_vs_folds(&s.folds).into()),
            })
        });
        let mesh_vs = fold_vs.as_ref().unwrap_or(&self.pipes.mesh_vs);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(&self.pipes.bgl_mu), Some(&pass_bgl)],
            immediate_size: 0,
        });
        let make_pipeline = |layout: &wgpu::PipelineLayout,
                             vs: &wgpu::ShaderModule,
                             buffers: &[Option<wgpu::VertexBufferLayout>],
                             blend: Option<wgpu::BlendState>| {
            let color = Some(wgpu::ColorTargetState {
                format: INTERNAL_FORMAT,
                blend,
                write_mask: wgpu::ColorWrites::COLOR,
            });
            let sink = Some(wgpu::ColorTargetState {
                format: WARP_SINK_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: vs,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &fs,
                    entry_point: Some(FRAGMENT_ENTRY),
                    compilation_options: Default::default(),
                    targets: &[color, sink][..if uv_sink { 2 } else { 1 }],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = make_pipeline(&layout, mesh_vs, &[Some(mesh_layout())], None);
        // MD2's preset blend draws the second pass of a shader pair alpha-blended over the first
        let alpha_pipelines = self.rules.md2_preset_blend.then(|| {
            [Blend::SrcAlpha, Blend::InvSrcAlpha]
                .map(|b| make_pipeline(&layout, mesh_vs, &[Some(mesh_layout())], blend_state(b)))
        });
        let warp_pipeline = if label == "warp" {
            let warp_vs_sm = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("warp_vs"),
                source: wgpu::ShaderSource::Wgsl(
                    if s.folds.is_empty() {
                        shaders::warp_vs(2)
                    } else {
                        shaders::warp_vs_folds(2, &s.folds)
                    }
                    .into(),
                ),
            });
            let warp_layout_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("warp gpu"),
                bind_group_layouts: &[
                    Some(&self.pipes.bgl_mu),
                    Some(&pass_bgl),
                    Some(&self.pipes.bgl_warp_vs),
                ],
                immediate_size: 0,
            });
            Some(make_pipeline(
                &warp_layout_pl,
                &warp_vs_sm,
                &[Some(warp_layout())],
                None,
            ))
        } else {
            None
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(RenderError::Shader(err.to_string()));
        }
        Ok(ShaderPass {
            pipeline,
            warp_pipeline,
            pass_bgl,
            interface: s.interface.clone(),
            uv_sink,
            alpha_pipelines,
        })
    }

    // ---- textures ---------------------------------------------------------------------

    /// Register a user texture (`sampler_<name>`): RGBA8, row 0 = top, with a CPU mip chain.
    /// `name` is matched case-insensitively against `TexSource::User`.
    pub fn set_user_texture(
        &mut self,
        name: &str,
        w: u32,
        h: u32,
        rgba8: &[u8],
    ) -> Result<(), RenderError> {
        check_frames(
            &format!("user texture {name}"),
            self.device.limits().max_texture_dimension_2d,
            w,
            h,
            &[rgba8],
        )?;
        let tex = upload_2d(&self.device, &self.queue, w, h, rgba8, true, name);
        self.user_tex
            .insert(name.to_ascii_lowercase(), UserTex { tex, anim: None });
        Ok(())
    }

    /// Register an animated user texture (an animated GIF as `sampler_<name>`, `Compat::MilkDrop3`): `frames` are
    /// `w * h * 4` RGBA8 images (row 0 = top), `delays` their durations in seconds. Every draw shows the frame
    /// [`gif_frame_index`](crate::gif_frame_index) picks for the time of the frame being rendered. No mip chain.
    pub fn set_user_animation(
        &mut self,
        name: &str,
        w: u32,
        h: u32,
        frames: &[&[u8]],
        delays: &[f64],
    ) -> Result<(), RenderError> {
        check_frames(
            name,
            self.device.limits().max_texture_dimension_2d,
            w,
            h,
            frames,
        )?;
        if delays.len() != frames.len() {
            return Err(RenderError::Invalid(format!(
                "animated texture {name}: {} frames but {} delays",
                frames.len(),
                delays.len()
            )));
        }
        let tex: Vec<Tex> = frames
            .iter()
            .map(|f| upload_2d(&self.device, &self.queue, w, h, f, false, name))
            .collect();
        self.user_tex.insert(
            name.to_ascii_lowercase(),
            UserTex {
                tex: tex[0].clone(),
                anim: Some(UserAnim {
                    frames: tex,
                    delays: delays.to_vec(),
                }),
            },
        );
        Ok(())
    }

    /// Register the image of an MD3 sprite under `id` ([`SpriteDraw::image`](crate::SpriteDraw::image)), replacing
    /// any image with that id. `frames` are `w * h * 4` RGBA8 images (row 0 = top, straight alpha, colour key already
    /// applied); a static image has one frame and `mips` (full chain, trilinear sampling), a GIF has no mips.
    pub fn set_sprite_image(
        &mut self,
        id: u32,
        w: u32,
        h: u32,
        frames: &[&[u8]],
        mips: bool,
    ) -> Result<(), RenderError> {
        check_frames(
            "sprite",
            self.device.limits().max_texture_dimension_2d,
            w,
            h,
            frames,
        )?;
        // a sprite is about to be drawn: build the pipelines now, not as a hitch in the first frame that draws one
        self.pipes.sprites(&self.device);
        let views = frames
            .iter()
            .map(|f| upload_2d(&self.device, &self.queue, w, h, f, mips, "sprite").view)
            .collect();
        self.sprite_images.insert(id, SpriteImage { frames: views });
        Ok(())
    }

    /// Forget every sprite image.
    pub fn clear_sprite_images(&mut self) {
        self.sprite_images.clear();
    }

    /// Forget every user texture (`set_user_texture` / `set_user_animation`); shaders sampling one see the
    /// placeholder until it is registered again.
    pub fn clear_user_textures(&mut self) {
        self.user_tex.clear();
    }

    /// Whether an image is registered under `id`.
    pub fn has_sprite_image(&self, id: u32) -> bool {
        self.sprite_images.contains_key(&id)
    }

    /// Forget the image registered under `id`, if any.
    pub fn remove_sprite_image(&mut self, id: u32) {
        self.sprite_images.remove(&id);
    }
    /// Draw sprites (any layer value) onto `target`, an `Rgba8Unorm` view: the sprite pass on its own, for hosts
    /// that composite themselves and for tests. Draws whose image or frame is not registered are skipped.
    pub fn draw_sprites(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        draws: &[SpriteDraw],
    ) {
        let refs: Vec<&SpriteDraw> = draws.iter().collect();
        self.sprite_pass(encoder, target, &refs);
    }

    /// Draw `draws` into the image the next frame's warp reads (projectM's `burn`), the ones with `burn` set only:
    /// call after [`Renderer::render_frame_to_output`], when that image is the one just produced.
    pub fn burn_sprites(&self, encoder: &mut wgpu::CommandEncoder, draws: &[SpriteDraw]) {
        let burn: Vec<&SpriteDraw> = draws.iter().filter(|d| d.burn).collect();
        if !burn.is_empty() {
            self.sprite_pass(encoder, &self.main[self.main_idx].view, &burn);
        }
    }

    /// Draw `draws` onto `target`, a view of the host's output (`RenderConfig::output_format`), loaded not cleared:
    /// sprites over a finished frame, e.g. over a soft cut's blend.
    pub fn draw_sprites_to_output(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        draws: &[SpriteDraw],
    ) {
        let refs: Vec<&SpriteDraw> = draws.iter().collect();
        self.pipes
            .sprites_for(&self.device, self.cfg.output_format)
            .draw(
                &self.device,
                encoder,
                target,
                &self.pipes.bgl_tex,
                self.pipes.sampler(Filter::Linear, Address::Wrap, false),
                &self.sprite_images,
                &refs,
            );
    }

    /// Read a stage back as `(width, height, RGBA8)` (row 0 = top). The command buffers of the
    /// frame must have been submitted. `Stage::Main` is the image the next frame's warp samples;
    /// `Warped` the image right after the warp pass; `Blur1..3` the user-visible blur levels
    /// (blur textures 1, 3, 5); `Final` the composite output.
    pub fn read_stage(&self, stage: Stage) -> Result<(u32, u32, Vec<u8>), RenderError> {
        let t = match stage {
            Stage::Final => &self.final_tex,
            Stage::Main => &self.main[self.main_idx],
            Stage::Warped => &self.warped,
            Stage::Blur1 => &self.blur[1],
            Stage::Blur2 => &self.blur[3],
            Stage::Blur3 => &self.blur[5],
        };
        self.read_texture(t)
    }

    fn read_texture(&self, t: &Tex) -> Result<(u32, u32, Vec<u8>), RenderError> {
        let data = readback_rgba8(&self.device, &self.queue, &t.texture, t.w, t.h)?;
        Ok((t.w, t.h, data))
    }

    // ---- rendering --------------------------------------------------------------------

    /// Blur levels forced on by a diagnostic request (0 without the `diagnostics` feature).
    fn diagnostic_blur_levels(&self) -> u8 {
        #[cfg(feature = "diagnostics")]
        {
            self.min_blur_levels.min(3)
        }
        #[cfg(not(feature = "diagnostics"))]
        {
            0
        }
    }

    /// The image a composite pass reads as its `sampler_main`: `regular`, or the injected one.
    fn comp_source_view<'a>(&'a self, regular: &'a wgpu::TextureView) -> &'a wgpu::TextureView {
        #[cfg(feature = "diagnostics")]
        if let Some(t) = &self.comp_source_override {
            return &t.view;
        }
        regular
    }

    /// Highest blur level (0..=3) the installed shaders require (including the old preset's while a preset blend
    /// keeps them: MD2's `ApplyShaderParams` counts every pass it applies, also `m_OldShaders`).
    pub fn blur_levels_required(&self) -> u8 {
        let level = |p: &Option<ShaderPass>| p.as_ref().map_or(0, |s| s.interface.max_blur_level);
        let mut m = level(&self.warp_shader).max(level(&self.comp_shader));
        if let Some(o) = &self.old_shaders {
            m = m.max(level(&o.warp)).max(level(&o.comp));
        }
        m.min(3)
    }

    /// Forget the previous frame's warp grid, vertex inputs and their pending UV readback.
    fn forget_prev_warp(&mut self) {
        self.prev_warp_uv = None;
        self.prev_warp_gpu = None;
        if let Some(rb) = &mut self.uv_readback {
            rb.pending = None;
        }
    }

    /// Submit the UV readback of `prev_warp_gpu` (the vertex inputs of the frame just recorded), or drop the pending
    /// one if there are none.
    fn submit_uv_readback(&mut self) {
        let Some(wg) = self.prev_warp_gpu.as_ref() else {
            if let Some(rb) = &mut self.uv_readback {
                rb.pending = None;
            }
            return;
        };
        let (w, h) = (self.geom.gx + 1, self.geom.gy + 1);
        let verts = geom::build_warp_gpu_vertices(&self.geom, &wg.vars);
        let toff = self.texel_offset_uv();
        let mut vu = self.warp_vs_uniforms(wg, toff, 1.0);
        vu.diffuse = [w as f32, h as f32, 0.0, 0.0];
        if !matches!(&self.uv_readback, Some(rb) if rb.w == w && rb.h == h) {
            self.uv_readback = Some(UvReadback::new(&self.device, &self.pipes, w, h));
        }
        if let Some(rb) = &mut self.uv_readback {
            rb.submit(&self.device, &self.queue, &self.pipes, &verts, &vu, toff);
        }
    }

    /// The GPU's own per-vertex warp UVs of `prev_warp_gpu` (`warp_vs` evaluated on the device, lactea mesh index
    /// order). projectM's motion vector lookup reads the interpolation of *these* (`RG16F` UV map written by the warp
    /// pass), so the CPU port of the formula (`warp_mesh`) differs from it by the GPU's `pow` / `sin` / `cos`
    /// approximations and fusion.
    ///
    /// Normally the readback was submitted at the end of the previous frame ([`Renderer::submit_uv_readback`]); it
    /// is only recorded here (and waited for) when that frame did not draw motion vectors or the texel offset
    /// changed since.
    fn gpu_warp_vertex_uv(&mut self) -> Result<Vec<[f32; 2]>, RenderError> {
        let toff = self.texel_offset_uv();
        let submitted = self
            .uv_readback
            .as_ref()
            .and_then(|rb| rb.pending.as_ref())
            .is_some_and(|(_, t)| *t == toff);
        if !submitted {
            self.submit_uv_readback();
        }
        let Some(rb) = self.uv_readback.as_mut() else {
            return Err(RenderError::Readback(
                "no previous warp vertex inputs".to_owned(),
            ));
        };
        let uv = rb.collect(&self.device, &self.geom);
        if uv.is_err() {
            self.uv_readback = None;
        }
        uv
    }

    fn texel_offset_uv(&self) -> [f32; 2] {
        warp_mesh::texel_offset_uv(self.cfg.texel_offset, self.cfg.width, self.cfg.height)
    }

    /// Default warp UVs when the engine supplies none: identity motion.
    fn default_warp_uv(&self, f: &FrameInput) -> Vec<[f32; 2]> {
        let anim = WarpAnim::new(f.time as f32, 1.0, 1.0);
        let v = VertexVars {
            warp: 0.0,
            ..Default::default()
        };
        warp_mesh::warp_mesh_uniform(&self.geom, &v, &anim, self.texel_offset_uv())
    }

    /// The CPU warp UV grid of `wg` (`warp_mesh`, as the engine computes it for a renderer without a GPU warp stage):
    /// the fallback of a motion vector lookup whose GPU readback failed.
    fn cpu_warp_uv(&self, wg: &crate::frame::WarpGpu) -> Vec<[f32; 2]> {
        let toff = self.texel_offset_uv();
        wg.vars
            .iter()
            .enumerate()
            .map(|(n, vv)| warp_mesh::warp_vertex_uv(&self.geom, n, vv, &wg.anim, toff))
            .collect()
    }

    /// Record one frame into `encoder`, ending with the composite blitted to `target`.
    ///
    /// All per-frame GPU data is baked into buffers at record time, so frames may be recorded in
    /// any order, but the encoder must be submitted before [`Renderer::read_stage`] is called.
    /// `target` must be a view of a `cfg.width x cfg.height` texture of `cfg.output_format`.
    ///
    /// Fails with [`RenderError::Invalid`] (recording nothing) if `input.warp_uv` does not match the mesh or
    /// `input.md2_blend` is set without `Compat::MilkDrop2` / `Compat::MilkDrop3`.
    pub fn render_frame(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        target: &wgpu::TextureView,
    ) -> Result<(), RenderError> {
        self.render_frame_inner(encoder, input, Some(target))
    }

    /// Like [`Renderer::render_frame`] but stops after the composite: the finished image stays in
    /// the renderer ([`Renderer::output_view`]) and nothing is written to a host target. Used for
    /// presets that are blended into the frame by a transition.
    pub fn render_frame_to_output(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
    ) -> Result<(), RenderError> {
        self.render_frame_inner(encoder, input, None)
    }

    /// The composited image of the last rendered frame (`Rgba8Unorm`, row 0 = top, the `final`
    /// stage): projectM's `Preset::OutputTexture()`.
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.final_tex.view
    }

    /// Start this preset from `src`'s composited image instead of black (projectM
    /// `DrawInitialImage`, used when `start_clean` is off): the image the first warp samples.
    /// `src` must have the same size. Call after [`Renderer::reset_images`].
    ///
    /// Fails with [`RenderError::Invalid`] if the two renderers differ in size.
    pub fn draw_initial_image(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        src: &Renderer,
    ) -> Result<(), RenderError> {
        if (self.cfg.width, self.cfg.height) != (src.cfg.width, src.cfg.height) {
            return Err(RenderError::Invalid(format!(
                "initial image size mismatch: {}x{} vs {}x{}",
                self.cfg.width, self.cfg.height, src.cfg.width, src.cfg.height
            )));
        }
        self.main_idx = 0;
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &src.final_tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.main[0].texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.cfg.width,
                height: self.cfg.height,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    /// Reject frame inputs the renderer cannot draw, before anything is recorded.
    fn check_input(&self, input: &FrameInput) -> Result<(), RenderError> {
        if let Some(u) = &input.warp_uv {
            let n = self.geom.vertex_count();
            if u.len() != n {
                return Err(RenderError::Invalid(format!(
                    "FrameInput.warp_uv has {} entries, the {}x{} mesh needs {n}",
                    u.len(),
                    self.geom.gx,
                    self.geom.gy
                )));
            }
        }
        if self.rules.warp_uv_on_gpu
            && let Some(wg) = &input.warp_gpu
            && wg.vars.len() != self.geom.vertex_count()
        {
            return Err(RenderError::Invalid(format!(
                "FrameInput.warp_gpu has {} vertices, the {}x{} mesh needs {}",
                wg.vars.len(),
                self.geom.gx,
                self.geom.gy,
                self.geom.vertex_count()
            )));
        }
        if input.md2_blend.is_some() && !self.rules.md2_preset_blend {
            return Err(RenderError::Invalid(
                "FrameInput.md2_blend needs Compat::MilkDrop2 or Compat::MilkDrop3".to_owned(),
            ));
        }
        Ok(())
    }

    // ---- fixed-function content ---------------------------------------------------------
}

const BLACK: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// Per-frame working buffers of the overlay / motion-vector builders, kept across frames so a frame
/// allocates nothing once they are warm. Contents are overwritten by every user.
#[derive(Default)]
struct FrameScratch {
    pm_wave: wavegen::PmWaveScratch,
    /// custom-wave source points / colours
    pts: Vec<[f32; 2]>,
    cols: Vec<[f32; 4]>,
    /// smoothed points / colours
    sp: Vec<[f32; 2]>,
    sc: Vec<[f32; 4]>,
    /// vertices handed to a `Batch` (dots, segments, motion vectors)
    verts: Vec<WfVertex>,
    /// one strip's segments before they are appended to `verts`
    seg: Vec<WfVertex>,
}

/// `SmoothWave` carrying per-vertex colours (`vo[j+1].Diffuse = vi[i].Diffuse`), into reusable buffers.
fn smooth_with_colors_into(
    sp: &mut Vec<[f32; 2]>,
    sc: &mut Vec<[f32; 4]>,
    pts: &[[f32; 2]],
    cols: &[[f32; 4]],
) {
    wavegen::smooth_wave_into(sp, pts);
    let n = pts.len();
    sc.clear();
    sc.reserve(sp.len());
    for c in &cols[..n - 1] {
        sc.push(*c);
        sc.push(*c);
    }
    sc.push(cols[n - 1]);
}

#[cfg(test)]
mod uv_output_tests {
    use super::with_uv_output;

    /// Regression (`sin((uv.x - 0.5) * 100)` gave 15 480 wrong pixels in 13 frames): the warp shader's
    /// fragment entry gets projectM's second `texCoords` output so `uv` has two uses, like projectM's.
    #[test]
    fn warp_entry_gets_a_second_uv_output() {
        let src = "fn f() {}\n@fragment\nfn fs_main(in: VsOut) -> @location(0) vec4<f32> {\n    return in.diffuse;\n}\n";
        let out = with_uv_output(src).unwrap();
        assert!(with_uv_output("this is not wgsl").is_none());
        assert!(out.contains("fn fs_main_color(in: VsOut) -> vec4<f32> {"));
        assert!(out.contains("fn fs_main(in: VsOut) -> WarpFsOut"));
        assert!(out.contains("o.uv = in.uv.xy;"));
        assert_eq!(out.matches("@fragment").count(), 1);
    }
}
