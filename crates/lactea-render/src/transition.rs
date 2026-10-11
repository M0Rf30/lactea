// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! projectM-style preset transitions: two finished preset images are blended by a full-screen
//! shader (one of six built-ins, chosen at random per transition) while both presets keep running.
//!
//! Behaviour read from projectM's `PresetTransition` / `TransitionShaderManager` and the six
//! `TransitionShaders/*` fragment shaders (GLSL 3.30); the shader bodies are re-expressed in WGSL:
//!
//! * `fragCoord` is `gl_FragCoord` (y up, pixel centres), `uv = fragCoord / iResolution.xy`. Our
//!   preset images are stored top-down (dump orientation), so every `texture(iChannel, p)` of the
//!   GLSL becomes a sample at `(p.x, 1 - p.y)`.
//! * Samplers are linear / clamp-to-edge, textures single-level (`textureSampleLevel(.., 0)`).
//! * `mod(x, y)` is GLSL's floored modulo; `smoothstep` is the plain Hermite formula (the plasma
//!   shader passes `edge0 > edge1`).
//! * The Zoom-blur shader's `random()` reads `gl_FragCoord.z`, which is 0.5 for projectM's quad.
//! * The output alpha is 1 and the quad replaces the target (no blending).
//!
//! The renderer only consumes the finished images; choosing the shader and the four static random
//! integers (deterministically from the run seed) is the engine's job.

use wgpu::util::DeviceExt;

/// The six built-in transition shaders, in projectM's `TransitionShaderManager` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransitionKind {
    /// Expanding circle reveal.
    Circle = 0,
    /// Plasma pattern blend.
    Plasma = 1,
    /// Plain cross-fade.
    SimpleBlend = 2,
    /// Sweeping wipe.
    Sweep = 3,
    /// Warping blend.
    Warp = 4,
    /// Zoom-blur blend.
    ZoomBlur = 5,
}

impl TransitionKind {
    /// All shaders in `TransitionShaderManager` order.
    pub const ALL: [TransitionKind; 6] = [
        TransitionKind::Circle,
        TransitionKind::Plasma,
        TransitionKind::SimpleBlend,
        TransitionKind::Sweep,
        TransitionKind::Warp,
        TransitionKind::ZoomBlur,
    ];

    /// Position in [`TransitionKind::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// `i % 6`, like `m_transitionShaders.at(rand % size)`.
    pub fn from_index(i: u32) -> Self {
        Self::ALL[i as usize % Self::ALL.len()]
    }

    /// Lower-case shader name (`"circle"`, `"zoom_blur"`, ...).
    pub fn name(self) -> &'static str {
        match self {
            TransitionKind::Circle => "circle",
            TransitionKind::Plasma => "plasma",
            TransitionKind::SimpleBlend => "simple_blend",
            TransitionKind::Sweep => "sweep",
            TransitionKind::Warp => "warp",
            TransitionKind::ZoomBlur => "zoom_blur",
        }
    }
}

/// Per-frame inputs of a transition draw (`PresetTransition::Draw`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionParams {
    /// The shader that blends the two images.
    pub kind: TransitionKind,
    /// `iProgressLinear`.
    pub linear: f64,
    /// `iProgressCosine`.
    pub cosine: f64,
    /// `iProgressBicubic`.
    pub bicubic: f64,
    /// `iTransitionDuration`.
    pub duration: f64,
    /// `iTime`: seconds since the transition started.
    pub time: f64,
    /// `iTimeDelta`: seconds since the previous draw.
    pub time_delta: f64,
    /// `iRandStatic`: four integers fixed for the whole transition.
    pub rand_static: [i32; 4],
}

impl TransitionParams {
    /// projectM's progress curves for `seconds_since_start` of a `duration` seconds transition
    /// (a non-positive duration yields 1 everywhere).
    pub fn new(
        kind: TransitionKind,
        rand_static: [i32; 4],
        duration: f64,
        seconds_since_start: f64,
        time_delta: f64,
    ) -> Self {
        let (linear, cosine, bicubic) = if duration > 0.0 {
            let l = seconds_since_start / duration;
            let c = (-(l * std::f64::consts::PI).cos() + 1.0) * 0.5;
            let b = if l < 0.5 {
                4.0 * l * l * l
            } else {
                1.0 - (-2.0 * l + 2.0).powf(3.0) / 2.0
            };
            (l, c, b)
        } else {
            (1.0, 1.0, 1.0)
        };
        Self {
            kind,
            linear,
            cosine,
            bicubic,
            duration,
            time: seconds_since_start,
            time_delta,
            rand_static,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct TransitionUniforms {
    /// `iResolution.xyz`, `w` = 1 for sRGB targets.
    resolution: [f32; 4],
    /// `durationParams`.
    duration_params: [f32; 4],
    /// `timeParams.xy`.
    time_params: [f32; 4],
    /// `iRandStatic`.
    rand_static: [i32; 4],
}

const HEADER: &str = r#"
struct TP {
    resolution: vec4<f32>,
    duration_params: vec4<f32>,
    time_params: vec4<f32>,
    rand_static: vec4<i32>,
};
@group(0) @binding(0) var<uniform> tp: TP;
@group(0) @binding(1) var tex_old: texture_2d<f32>;
@group(0) @binding(2) var tex_new: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    // z = 0.5: gl_FragCoord.z of projectM's z = 0 quad under the default [-1, 1] -> [0, 1] depth range
    return vec4<f32>(p[vi], 0.5, 1.0);
}

fn glsl_mod(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

fn glsl_smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// texture(iChannel0, p) / texture(iChannel1, p): GLSL v runs bottom-up, our images top-down.
fn old_at(p: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(tex_old, samp, vec2<f32>(p.x, 1.0 - p.y), 0.0).xyz;
}
fn new_at(p: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(tex_new, samp, vec2<f32>(p.x, 1.0 - p.y), 0.0).xyz;
}

fn i_progress_linear() -> f32 { return tp.duration_params.x; }
fn i_progress_cosine() -> f32 { return tp.duration_params.y; }
fn i_progress_bicubic() -> f32 { return tp.duration_params.z; }

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // gl_FragCoord: y up
    let frag = vec4<f32>(pos.x, tp.resolution.y - pos.y, pos.z, 1.0);
    var rgb = main_image(frag, frag.xy).xyz;
    if (tp.resolution.w > 0.5) {
        rgb = srgb_to_linear(rgb);
    }
    return vec4<f32>(rgb, 1.0);
}
"#;

const CIRCLE: &str = r#"
fn main_image(frag: vec4<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let uv = frag_coord / tp.resolution.xy;
    let aspect = tp.resolution.y / tp.resolution.x;

    let in_or_out = glsl_mod(f32(tp.rand_static.x) * 0.01, 2.0);

    var progress: f32;
    var img_inner: vec3<f32>;
    var img_outer: vec3<f32>;
    if (in_or_out < 1.0) {
        img_inner = old_at(uv);
        img_outer = new_at(uv);
        progress = i_progress_cosine();
    } else {
        img_outer = old_at(uv);
        img_inner = new_at(uv);
        progress = 1.0 - i_progress_cosine();
    }
    let blend_width = glsl_mod(f32(tp.rand_static.y) * 0.001, 0.1) + 0.05;
    progress = progress * (1.0 + blend_width) - blend_width;

    let center = vec2<f32>(0.5);
    let rad = sqrt(center.x / aspect * center.x / aspect + center.y * center.y) * progress;
    let rad2 = rad + blend_width;
    let r1 = sqrt((uv.x - center.x) / aspect * (uv.x - center.x) / aspect + (uv.y - center.y) * (uv.y - center.y));

    var col: vec3<f32>;
    if (r1 > rad2) {
        col = img_inner;
    } else if (r1 > rad) {
        let v1 = (r1 - rad) / (rad2 - rad);
        let v2 = 1.0 - v1;
        col = v1 * img_inner + v2 * img_outer;
    } else {
        col = img_outer;
    }
    return vec4<f32>(col, 1.0);
}
"#;

const PLASMA: &str = r#"
fn sin_noise(uv: vec2<f32>) -> f32 {
    return fract(abs(sin(uv.x * 0.018 + uv.y * 0.3077) * (f32(tp.rand_static.x) * 0.001)));
}

fn value_noise(uv: vec2<f32>, scale: f32) -> f32 {
    let luv = fract(uv * scale);
    let luvs = vec2<f32>(glsl_smoothstep(0.0, 1.0, fract(uv * scale).x), glsl_smoothstep(0.0, 1.0, fract(uv * scale).y));
    let id = floor(uv * scale);
    let tl = sin_noise(id + vec2<f32>(0.0, 1.0));
    let tr = sin_noise(id + vec2<f32>(1.0, 1.0));
    let t = mix(tl, tr, luvs.x);

    let bl = sin_noise(id + vec2<f32>(0.0, 0.0));
    let br = sin_noise(id + vec2<f32>(1.0, 0.0));
    let b = mix(bl, br, luvs.x);

    return mix(b, t, luvs.y) * 2.0 - 1.0;
}

fn main_image(frag: vec4<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    var uv = frag_coord / tp.resolution.xy;

    let img_old = old_at(uv);
    let img_new = new_at(uv);

    uv.y = uv.y / (tp.resolution.x / tp.resolution.y);

    let scale = glsl_mod(f32(tp.rand_static.y) * 0.01, 7.0) * 4.0 + 4.0;

    var fract_value = 0.0;
    var amp = 1.0;
    for (var i = 0; i < 16; i = i + 1) {
        fract_value = fract_value + value_noise(uv, f32(i + 1) * scale) * amp;
        amp = amp * 0.5;
    }

    fract_value = fract_value * 0.25;
    fract_value = fract_value + 0.5;

    let cutoff = glsl_smoothstep(i_progress_cosine() + 0.1, i_progress_cosine() - 0.1, fract_value);
    let col = mix(img_old, img_new, cutoff);
    return vec4<f32>(col, 1.0);
}
"#;

const SIMPLE_BLEND: &str = r#"
fn main_image(frag: vec4<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let uv = frag_coord / tp.resolution.xy;
    return vec4<f32>(mix(old_at(uv), new_at(uv), i_progress_cosine()), 1.0);
}
"#;

const SWEEP: &str = r#"
fn atan_y_over_x(y: f32, x: f32) -> f32 {
    if (x > 0.0) {
        return atan(y / x);
    } else if (x < 0.0 && y >= 0.0) {
        return atan(y / x) + 3.14159265;
    } else if (x < 0.0 && y < 0.0) {
        return atan(y / x) - 3.14159265;
    }
    return 1.57079632 * sign(y);
}

fn main_image(frag: vec4<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    var uv = frag_coord / tp.resolution.xy;
    let img_old = old_at(uv);
    let img_new = new_at(uv);

    let current_angle = glsl_mod(f32(tp.rand_static.x), 360.0);
    let blend_width = glsl_mod(f32(tp.rand_static.y) * 0.001, 0.3) + 0.1;

    uv = uv - 0.5;
    let angle = 90.0 * 0.017453292 - current_angle * 0.017453292 + atan_y_over_x(uv.y, uv.x);

    let len = length(uv) / sqrt(2.0);
    uv = vec2<f32>(cos(angle) * len, sin(angle) * len) + 0.5;

    let col = mix(img_new, img_old, glsl_smoothstep(i_progress_cosine(), i_progress_cosine() + blend_width, (uv.x / (1.0 + 2.0 * blend_width)) + blend_width));
    return vec4<f32>(col, 1.0);
}
"#;

const WARP: &str = r#"
fn main_image(frag: vec4<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let uv = frag_coord / tp.resolution.xy;

    let direction = glsl_mod(f32(tp.rand_static.x) * 0.01, 4.0);
    var coord: f32;
    if (direction < 1.0) {
        coord = uv.x;
    } else if (direction < 2.0) {
        coord = 1.0 - uv.x;
    } else if (direction < 3.0) {
        coord = uv.y;
    } else {
        coord = 1.0 - uv.y;
    }

    let x = glsl_smoothstep(0.0, 1.0, (i_progress_cosine() * 2.0 + coord - 1.0));
    let a = old_at((uv - 0.5) * (1.0 - x) + 0.5);
    let b = new_at((uv - 0.5) * x + 0.5);
    return vec4<f32>(mix(a, b, x), 1.0);
}
"#;

const ZOOM_BLUR: &str = r#"
const STRENGTH: f32 = 0.3;
const PI: f32 = 3.141592653589793;

fn linear_ease(begin: f32, change: f32, duration: f32, time: f32) -> f32 {
    return change * time / duration + begin;
}

fn exponential_ease_in_out(begin: f32, change: f32, duration: f32, time_in: f32) -> f32 {
    var time = time_in;
    if (time == 0.0) {
        return begin;
    } else if (time == duration) {
        return begin + change;
    }
    time = time / (duration / 2.0);
    if (time < 1.0) {
        return change / 2.0 * pow(2.0, 10.0 * (time - 1.0)) + begin;
    }
    return change / 2.0 * (-pow(2.0, -10.0 * (time - 1.0)) + 2.0) + begin;
}

fn sinusoidal_ease_in_out(begin: f32, change: f32, duration: f32, time: f32) -> f32 {
    return -change / 2.0 * (cos(PI * time / duration) - 1.0) + begin;
}

fn random3(scale: vec3<f32>, seed: f32, frag: vec3<f32>) -> f32 {
    return fract(sin(dot(frag + seed, scale)) * 43758.5453 + seed);
}

fn cross_fade(uv: vec2<f32>, dissolve: f32) -> vec3<f32> {
    return mix(old_at(uv), new_at(uv), dissolve);
}

fn main_image(frag: vec4<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let tex_coord = frag_coord / tp.resolution.xy;
    let progress = i_progress_cosine();
    let center = vec2<f32>(linear_ease(0.5, 0.0, 1.0, progress), 0.5);
    let dissolve = exponential_ease_in_out(0.0, 1.0, 1.0, progress);

    let strength = sinusoidal_ease_in_out(0.0, STRENGTH, 0.5, progress);

    var color = vec3<f32>(0.0);
    var total = 0.0;
    let to_center = center - tex_coord;

    let offset = random3(vec3<f32>(12.9898, 78.233, 151.7182), 0.0, frag.xyz) * 0.5;

    for (var t = 0.0; t <= 20.0; t = t + 1.0) {
        let percent = (t + offset) / 20.0;
        let weight = 1.0 * (percent - percent * percent);
        color = color + cross_fade(tex_coord + to_center * percent * strength, dissolve) * weight;
        total = total + weight;
    }

    return vec4<f32>(color / total, 1.0);
}
"#;

fn body(kind: TransitionKind) -> &'static str {
    match kind {
        TransitionKind::Circle => CIRCLE,
        TransitionKind::Plasma => PLASMA,
        TransitionKind::SimpleBlend => SIMPLE_BLEND,
        TransitionKind::Sweep => SWEEP,
        TransitionKind::Warp => WARP,
        TransitionKind::ZoomBlur => ZOOM_BLUR,
    }
}

/// The six transition pipelines and the bilinear / clamp sampler (`m_presetSampler`).
pub struct TransitionBlender {
    device: wgpu::Device,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipelines: Vec<wgpu::RenderPipeline>,
    srgb_target: bool,
}

impl TransitionBlender {
    /// `output_format` is the format of the targets passed to [`TransitionBlender::draw`].
    pub fn new(device: &wgpu::Device, output_format: wgpu::TextureFormat) -> Self {
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transition"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transition"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("transition"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let pipelines = TransitionKind::ALL
            .iter()
            .map(|&kind| {
                let src = format!("{HEADER}\n{}", body(kind));
                let sm = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(kind.name()),
                    source: wgpu::ShaderSource::Wgsl(src.into()),
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(kind.name()),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &sm,
                        entry_point: Some("vs_main"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &sm,
                        entry_point: Some("fs_main"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: output_format,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .collect();
        Self {
            device: device.clone(),
            bgl,
            sampler,
            pipelines,
            srgb_target: output_format.is_srgb(),
        }
    }

    /// Blend `old` and `new` (both `width x height` internal images, top-down rows) into `target`
    /// (a view of a `width x height` texture of the format given to [`TransitionBlender::new`]).
    pub fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        old: &wgpu::TextureView,
        new: &wgpu::TextureView,
        target: &wgpu::TextureView,
        size: (u32, u32),
        p: &TransitionParams,
    ) {
        let u = TransitionUniforms {
            resolution: [
                size.0 as f32,
                size.1 as f32,
                0.0,
                if self.srgb_target { 1.0 } else { 0.0 },
            ],
            duration_params: [
                p.linear as f32,
                p.cosine as f32,
                p.bicubic as f32,
                p.duration as f32,
            ],
            time_params: [p.time as f32, p.time_delta as f32, 0.0, 0.0],
            rand_static: p.rand_static,
        };
        let buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("transition uniforms"),
                contents: bytemuck::bytes_of(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transition"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(old),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(new),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("transition"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        rp.set_pipeline(&self.pipelines[p.kind.index()]);
        rp.set_bind_group(0, &bg, &[]);
        rp.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_curves_follow_projectm() {
        let p = TransitionParams::new(TransitionKind::SimpleBlend, [0; 4], 3.0, 0.0, 0.0);
        assert_eq!((p.linear, p.cosine, p.bicubic), (0.0, 0.0, 0.0));
        let p = TransitionParams::new(TransitionKind::SimpleBlend, [0; 4], 3.0, 1.5, 0.0);
        assert!((p.cosine - 0.5).abs() < 1e-15);
        assert!((p.bicubic - 0.5).abs() < 1e-15);
        let p = TransitionParams::new(TransitionKind::SimpleBlend, [0; 4], 3.0, 3.0, 0.0);
        assert_eq!(p.linear, 1.0);
        assert!((p.cosine - 1.0).abs() < 1e-15 && (p.bicubic - 1.0).abs() < 1e-15);
        // quarter way: bicubic = 4 l^3
        let p = TransitionParams::new(TransitionKind::SimpleBlend, [0; 4], 4.0, 1.0, 0.0);
        assert!((p.bicubic - 4.0 * 0.25f64.powi(3)).abs() < 1e-15);
        // zero duration: everything done
        let p = TransitionParams::new(TransitionKind::SimpleBlend, [0; 4], 0.0, 0.0, 0.0);
        assert_eq!((p.linear, p.cosine, p.bicubic), (1.0, 1.0, 1.0));
    }

    #[test]
    fn kind_order_matches_projectm() {
        let names: Vec<_> = TransitionKind::ALL.iter().map(|k| k.name()).collect();
        assert_eq!(
            names,
            [
                "circle",
                "plasma",
                "simple_blend",
                "sweep",
                "warp",
                "zoom_blur"
            ]
        );
        assert_eq!(TransitionKind::from_index(8), TransitionKind::SimpleBlend);
    }
}
