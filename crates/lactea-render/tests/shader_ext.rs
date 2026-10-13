// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! MilkDrop 3 shader inputs (`FrameInput::ext`, `Md2OldPass::ext`, `docs/render.md`): the `mx` block at
//! `@group(0) @binding(1)` (mouse, `q33..q64`), the FFT texture (`TexSource::Fft`) and the old state's own copy of
//! them during a preset blend. Hand-written WGSL built from `WGSL_COMMON`; pixel checks allow one 8-bit step.
//! Skipped (with a note) when there is no GPU adapter, or a failure if `LACTEA_REQUIRE_GPU` is set.

// Tests build `FrameInput`s field by field and walk pixel buffers in 4-byte chunks; clarity over lint style.
#![allow(clippy::field_reassign_with_default, clippy::chunks_exact_to_as_chunks)]

use lactea_abi::dump::Stage;
use lactea_abi::{
    Address, FFT_BINS, Filter, ShaderInterface, TexDim, TexSource, TextureBinding, WGSL_COMMON,
};
use lactea_render::{
    BlurParams, Compat, CompiledShader, FrameInput, Md2Blend, Md2OldPass, RenderConfig, Renderer,
    ShaderExt,
};
use lactea_testutil::{gpu_or_skip, quiet_input, read_rgba8};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 48;

fn cfg(compat: Compat) -> RenderConfig {
    RenderConfig {
        width: W,
        height: H,
        mesh: [16, 12],
        compat,
        ..RenderConfig::default()
    }
}

fn target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let t = device.create_texture(&wgpu::TextureDescriptor {
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
    let v = t.create_view(&wgpu::TextureViewDescriptor::default());
    (t, v)
}

fn render(
    r: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    input: &FrameInput,
    view: &wgpu::TextureView,
) {
    let mut enc = device.create_command_encoder(&Default::default());
    r.render_frame(&mut enc, input, view).unwrap();
    queue.submit(Some(enc.finish()));
}

fn shader(body: &str, bindings: &str, interface: ShaderInterface) -> CompiledShader {
    CompiledShader {
        wgsl: format!(
            "{WGSL_COMMON}\n{bindings}\n@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> {{\n{body}\n}}\n"
        ),
        interface,
        folds: Vec::new(),
    }
}

/// A shader without textures.
fn plain(body: &str) -> CompiledShader {
    shader(body, "", ShaderInterface::default())
}

fn assert_close(got: [u8; 3], want: [u8; 3], what: &str) {
    for c in 0..3 {
        assert!(
            got[c].abs_diff(want[c]) <= 1,
            "{what}: channel {c} is {} want {} (got {got:?}, want {want:?})",
            got[c],
            want[c]
        );
    }
}

/// Every pixel of `img` (RGBA) within one 8-bit step of `want`.
fn assert_uniform(img: &[u8], want: [u8; 3], what: &str) {
    for p in img.chunks_exact(4) {
        assert_close([p[0], p[1], p[2]], want, what);
    }
}

fn px(img: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

/// `FrameInput::ext.mouse` is `mx.mouse` (`_c14`), all four components, negative values included; a frame that does
/// not set it sees the neutral `(-1, -1, 0, 0)`.
#[test]
fn mouse_reaches_the_shader() {
    let (device, queue) = gpu_or_skip!();
    let mut r = Renderer::new(&device, &queue, cfg(Compat::MilkDrop3)).unwrap();
    let (_t, view) = target(&device);
    // x, y mapped to [0, 1]; z and w packed in blue (z/2 + w/4) so all four are told apart
    r.set_comp_shader(Some(plain(
        "return vec4<f32>(mx.mouse.x * 0.5 + 0.5, mx.mouse.y * 0.5 + 0.5, \
         mx.mouse.z * 0.5 + mx.mouse.w * 0.25, 1.0);",
    )))
    .unwrap();
    let cases: [(Option<[f32; 4]>, [u8; 3]); 4] = [
        (Some([0.25, 0.75, 1.0, 0.0]), [159, 223, 128]),
        (Some([-1.0, -1.0, 0.0, 1.0]), [0, 0, 64]),
        (Some([0.0, 1.0, 1.0, 1.0]), [128, 255, 191]),
        (None, [0, 0, 0]),
    ];
    for (mouse, want) in cases {
        let mut f = quiet_input();
        if let Some(m) = mouse {
            f.ext.mouse = m;
        }
        render(&mut r, &device, &queue, &f, &view);
        let (_, _, fin) = r.read_stage(Stage::Final).unwrap();
        assert_uniform(&fin, want, &format!("mouse {mouse:?}"));
    }
}

/// `ext.q_hi` (`q33..q64`) reaches `mx.q_hi`: first, last and a middle component.
#[test]
fn q_hi_reaches_the_shader() {
    let (device, queue) = gpu_or_skip!();
    let mut r = Renderer::new(&device, &queue, cfg(Compat::MilkDrop3)).unwrap();
    let (_t, view) = target(&device);
    r.set_comp_shader(Some(plain(
        "return vec4<f32>(mx.q_hi[0].x, mx.q_hi[7].w, mx.q_hi[3].y, 1.0);",
    )))
    .unwrap();
    let mut f = quiet_input();
    f.ext.q_hi[0] = 0.25; // q33
    f.ext.q_hi[31] = 0.5; // q64
    f.ext.q_hi[13] = 0.75; // q46 = bank 3, component y
    // q1..q32 live in `mu.q`; they must not move `mx`
    f.q[31] = 0.9;
    render(&mut r, &device, &queue, &f, &view);
    let (_, _, fin) = r.read_stage(Stage::Final).unwrap();
    assert_uniform(&fin, [64, 128, 191], "q33 / q64 / q46");

    // default: all zero
    render(&mut r, &device, &queue, &quiet_input(), &view);
    let (_, _, fin) = r.read_stage(Stage::Final).unwrap();
    assert_uniform(&fin, [0, 0, 0], "default q_hi");
}

fn fft_shader() -> CompiledShader {
    let interface = ShaderInterface {
        textures: vec![TextureBinding {
            hlsl_name: "sampler_fft".into(),
            source: TexSource::Fft,
            filter: Filter::Linear,
            address: Address::Clamp,
            dim: TexDim::D2,
        }],
        texsize_slots: vec![TexSource::Fft],
        max_blur_level: 0,
    };
    // bin 8x of the spectrum into red (the translated shaders `textureLoad` this R32Float texture, it cannot be
    // filtered); `texsize_fft` = (512, 1, 1/512, 1) into green / blue
    shader(
        "let i = i32(in.position.x) * 8;\n\
         let s = textureLoad(tex_sampler_fft, vec2<i32>(i, 0), 0).r;\n\
         return vec4<f32>(s, pu.texsize[0].x / 1024.0, pu.texsize[0].y, 1.0);",
        "@group(1) @binding(1) var tex_sampler_fft: texture_2d<f32>;",
        interface,
    )
}

fn ramp(rev: bool) -> Arc<[f32; FFT_BINS]> {
    let mut s = [0.0f32; FFT_BINS];
    for (i, v) in s.iter_mut().enumerate() {
        let u = i as f32 / (FFT_BINS - 1) as f32;
        *v = if rev { 1.0 - u } else { u };
    }
    Arc::new(s)
}

/// Check the red channel of every row against `S[8x]` of the ramp, and `texsize_fft`.
fn assert_fft_ramp(img: &[u8], rev: bool, what: &str) {
    let s = ramp(rev);
    for y in [0u32, 17, 47] {
        for x in 0..W {
            let want_r = (s[(x * 8) as usize] * 255.0).round() as u8;
            // green = 512/1024, blue = 1
            assert_close(
                px(img, x, y),
                [want_r, 128, 255],
                &format!("{what} ({x},{y})"),
            );
        }
    }
}

/// `TexSource::Fft` binds the renderer's 512 x 1 `R32Float` texture: zero before the first upload, replaced by
/// every frame that carries `ext.fft`, kept by one that does not, and private to each renderer (`sibling`).
#[test]
fn fft_texture_is_uploaded_kept_and_per_renderer() {
    let (device, queue) = gpu_or_skip!();
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut r = Renderer::new(&device, &queue, cfg(Compat::MilkDrop3)).unwrap();
    let (_t, view) = target(&device);
    r.set_comp_shader(Some(fft_shader())).unwrap();
    let final_img = |r: &Renderer| r.read_stage(Stage::Final).unwrap().2;

    // before any upload the texture is all zero (and `texsize_fft` is already right)
    render(&mut r, &device, &queue, &quiet_input(), &view);
    assert_uniform(&final_img(&r), [0, 128, 255], "zero FFT before an upload");

    let mut f = quiet_input();
    f.ext.fft = Some(ramp(false));
    render(&mut r, &device, &queue, &f, &view);
    assert_fft_ramp(&final_img(&r), false, "ramp upload");

    // `None` keeps the previous upload
    render(&mut r, &device, &queue, &quiet_input(), &view);
    assert_fft_ramp(&final_img(&r), false, "ramp kept");

    // a new spectrum replaces it
    f.ext.fft = Some(ramp(true));
    render(&mut r, &device, &queue, &f, &view);
    assert_fft_ramp(&final_img(&r), true, "reversed ramp upload");

    // a sibling has its own texture: zero, whatever `r` holds
    let mut s = r.sibling();
    s.set_comp_shader(Some(fft_shader())).unwrap();
    render(&mut s, &device, &queue, &quiet_input(), &view);
    assert_uniform(
        &s.read_stage(Stage::Final).unwrap().2,
        [0, 128, 255],
        "sibling FFT",
    );
    render(&mut r, &device, &queue, &quiet_input(), &view);
    assert_fft_ramp(&final_img(&r), true, "original unaffected by the sibling");

    assert!(
        pollster::block_on(scope.pop()).is_none(),
        "validation errors"
    );
}

/// Two frames of one renderer recorded into one encoder each see their own spectrum (the upload is part of the
/// recorded commands, not a queue write that lands before all of them).
#[test]
fn fft_upload_is_ordered_with_the_frame_that_carries_it() {
    let (device, queue) = gpu_or_skip!();
    let mut r = Renderer::new(&device, &queue, cfg(Compat::MilkDrop3)).unwrap();
    r.set_comp_shader(Some(fft_shader())).unwrap();
    let (ta, va) = target(&device);
    let (tb, vb) = target(&device);
    let mut a = quiet_input();
    a.ext.fft = Some(ramp(false));
    let mut b = quiet_input();
    b.ext.fft = Some(ramp(true));
    let mut enc = device.create_command_encoder(&Default::default());
    r.render_frame(&mut enc, &a, &va).unwrap();
    r.render_frame(&mut enc, &b, &vb).unwrap();
    queue.submit(Some(enc.finish()));
    assert_fft_ramp(
        &read_rgba8(&device, &queue, &ta, W, H),
        false,
        "first frame",
    );
    assert_fft_ramp(
        &read_rgba8(&device, &queue, &tb, W, H),
        true,
        "second frame",
    );
}

/// A frame rendered to the renderer's own output (`render_frame_to_output`) uploads the FFT as well.
#[test]
fn fft_is_uploaded_by_render_frame_to_output() {
    let (device, queue) = gpu_or_skip!();
    let mut r = Renderer::new(&device, &queue, cfg(Compat::MilkDrop3)).unwrap();
    r.set_comp_shader(Some(fft_shader())).unwrap();
    let mut f = quiet_input();
    f.ext.fft = Some(ramp(false));
    let mut enc = device.create_command_encoder(&Default::default());
    r.render_frame_to_output(&mut enc, &f).unwrap();
    queue.submit(Some(enc.finish()));
    assert_fft_ramp(
        &r.read_stage(Stage::Final).unwrap().2,
        false,
        "output-only frame",
    );
}

/// Warp / comp shader painting `(mx.mouse.x, mx.q_hi[0].x, _c2.x / 10)` with the vertex alpha as its alpha.
fn probe_shader() -> CompiledShader {
    plain("return vec4<f32>(mx.mouse.x, mx.q_hi[0].x, mu.c[2].x * 0.1, in.diffuse.w);")
}

/// Vertex alpha by mesh column: 0 left of column 8, 128 on it, 255 right of it (17 x 13 vertices).
fn ramp_alpha() -> Vec<u8> {
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

/// During a MilkDrop 2 / 3 blend the old state's shader passes (warp and composite) read `Md2OldPass::ext`
/// (mouse, `q33..`, shader time), the new state's read `FrameInput::ext`. The left part of the vertex-alpha ramp
/// shows the old side, the right part the new one.
#[test]
fn old_state_passes_see_their_own_ext() {
    let (device, queue) = gpu_or_skip!();
    let (_t, view) = target(&device);
    let mut r = Renderer::new(&device, &queue, cfg(Compat::MilkDrop3)).unwrap();
    for set in [Renderer::set_warp_shader, Renderer::set_comp_shader] {
        set(&mut r, Some(probe_shader())).unwrap();
    }
    let old = r.take_shader_state();
    r.set_old_shader_state(Some(old));
    for set in [Renderer::set_warp_shader, Renderer::set_comp_shader] {
        set(&mut r, Some(probe_shader())).unwrap();
    }

    let mut f = quiet_input();
    f.time = 3.0;
    f.ext.mouse = [192.0 / 255.0, 0.0, 0.0, 0.0];
    f.ext.q_hi[0] = 64.0 / 255.0;
    f.ext.time = Some(2.0);
    let mut old_ext = ShaderExt::default();
    old_ext.mouse = [32.0 / 255.0, 0.0, 0.0, 0.0];
    old_ext.q_hi[0] = 128.0 / 255.0;
    // no shader time of its own: `_c2.x` is `FrameInput::time`
    old_ext.time = None;
    f.md2_blend = Some(Md2Blend {
        progress: 0.5,
        vert_alpha: ramp_alpha(),
        old: Md2OldPass {
            q: [0.0; 32],
            blur: BlurParams::default(),
            warp_random: None,
            comp_random: None,
            ext: old_ext,
        },
        old_wave_mode: None,
        shape_alpha_mult: vec![],
        wave_alpha_mult: vec![],
    });
    render(&mut r, &device, &queue, &f, &view);

    // _c2.x / 10 * 255: new 2.0 -> 51, old 3.0 -> 76.5 (either rounding)
    let want_new = [192, 64, 51];
    let want_old = [32, 128, 76];
    for (stage, name) in [(Stage::Main, "warp"), (Stage::Final, "comp")] {
        let (_, _, img) = r.read_stage(stage).unwrap();
        for y in [3u32, 24, 44] {
            for x in [2u32, 14] {
                assert_close(
                    px(&img, x, y),
                    want_old,
                    &format!("{name}: old side ({x},{y})"),
                );
            }
            for x in [50u32, 62] {
                assert_close(
                    px(&img, x, y),
                    want_new,
                    &format!("{name}: new side ({x},{y})"),
                );
            }
        }
    }
}
