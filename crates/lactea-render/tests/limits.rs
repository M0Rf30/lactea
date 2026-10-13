// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sizes and meshes beyond what the device or the renderer supports are errors, not panics or wrapped arithmetic.

use lactea_render::{RenderConfig, RenderError, Renderer};
use lactea_testutil::gpu_or_skip;

fn cfg(width: u32, height: u32, mesh: [u32; 2]) -> RenderConfig {
    RenderConfig {
        width,
        height,
        mesh,
        ..RenderConfig::default()
    }
}

#[test]
fn oversized_targets_and_meshes_are_rejected() {
    let (device, queue) = gpu_or_skip!();
    let max = device.limits().max_texture_dimension_2d;

    for bad in [cfg(max + 1, 64, [16, 12]), cfg(64, u32::MAX, [16, 12])] {
        assert!(matches!(
            Renderer::new(&device, &queue, bad),
            Err(RenderError::Invalid(_))
        ));
    }
    // a mesh whose vertex count would overflow `u32` arithmetic
    assert!(matches!(
        Renderer::new(&device, &queue, cfg(64, 64, [u32::MAX - 1, 12])),
        Err(RenderError::Invalid(_))
    ));
    assert!(matches!(
        Renderer::new(&device, &queue, cfg(64, 64, [302, 12])),
        Err(RenderError::Invalid(_))
    ));

    let mut r = Renderer::new(&device, &queue, cfg(64, 64, [16, 12])).unwrap();
    assert!(matches!(
        r.resize(max + 1, 64),
        Err(RenderError::Invalid(_))
    ));
    assert!(matches!(
        r.set_mesh_size([302, 12]),
        Err(RenderError::Invalid(_))
    ));
    // a refused change leaves the renderer usable at its old size
    assert!(r.resize(32, 32).is_ok());
    assert!(r.set_mesh_size([300, 300]).is_ok());
}
