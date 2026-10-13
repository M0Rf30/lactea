//! The seam between the window/audio shell and whatever draws.
//!
//! The shell only knows this trait; `EngineVisualizer` (feature `engine`) adapts the MilkDrop
//! engine to it and [`ScopeVisualizer`] is a dependency-free oscilloscope used to develop and
//! test the shell (window, audio paths, preset keys) without an engine.

use crate::cli::CompatArg;
use bytemuck::{Pod, Zeroable};

/// How a loaded preset replaces the running one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    /// Immediately.
    Cut,
    /// Blend over this many seconds (visualizers that cannot blend cut).
    Soft { seconds: f64 },
}

pub trait Visualizer {
    /// Load a `.milk` preset (raw bytes, `name` = display name). An `Err` means "skip this one".
    fn load_preset(&mut self, bytes: &[u8], name: &str, change: Change) -> Result<(), String>;
    /// Show something alive although no preset could be loaded (the engine's built-in idle preset);
    /// visualizers that always draw have nothing to do.
    fn load_idle(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// PCM that became audible since the previous call: interleaved f32 in [-1, 1].
    fn push_pcm(&mut self, interleaved: &[f32], channels: usize);
    /// Draw one frame into `view` (the swapchain view, in the format the visualizer was built for).
    /// `time` is seconds since the start of the run, `frame` the frame counter.
    fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        time: f64,
        frame: u32,
    );
    /// The target size changed (physical pixels, never 0).
    fn resize(&mut self, w: u32, h: u32);
    /// The mouse for the next frame: the cursor as fractions of the target size (`px / width`, `py / height`, origin
    /// top left; `None` outside the window) and the left button. Only the MilkDrop 3 engine mode uses it.
    fn set_mouse(&mut self, _position: Option<[f32; 2]>, _left_down: bool) {}
    /// Pixels of every pipeline stage of the last rendered frame (`name`, width, height, RGBA8),
    /// for the debug dump key. Visualizers without internal stages return nothing.
    fn dump_stages(&mut self) -> Vec<(String, u32, u32, Vec<u8>)> {
        Vec::new()
    }
    /// Instant preset changes the visualizer asked for in the frames rendered since the last call
    /// (MD3 auto hard cuts, `docs/app.md`); the host loads ONE preset for any count > 0.
    fn take_auto_cuts(&mut self) -> u32 {
        0
    }
    /// Set the MD3 auto hard cut mode (0 = off, 1..=7); ignored by visualizers without it.
    fn set_auto_hard_cut_mode(&mut self, _mode: u8) {}
    /// The current auto hard cut mode, or `None` when the visualizer has no such feature or its
    /// compat mode does not support it (the key then tells the user what to enable).
    fn auto_hard_cut_mode(&self) -> Option<u8> {
        None
    }
    /// The compat mode the visualizer runs now (never `Auto`): after a successful `load_preset`, the mode the
    /// loaded preset runs under. `None` for visualizers without compat modes.
    fn current_compat(&self) -> Option<CompatArg> {
        None
    }
    /// How long the preset that `load_preset` just loaded is shown (the host's sampled display time); `progress` runs
    /// over it. Call it after every successful load. Ignored by visualizers without `progress`.
    fn set_loaded_preset_duration(&mut self, _seconds: f64) {}
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    color: [f32; 4],
}

const SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> VsOut {
    var o: VsOut;
    o.pos = vec4<f32>(pos, 0.0, 1.0);
    o.color = color;
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

/// Window of samples drawn per channel (the MilkDrop waveform length).
const WAVE_LEN: usize = 576;
/// History kept per channel: enough to search a rising zero crossing before the window.
const HISTORY: usize = 4096;
const MAX_VERTS: usize = 24_576;

/// Oscilloscope of the incoming PCM: left channel in the upper half, right in the lower half,
/// a level bar at the bottom. The preset only changes the colour, so preset switching is visible.
pub struct ScopeVisualizer {
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    vbuf: wgpu::Buffer,
    width: u32,
    height: u32,
    left: Vec<f32>,
    right: Vec<f32>,
    hue: f32,
    level: f32,
    verts: Vec<Vertex>,
    preset: String,
}

impl ScopeVisualizer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scope"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scope"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
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
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scope"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attrs,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scope vertices"),
            size: (MAX_VERTS * std::mem::size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            queue: queue.clone(),
            pipeline,
            vbuf,
            width: width.max(1),
            height: height.max(1),
            left: vec![0.0; HISTORY],
            right: vec![0.0; HISTORY],
            hue: 0.33,
            level: 0.0,
            verts: Vec::with_capacity(MAX_VERTS),
            preset: String::new(),
        }
    }

    /// Latest `WAVE_LEN` samples of `hist`, starting at a rising zero crossing when one exists in
    /// the preceding 512 samples (stable display of periodic signals).
    fn window(hist: &[f32]) -> &[f32] {
        let n = hist.len();
        let latest = n - WAVE_LEN;
        let earliest = latest.saturating_sub(512);
        let mut start = latest;
        for i in (earliest..latest).rev() {
            if hist[i] < 0.0 && hist[i + 1] >= 0.0 {
                start = i + 1;
                break;
            }
        }
        &hist[start..start + WAVE_LEN]
    }

    fn polyline(&mut self, pts: &[[f32; 2]], half_px: f32, color: [f32; 4]) {
        let (w, h) = (self.width as f32, self.height as f32);
        for seg in pts.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            // Work in pixel space so the line width is isotropic.
            let (ax, ay, bx, by) = (
                a[0] * w * 0.5,
                a[1] * h * 0.5,
                b[0] * w * 0.5,
                b[1] * h * 0.5,
            );
            let (dx, dy) = (bx - ax, by - ay);
            let len = (dx * dx + dy * dy).sqrt().max(1e-6);
            let (nx, ny) = (-dy / len * half_px, dx / len * half_px);
            let q = |x: f32, y: f32| Vertex {
                pos: [x / (w * 0.5), y / (h * 0.5)],
                color,
            };
            let (p0, p1, p2, p3) = (
                q(ax + nx, ay + ny),
                q(ax - nx, ay - ny),
                q(bx + nx, by + ny),
                q(bx - nx, by - ny),
            );
            self.verts.extend_from_slice(&[p0, p1, p2, p2, p1, p3]);
        }
    }

    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: [f32; 4]) {
        let v = |x, y| Vertex { pos: [x, y], color };
        self.verts.extend_from_slice(&[
            v(x0, y0),
            v(x1, y0),
            v(x0, y1),
            v(x0, y1),
            v(x1, y0),
            v(x1, y1),
        ]);
    }
}

fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let f = h - h.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match h as u32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

fn push_history(hist: &mut Vec<f32>, new: impl Iterator<Item = f32>) {
    hist.extend(new);
    if hist.len() > HISTORY {
        hist.drain(..hist.len() - HISTORY);
    }
}

impl Visualizer for ScopeVisualizer {
    fn load_preset(&mut self, bytes: &[u8], name: &str, _change: Change) -> Result<(), String> {
        // FNV-1a of the name -> hue; the scope has no use for the preset body.
        let mut h: u32 = 0x811c_9dc5;
        for b in name.bytes().chain(bytes.iter().take(64).copied()) {
            h = (h ^ b as u32).wrapping_mul(0x0100_0193);
        }
        self.hue = (h % 997) as f32 / 997.0;
        self.preset = name.to_string();
        Ok(())
    }

    fn push_pcm(&mut self, interleaved: &[f32], channels: usize) {
        if channels == 0 || interleaved.is_empty() {
            return;
        }
        let frames = interleaved.chunks_exact(channels);
        let (mut l, mut r): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
        for f in frames {
            l.push(f[0]);
            r.push(*f.get(1).unwrap_or(&f[0]));
        }
        let rms = (l.iter().chain(r.iter()).map(|s| s * s).sum::<f32>()
            / (l.len() + r.len()).max(1) as f32)
            .sqrt();
        // Fast attack, slow release.
        self.level = if rms > self.level {
            rms
        } else {
            self.level * 0.9 + rms * 0.1
        };
        push_history(&mut self.left, l.into_iter());
        push_history(&mut self.right, r.into_iter());
    }

    fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        time: f64,
        _frame: u32,
    ) {
        self.verts.clear();
        let hue = self.hue + (time as f32) * 0.01;
        let c_l = hsv(hue, 0.8, 1.0);
        let c_r = hsv(hue + 0.5, 0.8, 1.0);

        // Axes + level bar.
        self.rect(
            -1.0,
            0.5 - 0.0015,
            1.0,
            0.5 + 0.0015,
            [0.25, 0.25, 0.3, 1.0],
        );
        self.rect(
            -1.0,
            -0.5 - 0.0015,
            1.0,
            -0.5 + 0.0015,
            [0.25, 0.25, 0.3, 1.0],
        );
        let bar = (self.level * 2.0).clamp(0.0, 1.0);
        self.rect(
            -1.0,
            -0.99,
            -1.0 + 2.0 * bar,
            -0.95,
            [c_l[0], c_l[1], c_l[2], 0.9],
        );

        for (idx, color3) in [(0usize, c_l), (1usize, c_r)] {
            let hist = if idx == 0 { &self.left } else { &self.right };
            let win = Self::window(hist);
            let centre = if idx == 0 { 0.5 } else { -0.5 };
            let pts: Vec<[f32; 2]> = win
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    [
                        -1.0 + 2.0 * i as f32 / (WAVE_LEN - 1) as f32,
                        centre + 0.42 * s.clamp(-1.0, 1.0),
                    ]
                })
                .collect();
            self.polyline(&pts, 4.0, [color3[0], color3[1], color3[2], 0.18]);
            self.polyline(&pts, 1.3, [color3[0], color3[1], color3[2], 1.0]);
        }

        let n = self.verts.len().min(MAX_VERTS);
        self.queue
            .write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&self.verts[..n]));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scope"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.015,
                        g: 0.015,
                        b: 0.03,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.vbuf.slice(..));
        pass.draw(0..n as u32, 0..1);
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.width = w.max(1);
        self.height = h.max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_triggers_on_rising_zero_crossing() {
        let mut hist = vec![0.5f32; HISTORY];
        // Rising crossing 100 samples before the latest window start.
        let latest = HISTORY - WAVE_LEN;
        let cross = latest - 100;
        for s in &mut hist[cross - 10..cross] {
            *s = -0.5;
        }
        let w = ScopeVisualizer::window(&hist);
        assert_eq!(w.len(), WAVE_LEN);
        assert!(w[0] >= 0.0);
        // Start is at `cross` (first non-negative sample), so the window begins 100 samples
        // earlier than the untriggered one.
        assert_eq!(w.as_ptr() as usize, hist[cross..].as_ptr() as usize);
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv(0.0, 1.0, 1.0), [1.0, 0.0, 0.0]);
        let g = hsv(1.0 / 3.0, 1.0, 1.0);
        assert!((g[1] - 1.0).abs() < 1e-5 && g[0] < 1e-5);
    }
}
