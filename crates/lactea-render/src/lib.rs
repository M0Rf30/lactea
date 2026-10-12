// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! wgpu backend reproducing the MilkDrop 2 frame pipeline; host-provided device, renders into any texture.
//!
//! This crate is a port of MilkDrop 2's `CPlugin::RenderFrame` (milkdropfs.cpp) and the texture /
//! noise setup of plugin.cpp. It knows nothing about presets, expressions, HLSL or audio: the
//! engine evaluates a preset's equations, builds a [`FrameInput`] and calls
//! [`Renderer::render_frame`].
//!
//! Frame pipeline (all internal images `Rgba8Unorm`, never sRGB):
//!
//! 1. motion vectors are drawn onto the *previous* image (MD2 order),
//! 2. warp pass `prev -> next` (user warp shader, or `WarpedBlit_NoShaders`),
//! 3. blur chain from the previous image, only up to the highest level a shader uses,
//! 4. custom shapes, custom waves, main wave, darken-center, borders on `next`,
//! 5. composite `next -> final` (user comp shader on the 32x24 comp mesh, or
//!    `ShowToUser_NoShaders`: video echo, gamma, brighten/darken/solarize/invert),
//! 6. blit `final` to the host target (sRGB-compensated if needed).
//!
//! With `Compat::MilkDrop2` / `Compat::MilkDrop3` the engine may hand over [`Md2Blend`] data (`FrameInput::md2_blend`): MilkDrop 2's own
//! preset blend, where steps 2, 4 and 5 run once or twice (old and new shader, alpha-tiled) on a blended warp mesh.
//!
//! Where MilkDrop 2 and projectM differ, [`Compat`] picks the behaviour (default projectM).
//!
//! # Features
//!
//! * `diagnostics` (off by default): reference-comparison hooks (image injection, extra read-backs).
#![warn(missing_docs)]
// The pipeline is a port of MD2 / projectM code: their literals (`6.28`, `3.1415926535898`, ...) are deliberate and
// differ from the exact constants, the per-pass functions take the reference code's many parameters, and the loops
// index several parallel arrays the way the reference does.
#![allow(
    clippy::approx_constant,
    clippy::excessive_precision,
    clippy::too_many_arguments,
    clippy::needless_range_loop
)]
// Test code builds `FrameInput`s field by field and walks pixel buffers in 4-byte chunks; clarity over lint style.
#![cfg_attr(
    test,
    allow(clippy::field_reassign_with_default, clippy::chunks_exact_to_as_chunks)
)]

mod blend;
mod blur;
mod config;
mod frame;
mod geom;
mod motion;
pub mod noise;
mod renderer;
mod rng;
mod shaders;
mod stroke;
mod transition;
pub mod uniforms;
pub mod warp_mesh;
pub mod wavegen;

pub use blend::{Md2Blend, Md2OldPass, cosine_interp};
pub use config::{Compat, CompatRules, CompiledShader, RenderConfig, RenderError, StrokeRaster};
pub use frame::{
    BlurParams, Borders, CustomWave, FrameInput, MainWave, MotionVectors, PassRandom, PostParams,
    ShapeImage, ShapeInstance, WarpGpu, WavePoint,
};
pub use renderer::Renderer;
pub use rng::Rng;
pub use transition::{TransitionBlender, TransitionKind, TransitionParams};
#[cfg(feature = "diagnostics")]
pub use uniforms::RotParams;
pub use warp_mesh::MeshGeometry;

pub use lactea_abi as abi;
