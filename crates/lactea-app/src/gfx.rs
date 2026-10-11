//! wgpu device + window surface: format choice, (re)configuration, surface-loss recovery.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use winit::window::Window;

pub struct Gfx {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    window: Arc<Window>,
    device_lost: Arc<AtomicBool>,
    /// A `Suboptimal` frame was presented: reconfigure before the next one.
    reconfigure: bool,
}

/// Outcome of trying to get a frame to draw into.
pub enum Acquired {
    Frame(wgpu::SurfaceTexture),
    /// Nothing to draw this time (occluded, timeout, reconfigured, recreated); try again.
    Skip,
}

/// Prefer a non-sRGB 8-bit surface (the engine renders its own gamma-free image and expects
/// to write values as-is); otherwise take the first format and let the visualizer compensate.
pub fn choose_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    use wgpu::TextureFormat as F;
    [F::Bgra8Unorm, F::Rgba8Unorm]
        .into_iter()
        .find(|f| formats.contains(f))
        .or_else(|| {
            [F::Bgra8UnormSrgb, F::Rgba8UnormSrgb]
                .into_iter()
                .find(|f| formats.contains(f))
        })
        .or_else(|| formats.first().copied())
}

pub fn choose_present_mode(modes: &[wgpu::PresentMode], vsync: bool) -> wgpu::PresentMode {
    use wgpu::PresentMode as P;
    if vsync {
        return P::Fifo; // always supported
    }
    [P::Mailbox, P::Immediate]
        .into_iter()
        .find(|m| modes.contains(m))
        .unwrap_or(P::Fifo)
}

impl Gfx {
    pub fn new(
        instance: wgpu::Instance,
        window: Arc<Window>,
        vsync: bool,
        capture: bool,
    ) -> Result<Self, String> {
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("cannot create surface: {e}"))?;
        let adapter =
            pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::from_env()
                        .unwrap_or(wgpu::PowerPreference::HighPerformance),
                    compatible_surface: Some(&surface),
                    ..Default::default()
                }),
            )
            .map_err(|e| format!("no suitable GPU adapter: {e}"))?;
        let info = adapter.get_info();
        log::info!(
            "gpu: {} ({:?}, {:?})",
            info.name,
            info.device_type,
            info.backend
        );

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("lactea"),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| format!("cannot create device: {e}"))?;
        // Validation errors must not abort the program: a preset that produces one is skipped
        // by the error scope around load_preset; anything else is logged.
        device.on_uncaptured_error(Arc::new(|e| log::error!("wgpu error: {e}")));
        let device_lost = Arc::new(AtomicBool::new(false));
        {
            let flag = device_lost.clone();
            device.set_device_lost_callback(move |reason, msg| {
                log::error!("gpu device lost ({reason:?}): {msg}");
                flag.store(true, Ordering::SeqCst);
            });
        }

        let caps = surface.get_capabilities(&adapter);
        let format = choose_format(&caps.formats).ok_or("surface reports no formats")?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("surface is not supported by the adapter")?;
        config.format = format;
        if capture {
            if !caps.usages.contains(wgpu::TextureUsages::COPY_SRC) {
                return Err(
                    "--screenshot: the surface does not support reading back frames".into(),
                );
            }
            config.usage |= wgpu::TextureUsages::COPY_SRC;
        }
        config.present_mode = choose_present_mode(&caps.present_modes, vsync);
        config.desired_maximum_frame_latency = 2;
        if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        }
        log::info!(
            "surface: {format:?}{} {}x{} present={:?}",
            if format.is_srgb() {
                " (sRGB, visualizer must compensate)"
            } else {
                ""
            },
            config.width,
            config.height,
            config.present_mode
        );
        surface.configure(&device, &config);
        Ok(Self {
            device,
            queue,
            instance,
            surface,
            config,
            window,
            device_lost,
            reconfigure: false,
        })
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn device_lost(&self) -> bool {
        self.device_lost.load(Ordering::SeqCst)
    }

    /// Resize the swapchain. Returns whether anything changed.
    pub fn resize(&mut self, w: u32, h: u32) -> bool {
        if w == 0 || h == 0 || (w, h) == (self.config.width, self.config.height) {
            return false;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        true
    }

    /// Next swapchain image, recovering from the recoverable failures.
    pub fn acquire(&mut self) -> Acquired {
        if self.reconfigure {
            self.reconfigure = false;
            self.surface.configure(&self.device, &self.config);
        }
        use wgpu::CurrentSurfaceTexture as C;
        match self.surface.get_current_texture() {
            C::Success(t) => Acquired::Frame(t),
            C::Suboptimal(t) => {
                self.reconfigure = true;
                Acquired::Frame(t)
            }
            C::Timeout | C::Occluded => Acquired::Skip,
            C::Outdated => {
                log::debug!("surface outdated: reconfiguring");
                self.surface.configure(&self.device, &self.config);
                Acquired::Skip
            }
            C::Lost => {
                log::warn!("surface lost: recreating");
                match self.instance.create_surface(self.window.clone()) {
                    Ok(s) => {
                        self.surface = s;
                        self.surface.configure(&self.device, &self.config);
                    }
                    Err(e) => log::error!("cannot recreate surface: {e}"),
                }
                Acquired::Skip
            }
            C::Validation => {
                log::error!("surface validation error while acquiring a frame");
                Acquired::Skip
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::TextureFormat as F;

    #[test]
    fn prefers_non_srgb() {
        assert_eq!(
            choose_format(&[F::Bgra8UnormSrgb, F::Bgra8Unorm]),
            Some(F::Bgra8Unorm)
        );
        assert_eq!(
            choose_format(&[F::Rgba8UnormSrgb, F::Rgba8Unorm]),
            Some(F::Rgba8Unorm)
        );
        assert_eq!(
            choose_format(&[F::Rgba8Unorm, F::Bgra8Unorm]),
            Some(F::Bgra8Unorm)
        );
    }

    #[test]
    fn falls_back_to_srgb_then_first() {
        assert_eq!(choose_format(&[F::Rgba8UnormSrgb]), Some(F::Rgba8UnormSrgb));
        assert_eq!(
            choose_format(&[F::Bgra8UnormSrgb, F::Rgba8UnormSrgb]),
            Some(F::Bgra8UnormSrgb)
        );
        assert_eq!(choose_format(&[F::Rgba16Float]), Some(F::Rgba16Float));
        assert_eq!(choose_format(&[]), None);
    }

    #[test]
    fn present_modes() {
        use wgpu::PresentMode as P;
        assert_eq!(choose_present_mode(&[P::Immediate, P::Fifo], true), P::Fifo);
        assert_eq!(
            choose_present_mode(&[P::Immediate, P::Mailbox, P::Fifo], false),
            P::Mailbox
        );
        assert_eq!(choose_present_mode(&[P::Fifo], false), P::Fifo);
    }
}
