// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! GPU warp UV readback.

use super::*;

/// Persistent resources of the GPU warp UV readback ([`Renderer::gpu_warp_vertex_uv`]) for a `w x h` vertex grid.
///
/// The readback only depends on a frame's `warp_gpu` and texel offset, not on any image, so it is submitted as its
/// own small submission at the end of the frame that produced those inputs and only waited for (this submission
/// alone, never the frame's main work) by the next frame's motion vector pass.
pub(super) struct UvReadback {
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) vertices: wgpu::Buffer,
    pub(super) uniforms: wgpu::Buffer,
    pub(super) bind_group: wgpu::BindGroup,
    pub(super) texture: wgpu::Texture,
    pub(super) view: wgpu::TextureView,
    pub(super) out: wgpu::Buffer,
    pub(super) bytes_per_row: u32,
    /// The submitted readback of the previous frame's `warp_gpu` and the texel offset it was evaluated with.
    pub(super) pending: Option<(wgpu::SubmissionIndex, [f32; 2])>,
}

impl UvReadback {
    pub(super) fn new(device: &wgpu::Device, pipes: &Pipelines, w: u32, h: u32) -> Self {
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
            size: u64::from(bytes_per_row) * u64::from(h),
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
    pub(super) fn submit(
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
    pub(super) fn collect(
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
        let mut out = vec![[0.0f32; 2]; self.w as usize * self.h as usize];
        for y in 0..self.h {
            for x in 0..self.w {
                let o = y as usize * self.bytes_per_row as usize + x as usize * 16;
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
