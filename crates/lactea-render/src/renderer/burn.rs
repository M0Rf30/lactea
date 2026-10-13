// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `ProjectM::BurnInTexture`: one textured quad copied into the image the next warp samples.

use super::*;
use wgpu::util::DeviceExt;

const SRC: &str = r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) clip: vec4<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    o.position = clip;
    o.uv = uv;
    return o;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
"#;

const ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x2];

/// Corner of the quad: homogeneous clip position and texture coordinate (row 0 = top).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    clip: [f32; 4],
    uv: [f32; 2],
}

/// The copy pipeline (replace, no blending), built on the first burn.
pub(super) struct BurnPipe {
    pipeline: wgpu::RenderPipeline,
}

impl BurnPipe {
    pub(super) fn new(device: &wgpu::Device, bgl_tex: &wgpu::BindGroupLayout) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("burn"),
            source: wgpu::ShaderSource::Wgsl(SRC.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("burn"),
            bind_group_layouts: &[Some(bgl_tex)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("burn"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
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
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: INTERNAL_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline }
    }
}

/// The quad of `CopyTexture::Copy(left, top, width, height)` for a `target_w x target_h` target: projectM scales the
/// unit quad by `(width / W, height / H)` and feeds `(left / W, top / H)` through the matrix's fourth *column*, which
/// the row-vector product `position * matrix` turns into the homogeneous `w = 1 + x left / W + y top / H`, not into
/// a translation. The same arithmetic is kept so a burn lands where projectM puts it.
fn quad(target: (u32, u32), rect: [i32; 4]) -> [Vertex; 4] {
    let (w, h) = (target.0 as f32, target.1 as f32);
    let [left, top, width, height] = rect.map(|v| v as f32);
    let (sx, sy) = (width / w, height / h);
    let (tx, ty) = (left / w, top / h);
    // triangle strip: top-left, top-right, bottom-left, bottom-right (y up in clip space)
    [
        (-1.0f32, 1.0f32, 0.0f32, 0.0f32),
        (1.0, 1.0, 1.0, 0.0),
        (-1.0, -1.0, 0.0, 1.0),
        (1.0, -1.0, 1.0, 1.0),
    ]
    .map(|(x, y, u, v)| Vertex {
        clip: [x * sx, y * sy, 0.0, 1.0 + x * tx + y * ty],
        uv: [u, v],
    })
}

impl Renderer {
    /// `ProjectM::BurnInTexture`: draws `rgba8` (`w x h`, row 0 = top, straight alpha ignored) as one quad over the
    /// image the next frame's warp reads, opaque and without blending. `rect = [left, top, width, height]` in
    /// target pixels, with projectM's placement arithmetic (see [`quad`]); a rectangle of zero size draws nothing.
    /// Unlike projectM's OpenGL texture id the pixels are copied in, so the caller keeps its buffer.
    pub fn burn_texture(
        &mut self,
        rgba8: &[u8],
        w: u32,
        h: u32,
        rect: [i32; 4],
    ) -> Result<(), RenderError> {
        check_frames(
            "burn texture",
            self.device.limits().max_texture_dimension_2d,
            w,
            h,
            &[rgba8],
        )?;
        let src = upload_2d(&self.device, &self.queue, w, h, rgba8, false, "burn");
        let pipe = self
            .pipes
            .burn
            .get_or_init(|| BurnPipe::new(&self.device, &self.pipes.bgl_tex));
        let verts = quad((self.cfg.width, self.cfg.height), rect);
        let buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("burn verts"),
                contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("burn"),
            layout: &self.pipes.bgl_tex,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(self.pipes.sampler(
                        Filter::Point,
                        Address::Clamp,
                        true,
                    )),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("burn"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("burn"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.main[self.main_idx].view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&pipe.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.set_vertex_buffer(0, buf.slice(..));
            pass.draw(0..4, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::quad;

    /// A burn at the origin covers the centred `width x height` rectangle of the target, `w` stays 1.
    #[test]
    fn origin_burn_scales_about_the_centre() {
        let q = quad((100, 50), [0, 0, 50, 25]);
        assert_eq!(q[0].clip, [-0.5, 0.5, 0.0, 1.0]);
        assert_eq!(q[3].clip, [0.5, -0.5, 0.0, 1.0]);
    }
}
