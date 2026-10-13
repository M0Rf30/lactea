//! Headless wgpu setup and texture read-back.

use anyhow::{Result, anyhow};

pub use lactea_host::gpu::Gpu;

pub fn init() -> Result<Gpu> {
    lactea_host::gpu::init_gpu().map_err(anyhow::Error::msg)
}

/// The target the engine renders into (what a host would hand over).
pub struct Target {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

pub fn create_target(device: &wgpu::Device, width: u32, height: u32) -> Target {
    let (texture, view) = lactea_host::gpu::create_target(device, width, height);
    Target {
        texture,
        view,
        width,
        height,
    }
}

/// Read an `Rgba8Unorm` texture back as tightly packed RGBA8 (row 0 = top).
pub fn read_rgba8(gpu: &Gpu, t: &Target) -> Result<Vec<u8>> {
    let bpr = (t.width * 4).div_ceil(256) * 256;
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(bpr) * u64::from(t.height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&Default::default());
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
                rows_per_image: Some(t.height),
            },
        },
        wgpu::Extent3d {
            width: t.width,
            height: t.height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| anyhow!("poll: {e:?}"))?;
    rx.recv()?.map_err(|e| anyhow!("map: {e}"))?;
    let data = slice
        .get_mapped_range()
        .map_err(|e| anyhow!("mapped range: {e}"))?;
    let mut out = Vec::with_capacity((t.width * t.height * 4) as usize);
    for y in 0..t.height {
        let s = (y * bpr) as usize;
        out.extend_from_slice(&data[s..s + (t.width * 4) as usize]);
    }
    drop(data);
    buf.unmap();
    Ok(out)
}
