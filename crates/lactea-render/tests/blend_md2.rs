// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Renderer-level tests of MilkDrop 2's preset blend (`FrameInput::md2_blend`): the warp pass pairs on a vertex-alpha
//! ramp for every combination of old / new warp shader, old shaders kept alive, blur levels of old shaders.
//! Skipped (with a note) when there is no GPU adapter.

use lactea_abi::dump::Stage;
use lactea_abi::{ShaderInterface, WGSL_COMMON};
use lactea_render::{
    BlurParams, Compat, CompiledShader, FrameInput, Md2Blend, Md2OldPass, RenderConfig, Renderer,
};

const W: u32 = 64;
const H: u32 = 48;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

fn cfg() -> RenderConfig {
    RenderConfig {
        width: W,
        height: H,
        mesh: [16, 12],
        compat: Compat::MilkDrop2,
        ..RenderConfig::default()
    }
}

/// A warp / composite shader painting a constant colour with the vertex alpha as its alpha.
fn const_shader(r: f32, g: f32, b: f32, blur: u8) -> CompiledShader {
    CompiledShader {
        wgsl: format!(
            "{WGSL_COMMON}\n@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> {{\n\
             return vec4<f32>({r:?}, {g:?}, {b:?}, in.diffuse.w);\n}}\n"
        ),
        interface: ShaderInterface {
            textures: vec![],
            texsize_slots: vec![],
            max_blur_level: blur,
        },
    }
}

fn quiet_input() -> FrameInput {
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

/// Vertex alpha by mesh column: 0 left of column 8, 128 on it, 255 right of it (17 x 13 vertices).
fn ramp() -> Vec<u8> {
    let mut a = Vec::new();
    for _y in 0..13 {
        for x in 0..17u32 {
            a.push(match x {
                0..=7 => 0,
                8 => 128,
                _ => 255,
            });
        }
    }
    a
}

fn blend_input(progress: f32) -> FrameInput {
    let mut f = quiet_input();
    f.md2_blend = Some(Md2Blend {
        progress,
        vert_alpha: ramp(),
        old: Md2OldPass {
            q: [0.0; 32],
            blur: BlurParams::default(),
            warp_random: None,
            comp_random: None,
        },
        old_wave_mode: None,
        shape_alpha_mult: vec![],
        wave_alpha_mult: vec![],
    });
    f
}

fn pixel(img: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

/// Every combination of "old uses a warp shader" / "new uses a warp shader" (`warp_passes`): the left part of the
/// mesh (alpha 0) shows the old side, the right part (alpha 255) the new side, the column of alpha 128 a mix.
/// A side without a shader is the fixed-function `WarpedBlit_NoShaders` (previous image times the decay: here the
/// injected mid-grey).
#[test]
fn warp_pass_pairs_follow_the_vertex_alpha() {
    let Some((device, queue)) = gpu() else {
        assert!(
            std::env::var_os("LACTEA_REQUIRE_GPU").is_none(),
            "no wgpu adapter (LACTEA_REQUIRE_GPU is set)"
        );
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let grey = vec![128u8; (W * H * 4) as usize];
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    for (old_has, new_has) in [(true, true), (false, true), (true, false), (false, false)] {
        let mut r = Renderer::new(&device, &queue, cfg()).unwrap();
        // old preset's shaders -> m_OldShaders, then the new preset's
        if old_has {
            r.set_warp_shader(Some(const_shader(1.0, 0.0, 0.0, 0)))
                .unwrap();
        }
        let old = r.take_shader_state();
        r.set_old_shader_state(Some(old));
        assert!(r.has_old_shaders());
        if new_has {
            r.set_warp_shader(Some(const_shader(0.0, 0.0, 1.0, 0)))
                .unwrap();
        }
        assert_eq!(r.shader_flags(), ((new_has, false), (old_has, false)));
        r.inject_main(&grey).unwrap();
        let mut enc = device.create_command_encoder(&Default::default());
        r.render_frame(&mut enc, &blend_input(0.5), &view).unwrap();
        queue.submit(Some(enc.finish()));
        let (_, _, main) = r.read_stage(Stage::Main).unwrap();

        let want_old = if old_has {
            [255, 0, 0]
        } else {
            [128, 128, 128]
        };
        let want_new = if new_has {
            [0, 0, 255]
        } else {
            [128, 128, 128]
        };
        let case = format!("old shader {old_has}, new shader {new_has}");
        for y in [3u32, 24, 44] {
            // columns 0..7 (x < 28) and 9..16 (x > 36)
            for x in [2u32, 14, 26] {
                assert_eq!(
                    pixel(&main, x, y),
                    want_old,
                    "{case}: old side at ({x},{y})"
                );
            }
            for x in [38u32, 50, 62] {
                assert_eq!(
                    pixel(&main, x, y),
                    want_new,
                    "{case}: new side at ({x},{y})"
                );
            }
        }
        // the cells between columns 7..9 blend the two sides; both shaders -> a genuine mix at the 128 column
        let m = pixel(&main, 32, 24);
        if old_has && new_has {
            assert!(
                m[0] > 40 && m[2] > 40 && m[1] == 0,
                "{case}: mixed column {m:?}"
            );
        }
        if old_has != new_has {
            // the shader side fades into the grey of the fixed-function side
            assert_ne!(m, want_old, "{case}: not the old side {m:?}");
            assert_ne!(m, want_new, "{case}: not the new side {m:?}");
        }
    }
    assert!(
        pollster::block_on(scope.pop()).is_none(),
        "validation errors"
    );
}

/// The old shaders are kept in `m_OldShaders` (user textures included) and count for the blur levels; releasing them
/// (`set_old_shader_state(None)`, the end of a blend) drops both.
#[test]
fn old_shaders_are_kept_and_count_for_blur_levels() {
    let Some((device, queue)) = gpu() else {
        assert!(
            std::env::var_os("LACTEA_REQUIRE_GPU").is_none(),
            "no wgpu adapter (LACTEA_REQUIRE_GPU is set)"
        );
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let mut r = Renderer::new(&device, &queue, cfg()).unwrap();
    r.set_comp_shader(Some(const_shader(1.0, 1.0, 1.0, 2)))
        .unwrap();
    assert_eq!(r.blur_levels_required(), 2);
    let old = r.take_shader_state();
    assert_eq!(r.blur_levels_required(), 0, "detached shaders do not count");
    r.set_old_shader_state(Some(old));
    assert_eq!(
        r.blur_levels_required(),
        2,
        "the old shaders still require their blur levels"
    );
    r.set_comp_shader(Some(const_shader(1.0, 1.0, 1.0, 3)))
        .unwrap();
    assert_eq!(r.blur_levels_required(), 3);
    r.set_old_shader_state(None);
    assert!(!r.has_old_shaders());
    assert_eq!(r.blur_levels_required(), 3);
    // a state taken out and put back (failed load) is the same renderer again
    let st = r.take_shader_state();
    r.restore_shader_state(st);
    assert_eq!(r.blur_levels_required(), 3);
}
