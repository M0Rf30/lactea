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

/// The MilkDrop 3 shader FFT texture ([`TexSource::Fft`]): [`FFT_BINS`] x 1 `R32Float`, zero until the first upload.
/// `R32Float` is not filterable without `FLOAT32_FILTERABLE`; the translated shaders `textureLoad` it and blend the
/// two neighbouring bins by hand.
fn create_fft_tex(device: &wgpu::Device, queue: &wgpu::Queue) -> Tex {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fft"),
        size: wgpu::Extent3d {
            width: FFT_BINS as u32,
            height: 1,
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
        bytemuck::cast_slice(&[0.0f32; FFT_BINS]),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(FFT_BINS as u32 * 4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: FFT_BINS as u32,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Tex {
        texture,
        view,
        w: FFT_BINS as u32,
        h: 1,
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
}

const MESH_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4, 2 => Float32x4, 3 => Float32x2];
const WF_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];

fn mesh_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<MeshVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &MESH_ATTRS,
    }
}
fn wf_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<WfVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &WF_ATTRS,
    }
}

const WARP_ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x2, 4 => Float32x2, 5 => Float32x2
];

fn warp_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<WarpVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &WARP_ATTRS,
    }
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

fn sampler(device: &wgpu::Device, linear: bool, wrap: bool, level0_only: bool) -> wgpu::Sampler {
    let addr = if wrap {
        wgpu::AddressMode::Repeat
    } else {
        wgpu::AddressMode::ClampToEdge
    };
    let f = if linear {
        wgpu::FilterMode::Linear
    } else {
        wgpu::FilterMode::Nearest
    };
    device.create_sampler(&wgpu::SamplerDescriptor {
        address_mode_u: addr,
        address_mode_v: addr,
        address_mode_w: addr,
        mag_filter: f,
        min_filter: f,
        mipmap_filter: if linear {
            wgpu::MipmapFilterMode::Linear
        } else {
            wgpu::MipmapFilterMode::Nearest
        },
        lod_max_clamp: if level0_only { 0.0 } else { 32.0 },
        ..Default::default()
    })
}

fn tex_entry(
    binding: u32,
    dim: wgpu::TextureViewDimension,
    filterable: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: dim,
            multisampled: false,
        },
        count: None,
    }
}
fn samp_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}
fn ubo_entry(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

impl Pipelines {
    fn new(device: &wgpu::Device, output_format: wgpu::TextureFormat) -> Self {
        let bgl_tex = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tex"),
            entries: &[
                tex_entry(0, wgpu::TextureViewDimension::D2, true),
                samp_entry(1),
            ],
        });
        let bgl_blur = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur"),
            entries: &[
                ubo_entry(0, wgpu::ShaderStages::FRAGMENT),
                tex_entry(1, wgpu::TextureViewDimension::D2, true),
                samp_entry(2),
            ],
        });
        let bgl_blit = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                ubo_entry(0, wgpu::ShaderStages::FRAGMENT),
                tex_entry(1, wgpu::TextureViewDimension::D2, false),
            ],
        });
        let bgl_mu = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mu"),
            // `mu` (frozen MD2 block) and `mx` (MilkDrop 3 extension block) are always both bound; a shader that does
            // not declare `mx` is unaffected because the layout is a superset of what it uses
            entries: &[
                ubo_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                ubo_entry(BINDING_MILK_EXT, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let bgl_warp_vs = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("warp_vs"),
            entries: &[ubo_entry(0, wgpu::ShaderStages::VERTEX)],
        });

        let mesh_vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh_vs"),
            source: wgpu::ShaderSource::Wgsl(shaders::MESH_VS.into()),
        });
        let tex_fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tex_mod_fs"),
            source: wgpu::ShaderSource::Wgsl(shaders::TEX_MOD_FS.into()),
        });
        let flat_sm = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flat"),
            source: wgpu::ShaderSource::Wgsl(shaders::FLAT_SRC.into()),
        });
        let blur_sm = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blur"),
            source: wgpu::ShaderSource::Wgsl(shaders::BLUR_SRC.into()),
        });
        let blit_sm = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(shaders::BLIT_SRC.into()),
        });

        let rgb = wgpu::ColorWrites::COLOR;
        let mk = |label: &str,
                  layout: &wgpu::PipelineLayout,
                  vs: &wgpu::ShaderModule,
                  fs: &wgpu::ShaderModule,
                  fs_entry: &str,
                  buffers: &[Option<wgpu::VertexBufferLayout>],
                  format: wgpu::TextureFormat,
                  blend: Option<wgpu::BlendState>,
                  write: wgpu::ColorWrites| {
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
                    module: fs,
                    entry_point: Some(fs_entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: write,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let tex_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tex"),
            bind_group_layouts: &[Some(&bgl_tex)],
            immediate_size: 0,
        });
        let flat_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flat"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let blur_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blur"),
            bind_group_layouts: &[Some(&bgl_blur)],
            immediate_size: 0,
        });
        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&bgl_blit)],
            immediate_size: 0,
        });

        let mut tex = HashMap::new();
        for b in [
            Blend::Replace,
            Blend::Add,
            Blend::SrcAlpha,
            Blend::InvSrcAlpha,
            Blend::SrcAlphaAdd,
        ] {
            for alpha in [false, true] {
                tex.insert(
                    (b, alpha),
                    mk(
                        "tex",
                        &tex_layout,
                        &mesh_vs,
                        &tex_fs,
                        if alpha { "fs_main_ta" } else { "fs_main" },
                        &[Some(mesh_layout())],
                        INTERNAL_FORMAT,
                        blend_state(b),
                        if alpha { wgpu::ColorWrites::ALL } else { rgb },
                    ),
                );
            }
        }
        // projectM warp vertex stage + the fixed-function warp fragment (`texture * frag_COLOR`)
        let warp_vs_sm = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("warp_vs"),
            source: wgpu::ShaderSource::Wgsl(shaders::warp_vs(1).into()),
        });
        let warp_tex_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("warp_tex"),
            bind_group_layouts: &[Some(&bgl_tex), Some(&bgl_warp_vs)],
            immediate_size: 0,
        });
        let mut warp_tex = HashMap::new();
        for alpha in [false, true] {
            warp_tex.insert(
                alpha,
                mk(
                    "warp_tex",
                    &warp_tex_layout,
                    &warp_vs_sm,
                    &tex_fs,
                    if alpha { "fs_main_ta" } else { "fs_main" },
                    &[Some(warp_layout())],
                    INTERNAL_FORMAT,
                    None,
                    if alpha { wgpu::ColorWrites::ALL } else { rgb },
                ),
            );
        }
        // the same vertex stage drawn as one point per mesh vertex into an RGBA32F texel: the GPU's own vertex UVs
        // (`Renderer::gpu_warp_vertex_uv`, motion vector lookups)
        let warp_uv_readback = {
            let sm = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("warp_uv_readback"),
                source: wgpu::ShaderSource::Wgsl(shaders::warp_uv_readback(0).into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("warp_uv_readback"),
                bind_group_layouts: &[Some(&bgl_warp_vs)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("warp_uv_readback"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &sm,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(warp_layout())],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::PointList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &sm,
                    entry_point: Some("fs_probe"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba32Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let mut flat = HashMap::new();
        for b in [
            Blend::Replace,
            Blend::SrcAlpha,
            Blend::SrcAlphaAdd,
            Blend::InvDstZero,
            Blend::ZeroDst,
            Blend::ZeroInvDst,
            Blend::DstOne,
        ] {
            for (prim, topology) in [
                (Prim::Tri, wgpu::PrimitiveTopology::TriangleList),
                (Prim::Line, wgpu::PrimitiveTopology::LineList),
                (Prim::Point, wgpu::PrimitiveTopology::PointList),
            ] {
                for alpha in [false, true] {
                    let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some("flat"),
                        layout: Some(&flat_layout),
                        vertex: wgpu::VertexState {
                            module: &flat_sm,
                            entry_point: Some("vs_main"),
                            compilation_options: Default::default(),
                            buffers: &[Some(wf_layout())],
                        },
                        primitive: wgpu::PrimitiveState {
                            topology,
                            ..Default::default()
                        },
                        depth_stencil: None,
                        multisample: wgpu::MultisampleState::default(),
                        fragment: Some(wgpu::FragmentState {
                            module: &flat_sm,
                            entry_point: Some("fs_main"),
                            compilation_options: Default::default(),
                            targets: &[Some(wgpu::ColorTargetState {
                                format: INTERNAL_FORMAT,
                                blend: blend_state(b),
                                write_mask: if alpha { wgpu::ColorWrites::ALL } else { rgb },
                            })],
                        }),
                        multiview_mask: None,
                        cache: None,
                    });
                    flat.insert((b, prim, alpha), p);
                }
            }
        }
        let blur_h = mk(
            "blur_h",
            &blur_layout,
            &blur_sm,
            &blur_sm,
            "fs_blur_h",
            &[],
            INTERNAL_FORMAT,
            None,
            rgb,
        );
        let blur_v = mk(
            "blur_v",
            &blur_layout,
            &blur_sm,
            &blur_sm,
            "fs_blur_v",
            &[],
            INTERNAL_FORMAT,
            None,
            rgb,
        );
        let blit = mk(
            "blit",
            &blit_layout,
            &blit_sm,
            &blit_sm,
            "fs_main",
            &[],
            output_format,
            None,
            wgpu::ColorWrites::ALL,
        );

        let samplers = [
            sampler(device, true, true, false),
            sampler(device, true, false, false),
            sampler(device, false, true, false),
            sampler(device, false, false, false),
            sampler(device, true, true, true),
            sampler(device, true, false, true),
            sampler(device, false, true, true),
            sampler(device, false, false, true),
        ];
        Pipelines {
            bgl_tex,
            bgl_blur,
            bgl_blit,
            bgl_mu,
            bgl_warp_vs,
            warp_tex,
            warp_uv_readback,
            tex,
            flat,
            blur_h,
            blur_v,
            blit,
            mesh_vs,
            samplers,
            sprite: Default::default(),
        }
    }

    fn sampler(&self, f: Filter, a: Address, level0_only: bool) -> &wgpu::Sampler {
        let i = match (f, a) {
            (Filter::Linear, Address::Wrap) => 0,
            (Filter::Linear, Address::Clamp) => 1,
            (Filter::Point, Address::Wrap) => 2,
            (Filter::Point, Address::Clamp) => 3,
        };
        &self.samplers[i + if level0_only { 4 } else { 0 }]
    }

    fn sprites(&self, device: &wgpu::Device) -> &SpritePipes {
        self.sprite
            .get_or_init(|| SpritePipes::new(device, &self.bgl_tex, INTERNAL_FORMAT))
    }
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

/// `s.wgsl` (entry `fs_main(in: VsOut) -> @location(0) vec4<f32>`) with a second output `@location(1)` = `in.uv.xy`.
fn with_uv_output(wgsl: &str) -> Option<String> {
    const SIG: &str = "fn fs_main(in: VsOut) -> @location(0) vec4<f32> {";
    let at = wgsl.find(SIG)?;
    // drop the `@fragment` attribute (newline or space separated) in front of it: the original becomes a plain function
    let head = wgsl[..at].trim_end();
    let head = head.strip_suffix("@fragment").unwrap_or(head);
    let renamed = format!(
        "{head}\nfn fs_main_color(in: VsOut) -> vec4<f32> {{{}",
        &wgsl[at + SIG.len()..]
    );
    Some(format!(
        "{renamed}\nstruct WarpFsOut {{\n    @location(0) color: vec4<f32>,\n    @location(1) uv: vec2<f32>,\n}}\n\n\
         @fragment\nfn fs_main(in: VsOut) -> WarpFsOut {{\n    var o: WarpFsOut;\n    o.color = fs_main_color(in);\n    o.uv = in.uv.xy;\n    return o;\n}}\n"
    ))
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

fn is_srgb(f: wgpu::TextureFormat) -> bool {
    f.is_srgb()
}

/// Row pitch of a buffer copy of `row_bytes` bytes per row (wgpu requires multiples of 256).
fn padded_bytes_per_row(row_bytes: u32) -> u32 {
    row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
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

/// The mesh geometry of `cfg`, after checking that its size and mesh are usable.
fn checked_geometry(cfg: &RenderConfig) -> Result<MeshGeometry, RenderError> {
    if cfg.width == 0 || cfg.height == 0 {
        return Err(RenderError::Invalid(format!(
            "render size must be non-zero, got {}x{}",
            cfg.width, cfg.height
        )));
    }
    MeshGeometry::new(cfg.mesh[0], cfg.mesh[1], cfg.width, cfg.height)
}

/// Persistent resources of the GPU warp UV readback ([`Renderer::gpu_warp_vertex_uv`]) for a `w x h` vertex grid.
///
/// The readback only depends on a frame's `warp_gpu` and texel offset, not on any image, so it is submitted as its
/// own small submission at the end of the frame that produced those inputs and only waited for (this submission
/// alone, never the frame's main work) by the next frame's motion vector pass.
struct UvReadback {
    w: u32,
    h: u32,
    vertices: wgpu::Buffer,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    out: wgpu::Buffer,
    bytes_per_row: u32,
    /// The submitted readback of the previous frame's `warp_gpu` and the texel offset it was evaluated with.
    pending: Option<(wgpu::SubmissionIndex, [f32; 2])>,
}

impl UvReadback {
    fn new(device: &wgpu::Device, pipes: &Pipelines, w: u32, h: u32) -> Self {
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("warp uv verts"),
            size: u64::from(w) * u64::from(h) * std::mem::size_of::<WarpVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("warp uv uniforms"),
            size: std::mem::size_of::<WarpVsUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("warp uv readback"),
            layout: &pipes.bgl_warp_vs,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("warp uv readback"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bytes_per_row = padded_bytes_per_row(w * 16);
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("warp uv readback"),
            size: u64::from(bytes_per_row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            w,
            h,
            vertices,
            uniforms,
            bind_group,
            texture,
            view,
            out,
            bytes_per_row,
            pending: None,
        }
    }

    /// Evaluate the vertex stage over `verts` into the readback buffer. The writes land ahead of this submission
    /// and behind every earlier one, so a readback still in flight is never disturbed.
    fn submit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipes: &Pipelines,
        verts: &[WarpVertex],
        vu: &WarpVsUniforms,
        toff: [f32; 2],
    ) {
        queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(verts));
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(vu));
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("warp uv readback"),
        });
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("warp uv readback"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            rp.set_pipeline(&pipes.warp_uv_readback);
            rp.set_bind_group(0, &self.bind_group, &[]);
            rp.set_vertex_buffer(0, self.vertices.slice(..));
            rp.draw(0..self.w * self.h, 0..1);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.out,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.bytes_per_row),
                    rows_per_image: Some(self.h),
                },
            },
            wgpu::Extent3d {
                width: self.w,
                height: self.h,
                depth_or_array_layers: 1,
            },
        );
        self.pending = Some((queue.submit(Some(enc.finish())), toff));
    }

    /// Wait for the pending readback and return the UVs in lactea mesh index order. On an error the buffer may be
    /// left mapped: the caller drops the whole `UvReadback`.
    fn collect(
        &mut self,
        device: &wgpu::Device,
        geom: &MeshGeometry,
    ) -> Result<Vec<[f32; 2]>, RenderError> {
        let (submission, _) = self
            .pending
            .take()
            .ok_or_else(|| RenderError::Readback("no warp uv readback submitted".to_owned()))?;
        let slice = self.out.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|e| RenderError::Readback(format!("{e:?}")))?;
        let mapped = match rx.try_recv() {
            Ok(r) => r,
            Err(_) => {
                device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .map_err(|e| RenderError::Readback(format!("{e:?}")))?;
                rx.recv()
                    .map_err(|e| RenderError::Readback(e.to_string()))?
            }
        };
        mapped.map_err(|e| RenderError::Readback(e.to_string()))?;
        let data = slice
            .get_mapped_range()
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let mut out = vec![[0.0f32; 2]; (self.w * self.h) as usize];
        for y in 0..self.h {
            for x in 0..self.w {
                let o = (y * self.bytes_per_row + x * 16) as usize;
                let f = |k: usize| {
                    f32::from_le_bytes([
                        data[o + k],
                        data[o + k + 1],
                        data[o + k + 2],
                        data[o + k + 3],
                    ])
                };
                out[geom.index(x, y)] = [f(0), f(4)];
            }
        }
        drop(data);
        self.out.unmap();
        Ok(out)
    }
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
    /// `sampler_fft` ([`TexSource::Fft`]): the smoothed MilkDrop 3 shader FFT. Owned per renderer (a
    /// [`Renderer::sibling`] has its own) and replaced whenever a frame carries `FrameInput::ext.fft`.
    fft: Tex,
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
        if rgba8.len() != (t.w * t.h * 4) as usize {
            return Err(RenderError::Invalid(format!(
                "{what}: need {} bytes, got {}",
                t.w * t.h * 4,
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
        let geom = checked_geometry(&cfg)?;
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
                    let data = noise::generate(t, &mut noise_rng);
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
        let fft = create_fft_tex(device, queue);

        let warp_indices = warp_index_buffer(device, &geom, &rules);
        let r = Renderer {
            device: device.clone(),
            queue: queue.clone(),
            cfg,
            rules,
            geom,
            warp_indices,
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
        if width == 0 || height == 0 {
            return Err(RenderError::Invalid(format!(
                "render size must be non-zero, got {width}x{height}"
            )));
        }
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
            // R32Float is not filterable: the translated shader only `textureLoad`s the FFT texture (its sampler
            // entry stays `Filtering` and is never paired with it in a `textureSample`)
            let filterable = !matches!(t.source, TexSource::Fft);
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
        let bpr = padded_bytes_per_row(t.w * 4);
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (bpr * t.h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &t.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(t.h),
                },
            },
            wgpu::Extent3d {
                width: t.w,
                height: t.h,
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
            .map_err(|e| RenderError::Readback(format!("{e:?}")))?;
        rx.recv()
            .map_err(|e| RenderError::Readback(e.to_string()))?
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let data = slice
            .get_mapped_range()
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let mut out = Vec::with_capacity((t.w * t.h * 4) as usize);
        for y in 0..t.h {
            let s = (y * bpr) as usize;
            out.extend_from_slice(&data[s..s + (t.w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        Ok((t.w, t.h, out))
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

    fn render_frame_inner(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        target: Option<&wgpu::TextureView>,
    ) -> Result<(), RenderError> {
        self.check_input(input)?;
        // before any pass is recorded: every shader of this frame (and of the old state of a blend) reads the new FFT
        if let Some(fft) = &input.ext.fft {
            self.record_fft_upload(encoder, fft);
        }
        self.anim_time = input.time;
        let rules = self.rules;
        let (w, h) = (self.cfg.width, self.cfg.height);
        let prev_i = self.main_idx;
        let next_i = 1 - prev_i;
        let aspect = (self.geom.aspect_x, self.geom.aspect_y);

        // projectM: the GPU evaluates the warp UV formula (see `shaders::warp_vs`)
        let gpu_warp = if rules.warp_uv_on_gpu {
            input.warp_gpu.as_ref()
        } else {
            None
        };
        // With a GPU warp stage the CPU grid is only the motion vector lookup's fallback, which `cpu_warp_uv` derives
        // from `warp_gpu` when it is needed: a frame that supplies none costs nothing.
        let uv_unneeded =
            input.warp_uv.is_none() && gpu_warp.is_some() && input.md2_blend.is_none();
        let warp_uv: std::borrow::Cow<'_, [[f32; 2]]> = match &input.warp_uv {
            Some(u) => std::borrow::Cow::Borrowed(u),
            None if uv_unneeded => std::borrow::Cow::Borrowed(&[]),
            None => std::borrow::Cow::Owned(self.default_warp_uv(input)),
        };

        // ---- 1. motion vectors onto the previous image ---------------------------------
        let mv_visible = input.mv.rgba[3]
            >= if rules.mv_skip_first_frame {
                0.0001
            } else {
                0.001
            };
        if mv_visible && !(rules.mv_skip_first_frame && self.frames_since_resize == 0) {
            let gpu_uv = if rules.warp_uv_on_gpu && self.prev_warp_gpu.is_some() {
                self.gpu_warp_vertex_uv().ok()
            } else {
                None
            };
            let cpu_uv = if gpu_uv.is_none() && self.prev_warp_uv.is_none() {
                self.prev_warp_gpu.as_ref().map(|wg| self.cpu_warp_uv(wg))
            } else {
                None
            };
            if let Some(prev_uv) = gpu_uv
                .as_ref()
                .or(self.prev_warp_uv.as_ref())
                .or(cpu_uv.as_ref())
            {
                let color = geom::conv_color_raw(&rules, input.mv.rgba);
                let verts: Vec<WfVertex> = if rules.mv_mirrored_lookup {
                    // MilkDrop 2: one D3D line list, mirrored-row lookup into the previous mesh
                    geom::motion_vector_lines(&input.mv, &self.geom, prev_uv, w, h, &rules)
                        .iter()
                        .map(|&pos| WfVertex { pos, color })
                        .collect()
                } else {
                    // projectM: GL_LINES over its (never cleared) vertex buffer, see `motion.rs`
                    self.mv_state.segments(
                        &input.mv,
                        &self.geom,
                        prev_uv,
                        w,
                        h,
                        rules.warp_diag_flipped,
                        rules.mv_min_len_px,
                        color,
                    )
                };
                if !verts.is_empty() {
                    let mut b = Batch::default();
                    b.segments(
                        self.stroke_mode(),
                        LineRule::Diamond,
                        Blend::SrcAlpha,
                        w as f32,
                        h as f32,
                        &verts,
                    );
                    self.run_batch(
                        encoder,
                        &b,
                        &self.main[prev_i].view,
                        wgpu::LoadOp::Load,
                        None,
                        None,
                        "motion vectors",
                    );
                }
            }
        }

        // ---- 2. warp pass --------------------------------------------------------------
        let decay = if rules.decay_8bit {
            geom::d3d_color_component(input.post.decay)
        } else {
            input.post.decay.min(1.0)
        };
        let toff = self.texel_offset_uv();
        if let Some(bl) = input.md2_blend.as_ref() {
            let wrap = if input.post.wrap {
                Address::Wrap
            } else {
                Address::Clamp
            };
            self.run_md2_warp(
                encoder, input, bl, &warp_uv, decay, toff, wrap, prev_i, next_i,
            );
        } else {
            let sampler_wrap = if input.post.wrap {
                Address::Wrap
            } else {
                Address::Clamp
            };
            match &self.warp_shader {
                None => {
                    if let Some(wg) = gpu_warp {
                        let verts = geom::build_warp_gpu_vertices(&self.geom, &wg.vars);
                        let vu = self.warp_vs_uniforms(wg, toff, decay);
                        self.run_warp_gpu_plain(
                            encoder,
                            &verts,
                            &vu,
                            &self.main[prev_i].view,
                            self.pipes.sampler(Filter::Linear, sampler_wrap, false),
                            &self.main[next_i].view,
                        );
                    } else {
                        let mut verts = geom::build_warp_vertices(
                            &self.geom,
                            &warp_uv,
                            [decay, decay, decay, 1.0],
                            toff,
                            rules.ang_seam_fix,
                            rules.warp_diag_flipped,
                        );
                        geom::fix_nan_uv_triangles(&mut verts);
                        let mut b = Batch::default();
                        b.tex(Blend::Replace, &verts);
                        let bg = self.tex_bind_group(
                            &self.main[prev_i].view,
                            self.pipes.sampler(Filter::Linear, sampler_wrap, false),
                        );
                        self.run_batch(
                            encoder,
                            &b,
                            &self.main[next_i].view,
                            wgpu::LoadOp::Clear(BLACK),
                            Some(&bg),
                            None,
                            "warp (no shader)",
                        );
                    }
                }
                Some(pass) => {
                    let diffuse_scalar = if rules.shader_warp_diffuse_is_decay {
                        decay
                    } else {
                        1.0
                    };

                    let mut rng = Rng::new(self.cfg.seed).fork(input.frame as u64 * 4);
                    let mu = uniforms::fill_milk_uniforms(
                        &MilkUniformInputs::for_frame(
                            w,
                            h,
                            input,
                            &self.rot,
                            &rules,
                            input.warp_random.as_ref(),
                        ),
                        &mut rng,
                    );
                    let main_view = &self.main[prev_i].view;
                    let warp_wrap = self
                        .rules
                        .warp_main_sampler_from_wrap
                        .then_some(sampler_wrap);
                    if let Some(wg) = gpu_warp {
                        let verts = geom::build_warp_gpu_vertices(&self.geom, &wg.vars);
                        let vu = self.warp_vs_uniforms(wg, toff, diffuse_scalar);
                        self.run_shader_pass(
                            encoder,
                            pass,
                            &mu,
                            &input.ext,
                            main_view,
                            PassVerts::Warp(&verts, &vu),
                            &self.main[next_i].view,
                            "warp (shader)",
                            warp_wrap,
                        );
                    } else {
                        let mut verts = geom::build_warp_vertices(
                            &self.geom,
                            &warp_uv,
                            [diffuse_scalar, diffuse_scalar, diffuse_scalar, 1.0],
                            toff,
                            rules.ang_seam_fix,
                            rules.warp_diag_flipped,
                        );
                        geom::fix_nan_uv_triangles(&mut verts);
                        self.run_shader_pass(
                            encoder,
                            pass,
                            &mu,
                            &input.ext,
                            main_view,
                            PassVerts::Mesh(&verts),
                            &self.main[next_i].view,
                            "warp (shader)",
                            warp_wrap,
                        );
                    }
                }
            }
        }

        // ---- 3. snapshot after warp ----------------------------------------------------
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.main[next_i].texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.warped.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );

        // ---- 4. blur chain (from the previous image, like MD2 / projectM) ---------------
        // `native_blur`: a shader of the preset samples blur (projectM's `BlurTexture::m_blurLevel > 0`). The blur chain
        // may also run only because a diagnostic stage was requested; that never has side effects in projectM's own
        // flow, so everything below keys off `native_blur`, not `levels`.
        let native_blur = self.blur_levels_required();
        let levels = native_blur.max(self.diagnostic_blur_levels());
        self.last_blur_levels = native_blur;
        if levels > 0 {
            self.run_blur(encoder, input, &self.main[prev_i].view, levels);
        }

        // ---- 4b. MD3 sprites, layer 1 ("deepest"): into the feedback image after warp and blur -------
        self.draw_sprite_layer(encoder, input, 1, &self.main[next_i].view);

        // ---- 5. shapes, waves, main wave, darken center, borders on `next` -------------
        {
            let mut b = Batch::default();
            // projectM quirk (`BlurTexture::Update` runs `AllocateTextures` -> `Framebuffer::SetSize`, which leaves FBO 0
            // bound *before* its "remember previously bound framebuffer" snapshot, so on the call that allocates the blur
            // textures — the first frame after load/resize — it restores the default framebuffer): every overlay of that
            // frame goes to the wrong target and is lost. Only presets whose shaders sample blur natively are affected.
            let overlays_lost =
                rules.blur_alloc_drops_overlays && native_blur > 0 && self.frames_since_resize == 0;
            // `GetVertices` runs every frame, also when the overlays are lost (it advances the wave-buffer tail).
            let pm_wave = if rules.pm_waveforms {
                self.advance_main_wave_pm(input)
            } else {
                None
            };
            if !overlays_lost {
                self.build_overlay(&mut b, input, aspect, pm_wave);
            }
            let sprite_split = input.sprites.iter().any(|s| matches!(s.layer, 2 | 3));
            if !b.is_empty() || sprite_split {
                let bg = self.tex_bind_group(
                    &self.main[prev_i].view,
                    self.pipes.sampler(
                        if rules.shape_main_nearest {
                            Filter::Point
                        } else {
                            Filter::Linear
                        },
                        Address::Wrap,
                        false,
                    ),
                );
                let leak = self.tex_bind_group(
                    &self.main[prev_i].view,
                    self.pipes.sampler(Filter::Linear, Address::Clamp, false),
                );
                self.run_overlay(encoder, input, &b, next_i, &bg, &leak);
            }
        }

        // ---- 5b. MD3 sprites, layer 4 ("after waveform") --------------------------------------------
        self.draw_sprite_layer(encoder, input, 4, &self.main[next_i].view);

        // ---- 6. composite --------------------------------------------------------------
        if let Some(bl) = input.md2_blend.as_ref() {
            self.run_md2_comp(encoder, input, bl, aspect, prev_i, next_i);
        } else {
            let comp_src = if rules.comp_reads_previous && self.comp_shader.is_some() {
                prev_i
            } else {
                next_i
            };
            match &self.comp_shader {
                Some(pass) => {
                    let shade = geom::hue_shade(
                        input.time as f32,
                        input.hue_offsets.unwrap_or(self.rot.hue_offsets),
                    );
                    let verts =
                        geom::build_comp_vertices(w, h, aspect, &shade, rules.comp_uv_half_texel);
                    let mut rng = Rng::new(self.cfg.seed).fork(input.frame as u64 * 4 + 1);
                    let mu = uniforms::fill_milk_uniforms(
                        &MilkUniformInputs::for_frame(
                            w,
                            h,
                            input,
                            &self.rot,
                            &rules,
                            input.comp_random.as_ref(),
                        ),
                        &mut rng,
                    );
                    let main_view = self.comp_source_view(&self.main[comp_src].view);
                    self.run_shader_pass(
                        encoder,
                        pass,
                        &mu,
                        &input.ext,
                        main_view,
                        PassVerts::Mesh(&verts),
                        &self.final_tex.view,
                        "comp (shader)",
                        None,
                    );
                }
                None => {
                    let b = self.build_noshader_comp(input);
                    let wrap = if self.rules.noshader_comp_wrap {
                        Address::Wrap
                    } else {
                        Address::Clamp
                    };
                    let src_view = self.comp_source_view(&self.main[next_i].view);
                    let bg = self
                        .tex_bind_group(src_view, self.pipes.sampler(Filter::Linear, wrap, false));
                    self.run_batch(
                        encoder,
                        &b,
                        &self.final_tex.view,
                        wgpu::LoadOp::Clear(BLACK),
                        Some(&bg),
                        None,
                        "comp (no shader)",
                    );
                }
            }
        }

        // ---- 6b. MD3 sprites, layer 0: on the composited image; burn also feeds the next frame --------
        self.draw_top_sprites(
            encoder,
            input,
            &self.final_tex.view,
            &self.main[next_i].view,
        );

        // ---- 7. blit to the host target ------------------------------------------------
        if let Some(target) = target {
            self.blit(encoder, target);
        }
        #[cfg(feature = "diagnostics")]
        {
            self.comp_source_override = None;
        }

        // keep this frame's UVs for the next frame's motion vectors, reusing the previous buffer
        if uv_unneeded {
            self.prev_warp_uv = None;
        } else {
            let mut kept = self.prev_warp_uv.take().unwrap_or_default();
            kept.clear();
            kept.extend_from_slice(&warp_uv);
            self.prev_warp_uv = Some(kept);
        }
        let gpu_src = gpu_warp;
        match (gpu_src, &mut self.prev_warp_gpu) {
            (Some(src), Some(dst)) => {
                dst.vars.clone_from(&src.vars);
                dst.anim = src.anim;
            }
            (Some(src), slot) => *slot = Some(src.clone()),
            (None, slot) => *slot = None,
        }
        // the next frame's motion vector pass reads this frame's GPU UVs: start them now if it is likely to
        if rules.warp_uv_on_gpu && mv_visible {
            self.submit_uv_readback();
        } else if let Some(rb) = &mut self.uv_readback {
            rb.pending = None;
        }
        self.main_idx = next_i;
        self.frames_since_resize += 1;
        Ok(())
    }

    /// Replace the FFT texture's contents ([`TexSource::Fft`]). Recorded as a buffer copy in `encoder` rather than
    /// `queue.write_texture`: a queue write lands at the next submit, ahead of everything already recorded, so two
    /// frames of one renderer recorded into one encoder would both see the last spectrum.
    fn record_fft_upload(&self, encoder: &mut wgpu::CommandEncoder, fft: &[f32; FFT_BINS]) {
        let staging = buffer_init(
            &self.device,
            "fft",
            fft.as_slice(),
            wgpu::BufferUsages::COPY_SRC,
        );
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(FFT_BINS as u32 * 4),
                    rows_per_image: Some(1),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.fft.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: FFT_BINS as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }

    fn tex_bind_group(&self, view: &wgpu::TextureView, sampler: &wgpu::Sampler) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tex"),
            layout: &self.pipes.bgl_tex,
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
        })
    }

    /// Execute a fixed-function batch in one render pass.
    fn run_batch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        batch: &Batch,
        view: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        tex_bg: Option<&wgpu::BindGroup>,
        leak_bg: Option<&wgpu::BindGroup>,
        label: &str,
    ) {
        self.run_batch_range(
            encoder,
            batch,
            0..batch.cmds.len(),
            view,
            load,
            tex_bg,
            leak_bg,
            label,
        );
    }

    /// [`Renderer::run_batch`] for the commands `cmds` only.
    fn run_batch_range(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        batch: &Batch,
        cmds: std::ops::Range<usize>,
        view: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        tex_bg: Option<&wgpu::BindGroup>,
        leak_bg: Option<&wgpu::BindGroup>,
        label: &str,
    ) {
        let wf_buf = buffer_init(&self.device, "wf", &batch.wf, wgpu::BufferUsages::VERTEX);
        let mesh_buf = buffer_init(
            &self.device,
            "mesh",
            &batch.mesh,
            wgpu::BufferUsages::VERTEX,
        );
        // `image=` textures: projectM's built-in noise textures by name, else the user texture (a 1x1 black
        // placeholder when the host had none, like projectM's `TextureManager` placeholder) with the sampler its
        // prefix asks for (no mip chain: filter only)
        let image_bgs: Vec<wgpu::BindGroup> = batch
            .images
            .iter()
            .map(|im| {
                let view = match noise::builtin_image_noise(&im.name) {
                    Some(n) => &self.noise[n.index()].view,
                    None => self
                        .user_tex
                        .get(&im.name)
                        .map_or(&self.dummy_2d.view, |u| u.view_at(self.anim_time)),
                };
                let filter = if im.linear {
                    Filter::Linear
                } else {
                    Filter::Point
                };
                let address = if im.wrap {
                    Address::Wrap
                } else {
                    Address::Clamp
                };
                self.tex_bind_group(view, self.pipes.sampler(filter, address, true))
            })
            .collect();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        for &(kind, blend, prim, start, count) in &batch.cmds[cmds] {
            match kind {
                Kind::Flat => {
                    pass.set_pipeline(&self.pipes.flat[&(blend, prim, self.rules.main_alpha)]);
                    pass.set_vertex_buffer(0, wf_buf.slice(..));
                }
                // every caller binds the groups its batch kinds need; a draw without one is skipped
                Kind::Tex => {
                    let Some(bg) = tex_bg else {
                        continue;
                    };
                    pass.set_pipeline(&self.pipes.tex[&(blend, self.rules.main_alpha)]);
                    pass.set_bind_group(0, bg, &[]);
                    pass.set_vertex_buffer(0, mesh_buf.slice(..));
                }
                Kind::TexLeak => {
                    let Some(bg) = leak_bg else {
                        continue;
                    };
                    pass.set_pipeline(&self.pipes.tex[&(blend, self.rules.main_alpha)]);
                    pass.set_bind_group(0, bg, &[]);
                    pass.set_vertex_buffer(0, mesh_buf.slice(..));
                }
                Kind::TexImage(i) => {
                    pass.set_pipeline(&self.pipes.tex[&(blend, self.rules.main_alpha)]);
                    pass.set_bind_group(0, &image_bgs[i as usize], &[]);
                    pass.set_vertex_buffer(0, mesh_buf.slice(..));
                }
            }
            pass.draw(start..start + count, 0..1);
        }
    }

    fn texsize_of(&self, src: &TexSource, user_tex: &HashMap<String, UserTex>) -> [f32; 4] {
        let (w, h) = match src {
            TexSource::Main => (self.cfg.width, self.cfg.height),
            TexSource::Blur(k) if (1..=3).contains(k) => {
                let t = &self.blur[*k as usize * 2 - 1];
                (t.w, t.h)
            }
            TexSource::Blur(_) => (1, 1),
            TexSource::Noise(n) => {
                let t = &self.noise[n.index()];
                (t.w, t.h)
            }
            TexSource::User(name) => match user_tex.get(&name.to_ascii_lowercase()) {
                Some(u) => (u.tex.w, u.tex.h),
                None => (1, 1),
            },
            TexSource::Fft => (self.fft.w, self.fft.h),
        };
        [w as f32, h as f32, 1.0 / w as f32, 1.0 / h as f32]
    }

    /// Uniforms of the GPU warp vertex stage ([`shaders::warp_vs`]).
    fn warp_vs_uniforms(
        &self,
        wg: &crate::frame::WarpGpu,
        toff: [f32; 2],
        diffuse: f32,
    ) -> WarpVsUniforms {
        let (ax, ay) = (self.geom.aspect_x, self.geom.aspect_y);
        WarpVsUniforms {
            aspect: [ax, ay, 1.0 / ax, 1.0 / ay],
            timing: [wg.anim.warp_time, wg.anim.scale_inv, toff[0], toff[1]],
            factors: wg.anim.f,
            diffuse: [diffuse, diffuse, diffuse, 1.0],
        }
    }

    fn warp_vs_bind_group(&self, vu: &WarpVsUniforms) -> wgpu::BindGroup {
        let buf = buffer_init(
            &self.device,
            "warp_vs",
            std::slice::from_ref(vu),
            wgpu::BufferUsages::UNIFORM,
        );
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("warp_vs"),
            layout: &self.pipes.bgl_warp_vs,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        })
    }

    /// `WarpedBlit_NoShaders` with the GPU vertex stage: `next = prev(uv) * decay`.
    fn run_warp_gpu_plain(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        verts: &[WarpVertex],
        vu: &WarpVsUniforms,
        src: &wgpu::TextureView,
        samp: &wgpu::Sampler,
        target: &wgpu::TextureView,
    ) {
        let tex_bg = self.tex_bind_group(src, samp);
        let vs_bg = self.warp_vs_bind_group(vu);
        let vb = buffer_init(
            &self.device,
            "warp verts",
            verts,
            wgpu::BufferUsages::VERTEX,
        );
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("warp (no shader, gpu uv)"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        rp.set_pipeline(&self.pipes.warp_tex[&self.rules.main_alpha]);
        rp.set_bind_group(0, &tex_bg, &[]);
        rp.set_bind_group(1, &vs_bg, &[]);
        rp.set_vertex_buffer(0, vb.slice(..));
        if let Some((ib, n)) = &self.warp_indices {
            rp.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
            rp.draw_indexed(0..*n, 0, 0..1);
        }
    }

    /// Draw a user warp / composite shader over `verts` into `target`.
    fn run_shader_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass: &ShaderPass,
        mu: &MilkUniforms,
        ext: &ShaderExt,
        main_view: &wgpu::TextureView,
        verts: PassVerts<'_>,
        target: &wgpu::TextureView,
        label: &str,
        warp_wrap: Option<Address>,
    ) {
        self.run_shader_pass_ex(
            encoder,
            pass,
            &self.user_tex,
            PassBlend::Replace,
            mu,
            ext,
            main_view,
            verts,
            target,
            label,
            warp_wrap,
        );
    }

    /// [`Renderer::run_shader_pass`] with the user textures of the pass's own preset (`m_OldShaders` keeps the old
    /// preset's) and MD2's blend-pass output modes: `Replace` clears the target first, the alpha modes keep it and
    /// blend the shader's output by the vertex alpha.
    fn run_shader_pass_ex(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass: &ShaderPass,
        user_tex: &HashMap<String, UserTex>,
        blend: PassBlend,
        mu: &MilkUniforms,
        ext: &ShaderExt,
        main_view: &wgpu::TextureView,
        verts: PassVerts<'_>,
        target: &wgpu::TextureView,
        label: &str,
        warp_wrap: Option<Address>,
    ) {
        let mu_buf = buffer_init(
            &self.device,
            "mu",
            std::slice::from_ref(mu),
            wgpu::BufferUsages::UNIFORM,
        );
        let mx_buf = buffer_init(
            &self.device,
            "mx",
            std::slice::from_ref(&ext.uniforms()),
            wgpu::BufferUsages::UNIFORM,
        );
        let bg0 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mu"),
            layout: &self.pipes.bgl_mu,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: mu_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: BINDING_MILK_EXT,
                    resource: mx_buf.as_entire_binding(),
                },
            ],
        });

        let mut pu = PassUniforms::default();
        for (i, src) in pass
            .interface
            .texsize_slots
            .iter()
            .enumerate()
            .take(lactea_abi::MAX_PASS_TEXTURES)
        {
            pu.texsize[i] = self.texsize_of(src, user_tex);
        }
        let pu_buf = buffer_init(
            &self.device,
            "pu",
            std::slice::from_ref(&pu),
            wgpu::BufferUsages::UNIFORM,
        );

        let mut entries: Vec<wgpu::BindGroupEntry> = vec![wgpu::BindGroupEntry {
            binding: lactea_abi::BINDING_PASS_UNIFORMS,
            resource: pu_buf.as_entire_binding(),
        }];
        // projectM `WarpedBlit`: the per-frame `wrap` sampler replaces the one of the first `main` descriptor.
        let wrap_slot = warp_wrap.and_then(|_| {
            let name_of = |t: &lactea_abi::TextureBinding| {
                t.hlsl_name
                    .strip_prefix("sampler_")
                    .unwrap_or(&t.hlsl_name)
                    .to_owned()
            };
            let first = pass
                .interface
                .textures
                .iter()
                .enumerate()
                .filter(|(_, t)| matches!(t.source, TexSource::Main))
                .min_by(|a, b| name_of(a.1).cmp(&name_of(b.1)))?;
            // `main` is always in the set: if it sorts first and the shader does not use it, no bound sampler changes
            (name_of(first.1).as_str() <= "main").then_some(first.0)
        });
        for (i, t) in pass.interface.textures.iter().enumerate() {
            let want3d = t.dim == TexDim::D3;
            let view: &wgpu::TextureView = match &t.source {
                TexSource::Main => main_view,
                TexSource::Blur(k) if (1..=3).contains(k) => &self.blur[*k as usize * 2 - 1].view,
                TexSource::Blur(_) => &self.dummy_2d.view,
                TexSource::Noise(n) => &self.noise[n.index()].view,
                TexSource::User(name) => match user_tex.get(&name.to_ascii_lowercase()) {
                    Some(u) => u.view_at(self.anim_time),
                    None => &self.dummy_2d.view,
                },
                TexSource::Fft => &self.fft.view,
            };
            // the bound view must match the declared dimensionality
            let is3d = matches!(
                &t.source,
                TexSource::Noise(NoiseTex::VolLq | NoiseTex::VolHq)
            );
            let view = if want3d == is3d {
                view
            } else if want3d {
                &self.dummy_3d.view
            } else {
                &self.dummy_2d.view
            };
            entries.push(wgpu::BindGroupEntry {
                binding: lactea_abi::texture_binding(i as u32),
                resource: wgpu::BindingResource::TextureView(view),
            });
            let (filter, address) = match (wrap_slot == Some(i), warp_wrap) {
                (true, Some(a)) => (Filter::Linear, a),
                _ => (t.filter, t.address),
            };
            entries.push(wgpu::BindGroupEntry {
                binding: lactea_abi::sampler_binding(i as u32),
                resource: wgpu::BindingResource::Sampler(self.pipes.sampler(
                    filter,
                    address,
                    self.rules.no_mipmaps,
                )),
            });
        }
        let bg1 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pass"),
            layout: &pass.pass_bgl,
            entries: &entries,
        });

        let (vb, count, vs_bg) = match &verts {
            PassVerts::Mesh(v) => (
                buffer_init(&self.device, "verts", v, wgpu::BufferUsages::VERTEX),
                v.len(),
                None,
            ),
            PassVerts::Warp(v, vu) => (
                buffer_init(&self.device, "warp verts", v, wgpu::BufferUsages::VERTEX),
                v.len(),
                Some(self.warp_vs_bind_group(vu)),
            ),
        };
        let sink = pass.uv_sink.then_some(wgpu::RenderPassColorAttachment {
            view: &self.warp_sink.view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(BLACK),
                store: wgpu::StoreOp::Store,
            },
        });
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[
                Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: match blend {
                            PassBlend::Replace => wgpu::LoadOp::Clear(BLACK),
                            _ => wgpu::LoadOp::Load,
                        },
                        store: wgpu::StoreOp::Store,
                    },
                }),
                sink,
            ][..if pass.uv_sink { 2 } else { 1 }],
            ..Default::default()
        });
        let alpha_pipeline = match (blend, pass.alpha_pipelines.as_ref()) {
            (PassBlend::SrcAlpha, Some(a)) => Some(&a[0]),
            (PassBlend::InvSrcAlpha, Some(a)) => Some(&a[1]),
            (PassBlend::Replace, _) => Some(&pass.pipeline),
            _ => None,
        };
        let pipeline = match &vs_bg {
            Some(_) => pass.warp_pipeline.as_ref(),
            None => alpha_pipeline,
        };
        // `render_frame` rejects the inputs that lack a pipeline (a blend without `md2_preset_blend`); a
        // warp-vertex pass is only requested for warp shader passes.
        let Some(pipeline) = pipeline else {
            return;
        };
        rp.set_pipeline(pipeline);
        if let Some(bg) = &vs_bg {
            rp.set_bind_group(2, bg, &[]);
        }
        rp.set_bind_group(0, &bg0, &[]);
        rp.set_bind_group(1, &bg1, &[]);
        rp.set_vertex_buffer(0, vb.slice(..));
        match (&verts, &self.warp_indices) {
            (PassVerts::Mesh(_), _) => rp.draw(0..count as u32, 0..1),
            (PassVerts::Warp(..), Some((ib, n))) => {
                rp.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..*n, 0, 0..1);
            }
            // only projectM's rules request a warp-vertex pass, and they always have the index buffer
            (PassVerts::Warp(..), None) => {}
        }
    }

    /// The warp pass of `RenderFrame` while MD2 blends two presets: `WarpedBlit_Shaders` / `WarpedBlit_NoShaders` once
    /// or twice depending on which of the two presets use a warp shader ([`blend::warp_passes`]), all on the *blended*
    /// warp mesh (`warp_uv`, per-vertex alpha `bl.vert_alpha`). Pass 0 is opaque (old shader / old state), pass 1
    /// is alpha-blended over it (new shader); tiles whose alphas make a pass invisible are not drawn.
    #[allow(clippy::too_many_arguments)]
    fn run_md2_warp(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        bl: &blend::Md2Blend,
        warp_uv: &[[f32; 2]],
        decay: f32,
        toff: [f32; 2],
        sampler_wrap: Address,
        prev_i: usize,
        next_i: usize,
    ) {
        let rules = self.rules;
        let (w, h) = (self.cfg.width, self.cfg.height);
        let old = self.old_shaders.as_ref();
        let passes = blend::warp_passes(
            old.is_some_and(|o| o.warp.is_some()),
            self.warp_shader.is_some(),
        );
        let src = &self.main[prev_i].view;
        let target = &self.main[next_i].view;
        for dp in passes {
            let cull = dp.cull_tiles.then_some(dp.flip_culling);
            let mode = match (dp.alpha_blend, dp.flip_alpha) {
                (false, _) => PassBlend::Replace,
                (true, false) => PassBlend::SrcAlpha,
                (true, true) => PassBlend::InvSrcAlpha,
            };
            if dp.shaders {
                let (pass, user_tex, q, blur, ext, rand) = match dp.which {
                    Which::New => {
                        let Some(pass) = self.warp_shader.as_ref() else {
                            continue;
                        };
                        (
                            pass,
                            &self.user_tex,
                            &input.q,
                            &input.blur,
                            &input.ext,
                            input.warp_random.as_ref(),
                        )
                    }
                    Which::Old => {
                        let Some((o, pass)) = old.and_then(|o| Some((o, o.warp.as_ref()?))) else {
                            continue;
                        };
                        (
                            pass,
                            &o.user_tex,
                            &bl.old.q,
                            &bl.old.blur,
                            &bl.old.ext,
                            bl.old.warp_random.as_ref(),
                        )
                    }
                };
                let diffuse = if rules.shader_warp_diffuse_is_decay {
                    decay
                } else {
                    1.0
                };
                let mut rng = Rng::new(self.cfg.seed).fork(input.frame as u64 * 4);
                let mu = uniforms::fill_milk_uniforms(
                    &MilkUniformInputs {
                        q,
                        blur,
                        ext,
                        ..MilkUniformInputs::for_frame(w, h, input, &self.rot, &rules, rand)
                    },
                    &mut rng,
                );
                let mut verts = geom::build_warp_vertices_blend(
                    &self.geom,
                    warp_uv,
                    [diffuse, diffuse, diffuse],
                    &bl.vert_alpha,
                    toff,
                    rules.ang_seam_fix,
                    cull,
                );
                geom::fix_nan_uv_triangles(&mut verts);
                let warp_wrap = rules.warp_main_sampler_from_wrap.then_some(sampler_wrap);
                self.run_shader_pass_ex(
                    encoder,
                    pass,
                    user_tex,
                    mode,
                    &mu,
                    ext,
                    src,
                    PassVerts::Mesh(&verts),
                    target,
                    "warp (shader, blend)",
                    warp_wrap,
                );
            } else {
                let mut verts = geom::build_warp_vertices_blend(
                    &self.geom,
                    warp_uv,
                    [decay, decay, decay],
                    &bl.vert_alpha,
                    toff,
                    rules.ang_seam_fix,
                    cull,
                );
                geom::fix_nan_uv_triangles(&mut verts);
                let mut b = Batch::default();
                b.tex(
                    match mode {
                        PassBlend::Replace => Blend::Replace,
                        PassBlend::SrcAlpha => Blend::SrcAlpha,
                        PassBlend::InvSrcAlpha => Blend::InvSrcAlpha,
                    },
                    &verts,
                );
                let bg = self
                    .tex_bind_group(src, self.pipes.sampler(Filter::Linear, sampler_wrap, false));
                self.run_batch(
                    encoder,
                    &b,
                    target,
                    if mode == PassBlend::Replace {
                        wgpu::LoadOp::Clear(BLACK)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    Some(&bg),
                    None,
                    "warp (no shader, blend)",
                );
            }
        }
    }

    /// The composite of `RenderFrame` while MD2 blends two presets: `ShowToUser_NoShaders` (whole quad, from the
    /// blended per-frame values) and / or `ShowToUser_Shaders` with the old and the new composite shader on the 32x24
    /// composite mesh, whose vertex alphas are looked up in the warp mesh's ([`blend::comp_passes`],
    /// [`blend::comp_vertex_alpha`]).
    fn run_md2_comp(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        bl: &blend::Md2Blend,
        aspect: (f32, f32),
        prev_i: usize,
        next_i: usize,
    ) {
        let rules = self.rules;
        let (w, h) = (self.cfg.width, self.cfg.height);
        let old = self.old_shaders.as_ref();
        let passes = blend::comp_passes(
            old.is_some_and(|o| o.comp.is_some()),
            self.comp_shader.is_some(),
        );
        let shade = geom::hue_shade(
            input.time as f32,
            input.hue_offsets.unwrap_or(self.rot.hue_offsets),
        );
        let mut grid = geom::comp_grid(w, h, aspect, &shade, rules.comp_uv_half_texel);
        let mut alpha = vec![255u8; grid.len()];
        for j in 0..geom::FCGSY {
            for i in 0..geom::FCGSX {
                let (x01, y01) = blend::comp_vertex_xy01(i, j, w, h);
                let a = blend::comp_vertex_alpha(
                    &bl.vert_alpha,
                    [self.geom.gx, self.geom.gy],
                    x01,
                    y01,
                );
                alpha[i + j * geom::FCGSX] = a;
                grid[i + j * geom::FCGSX].diffuse[3] = f32::from(a) / 255.0;
            }
        }
        let idx = geom::comp_indices();
        for dp in passes {
            if dp.shaders {
                let mut verts = Vec::with_capacity(idx.len());
                for t in idx.as_chunks::<3>().0 {
                    if !dp.cull_tiles
                        || blend::tile_needed(
                            [alpha[t[0]], alpha[t[1]], alpha[t[2]]],
                            dp.flip_culling,
                        )
                    {
                        verts.extend(t.iter().map(|&k| grid[k]));
                    }
                }
                let mode = match (dp.alpha_blend, dp.flip_alpha) {
                    (false, _) => PassBlend::Replace,
                    (true, false) => PassBlend::SrcAlpha,
                    (true, true) => PassBlend::InvSrcAlpha,
                };
                let (pass, user_tex, q, blur, ext, rand) = match dp.which {
                    Which::New => {
                        let Some(pass) = self.comp_shader.as_ref() else {
                            continue;
                        };
                        (
                            pass,
                            &self.user_tex,
                            &input.q,
                            &input.blur,
                            &input.ext,
                            input.comp_random.as_ref(),
                        )
                    }
                    Which::Old => {
                        let Some((o, pass)) = old.and_then(|o| Some((o, o.comp.as_ref()?))) else {
                            continue;
                        };
                        (
                            pass,
                            &o.user_tex,
                            &bl.old.q,
                            &bl.old.blur,
                            &bl.old.ext,
                            bl.old.comp_random.as_ref(),
                        )
                    }
                };
                let mut rng = Rng::new(self.cfg.seed).fork(input.frame as u64 * 4 + 1);
                let mu = uniforms::fill_milk_uniforms(
                    &MilkUniformInputs {
                        q,
                        blur,
                        ext,
                        ..MilkUniformInputs::for_frame(w, h, input, &self.rot, &rules, rand)
                    },
                    &mut rng,
                );
                let comp_src = if rules.comp_reads_previous {
                    prev_i
                } else {
                    next_i
                };
                let main_view = self.comp_source_view(&self.main[comp_src].view);
                self.run_shader_pass_ex(
                    encoder,
                    pass,
                    user_tex,
                    mode,
                    &mu,
                    ext,
                    main_view,
                    PassVerts::Mesh(&verts),
                    &self.final_tex.view,
                    "comp (shader, blend)",
                    None,
                );
            } else {
                let b = self.build_noshader_comp(input);
                let wrap = if self.rules.noshader_comp_wrap {
                    Address::Wrap
                } else {
                    Address::Clamp
                };
                let src_view = self.comp_source_view(&self.main[next_i].view);
                let bg =
                    self.tex_bind_group(src_view, self.pipes.sampler(Filter::Linear, wrap, false));
                self.run_batch(
                    encoder,
                    &b,
                    &self.final_tex.view,
                    wgpu::LoadOp::Clear(BLACK),
                    Some(&bg),
                    None,
                    "comp (no shader, blend)",
                );
            }
        }
    }

    /// `BlurPasses`.
    fn run_blur(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        source: &wgpu::TextureView,
        levels: u8,
    ) {
        let passes = blur::pass_count(levels);
        let (bmin, bmax) = blur::safe_blur_min_max(input.blur.min, input.blur.max);
        let (scale, bias) = blur::scale_bias(bmin, bmax);
        let ht = blur::horizontal_taps();
        let vt = blur::vertical_taps();
        let edge_darken = input.blur.edge_darken;
        let clamp_linear = self.pipes.sampler(Filter::Linear, Address::Clamp, false);
        for i in 0..passes {
            let (src_view, sw, sh) = if i == 0 {
                (source, self.cfg.width as f32, self.cfg.height as f32)
            } else {
                (
                    &self.blur[i - 1].view,
                    self.blur[i - 1].w as f32,
                    self.blur[i - 1].h as f32,
                )
            };
            let c0 = [sw, sh, 1.0 / sw, 1.0 / sh];
            let mut p = [[0.0f32; 4]; 6]; // c0, c1, c2, c3, c5, c6
            p[0] = c0;
            if i % 2 == 0 {
                p[1] = ht.w;
                p[2] = ht.d;
                // projectM samples the (upside-down stored) source of the first pass with
                // `uv.y` flipped, so its "+1 texel" bias moves the other way (c3.w).
                let vsign = if i == 0 && self.rules.blur_src_flipped {
                    -1.0
                } else {
                    1.0
                };
                p[3] = [scale[i / 2], bias[i / 2], ht.w_div, vsign];
            } else {
                p[4] = [vt.w[0], vt.w[1], vt.d[0], vt.d[1]];
                p[5] = if i == 1 {
                    [vt.w_div, 1.0 - edge_darken, edge_darken, 5.0]
                } else {
                    [vt.w_div, 1.0, 0.0, 5.0]
                };
            }
            let ub = buffer_init(
                &self.device,
                "blur params",
                std::slice::from_ref(&p),
                wgpu::BufferUsages::UNIFORM,
            );
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blur"),
                layout: &self.pipes.bgl_blur,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ub.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(clamp_linear),
                    },
                ],
            });
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blur"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.blur[i].view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            rp.set_pipeline(if i % 2 == 0 {
                &self.pipes.blur_h
            } else {
                &self.pipes.blur_v
            });
            rp.set_bind_group(0, &bg, &[]);
            rp.draw(0..3, 0..1);
        }
    }

    fn blit(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let params: [[f32; 4]; 2] = [
            [
                self.cfg.width as f32,
                self.cfg.height as f32,
                self.cfg.width as f32,
                self.cfg.height as f32,
            ],
            [
                if is_srgb(self.cfg.output_format) {
                    1.0
                } else {
                    0.0
                },
                0.0,
                0.0,
                0.0,
            ],
        ];
        let ub = buffer_init(
            &self.device,
            "blit",
            std::slice::from_ref(&params),
            wgpu::BufferUsages::UNIFORM,
        );
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &self.pipes.bgl_blit,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ub.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.final_tex.view),
                },
            ],
        });
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        rp.set_pipeline(&self.pipes.blit);
        rp.set_bind_group(0, &bg, &[]);
        rp.draw(0..3, 0..1);
    }

    // ---- MD3 sprites -------------------------------------------------------------------------

    fn sprite_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        draws: &[&SpriteDraw],
    ) {
        self.pipes.sprites(&self.device).draw(
            &self.device,
            encoder,
            view,
            &self.pipes.bgl_tex,
            self.pipes.sampler(Filter::Linear, Address::Wrap, false),
            &self.sprite_images,
            draws,
        );
    }

    /// The sprites of `layer` (1..=4) onto `view`, one after the other in the order of `FrameInput::sprites`. With
    /// `CompatRules::md3_sprite_burn_twice` a burning sprite is drawn a second time right after the first (MD3 burns
    /// into the very image it draws on).
    fn draw_sprite_layer(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        layer: u8,
        view: &wgpu::TextureView,
    ) {
        let mut draws: Vec<&SpriteDraw> = Vec::new();
        for s in input.sprites.iter().filter(|s| s.layer == layer) {
            draws.push(s);
            if s.burn && self.rules.md3_sprite_burn_twice {
                draws.push(s);
            }
        }
        if !draws.is_empty() {
            self.sprite_pass(encoder, view, &draws);
        }
    }

    /// Layer 0: the sprites onto the composited image; those that burn are also drawn into the feedback image,
    /// which the composite has already read.
    fn draw_top_sprites(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        final_view: &wgpu::TextureView,
        feedback_view: &wgpu::TextureView,
    ) {
        let draws: Vec<&SpriteDraw> = input.sprites.iter().filter(|s| s.layer == 0).collect();
        if draws.is_empty() {
            return;
        }
        self.sprite_pass(encoder, final_view, &draws);
        let burn: Vec<&SpriteDraw> = draws.iter().copied().filter(|s| s.burn).collect();
        if !burn.is_empty() {
            self.sprite_pass(encoder, feedback_view, &burn);
        }
    }

    /// Shapes, waves, main wave and borders into the feedback image `main[next_i]`. Sprite layers 2 (after the
    /// shapes) and 3 (after the waves) split the batch into several passes; without them it is one pass.
    fn run_overlay(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &FrameInput,
        b: &Batch,
        next_i: usize,
        bg: &wgpu::BindGroup,
        leak: &wgpu::BindGroup,
    ) {
        const LABEL: &str = "shapes+waves+borders";
        let view = &self.main[next_i].view;
        if !input.sprites.iter().any(|s| matches!(s.layer, 2 | 3)) {
            self.run_batch(
                encoder,
                b,
                view,
                wgpu::LoadOp::Load,
                Some(bg),
                Some(leak),
                LABEL,
            );
            return;
        }
        let end = b.cmds.len();
        for (range, layer) in [
            (0..b.marks[0], Some(2)),
            (b.marks[0]..b.marks[1], Some(3)),
            (b.marks[1]..end, None),
        ] {
            if !range.is_empty() {
                self.run_batch_range(
                    encoder,
                    b,
                    range,
                    view,
                    wgpu::LoadOp::Load,
                    Some(bg),
                    Some(leak),
                    LABEL,
                );
            }
            if let Some(layer) = layer {
                self.draw_sprite_layer(encoder, input, layer, view);
            }
        }
    }

    // ---- fixed-function content ---------------------------------------------------------

    fn build_overlay(
        &self,
        b: &mut Batch,
        f: &FrameInput,
        aspect: (f32, f32),
        pm_wave: Option<wavegen::PmWaveGeom>,
    ) {
        let rules = &self.rules;
        let (w, h) = (self.cfg.width as f32, self.cfg.height as f32);
        let width = self.cfg.width;
        let (asp_x, asp_y) = aspect;
        let inv_aspect = (1.0 / asp_x, 1.0 / asp_y);
        let mode = self.stroke_mode();

        // ---- custom shapes ----
        // scratch shared by every instance: a preset can have thousands
        let mut g = geom::ShapeGeom::default();
        let mut tri: Vec<MeshVertex> = Vec::new();
        let mut edge: Vec<[f32; 2]> = Vec::new();
        let mut segs: Vec<WfVertex> = Vec::new();
        for (si, s) in f.shapes.iter().enumerate() {
            // a textured shape with an `image=` samples that user texture; its texture aspect is 1
            let image = s.image.as_ref().filter(|_| s.textured);
            geom::shape_geometry_into(
                &mut g,
                s,
                asp_y,
                if image.is_some() { 1.0 } else { asp_y },
                rules.max_shape_sides,
                s.textured,
            );
            let am = f
                .md2_blend
                .as_ref()
                .and_then(|m| m.shape_alpha_mult.get(si).copied())
                .unwrap_or(1.0);
            let c1 = geom::conv_color_alpha_mult(rules, s.rgba, am);
            let c2 = geom::conv_color_alpha_mult(rules, s.rgba2, am);
            let blend = if s.additive {
                Blend::SrcAlphaAdd
            } else {
                Blend::SrcAlpha
            };
            if s.textured {
                // MD2 textures the fill with the previous frame; uv per vertex
                tri.clear();
                tri.reserve(3 * g.pts.len().saturating_sub(2));
                for i in 1..g.pts.len() - 1 {
                    for k in [0, i, i + 1] {
                        tri.push(MeshVertex {
                            pos: g.pts[k],
                            diffuse: if k == 0 { c1 } else { c2 },
                            uv: [g.uv[k][0], g.uv[k][1], 0.0, 0.0],
                            rad_ang: [0.0; 2],
                        });
                    }
                }
                // no `gl_fill_ties` here: the nearest-sampled UV of a moved vertex flips texel choices at pixel centres
                // (`Zylot - Star Ornament` main 79.0 -> 72.2 dB)
                let tex_blend = if s.additive {
                    Blend::SrcAlphaAdd
                } else {
                    Blend::SrcAlpha
                };
                if let Some(im) = image {
                    // projectM `CustomShape::Draw`: the `image=` texture with its own sampler instead of the main image
                    b.tex_image(tex_blend, &tri, im);
                } else if si == 0 && rules.shape_main_nearest && self.last_blur_levels > 0 {
                    // projectM: `BlurTexture::Update` (only runs when a shader samples blur) leaves its (linear,
                    // clamp-to-edge) sampler bound on unit 0; the first shape draw consumes it and
                    // `Sampler::Unbind(0)` follows, so every later textured draw falls back to the texture's own
                    // NEAREST/REPEAT parameters. `last_blur_levels` is the *native* level: a diagnostic blur chain
                    // must not trigger it (projectM restores the sampler after its own forced blur).
                    b.tex_leak(tex_blend, &tri);
                } else {
                    b.tex(tex_blend, &tri);
                }
            } else {
                if rules.gl_fill_ties {
                    let colors: Vec<[f32; 4]> = (0..g.pts.len())
                        .map(|k| if k == 0 { c1 } else { c2 })
                        .collect();
                    let mut tris = geom::fan_to_list(&g.pts, &colors);
                    geom::gl_fill_ties(&mut tris, w, h);
                    b.flat(blend, &tris);
                } else {
                    b.flat_extend(Prim::Tri, blend, |out| {
                        geom::push_fan(out, &g.pts, c1, c2);
                    });
                }
            }
            // projectM `CustomShape::Draw`: `border_a > 0.0001f`
            if s.border_rgba[3] > 0.0001 {
                let bc = geom::conv_color_raw_alpha_mult(rules, s.border_rgba, am);
                let its = if s.thick { 4 } else { 1 };
                let (inc_x, inc_y) = if rules.shape_thick_full_pixel {
                    (2.0 / w, 2.0 / h)
                } else {
                    (1.0 / w, 1.0 / h)
                };
                edge.clear();
                edge.extend_from_slice(&g.pts[1..]); // sides + 1 points (closed)
                for it in 0..its {
                    match it {
                        1 => edge.iter_mut().for_each(|p| p[0] += inc_x),
                        2 => edge.iter_mut().for_each(|p| p[1] -= inc_y),
                        3 => edge.iter_mut().for_each(|p| p[0] -= inc_x),
                        _ => {}
                    }
                    geom::strip_segments_into(&mut segs, &edge, |_| bc);
                    b.segments(mode, LineRule::Diamond, blend, w, h, &segs);
                }
            }
        }
        b.mark(0);

        // ---- custom waves ----
        for (wave_i, cw) in f.waves.iter().enumerate() {
            let wam = f
                .md2_blend
                .as_ref()
                .and_then(|m| m.wave_alpha_mult.get(wave_i).copied())
                .unwrap_or(1.0);
            let n = cw.points.len();
            if n < 2 && !(cw.use_dots && n >= 1) {
                continue;
            }
            let mut pts: Vec<[f32; 2]> = Vec::with_capacity(n);
            let mut cols: Vec<[f32; 4]> = Vec::with_capacity(n);
            for p in &cw.points {
                pts.push(geom::wave_point_clip(rules, p, inv_aspect));
                cols.push(geom::conv_color_alpha_mult(rules, p.rgba, wam));
            }
            let blend = if cw.additive {
                Blend::SrcAlphaAdd
            } else {
                Blend::SrcAlpha
            };
            let (x_inc, y_inc) = if rules.wave_thick_ndc_inc {
                (1.0 / w, 1.0 / w)
            } else {
                (2.0 / w, 2.0 / h)
            };
            if cw.use_dots {
                // projectM draws every vertex of SmoothWave (the n source points *and* the n-1 interpolated ones)
                // as GL_POINTS and ignores gl_PointSize (always 1 px)
                let smoothed;
                let (pp, pc): (&[[f32; 2]], &[[f32; 4]]) = if rules.wave_dots_smoothed {
                    smoothed = smooth_with_colors(&pts, &cols);
                    (&smoothed.0, &smoothed.1)
                } else {
                    (&pts, &cols)
                };
                let size = if rules.point_size_honored {
                    (if width >= 1024 { 2.0 } else { 1.0 }) + if cw.thick { 1.0 } else { 0.0 }
                } else {
                    1.0
                };
                let v: Vec<WfVertex> = pp
                    .iter()
                    .zip(pc)
                    .map(|(p, c)| WfVertex { pos: *p, color: *c })
                    .collect();
                b.points(mode, blend, w, h, &v, size);
            } else {
                // SmoothWave interpolates positions; colours follow the source point (vo[j+1].Diffuse = vi[i].Diffuse)
                let (sp, sc) = smooth_with_colors(&pts, &cols);
                let its = if cw.thick { 4 } else { 1 };
                let mut sp = sp;
                for it in 0..its {
                    match it {
                        1 => sp.iter_mut().for_each(|p| p[0] += x_inc),
                        // the custom waves step y by `-y_inc` (projectM's main waveform: `+y_inc`)
                        2 => sp.iter_mut().for_each(|p| p[1] -= y_inc),
                        3 => sp.iter_mut().for_each(|p| p[0] -= x_inc),
                        _ => {}
                    }
                    let v = geom::strip_segments(&sp, |i| sc[i]);
                    b.segments(mode, LineRule::Strip, blend, w, h, &v);
                }
            }
        }
        b.mark(1);

        // ---- main wave ----
        self.build_main_wave(b, f, aspect, pm_wave);

        // ---- darken center ----
        if f.darken_center {
            b.flat(
                Blend::SrcAlpha,
                &geom::darken_center_triangles(asp_y, rules),
            );
        }

        // ---- borders ----
        for it in 0..2 {
            let (size_rgba, label_a) = if it == 0 {
                (f.borders.ob_rgba, f.borders.ob_rgba[3])
            } else {
                (f.borders.ib_rgba, f.borders.ib_rgba[3])
            };
            if label_a > 0.001 && !f.borders.hidden {
                let c = geom::conv_color_raw(rules, size_rgba);
                let (ob, ib) = (f.borders.ob_size, f.borders.ib_size);
                let mut tris = if rules.pm_border_mesh {
                    geom::border_triangles(it, ob, ib, c)
                } else {
                    geom::border_triangles_md2(it, ob, ib, c)
                };
                if rules.gl_fill_ties {
                    geom::gl_fill_ties(&mut tris, w, h);
                }
                b.flat(Blend::SrcAlpha, &tris);
            }
        }
    }

    /// One `DrawWave` pass of the MD2 family: the MD2 port, or MilkDrop 3's 17 modes (`md3_wave_modes`).
    fn main_wave_raw(
        &self,
        m: &crate::frame::MainWave,
        wl: &[f32],
        wr: &[f32],
        spec_l: &[f32],
        spec_r: &[f32],
        f: &FrameInput,
        aspect: (f32, f32),
        blending: bool,
    ) -> Option<wavegen::WaveGeom> {
        if self.rules.md3_wave_modes {
            return wavegen_md3::main_wave_geometry_md3_raw(
                m,
                wl,
                wr,
                spec_l,
                f.time as f32,
                [f.bass, f.mid, f.treb],
                f.imm_abs,
                self.cfg.width,
                aspect,
                blending,
            );
        }
        wavegen::main_wave_geometry_raw(
            m,
            wl,
            wr,
            spec_l,
            spec_r,
            f.time as f32,
            [f.bass, f.mid, f.treb],
            f.imm_abs,
            self.cfg.width,
            aspect,
            blending,
        )
    }

    /// `DrawWave` while MD2 blends two presets: the new wave (`it == 0`) and, if the wave mode changed, the old
    /// preset's mode drawn from the same parameters (`it == 1`), mixed by `CosineInterp(progress)`; alpha is blended
    /// the same way and the "don't draw if alpha < 0.004" test applies to the result. A mode this port does not
    /// draw (BeatDrop's modes beyond 8) cannot be blended: the new wave is then drawn alone, an unsupported new mode
    /// draws nothing.
    fn main_wave_geometry_blend(
        &self,
        f: &FrameInput,
        bl: &blend::Md2Blend,
        wl: &[f32],
        wr: &[f32],
        aspect: (f32, f32),
    ) -> Option<wavegen::WaveGeom> {
        let mw = &f.main_wave;
        let call = |m: &crate::frame::MainWave| {
            self.main_wave_raw(m, wl, wr, &mw.spectrum[0], &mw.spectrum[1], f, aspect, true)
        };
        let new = call(mw)?;
        let mut out = new.clone();
        if let Some(old_mode) = bl.old_wave_mode {
            let mut old_mw = mw.clone();
            old_mw.mode = old_mode;
            if let Some(old) = call(&old_mw) {
                let mix = blend::cosine_interp(bl.progress);
                let line = blend::blend_waves(
                    &blend::WaveLine {
                        verts: new.verts.clone(),
                        brk: new.brk,
                    },
                    &blend::WaveLine {
                        verts: old.verts,
                        brk: old.brk,
                    },
                    mix,
                );
                out.verts = line.verts;
                out.brk = line.brk;
                out.alpha = blend::blend_wave_alpha(new.alpha, old.alpha, mix);
            }
        }
        (out.alpha >= 0.004).then_some(out)
    }

    fn build_main_wave(
        &self,
        b: &mut Batch,
        f: &FrameInput,
        aspect: (f32, f32),
        pm_wave: Option<wavegen::PmWaveGeom>,
    ) {
        let rules = &self.rules;
        let mode = self.stroke_mode();
        let mw = &f.main_wave;
        let (w, h) = (self.cfg.width as f32, self.cfg.height as f32);
        if rules.pm_waveforms {
            if let Some(g) = pm_wave {
                self.build_main_wave_pm(b, f, g);
            }
            return;
        }
        let (wl, wr) = wavegen::prepare_main_waveform(mw);
        let geometry = match f.md2_blend.as_ref() {
            None => self
                .main_wave_raw(
                    mw,
                    &wl,
                    &wr,
                    &mw.spectrum[0],
                    &mw.spectrum[1],
                    f,
                    aspect,
                    false,
                )
                .filter(|g| g.alpha >= 0.004),
            Some(bl) => self.main_wave_geometry_blend(f, bl, &wl, &wr, aspect),
        };
        let Some(g) = geometry else {
            return;
        };
        let color = geom::conv_color_raw(rules, [g.rgb[0], g.rgb[1], g.rgb[2], g.alpha]);
        let blend = if mw.additive {
            Blend::SrcAlphaAdd
        } else {
            Blend::SrcAlpha
        };

        // TESSELLATE (SmoothWave, per strip)
        let strips: Vec<Vec<[f32; 2]>> = match g.brk {
            None => vec![wavegen::smooth_wave(&g.verts)],
            Some(k) => vec![
                wavegen::smooth_wave(&g.verts[..k]),
                wavegen::smooth_wave(&g.verts[k..]),
            ],
        };
        // MD2 only draws the thick / dots passes of the main wave from a texture size of 512 (`thick_needs_512`);
        // projectM (`build_main_wave_pm`) always draws them
        let its = if (mw.thick || mw.usedots) && (!rules.thick_needs_512 || self.cfg.width >= 512) {
            4
        } else {
            1
        };
        let (x_inc, y_inc) = (2.0 / w, 2.0 / h);
        let mut strips = strips;
        for it in 0..its {
            match it {
                1 => strips.iter_mut().flatten().for_each(|p| p[0] += x_inc),
                2 => strips.iter_mut().flatten().for_each(|p| p[1] -= y_inc),
                3 => strips.iter_mut().flatten().for_each(|p| p[0] -= x_inc),
                _ => {}
            }
            let mut v = Vec::new();
            for s in &strips {
                if mw.usedots {
                    v.extend(s.iter().map(|&pos| WfVertex { pos, color }));
                } else {
                    v.extend(geom::strip_segments(s, |_| color));
                }
            }
            if mw.usedots {
                b.points(mode, blend, w, h, &v, 1.0);
            } else {
                b.segments(mode, LineRule::Strip, blend, w, h, &v);
            }
        }
    }

    /// projectM main waveform (`Waveform::Draw`): vertices from [`wavegen::main_wave_geometry_projectm`] (computed by
    /// [`Renderer::advance_main_wave_pm`] every frame), line strip / loop, thick = 4 passes at (0,0) (+x) (+x,+y) (+y)
    /// pixel offsets.
    fn build_main_wave_pm(&self, b: &mut Batch, f: &FrameInput, g: wavegen::PmWaveGeom) {
        let rules = &self.rules;
        let mode = self.stroke_mode();
        let mw = &f.main_wave;
        let (w, h) = (self.cfg.width as f32, self.cfg.height as f32);
        if g.alpha <= 0.0 {
            return;
        }

        let color = geom::conv_color_raw(rules, [g.rgb[0], g.rgb[1], g.rgb[2], g.alpha]);
        let blend = if mw.additive {
            Blend::SrcAlphaAdd
        } else {
            Blend::SrcAlpha
        };
        let its = if mw.thick || mw.usedots { 4 } else { 1 };
        let (x_inc, y_inc) = (2.0 / w, 2.0 / h);
        let mut pts: Vec<[f32; 2]> = Vec::new();
        for mut strip in g.strips {
            for it in 0..its {
                match it {
                    1 => strip.iter_mut().for_each(|p| p[0] += x_inc),
                    // projectM's main waveform steps y by `+y_inc` (the custom waves and MD2's main wave use `-y_inc`)
                    2 => strip.iter_mut().for_each(|p| p[1] += y_inc),
                    3 => strip.iter_mut().for_each(|p| p[0] -= x_inc),
                    _ => {}
                }
                if mw.usedots {
                    let v: Vec<WfVertex> =
                        strip.iter().map(|&pos| WfVertex { pos, color }).collect();
                    b.points(mode, blend, w, h, &v, 1.0);
                } else if strip.iter().any(|p| *p != strip[0]) {
                    // Waveform::Draw disables GL_LINE_SMOOTH: half-open strip rule, not diamond exit.
                    pts.clear();
                    pts.extend_from_slice(&strip);
                    if g.is_loop {
                        pts.push(strip[0]);
                    }
                    let v = geom::strip_segments(&pts, |_| color);
                    b.segments(mode, LineRule::Strip, blend, w, h, &v);
                }
            }
        }
    }

    /// `GetVertices` of projectM's main waveform. Runs every frame (also when the overlays of the frame are lost or the
    /// alpha is 0) because it advances the persistent wave-buffer tail ([`wavegen::PmWaveTail`]).
    fn advance_main_wave_pm(&mut self, f: &FrameInput) -> Option<wavegen::PmWaveGeom> {
        wavegen::main_wave_geometry_projectm(
            &f.main_wave,
            f.time as f32,
            [f.bass, f.mid, f.treb],
            (self.cfg.width, self.cfg.height),
            &mut self.main_wave_tail,
        )
    }

    /// `ShowToUser_NoShaders` (== projectM `VideoEcho` + `Filters`).
    fn build_noshader_comp(&self, f: &FrameInput) -> Batch {
        let rules = &self.rules;
        let (w, h) = (self.cfg.width, self.cfg.height);
        let mut b = Batch::default();
        let p = &f.post;

        // hue-shader corner tint
        let mut shade = [[1.0f32; 3]; 4];
        let amount = p.fshader;
        if rules.hue_tint_always {
            shade = geom::hue_shade(f.time as f32, f.hue_offsets.unwrap_or(self.rot.hue_offsets));
        } else if amount > 0.001 {
            let s = geom::hue_shade(f.time as f32, f.hue_offsets.unwrap_or(self.rot.hue_offsets));
            for i in 0..4 {
                for k in 0..3 {
                    shade[i][k] = s[i][k] * amount + 1.0 * (1.0 - amount);
                }
            }
        }
        let col = |mult: f32| -> [[f32; 4]; 4] {
            let mut c = [[0.0; 4]; 4];
            for (k, out) in c.iter_mut().enumerate() {
                // corner k of `echo_quad` is bottom-left, bottom-right, top-left, top-right
                let s = if rules.shade_top_first { k ^ 2 } else { k };
                *out = geom::conv_color_raw(
                    rules,
                    [
                        mult * shade[s][0],
                        mult * shade[s][1],
                        mult * shade[s][2],
                        1.0,
                    ],
                );
            }
            c
        };
        let gamma = p.gamma;
        let orient = p.echo_orient % 4;

        let mult = if rules.echo_quad_aspect {
            geom::echo_aspect_mult(w, h)
        } else {
            [1.0, 1.0]
        };
        if p.echo_alpha > 0.001 {
            for i in 0..2 {
                let zoom = if i == 0 { 1.0 } else { p.echo_zoom };
                let lo = 0.5 - 0.5 / zoom;
                let hi = 0.5 + 0.5 / zoom;
                let mut uv = [[lo, hi], [hi, hi], [lo, lo], [hi, lo]];
                if i == 1 {
                    for t in uv.iter_mut() {
                        if orient % 2 != 0 {
                            t[0] = 1.0 - t[0];
                        }
                        if orient >= 2 {
                            t[1] = 1.0 - t[1];
                        }
                    }
                }
                let mix = if i == 1 {
                    p.echo_alpha
                } else {
                    1.0 - p.echo_alpha
                };
                b.tex(
                    if i == 0 { Blend::Replace } else { Blend::Add },
                    &geom::echo_quad(w, h, uv, col(mix), rules.echo_diag_flipped, mult),
                );
                if gamma > 0.001 {
                    let n_redraws = (gamma - 0.0001) as i32;
                    for r in 0..n_redraws {
                        let g = if r == n_redraws - 1 {
                            gamma - (gamma - 0.0001) as i32 as f32
                        } else {
                            1.0
                        };
                        b.tex(
                            Blend::Add,
                            &geom::echo_quad(w, h, uv, col(g * mix), rules.echo_diag_flipped, mult),
                        );
                    }
                }
            }
        } else {
            let uv = [[0.0, 1.0], [1.0, 1.0], [0.0, 0.0], [1.0, 0.0]];
            let n_passes = (gamma - rules.gamma_pass_eps) as i32 + 1;
            for pass in 0..n_passes {
                let g = if pass == n_passes - 1 {
                    gamma - pass as f32
                } else {
                    1.0
                };
                b.tex(
                    if pass == 0 {
                        Blend::Replace
                    } else {
                        Blend::Add
                    },
                    &geom::echo_quad(w, h, uv, col(g), rules.echo_diag_flipped, mult),
                );
            }
        }

        // brighten / darken / solarize / invert: white quads with destination-colour blends
        let white = [1.0f32; 4];
        let quad = |blend: Blend, b: &mut Batch| {
            let v = geom::echo_quad(w, h, [[0.0; 2]; 4], [white; 4], false, [1.0, 1.0]);
            let flat: Vec<WfVertex> = v
                .iter()
                .map(|m| WfVertex {
                    pos: m.pos,
                    color: white,
                })
                .collect();
            b.flat(blend, &flat);
        };
        if p.brighten {
            quad(Blend::InvDstZero, &mut b);
            quad(Blend::ZeroDst, &mut b);
            quad(Blend::InvDstZero, &mut b);
        }
        if p.darken {
            quad(Blend::ZeroDst, &mut b);
        }
        if p.solarize {
            quad(Blend::ZeroInvDst, &mut b);
            quad(Blend::DstOne, &mut b);
        }
        if p.invert {
            quad(Blend::InvDstZero, &mut b);
        }
        b
    }
}

const BLACK: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// `SmoothWave` carrying per-vertex colours (`vo[j+1].Diffuse = vi[i].Diffuse`).
fn smooth_with_colors(pts: &[[f32; 2]], cols: &[[f32; 4]]) -> (Vec<[f32; 2]>, Vec<[f32; 4]>) {
    let sp = wavegen::smooth_wave(pts);
    let n = pts.len();
    let mut sc = Vec::with_capacity(sp.len());
    for c in &cols[..n - 1] {
        sc.push(*c);
        sc.push(*c);
    }
    sc.push(cols[n - 1]);
    (sp, sc)
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
