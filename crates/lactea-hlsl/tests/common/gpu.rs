#![allow(dead_code)]
//! Headless wgpu harness: renders a pixel shader (our WGSL or glslang's SPIR-V) at 64x64 with
//! fixed deterministic inputs through the ABI bind layout.

use lactea_abi::*;
use wgpu::util::DeviceExt;

pub const SIZE: u32 = 64;
const GRID: u32 = 32;

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u32 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 32) as u32
    }
    pub fn f(&mut self) -> f32 {
        self.next() as f32 / 4294967296.0
    }
}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_name: String,
    main: wgpu::TextureView,
    blur: [wgpu::TextureView; 3],
    noise: [wgpu::TextureView; 4],
    vol: [wgpu::TextureView; 2],
    user: wgpu::TextureView,
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    index_count: u32,
    vs_vec4: wgpu::ShaderModule,
    vs_vec2: wgpu::ShaderModule,
    mu_buf: wgpu::Buffer,
    pub target: wgpu::Texture,
    readback: wgpu::Buffer,
    pub kind_state: Option<PassKind>,
}

fn tex2d(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    w: u32,
    h: u32,
    data: &[u8],
) -> wgpu::TextureView {
    let t = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        data,
    );
    t.create_view(&Default::default())
}

fn tex3d(device: &wgpu::Device, queue: &wgpu::Queue, n: u32, data: &[u8]) -> wgpu::TextureView {
    let t = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: n,
                height: n,
                depth_or_array_layers: n,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        data,
    );
    t.create_view(&Default::default())
}

fn pattern(w: u32, h: u32, phase: f32, freq: f32) -> Vec<u8> {
    let mut v = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let r = 0.5 + 0.5 * (freq * std::f32::consts::TAU * fx + phase).sin();
            let g = 0.5 + 0.5 * (freq * std::f32::consts::TAU * fy + 2.0 * phase).cos();
            let b = 0.5 + 0.5 * (freq * 3.1 * (fx + fy) + 3.0 * phase).sin();
            let chk = if ((x / 8) + (y / 8)) % 2 == 0 {
                1.0
            } else {
                0.85
            };
            v.extend_from_slice(&[
                (r * chk * 255.0) as u8,
                (g * chk * 255.0) as u8,
                (b * 255.0) as u8,
                255,
            ]);
        }
    }
    v
}

fn noise_bytes(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng(seed);
    (0..n).map(|_| (r.next() >> 8) as u8).collect()
}

pub fn make_mesh(comp: bool) -> (Vec<MeshVertex>, Vec<u32>) {
    let mut verts = Vec::new();
    for iy in 0..=GRID {
        for ix in 0..=GRID {
            let u = ix as f32 / GRID as f32;
            let v = iy as f32 / GRID as f32;
            let (x, y) = (u * 2.0 - 1.0, 1.0 - v * 2.0);
            let (xc, yc) = (u - 0.5, v - 0.5);
            let rad = (xc * xc + yc * yc).sqrt() * 2.0;
            let ang = yc.atan2(xc) + std::f32::consts::PI;
            let (uw, vw) = if comp {
                (u, v)
            } else {
                (
                    u + 0.04 * (7.0 * u + 3.0 * v).sin(),
                    v + 0.04 * (5.0 * u - 4.0 * v).cos(),
                )
            };
            let diffuse = if comp {
                [
                    0.5 + 0.5 * (3.0 * u + 1.0).cos(),
                    0.5 + 0.5 * (2.0 * v + 2.0).sin(),
                    0.5 + 0.5 * (u + v).cos(),
                    0.9,
                ]
            } else {
                [1.0, 1.0, 1.0, 0.9 + 0.1 * (u * 4.0).sin()]
            };
            verts.push(MeshVertex {
                pos: [x, y],
                diffuse,
                uv: [uw, vw, u, v],
                rad_ang: [rad, ang],
            });
        }
    }
    let mut idx = Vec::new();
    for iy in 0..GRID {
        for ix in 0..GRID {
            let a = iy * (GRID + 1) + ix;
            let (b, c, d) = (a + 1, a + GRID + 1, a + GRID + 2);
            idx.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    (verts, idx)
}

type M4 = [[f32; 4]; 4];
fn mmul(a: &M4, b: &M4) -> M4 {
    let mut r = [[0.0; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            for k in 0..4 {
                r[i][j] += a[i][k] * b[k][j];
            }
        }
    }
    r
}
fn rot_x(a: f32) -> M4 {
    let (s, c) = a.sin_cos();
    [
        [1., 0., 0., 0.],
        [0., c, s, 0.],
        [0., -s, c, 0.],
        [0., 0., 0., 1.],
    ]
}
fn rot_y(a: f32) -> M4 {
    let (s, c) = a.sin_cos();
    [
        [c, 0., -s, 0.],
        [0., 1., 0., 0.],
        [s, 0., c, 0.],
        [0., 0., 0., 1.],
    ]
}
fn rot_z(a: f32) -> M4 {
    let (s, c) = a.sin_cos();
    [
        [c, s, 0., 0.],
        [-s, c, 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ]
}
fn trans(x: f32, y: f32, z: f32) -> M4 {
    [
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [x, y, z, 1.],
    ]
}

/// Fixed uniform values (deterministic stand-ins for ApplyShaderParams).
pub fn make_uniforms() -> MilkUniforms {
    let mut mu = MilkUniforms::default();
    let time = 7.123f32;
    mu.rand_frame = [0.123, 0.456, 0.789, 0.321];
    mu.rand_preset = [0.77, 0.31, 0.52, 0.93];
    mu.c[0] = [1.0, 0.5625, 1.0, 1.0 / 0.5625];
    mu.c[1] = [1.0, 0.5625, 1.0, 1.0 / 0.5625];
    mu.c[2] = [time, 60.0, 427.0, 0.4];
    mu.c[3] = [0.9, 1.15, 0.7, 0.3333 * 0.7];
    mu.c[4] = [1.1, 0.95, 1.05, 0.3333 * 1.05];
    mu.c[5] = [0.4, 0.1, 0.6, 0.05];
    mu.c[6] = [0.7, 0.02, 0.1, 0.9];
    mu.c[7] = [
        SIZE as f32,
        SIZE as f32,
        1.0 / SIZE as f32,
        1.0 / SIZE as f32,
    ];
    let f = |k: [f32; 4], p: [f32; 4], sin: bool| {
        let mut o = [0.0; 4];
        for i in 0..4 {
            let a = time * k[i] + p[i];
            o[i] = 0.5 + 0.5 * if sin { a.sin() } else { a.cos() };
        }
        o
    };
    mu.c[8] = f([0.329, 1.293, 5.070, 20.051], [1.2, 3.9, 2.5, 5.4], false);
    mu.c[9] = f([0.329, 1.293, 5.070, 20.051], [1.2, 3.9, 2.5, 5.4], true);
    mu.c[10] = f(
        [0.0050, 0.0085, 0.0133, 0.0217],
        [2.7, 5.3, 4.5, 3.8],
        false,
    );
    mu.c[11] = f([0.0050, 0.0085, 0.0133, 0.0217], [2.7, 5.3, 4.5, 3.8], true);
    mu.c[12] = [6.0, 6.0, 6.0, 0.0];
    mu.c[13] = [0.05, 0.8, 0.0, 0.7];
    let mut r = Rng(0x1234_5678_9abc);
    for i in 0..8 {
        for j in 0..4 {
            mu.q[i][j] = (r.f() * 2.0 - 1.0) * 1.5;
        }
    }
    for i in 0..NUM_ROT {
        let m = if i < 20 {
            let b = [
                r.f() * std::f32::consts::TAU,
                r.f() * std::f32::consts::TAU,
                r.f() * std::f32::consts::TAU,
            ];
            let s = [r.f() * 0.6 - 0.3, r.f() * 0.6 - 0.3, r.f() * 0.6 - 0.3];
            let x = [r.f() - 0.5, r.f() - 0.5, r.f() - 0.5];
            let t = mmul(&rot_x(b[0] + s[0] * time), &trans(x[0], x[1], x[2]));
            let t = mmul(&t, &rot_z(b[2] + s[2] * time));
            mmul(&t, &rot_y(b[1] + s[1] * time))
        } else {
            let t = mmul(
                &rot_x(r.f() * std::f32::consts::TAU),
                &trans(r.f(), r.f(), r.f()),
            );
            let t = mmul(&t, &rot_z(r.f() * std::f32::consts::TAU));
            mmul(&t, &rot_y(r.f() * std::f32::consts::TAU))
        };
        // rot[i][col][row] = m[row][col]
        #[allow(clippy::needless_range_loop)] // the transposed index pair is the point
        for col in 0..3 {
            for row in 0..4 {
                mu.rot[i][col][row] = m[row][col];
            }
        }
    }
    let n = [
        SIZE as f32,
        SIZE as f32,
        1.0 / SIZE as f32,
        1.0 / SIZE as f32,
    ];
    let v = [16.0, 16.0, 1.0 / 16.0, 1.0 / 16.0];
    mu.texsize_noise = [n, n, n, n, v, v];
    mu
}

impl Gpu {
    pub fn new() -> Option<Gpu> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .ok()?;
        let adapter_name = adapter.get_info().name;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
        let n = SIZE;
        let main = tex2d(&device, &queue, n, n, &pattern(n, n, 0.3, 3.0));
        let blur = [
            tex2d(&device, &queue, n, n, &pattern(n, n, 1.1, 1.0)),
            tex2d(&device, &queue, n, n, &pattern(n, n, 2.2, 0.7)),
            tex2d(&device, &queue, n, n, &pattern(n, n, 3.3, 0.5)),
        ];
        let noise = [
            tex2d(
                &device,
                &queue,
                n,
                n,
                &noise_bytes((n * n * 4) as usize, 11),
            ),
            tex2d(
                &device,
                &queue,
                n,
                n,
                &noise_bytes((n * n * 4) as usize, 22),
            ),
            tex2d(
                &device,
                &queue,
                n,
                n,
                &noise_bytes((n * n * 4) as usize, 33),
            ),
            tex2d(
                &device,
                &queue,
                n,
                n,
                &noise_bytes((n * n * 4) as usize, 44),
            ),
        ];
        let vol = [
            tex3d(&device, &queue, 16, &noise_bytes(16 * 16 * 16 * 4, 55)),
            tex3d(&device, &queue, 16, &noise_bytes(16 * 16 * 16 * 4, 66)),
        ];
        let mut chk = Vec::new();
        for y in 0..16u32 {
            for x in 0..16u32 {
                let c = ((x / 4) + (y / 4)) % 2 == 0;
                chk.extend_from_slice(&if c {
                    [230, 60, 30, 255]
                } else {
                    [20, 140, 220, 255]
                });
            }
        }
        let user = tex2d(&device, &queue, 16, 16, &chk);

        let make = |comp: bool| {
            let (v, i) = make_mesh(comp);
            (bytemuck::cast_slice::<MeshVertex, u8>(&v).to_vec(), i)
        };
        let (vb, idx) = make(false);
        let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: &vb,
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&idx),
            usage: wgpu::BufferUsages::INDEX,
        });
        let vs_src = |uv: &str| {
            format!(
                "struct VIn {{ @location(0) pos: vec2<f32>, @location(1) diffuse: vec4<f32>, @location(2) uv: vec4<f32>, @location(3) rad_ang: vec2<f32> }}
struct VOut {{ @builtin(position) position: vec4<f32>, @location(0) diffuse: vec4<f32>, @location(1) uv: {uv}, @location(2) rad_ang: vec2<f32> }}
@vertex fn vs_main(v: VIn) -> VOut {{ var o: VOut; o.position = vec4<f32>(v.pos, 0.0, 1.0); o.diffuse = v.diffuse; o.uv = v.uv{sw}; o.rad_ang = v.rad_ang; return o; }}",
                sw = if uv == "vec2<f32>" { ".xy" } else { "" }
            )
        };
        let vs_vec4 = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(vs_src("vec4<f32>").into()),
        });
        let vs_vec2 = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(vs_src("vec2<f32>").into()),
        });
        let mu = make_uniforms();
        let mu_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&mu),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (SIZE * SIZE * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Some(Gpu {
            device,
            queue,
            adapter_name,
            main,
            blur,
            noise,
            vol,
            user,
            vertex_buf,
            index_buf,
            index_count: idx.len() as u32,
            vs_vec4,
            vs_vec2,
            mu_buf,
            target,
            readback,
            kind_state: None,
        })
    }

    fn view_for(&self, src: &TexSource, dim: TexDim) -> &wgpu::TextureView {
        match src {
            TexSource::Main => &self.main,
            TexSource::Blur(n) => &self.blur[(*n as usize - 1).min(2)],
            TexSource::Noise(NoiseTex::Lq) => &self.noise[0],
            TexSource::Noise(NoiseTex::LqLite) => &self.noise[1],
            TexSource::Noise(NoiseTex::Mq) => &self.noise[2],
            TexSource::Noise(NoiseTex::Hq) => &self.noise[3],
            TexSource::Noise(NoiseTex::VolLq) => &self.vol[0],
            TexSource::Noise(NoiseTex::VolHq) => &self.vol[1],
            TexSource::User(_) => {
                let _ = dim;
                &self.user
            }
        }
    }

    fn size_of(&self, src: &TexSource) -> [f32; 4] {
        let (w, h) = match src {
            TexSource::User(_) => (16.0, 16.0),
            TexSource::Noise(NoiseTex::VolLq | NoiseTex::VolHq) => (16.0, 16.0),
            _ => (SIZE as f32, SIZE as f32),
        };
        [w, h, 1.0 / w, 1.0 / h]
    }

    pub fn group_layouts(
        &self,
        iface: &ShaderInterface,
    ) -> (wgpu::BindGroupLayout, wgpu::BindGroupLayout) {
        let d = &self.device;
        let g0 = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let mut e = vec![wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }];
        for (i, tb) in iface.textures.iter().enumerate() {
            e.push(wgpu::BindGroupLayoutEntry {
                binding: texture_binding(i as u32),
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: if tb.dim == TexDim::D3 {
                        wgpu::TextureViewDimension::D3
                    } else {
                        wgpu::TextureViewDimension::D2
                    },
                    multisampled: false,
                },
                count: None,
            });
            e.push(wgpu::BindGroupLayoutEntry {
                binding: sampler_binding(i as u32),
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            });
        }
        let g1 = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &e,
        });
        (g0, g1)
    }

    /// Render the fragment module (entry point `entry`) and return RGBA8 (row 0 = top).
    pub fn render(
        &self,
        kind: PassKind,
        fs: &wgpu::ShaderModule,
        entry: &str,
        spirv_vec2_uv: bool,
        iface: &ShaderInterface,
        texsize_slots: &[TexSource],
    ) -> Vec<u8> {
        let d = &self.device;
        let (g0l, g1l) = self.group_layouts(iface);
        let layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&g0l), Some(&g1l)],
            immediate_size: 0,
        });
        let vs = if spirv_vec2_uv && kind == PassKind::Comp {
            &self.vs_vec2
        } else {
            &self.vs_vec4
        };
        let attrs = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 8,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 24,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 40,
                shader_location: 3,
            },
        ];
        let vbl = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attrs,
        };
        let pipeline = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: vs,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(vbl)],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: fs,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let g0 = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &g0l,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.mu_buf.as_entire_binding(),
            }],
        });
        let mut pu = PassUniforms::default();
        for (i, s) in texsize_slots.iter().enumerate().take(MAX_PASS_TEXTURES) {
            pu.texsize[i] = self.size_of(s);
        }
        let pu_buf = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&pu),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let samplers: Vec<wgpu::Sampler> = iface
            .textures
            .iter()
            .map(|tb| {
                let addr = if tb.address == Address::Wrap {
                    wgpu::AddressMode::Repeat
                } else {
                    wgpu::AddressMode::ClampToEdge
                };
                let f = if tb.filter == Filter::Linear {
                    wgpu::FilterMode::Linear
                } else {
                    wgpu::FilterMode::Nearest
                };
                d.create_sampler(&wgpu::SamplerDescriptor {
                    address_mode_u: addr,
                    address_mode_v: addr,
                    address_mode_w: addr,
                    mag_filter: f,
                    min_filter: f,
                    mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                    ..Default::default()
                })
            })
            .collect();
        let mut entries = vec![wgpu::BindGroupEntry {
            binding: 0,
            resource: pu_buf.as_entire_binding(),
        }];
        for (i, tb) in iface.textures.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: texture_binding(i as u32),
                resource: wgpu::BindingResource::TextureView(self.view_for(&tb.source, tb.dim)),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: sampler_binding(i as u32),
                resource: wgpu::BindingResource::Sampler(&samplers[i]),
            });
        }
        let g1 = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &g1l,
            entries: &entries,
        });
        let view = self.target.create_view(&Default::default());
        let mut enc = d.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &g0, &[]);
            pass.set_bind_group(1, &g1, &[]);
            pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
            pass.set_index_buffer(self.index_buf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.index_count, 0, 0..1);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(SIZE * 4),
                    rows_per_image: Some(SIZE),
                },
            },
            wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        let slice = self.readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
        let _ = d.poll(wgpu::PollType::wait_indefinitely());
        let data = { slice.get_mapped_range().unwrap().to_vec() };
        self.readback.unmap();
        data
    }
}
