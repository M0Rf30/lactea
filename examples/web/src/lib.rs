//! wasm-bindgen host for the lactea engine: a `<canvas>`, a WebGPU surface, PCM and files from JS.
//!
//! The engine crates open no windows, audio devices or files, so the whole host is this file: create
//! the wgpu device and surface from the canvas, hand `Engine` the device, then per frame push PCM
//! and render into the surface texture. `main.js` owns audio (Web Audio), files (drag and drop,
//! fetch) and the frame loop (`requestAnimationFrame`).

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use lactea::{Engine, EngineConfig, FrameClock, Image, TextureProvider, Transition};
use wasm_bindgen::prelude::*;

/// Textures the page handed over, keyed like [`TextureProvider::load`] names. Shared with the
/// engine's provider so textures added after a preset loaded are found by later loads.
type TextureMap = Rc<RefCell<HashMap<String, Image>>>;

struct MemTextures(TextureMap);

impl TextureProvider for MemTextures {
    fn load(&mut self, name: &str) -> Option<Image> {
        self.0.borrow().get(&name.to_ascii_lowercase()).cloned()
    }

    fn random_pool(&self) -> Vec<String> {
        // sorted: the order is part of the engine's determinism contract
        let mut names: Vec<String> = self.0.borrow().keys().cloned().collect();
        names.sort();
        names
    }
}

fn js_err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// Lowercase file stem: the name presets use for a texture (`sampler_clouds2` → `clouds2`).
fn texture_key(file_name: &str) -> String {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
    stem.to_ascii_lowercase()
}

/// The engine plus the WebGPU surface it renders into.
#[wasm_bindgen]
pub struct Lactea {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    engine: Engine,
    textures: TextureMap,
}

#[wasm_bindgen]
impl Lactea {
    /// Opens WebGPU on `canvas` (its current `width`/`height` are the render size) and shows the
    /// built-in idle preset. `sample_rate` is the rate of the PCM given to [`Lactea::push_pcm`].
    pub async fn create(
        canvas: web_sys::HtmlCanvasElement,
        sample_rate: u32,
        seed: u32,
    ) -> Result<Lactea, JsError> {
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(|| {
            std::panic::set_hook(Box::new(|info| {
                web_sys::console::error_1(&JsValue::from_str(&info.to_string()));
            }));
        });
        let (width, height) = (canvas.width().max(1), canvas.height().max(1));
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(js_err)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(js_err)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("lactea web"),
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(js_err)?;
        // a validation error from one preset must not take the page down
        device.on_uncaptured_error(std::sync::Arc::new(|e| {
            web_sys::console::error_1(&JsValue::from_str(&format!("wgpu error: {e}")));
        }));

        let caps = surface.get_capabilities(&adapter);
        // the engine's output is not sRGB-encoded: prefer a linear surface format
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| js_err("the canvas reports no surface formats"))?;
        let mut config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| js_err("the canvas is not supported by the adapter"))?;
        config.format = format;
        if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        }
        surface.configure(&device, &config);

        let cfg = EngineConfig {
            width,
            height,
            seed: u64::from(seed),
            auto_compat: true,
            output_format: format,
            sample_rate,
            ..Default::default()
        };
        let mut engine = Engine::new(&device, &queue, cfg).map_err(js_err)?;
        let textures = TextureMap::default();
        engine.set_texture_provider(Box::new(MemTextures(textures.clone())));
        engine.load_idle_preset().map_err(js_err)?;
        Ok(Lactea {
            device,
            queue,
            surface,
            config,
            engine,
            textures,
        })
    }

    /// Loads a `.milk` / `.milk2` preset (the file's bytes). `blend_seconds > 0` blends from the
    /// running preset, otherwise it cuts. On error the running preset stays.
    pub fn load_preset(
        &mut self,
        bytes: &[u8],
        name: &str,
        blend_seconds: f64,
    ) -> Result<(), JsError> {
        let transition = if blend_seconds > 0.0 {
            Transition::Soft {
                seconds: blend_seconds,
            }
        } else {
            Transition::Cut
        };
        self.engine
            .load_preset(bytes, name, transition)
            .map(|_| ())
            .map_err(js_err)
    }

    /// Registers an image (PNG / JPEG / TGA / BMP bytes) as the texture named after the file's
    /// stem, for presets loaded afterwards.
    pub fn add_texture(&mut self, file_name: &str, bytes: &[u8]) -> Result<(), JsError> {
        let image = lactea_host::decode_image(bytes).map_err(js_err)?;
        self.textures
            .borrow_mut()
            .insert(texture_key(file_name), image);
        Ok(())
    }

    /// Feeds interleaved float PCM (`-1..1`) at the rate given to [`Lactea::create`].
    pub fn push_pcm(&mut self, interleaved: &[f32], channels: usize) {
        self.engine.push_pcm_f32(interleaved, channels.max(1));
    }

    /// Mouse position in `0..1` canvas coordinates (y down); `undefined` when outside.
    pub fn set_mouse(&mut self, x: Option<f32>, y: Option<f32>, left_down: bool) {
        self.engine.set_mouse(lactea::MouseState {
            position: x.zip(y).map(|(x, y)| [x, y]),
            left_down,
        });
    }

    /// Resizes the surface and the engine's images (no-op for an unchanged or empty size).
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), JsError> {
        if width == 0 || height == 0 || (width, height) == (self.config.width, self.config.height) {
            return Ok(());
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.engine.resize(width, height).map_err(js_err)
    }

    /// Renders and presents one frame at `time` seconds.
    pub fn render(&mut self, time: f64, frame: u32) -> Result<(), JsError> {
        use wgpu::CurrentSurfaceTexture as C;
        let texture = match self.surface.get_current_texture() {
            C::Success(t) | C::Suboptimal(t) => t,
            C::Outdated | C::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            C::Timeout | C::Occluded => return Ok(()),
            C::Validation => return Err(js_err("surface validation error")),
        };
        let view = texture.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.engine
            .render(&mut encoder, &view, FrameClock { time, frame });
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(texture);
        Ok(())
    }
}
