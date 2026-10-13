//! Tiny wasm-bindgen host for the lactea engine: a `<canvas>`, a WebGPU surface, PCM from JS.
//!
//! The engine crates open no windows, audio devices or files, so the whole host is this file: create
//! the wgpu device and surface from the canvas, hand `Engine` the device, then per frame push PCM
//! (from a WebAudio `AnalyserNode`, a file, or a synthetic signal) and render into the surface
//! texture. `index.html` drives it.

#![cfg(target_arch = "wasm32")]

use lactea::{Engine, EngineConfig, FrameClock, Transition};
use wasm_bindgen::prelude::*;

fn js_err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// The engine plus the WebGPU surface it renders into.
#[wasm_bindgen]
pub struct Demo {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    engine: Engine,
    frame: u32,
}

#[wasm_bindgen]
impl Demo {
    /// Open a WebGPU device on `canvas` and create the engine at the canvas' size.
    pub async fn create(canvas: web_sys::HtmlCanvasElement) -> Result<Demo, JsValue> {
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(|| {
            std::panic::set_hook(Box::new(|info| {
                web_sys::console::error_1(&JsValue::from_str(&info.to_string()));
            }));
        });
        let (w, h) = (canvas.width().max(1), canvas.height().max(1));
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(js_err)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(js_err)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("lactea web demo"),
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(js_err)?;
        device.on_uncaptured_error(std::sync::Arc::new(|e| {
            web_sys::console::error_1(&JsValue::from_str(&format!("wgpu error: {e}")));
        }));

        let formats = surface.get_capabilities(&adapter).formats;
        // The engine's output is not sRGB-encoded: prefer a linear surface format.
        let format = formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| formats.first().copied())
            .ok_or_else(|| js_err("the canvas reports no surface formats"))?;
        let mut config = surface
            .get_default_config(&adapter, w, h)
            .ok_or_else(|| js_err("the canvas is not supported by the adapter"))?;
        config.format = format;
        surface.configure(&device, &config);

        let engine = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: w,
                height: h,
                output_format: format,
                ..Default::default()
            },
        )
        .map_err(js_err)?;
        Ok(Demo { device, queue, surface, config, engine, frame: 0 })
    }

    /// Load a `.milk` preset (the text of the file). `soft_seconds > 0` blends from the running
    /// preset over that many seconds, `0` cuts. Returns a one-line summary.
    pub fn load_preset(&mut self, milk: &str, name: &str, soft_seconds: f64) -> Result<String, JsValue> {
        let transition = if soft_seconds > 0.0 { Transition::Soft { seconds: soft_seconds } } else { Transition::Cut };
        let report = self.engine.load_preset(milk.as_bytes(), name, transition).map_err(js_err)?;
        Ok(format!(
            "{}: {} warnings, {} shader fallbacks, {} missing textures",
            report.name,
            report.warnings.len(),
            report.shader_fallbacks.len(),
            report.unresolved_textures.len()
        ))
    }

    /// The built-in idle preset (a hard cut).
    pub fn load_idle(&mut self) -> Result<(), JsValue> {
        self.engine.load_idle_preset().map(|_| ()).map_err(js_err)
    }

    /// Render one frame at `time_s` seconds. `pcm` is interleaved stereo float PCM (`-1..1`),
    /// any length up to a few hundred frames; pass an empty array for silence.
    pub fn render(&mut self, time_s: f64, pcm: &[f32]) -> Result<(), JsValue> {
        use wgpu::CurrentSurfaceTexture as C;
        if !pcm.is_empty() {
            self.engine.push_pcm_f32(pcm, 2);
        }
        let frame = match self.surface.get_current_texture() {
            C::Success(t) | C::Suboptimal(t) => t,
            C::Outdated | C::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            C::Timeout | C::Occluded => return Ok(()),
            C::Validation => return Err(js_err("surface validation error")),
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.engine.render(&mut enc, &view, FrameClock { time: time_s, frame: self.frame });
        self.queue.submit(Some(enc.finish()));
        self.queue.present(frame);
        self.frame += 1;
        Ok(())
    }

    /// Render one frame like [`render`](Self::render) but into an offscreen texture, and read it
    /// back: resolves to tightly packed RGBA8 (`width * height * 4` bytes, row 0 = top). The frame
    /// is not presented. This is what the headless check of the demo asserts on.
    pub fn snapshot(&mut self, time_s: f64, pcm: &[f32]) -> Result<js_sys::Promise, JsValue> {
        if !pcm.is_empty() {
            self.engine.push_pcm_f32(pcm, 2);
        }
        let (w, h, format) = (self.config.width, self.config.height, self.config.format);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lactea snapshot"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let bytes_per_row = (w * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lactea snapshot"),
            size: u64::from(bytes_per_row) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let view = texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.engine.render(&mut enc, &view, FrameClock { time: time_s, frame: self.frame });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        self.frame += 1;
        let bgra = matches!(format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
        Ok(js_sys::Promise::new(&mut move |resolve, reject| {
            let mapped = buffer.clone();
            buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
                let outcome = result.map_err(js_err).and_then(|()| {
                    let data = mapped.slice(..).get_mapped_range().map_err(js_err)?;
                    let row = (w * 4) as usize;
                    let mut out = Vec::with_capacity(row * h as usize);
                    for y in 0..h as usize {
                        out.extend_from_slice(&data[y * bytes_per_row as usize..][..row]);
                    }
                    drop(data);
                    mapped.unmap();
                    if bgra {
                        out.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
                    }
                    Ok(js_sys::Uint8Array::from(out.as_slice()))
                });
                let _ = match outcome {
                    Ok(bytes) => resolve.call1(&JsValue::NULL, &bytes),
                    Err(e) => reject.call1(&JsValue::NULL, &e),
                };
            });
        }))
    }

    /// Change the canvas (and render) size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), JsValue> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.engine.resize(width, height).map_err(js_err)
    }

    /// Frames rendered so far.
    pub fn frames(&self) -> u32 {
        self.frame
    }
}
