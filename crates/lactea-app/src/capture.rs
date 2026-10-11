//! Read a rendered frame back from the swapchain and save it as PNG (`--screenshot`).
//!
//! Captures exactly the pixels the visualizer drew into the window's surface texture, so it
//! works regardless of the compositor and never touches the rest of the desktop; together with
//! `--deterministic` it produces reproducible frame dumps.

use std::path::Path;

pub struct Readback {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
    format: wgpu::TextureFormat,
}

/// Record a copy of `texture` (must have `COPY_SRC`) into a mappable buffer.
pub fn encode_copy(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
) -> Result<Readback, String> {
    let format = texture.format();
    let bpp = format
        .block_copy_size(None)
        .ok_or_else(|| format!("cannot read back {format:?}"))?;
    if bpp != 4 {
        return Err(format!(
            "screenshots support 8-bit RGBA/BGRA surfaces, not {format:?}"
        ));
    }
    let (width, height) = (texture.width(), texture.height());
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = (width * bpp).div_ceil(align) * align;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screenshot"),
        size: padded_bytes_per_row as u64 * height as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    Ok(Readback {
        buffer,
        width,
        height,
        padded_bytes_per_row,
        format,
    })
}

impl Readback {
    /// Wait for the GPU (the copy must already be submitted) and return tightly packed RGBA8.
    pub fn into_rgba(self, device: &wgpu::Device) -> Result<(u32, u32, Vec<u8>), String> {
        let slice = self.buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("gpu poll failed: {e}"))?;
        rx.recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("cannot map screenshot buffer: {e}"))?;
        let view = slice.get_mapped_range().map_err(|e| e.to_string())?;
        let bgra = matches!(
            self.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            let start = row * self.padded_bytes_per_row as usize;
            let line = &view[start..start + w * 4];
            let (pixels, _) = line.as_chunks::<4>();
            if bgra {
                for px in pixels {
                    out.extend_from_slice(&[px[2], px[1], px[0], 255]);
                }
            } else {
                for px in pixels {
                    out.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
        }
        drop(view);
        self.buffer.unmap();
        Ok((self.width, self.height, out))
    }
}

pub fn save_png(path: &Path, w: u32, h: u32, rgba: Vec<u8>) -> Result<(), String> {
    let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("bad pixel buffer size")?;
    img.save_with_format(path, image::ImageFormat::Png)
        .map_err(|e| format!("{}: {e}", path.display()))
}
