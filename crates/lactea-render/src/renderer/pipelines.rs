// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Shared pipeline, layout and sampler construction.

use super::*;

pub(super) const MESH_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4, 2 => Float32x4, 3 => Float32x2];
pub(super) const WF_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];

pub(super) fn mesh_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<MeshVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &MESH_ATTRS,
    }
}
pub(super) fn wf_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<WfVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &WF_ATTRS,
    }
}

pub(super) const WARP_ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x2, 4 => Float32x2, 5 => Float32x2
];

pub(super) fn warp_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<WarpVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &WARP_ATTRS,
    }
}

pub(super) fn sampler(
    device: &wgpu::Device,
    linear: bool,
    wrap: bool,
    level0_only: bool,
) -> wgpu::Sampler {
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

pub(super) fn tex_entry(
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
pub(super) fn samp_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}
pub(super) fn ubo_entry(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
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
    pub(super) fn new(device: &wgpu::Device, output_format: wgpu::TextureFormat) -> Self {
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
            source: wgpu::ShaderSource::Wgsl(shaders::mesh_vs().into()),
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
        for b in Blend::ALL {
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
        for b in Blend::ALL {
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
            burn: Default::default(),
            sprite_out: Default::default(),
        }
    }

    pub(super) fn sampler(&self, f: Filter, a: Address, level0_only: bool) -> &wgpu::Sampler {
        let i = match (f, a) {
            (Filter::Linear, Address::Wrap) => 0,
            (Filter::Linear, Address::Clamp) => 1,
            (Filter::Point, Address::Wrap) => 2,
            (Filter::Point, Address::Clamp) => 3,
        };
        &self.samplers[i + if level0_only { 4 } else { 0 }]
    }

    pub(super) fn sprites(&self, device: &wgpu::Device) -> &SpritePipes {
        self.sprite
            .get_or_init(|| SpritePipes::new(device, &self.bgl_tex, INTERNAL_FORMAT))
    }

    /// Sprite pipelines drawing into `format`.
    pub(super) fn sprites_for(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> &SpritePipes {
        if format == INTERNAL_FORMAT {
            return self.sprites(device);
        }
        self.sprite_out
            .get_or_init(|| SpritePipes::new(device, &self.bgl_tex, format))
    }
}

/// `s.wgsl` (entry `fs_main(in: VsOut) -> @location(0) vec4<f32>`) with a second output `@location(1)` = `in.uv.xy`.
pub(super) fn with_uv_output(wgsl: &str) -> Option<String> {
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
