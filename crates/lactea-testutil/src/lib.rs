// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Helpers shared by the GPU tests of the workspace: a headless device, texture read-back, a frame input that draws
//! nothing, and an image comparison.

use lactea_render::FrameInput;

/// A headless device, or `None` when no adapter is available.
pub fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// `(device, queue)` of [`gpu`]; without an adapter the calling test returns after printing a message, unless
/// `LACTEA_REQUIRE_GPU` is set (CI), which makes it a failure.
#[macro_export]
macro_rules! gpu_or_skip {
    () => {
        match $crate::gpu() {
            Some(g) => g,
            None => {
                assert!(
                    std::env::var_os("LACTEA_REQUIRE_GPU").is_none(),
                    "no wgpu adapter (LACTEA_REQUIRE_GPU is set)"
                );
                eprintln!("SKIP: no wgpu adapter");
                return;
            }
        }
    };
}

/// Read a `w` x `h` RGBA8 texture back as tightly packed bytes (row 0 = top).
pub fn read_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Vec<u8> {
    lactea_render::readback_rgba8(device, queue, texture, w, h).expect("texture read-back")
}

/// A frame that draws nothing of its own and keeps the image as it is: identity warp, decay 1, no echo / gamma
/// tint, no wave / motion vectors / borders.
pub fn quiet_input() -> FrameInput {
    let mut f = FrameInput::default();
    f.post.decay = 1.0;
    f.post.gamma = 1.0;
    f.post.wrap = false;
    f.main_wave.rgba[3] = 0.0;
    f.mv.rgba[3] = 0.0;
    f.borders.ob_rgba[3] = 0.0;
    f.borders.ib_rgba[3] = 0.0;
    f
}

/// Largest per-byte difference of two equally long images.
pub fn max_abs_diff(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len(), "images differ in size");
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}
