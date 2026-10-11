// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Headless GPU tests. They skip (with a message) when no adapter is available, or fail if `LACTEA_REQUIRE_GPU` is set.

// Tests build `FrameInput`s field by field and walk pixel buffers; clarity over lint style.
#![allow(
    clippy::field_reassign_with_default,
    clippy::chunks_exact_to_as_chunks,
    clippy::manual_div_ceil,
    clippy::manual_is_multiple_of
)]
use lactea_abi::dump::Stage;
use lactea_abi::{
    Address, Filter, NoiseTex, ShaderInterface, TexDim, TexSource, TextureBinding, WGSL_COMMON,
};
use lactea_render::{
    Compat, CompiledShader, CustomWave, FrameInput, RenderConfig, Renderer, ShapeInstance,
    WavePoint,
};

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// No adapter: skip with a message, unless `LACTEA_REQUIRE_GPU` is set (CI), which makes it a failure.
macro_rules! need_gpu {
    () => {
        match gpu() {
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

fn cfg(w: u32, h: u32) -> RenderConfig {
    RenderConfig {
        width: w,
        height: h,
        mesh: [16, 12],
        ..RenderConfig::default()
    }
}

fn target(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let t = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let v = t.create_view(&wgpu::TextureViewDescriptor::default());
    (t, v)
}

fn read_target(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    t: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let bpr = (w * 4 + 255) / 256 * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: t,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(enc.finish()));
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buf.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::new();
    for y in 0..h {
        out.extend_from_slice(&data[(y * bpr) as usize..(y * bpr + w * 4) as usize]);
    }
    out
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

/// An input that draws nothing: identity warp, decay 1, no echo/gamma tint, no wave/mv/borders.
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

fn gradient(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            v[i] = (x * 255 / (w - 1)) as u8;
            v[i + 1] = (y * 255 / (h - 1)) as u8;
            v[i + 2] = ((x + y) % 64 * 4) as u8;
            v[i + 3] = 255;
        }
    }
    v
}

fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

#[test]
fn identity_warp_preserves_image_and_orientation() {
    let (device, queue) = need_gpu!();
    let (w, h) = (64, 48);
    // MilkDrop2 compat: no always-on hue tint in the no-shader composite (fShader = 0)
    let mut c = cfg(w, h);
    c.compat = Compat::MilkDrop2;
    let mut r = Renderer::new(&device, &queue, c).unwrap();
    let (t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);
    let mut img = gradient(w, h);
    // marker: pure red 6x6 block in the TOP-LEFT corner (row 0 = top)
    for y in 0..6 {
        for x in 0..6 {
            let i = ((y * w + x) * 4) as usize;
            img[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    r.inject_main(&img).unwrap();
    // readback orientation of the injected image itself
    let (rw, rh, back) = r.read_stage(Stage::Main).unwrap();
    assert_eq!((rw, rh), (w, h));
    assert_eq!(
        back, img,
        "read_stage(Main) must return the injected bytes, row 0 = top"
    );

    render(&mut r, &device, &queue, &quiet_input(), &view);
    let (_, _, main) = r.read_stage(Stage::Main).unwrap();
    let d = max_diff(&main, &img);
    assert!(
        d <= 1,
        "identity warp (decay 1) must keep the image; max diff {d}"
    );
    // red marker still top-left, not bottom-left
    assert_eq!(&main[0..4], &[255, 0, 0, 255]);
    let bl = ((h - 1) * w * 4) as usize;
    assert_ne!(&main[bl..bl + 3], &[255, 0, 0]);
    // final composite (no echo, gamma 1) equals main; target equals final stage
    let (_, _, fin) = r.read_stage(Stage::Final).unwrap();
    // the no-shader composite quad extends 1 px past the screen (MD2), so edges blend by < 1 texel
    let mut worst = 0u8;
    for y in 0..h {
        for x in 0..w {
            if (x < 8 && y < 8) || x < 2 || y < 2 || x >= w - 2 || y >= h - 2 {
                continue; // marker edge / MD2 wrap-sampler blend at the 1-px overscan border
            }
            let i = ((y * w + x) * 4) as usize;
            worst = worst.max(max_diff(&fin[i..i + 2], &img[i..i + 2])); // R,G ramps (B is a sawtooth)
        }
    }
    assert!(worst <= 12, "final diff {worst}");
    assert!(
        fin[0] > 150 && fin[1] < 100,
        "marker still top-left in the composite: {:?}",
        &fin[0..4]
    );
    let tb = read_target(&device, &queue, &t, w, h);
    assert_eq!(tb, fin, "target bytes == Final stage");
}

#[test]
fn srgb_target_stores_same_bytes_as_unorm() {
    let (device, queue) = need_gpu!();
    let (w, h) = (32, 32);
    let mut c = cfg(w, h);
    c.output_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut r = Renderer::new(&device, &queue, c).unwrap();
    let (t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8UnormSrgb);
    r.inject_main(&gradient(w, h)).unwrap();
    render(&mut r, &device, &queue, &quiet_input(), &view);
    let (_, _, fin) = r.read_stage(Stage::Final).unwrap();
    let tb = read_target(&device, &queue, &t, w, h);
    assert!(
        max_diff(&tb, &fin) <= 1,
        "sRGB compensation: max diff {}",
        max_diff(&tb, &fin)
    );
}

#[test]
fn decay_modes_follow_compat() {
    let (device, queue) = need_gpu!();
    let (w, h) = (32, 32);
    let mut f = quiet_input();
    f.post.decay = 0.5;
    let white = vec![255u8; (w * h * 4) as usize];
    // projectM: float decay -> 127.5 -> 128 (round to nearest) ; MD2: (int)(0.5*255)=127 -> 127/255*255
    let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
    let (_t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);
    r.inject_main(&white).unwrap();
    render(&mut r, &device, &queue, &f, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert!(
        (m[4 * (w as usize * 16 + 16)] as i32 - 128).abs() <= 1,
        "projectM decay {}",
        m[4 * (w as usize * 16 + 16)]
    );

    let mut c = cfg(w, h);
    c.compat = Compat::MilkDrop2;
    let mut r = Renderer::new(&device, &queue, c).unwrap();
    r.inject_main(&white).unwrap();
    render(&mut r, &device, &queue, &f, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert!(
        (m[4 * (w as usize * 16 + 16)] as i32 - 127).abs() <= 1,
        "MD2 decay {}",
        m[4 * (w as usize * 16 + 16)]
    );
}

#[test]
fn main_alpha_follows_projectm_rgba_semantics() {
    let (device, queue) = need_gpu!();
    let (w, h) = (32, 32);
    let (_t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);
    let px = |m: &[u8], x: usize, y: usize| {
        let i = 4 * (y * w as usize + x);
        [m[i], m[i + 1], m[i + 2], m[i + 3]]
    };
    let mut img = vec![0u8; (w * h * 4) as usize];
    for p in img.chunks_exact_mut(4) {
        p.copy_from_slice(&[200, 100, 50, 100]);
    }
    let mut f = quiet_input();
    f.post.decay = 1.0;

    // projectM: the no-shader warp keeps `tex.a` (decay only scales RGB), a fresh renderer starts at alpha 0
    let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
    r.inject_main(&img).unwrap();
    render(&mut r, &device, &queue, &f, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert_eq!(px(&m, 16, 16), [200, 100, 50, 100]);
    let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
    render(&mut r, &device, &queue, &f, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert_eq!(px(&m, 16, 16)[3], 0, "main alpha starts at 0");

    // overlays blend the alpha with the colour factors: a = a*a + dst*(1-a), here 0.5*0.5 + 0 (outer border)
    let mut g = f.clone();
    g.borders.ob_size = 0.2;
    g.borders.ob_rgba = [1.0, 0.0, 0.0, 0.5];
    let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
    render(&mut r, &device, &queue, &g, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert_eq!(px(&m, 16, 16)[3], 0);
    let edge = px(&m, 0, 16);
    assert!((edge[3] as i32 - 64).abs() <= 1, "border alpha {edge:?}");

    // MD2 (X8R8G8B8): alpha is not a thing, the warp writes the diffuse alpha (1)
    let mut c = cfg(w, h);
    c.compat = Compat::MilkDrop2;
    let mut r = Renderer::new(&device, &queue, c).unwrap();
    r.inject_main(&img).unwrap();
    render(&mut r, &device, &queue, &f, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert_eq!(px(&m, 16, 16)[3], 255);
}

/// A pixel centre exactly on a horizontal border edge goes to the polygon above the edge in projectM (GL window space)
/// and below it in MD2 (D3D9 top-left rule): `ob_size = 0.1` on 30 rows puts the edges at y = 1.5 and 28.5.
#[test]
fn border_edges_on_pixel_centres_follow_the_oracle_rule() {
    let (device, queue) = need_gpu!();
    let (w, h) = (40u32, 30u32);
    let (_t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);
    let mut f = quiet_input();
    f.borders.ob_size = 0.1;
    f.borders.ob_rgba = [1.0, 0.0, 0.0, 1.0];
    let red_rows = |compat: Compat| {
        let mut c = cfg(w, h);
        c.compat = compat;
        let mut r = Renderer::new(&device, &queue, c).unwrap();
        render(&mut r, &device, &queue, &f, &view);
        let (_, _, m) = r.read_stage(Stage::Main).unwrap();
        (0..h as usize)
            .filter(|&y| m[4 * (y * w as usize + 20)] == 255)
            .collect::<Vec<_>>()
    };
    // top band: rows 0, 1 (centre 1.5 on its bottom edge: included); bottom band: row 29 only (centre 28.5 on its top edge)
    assert_eq!(red_rows(Compat::ProjectM), vec![0, 1, 29]);
    assert_eq!(red_rows(Compat::MilkDrop2), vec![0, 28, 29]);
}

/// projectM: the first frame of a preset whose shaders sample blur loses every overlay (`BlurTexture::Update` restores
/// FBO 0 after allocating its textures); presets without blur - or with only a diagnostic blur chain - draw them.
#[test]
fn first_frame_overlays_are_lost_only_with_native_blur() {
    let (device, queue) = need_gpu!();
    let (w, h) = (32u32, 32u32);
    let (_t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);
    let mut f = quiet_input();
    f.borders.ob_size = 0.2;
    f.borders.ob_rgba = [1.0, 0.0, 0.0, 1.0];
    let edge_red = |m: &[u8]| m[4 * (16 * w as usize)]; // R at (0, 16): inside the outer border
    let blur_comp = || {
        let iface = ShaderInterface {
            textures: vec![binding(
                "sampler_blur1",
                TexSource::Blur(1),
                TexDim::D2,
                Address::Clamp,
            )],
            texsize_slots: vec![TexSource::Blur(1)],
            max_blur_level: 1,
        };
        let wgsl = tex_shader_wgsl(
            "return vec4<f32>(textureSample(t0, s0, in.uv.xy).rgb + 0.0 * pu.texsize[0].xyz, 1.0);",
            "@group(1) @binding(1) var t0: texture_2d<f32>;\n@group(1) @binding(2) var s0: sampler;",
        );
        CompiledShader {
            wgsl,
            interface: iface,
        }
    };

    // no blur anywhere: frame 0 has its border
    let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
    render(&mut r, &device, &queue, &f, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert!(
        edge_red(&m) > 200,
        "plain preset, frame 0: {}",
        edge_red(&m)
    );

    // a diagnostic blur chain (blur stages requested) must not change that
    let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
    r.set_min_blur_levels(3);
    render(&mut r, &device, &queue, &f, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert!(
        edge_red(&m) > 200,
        "diagnostic blur, frame 0: {}",
        edge_red(&m)
    );

    // the composite shader samples blur: frame 0 loses the overlay, frame 1 has it
    let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
    r.set_comp_shader(Some(blur_comp())).unwrap();
    assert_eq!(r.blur_levels_required(), 1);
    render(&mut r, &device, &queue, &f, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert_eq!(edge_red(&m), 0, "native blur, frame 0");
    let mut f1 = f.clone();
    f1.frame = 1;
    render(&mut r, &device, &queue, &f1, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert!(edge_red(&m) > 200, "native blur, frame 1: {}", edge_red(&m));

    // MD2 draws them on frame 0 too
    let mut c = cfg(w, h);
    c.compat = Compat::MilkDrop2;
    let mut r = Renderer::new(&device, &queue, c).unwrap();
    r.set_comp_shader(Some(blur_comp())).unwrap();
    render(&mut r, &device, &queue, &f, &view);
    let (_, _, m) = r.read_stage(Stage::Main).unwrap();
    assert!(
        edge_red(&m) > 200,
        "MD2 native blur, frame 0: {}",
        edge_red(&m)
    );
}

fn busy_input(frame: u32) -> FrameInput {
    let mut f = FrameInput::default();
    f.frame = frame;
    f.time = frame as f64 / 60.0;
    f.post.decay = 0.97;
    f.post.echo_alpha = 0.4;
    f.post.echo_zoom = 1.3;
    f.post.echo_orient = 1;
    f.post.gamma = 1.5;
    f.post.brighten = frame % 2 == 0;
    f.post.solarize = frame % 3 == 0;
    f.mv.rgba[3] = 0.5;
    f.borders.ob_rgba = [1.0, 0.0, 0.0, 0.5];
    f.borders.ib_rgba = [0.0, 1.0, 0.0, 0.5];
    f.darken_center = true;
    let g = lactea_render::MeshGeometry::new(16, 12, 64, 48).unwrap();
    let anim = lactea_render::warp_mesh::WarpAnim::new(f.time as f32, 1.0, 1.0);
    let v = lactea_render::warp_mesh::VertexVars {
        zoom: 1.03,
        rot: 0.02 * frame as f32,
        warp: 1.0,
        ..Default::default()
    };
    f.warp_uv = Some(lactea_render::warp_mesh::warp_mesh_uniform(
        &g, &v, &anim, [0.0; 2],
    ));
    for i in 0..576 {
        f.main_wave.wave[0][i] = 60.0 * (i as f32 * 0.07 + frame as f32).sin();
        f.main_wave.wave[1][i] = 60.0 * (i as f32 * 0.05).cos();
    }
    f.shapes.push(ShapeInstance {
        sides: 5,
        x: 0.4,
        y: 0.6,
        rad: 0.2,
        textured: frame % 2 == 1,
        ..Default::default()
    });
    f.waves.push(CustomWave {
        points: (0..32)
            .map(|i| WavePoint {
                x: i as f64 / 31.0,
                y: 0.5 + 0.2 * (i as f64 * 0.5).sin(),
                rgba: [1.0, 0.5, 0.2, 0.9],
            })
            .collect(),
        ..Default::default()
    });
    f
}

#[test]
fn default_state_sequence_renders_without_validation_errors() {
    let (device, queue) = need_gpu!();
    let (w, h) = (64, 48);
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
    let (_t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);
    for n in 0..6 {
        let mut f = FrameInput::default();
        f.frame = n;
        f.time = n as f64 / 60.0;
        render(&mut r, &device, &queue, &f, &view);
    }
    for n in 0..6 {
        render(&mut r, &device, &queue, &busy_input(n), &view);
    }
    for s in Stage::ALL {
        let (sw, sh, data) = r.read_stage(s).unwrap();
        assert_eq!(data.len(), (sw * sh * 4) as usize, "{s:?}");
    }
    let err = pollster::block_on(scope.pop());
    assert!(err.is_none(), "validation error: {err:?}");
}

#[test]
fn main_wave_draws_a_ring_in_every_mode() {
    let (device, queue) = need_gpu!();
    let (w, h) = (96, 64);
    let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
    let (_t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);
    for mode in 0..8 {
        let mut f = quiet_input();
        f.main_wave.rgba = [1.0, 1.0, 1.0, 1.0];
        f.main_wave.mode = mode;
        f.treb = 1.0;
        f.imm_abs = [1.0; 3];
        f.main_wave.wave[0]
            .iter_mut()
            .enumerate()
            .for_each(|(i, s)| *s = 70.0 * (i as f32 * 0.09).sin());
        f.main_wave.wave[1]
            .iter_mut()
            .enumerate()
            .for_each(|(i, s)| *s = 70.0 * (i as f32 * 0.11).cos());
        r.inject_main(&vec![0u8; (w * h * 4) as usize]).unwrap();
        render(&mut r, &device, &queue, &f, &view);
        let (_, _, m) = r.read_stage(Stage::Main).unwrap();
        // projectM scales the alpha of modes 2/3/5 by texsize (0.07..0.11 at <= 1024 px), so they are faint here
        let lit = m.chunks_exact(4).filter(|p| p[0] > 8).count();
        assert!(lit > 20, "mode {mode}: only {lit} lit pixels");
    }
}

#[test]
fn determinism_same_inputs_same_bytes() {
    let (device, queue) = need_gpu!();
    let (w, h) = (64, 48);
    let run = || {
        let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
        let (_t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);
        for n in 0..5 {
            render(&mut r, &device, &queue, &busy_input(n), &view);
        }
        Stage::ALL.map(|s| r.read_stage(s).unwrap().2)
    };
    let a = run();
    let b = run();
    for (i, s) in Stage::ALL.iter().enumerate() {
        assert_eq!(a[i], b[i], "{s:?} differs between runs");
    }
    assert!(
        a[0].iter().any(|&x| x != 0),
        "final image should not be black"
    );
}

fn tex_shader_wgsl(body: &str, bindings: &str) -> String {
    format!(
        "{WGSL_COMMON}\n{bindings}\n@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> {{\n{body}\n}}\n"
    )
}

fn binding(name: &str, src: TexSource, dim: TexDim, addr: Address) -> TextureBinding {
    TextureBinding {
        hlsl_name: name.into(),
        source: src,
        filter: Filter::Linear,
        address: addr,
        dim,
    }
}

#[test]
fn user_warp_shader_and_blur_levels() {
    let (device, queue) = need_gpu!();
    let (w, h) = (128, 96);
    let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
    let (_t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);

    // invalid WGSL is rejected, not a panic
    let bad = CompiledShader {
        wgsl: "this is not wgsl".into(),
        interface: ShaderInterface::default(),
    };
    assert!(r.set_warp_shader(Some(bad)).is_err());

    // warp shader: copy main (fw_main) at uv
    let iface = ShaderInterface {
        textures: vec![binding(
            "sampler_fw_main",
            TexSource::Main,
            TexDim::D2,
            Address::Clamp,
        )],
        texsize_slots: vec![TexSource::Main],
        max_blur_level: 0,
    };
    let wgsl = tex_shader_wgsl(
        "return vec4<f32>(textureSample(t0, s0, in.uv.xy).rgb * pu.texsize[0].x / 128.0, 1.0);",
        "@group(1) @binding(1) var t0: texture_2d<f32>;\n@group(1) @binding(2) var s0: sampler;",
    );
    r.set_warp_shader(Some(CompiledShader {
        wgsl,
        interface: iface,
    }))
    .unwrap();
    let img = gradient(w, h);
    r.inject_main(&img).unwrap();
    render(&mut r, &device, &queue, &quiet_input(), &view);
    let (_, _, main) = r.read_stage(Stage::Main).unwrap();
    // pu.texsize[0].x == 128 -> factor 1: identity (diffuse unused)
    assert!(
        max_diff(&main, &img) <= 1,
        "shader identity warp diff {}",
        max_diff(&main, &img)
    );

    // comp shader using blur3 + noise volume, forcing the whole blur chain
    let iface = ShaderInterface {
        textures: vec![
            binding(
                "sampler_blur1",
                TexSource::Blur(1),
                TexDim::D2,
                Address::Clamp,
            ),
            binding(
                "sampler_blur3",
                TexSource::Blur(3),
                TexDim::D2,
                Address::Clamp,
            ),
            binding(
                "sampler_noisevol_lq",
                TexSource::Noise(NoiseTex::VolLq),
                TexDim::D3,
                Address::Wrap,
            ),
        ],
        texsize_slots: vec![TexSource::Blur(3), TexSource::Noise(NoiseTex::VolLq)],
        max_blur_level: 3,
    };
    let wgsl = tex_shader_wgsl(
        "let a = textureSample(t0, s0, in.uv.xy).rgb * mu.c[5].x + mu.c[5].y;\n\
         let b = textureSample(t1, s1, in.uv.xy).rgb * mu.c[6].x + mu.c[6].y;\n\
         let n = textureSample(t2, s2, vec3<f32>(in.uv.xy, 0.5)).rgb;\n\
         return vec4<f32>(0.5 * (a + b) + 0.0 * n + 0.0 * pu.texsize[1].xyz, 1.0);",
        "@group(1) @binding(1) var t0: texture_2d<f32>;\n@group(1) @binding(2) var s0: sampler;\n\
         @group(1) @binding(3) var t1: texture_2d<f32>;\n@group(1) @binding(4) var s1: sampler;\n\
         @group(1) @binding(5) var t2: texture_3d<f32>;\n@group(1) @binding(6) var s2: sampler;",
    );
    r.set_comp_shader(Some(CompiledShader {
        wgsl,
        interface: iface,
    }))
    .unwrap();
    assert_eq!(r.blur_levels_required(), 3);
    r.set_warp_shader(None).unwrap();
    r.inject_main(&vec![255u8; (w * h * 4) as usize]).unwrap();
    render(&mut r, &device, &queue, &quiet_input(), &view);
    // blur source is the previous (white) image: interior stays ~white, edges are darkened by blur1
    for (stage, bw, bh) in [
        (Stage::Blur1, 64, 48),
        (Stage::Blur2, 32, 24),
        (Stage::Blur3, 16, 12),
    ] {
        let (sw, sh, b) = r.read_stage(stage).unwrap();
        assert!(sw <= bw + 16 && sh <= bh + 4, "{stage:?} size {sw}x{sh}");
        let mid = ((sh / 2 * sw + sw / 2) * 4) as usize;
        assert!(b[mid] > 200, "{stage:?} centre {}", b[mid]);
    }
    let (sw, sh, b1) = r.read_stage(Stage::Blur1).unwrap();
    let corner = b1[0] as i32;
    let centre = b1[((sh / 2 * sw + sw / 2) * 4) as usize] as i32;
    assert!(
        corner < centre,
        "edge darken: corner {corner} vs centre {centre}"
    );
    let (_, _, fin) = r.read_stage(Stage::Final).unwrap();
    assert!(
        fin.chunks_exact(4).any(|p| p[0] > 150),
        "comp shader output should be bright"
    );
}

#[test]
fn noise_injection_and_user_textures() {
    let (device, queue) = need_gpu!();
    let mut r = Renderer::new(&device, &queue, cfg(32, 32)).unwrap();
    let ok = vec![7u8; 256 * 256 * 4];
    r.inject_noise(NoiseTex::Lq, &ok).unwrap();
    assert!(r.inject_noise(NoiseTex::Lq, &[0; 4]).is_err());
    r.inject_noise(NoiseTex::VolLq, &vec![1u8; 32 * 32 * 32 * 4])
        .unwrap();
    r.set_user_texture("Clouds2", 4, 4, &[255u8; 64]).unwrap();
    assert!(r.set_user_texture("bad", 4, 4, &[0u8; 3]).is_err());
}

#[test]
fn resize_resets_and_keeps_working() {
    let (device, queue) = need_gpu!();
    let mut r = Renderer::new(&device, &queue, cfg(64, 48)).unwrap();
    let (_t, view) = target(&device, 32, 24, wgpu::TextureFormat::Rgba8Unorm);
    r.resize(32, 24).unwrap();
    render(&mut r, &device, &queue, &busy_input(0), &view);
    let (w, h, d) = r.read_stage(Stage::Final).unwrap();
    assert_eq!((w, h), (32, 24));
    assert_eq!(d.len(), 32 * 24 * 4);
}

/// Bad sizes, meshes and frame inputs are errors, not panics, and leave the renderer usable.
#[test]
fn invalid_arguments_are_errors() {
    use lactea_render::{MeshGeometry, RenderError};
    let (device, queue) = need_gpu!();
    assert!(matches!(
        Renderer::new(&device, &queue, cfg(0, 48)),
        Err(RenderError::Invalid(_))
    ));
    let mut odd = cfg(64, 48);
    odd.mesh = [15, 12];
    assert!(Renderer::new(&device, &queue, odd).is_err());
    assert!(MeshGeometry::new(0, 4, 64, 48).is_err());
    assert!(MeshGeometry::new(4, 3, 64, 48).is_err());

    let mut r = Renderer::new(&device, &queue, cfg(64, 48)).unwrap();
    assert!(r.resize(0, 10).is_err());
    assert!(r.set_mesh_size([0, 12]).is_err());
    assert!(r.set_mesh_size([17, 12]).is_err());
    assert_eq!(r.config().mesh, [16, 12], "a rejected size changes nothing");
    assert_eq!((r.config().width, r.config().height), (64, 48));

    let (_t, view) = target(&device, 64, 48, wgpu::TextureFormat::Rgba8Unorm);
    let mut enc = device.create_command_encoder(&Default::default());
    let mut f = FrameInput::default();
    f.warp_uv = Some(vec![[0.0; 2]; 3]);
    assert!(matches!(
        r.render_frame(&mut enc, &f, &view),
        Err(RenderError::Invalid(_))
    ));
    // an MD2 blend needs the MD2 compat rules
    let mut f = FrameInput::default();
    f.md2_blend = Some(lactea_render::Md2Blend {
        progress: 0.5,
        vert_alpha: vec![255; 17 * 13],
        old: lactea_render::Md2OldPass {
            q: [0.0; 32],
            blur: Default::default(),
            warp_random: None,
            comp_random: None,
        },
        old_wave_mode: None,
        shape_alpha_mult: vec![],
        wave_alpha_mult: vec![],
    });
    assert!(r.render_frame(&mut enc, &f, &view).is_err());
    queue.submit(Some(enc.finish()));
    // and the renderer still draws
    render(&mut r, &device, &queue, &quiet_input(), &view);
}

/// projectM textures have a single level (`GL_LINEAR`, no `glGenerateMipmap`): `tex2Dlod` / derivative-based LOD never
/// reaches a mip. MD2 (D3D9) trilinear-filters a mip chain. Regression for Iris Storm / Tokamak (noise lookups at
/// ~1 texel per pixel picked up the mip-1 level for LOD ~ +0.01: warped 61 -> 86 dB once removed).
#[test]
fn projectm_samplers_read_level_zero_only() {
    let (device, queue) = need_gpu!();
    let (w, h) = (32, 32);
    // 16x16 1-px checkerboard: level 0 has full contrast, level 4 (1x1) is flat grey
    let mut chk = vec![255u8; 16 * 16 * 4];
    for y in 0..16usize {
        for x in 0..16usize {
            let v = if (x + y) % 2 == 0 { 255 } else { 0 };
            let i = (y * 16 + x) * 4;
            chk[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let spread = |compat: Compat| -> u8 {
        let mut c = cfg(w, h);
        c.compat = compat;
        let mut r = Renderer::new(&device, &queue, c).unwrap();
        let (_t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);
        r.set_user_texture("chk", 16, 16, &chk).unwrap();
        let iface = ShaderInterface {
            textures: vec![binding(
                "sampler_chk",
                TexSource::User("chk".into()),
                TexDim::D2,
                Address::Wrap,
            )],
            texsize_slots: vec![TexSource::User("chk".into())],
            max_blur_level: 0,
        };
        let wgsl = tex_shader_wgsl(
            "return vec4<f32>(textureSampleLevel(t0, s0, in.uv.xy * 3.0, 4.0).rgb, 1.0);",
            "@group(1) @binding(1) var t0: texture_2d<f32>;\n@group(1) @binding(2) var s0: sampler;",
        );
        r.set_comp_shader(Some(CompiledShader {
            wgsl,
            interface: iface,
        }))
        .unwrap();
        render(&mut r, &device, &queue, &quiet_input(), &view);
        let (_, _, fin) = r.read_stage(Stage::Final).unwrap();
        let (mut lo, mut hi) = (255u8, 0u8);
        for p in fin.chunks_exact(4) {
            lo = lo.min(p[0]);
            hi = hi.max(p[0]);
        }
        hi - lo
    };
    assert!(spread(Compat::ProjectM) > 100, "projectM must see level 0");
    assert!(spread(Compat::MilkDrop2) < 8, "MD2 reads the mip chain");
}

/// The GPU warp vertex stage (projectM's `PresetWarpVertexShaderGlsl330.vert`) must agree with the CPU
/// formula (`warp_mesh::warp_vertex_uv`) up to GPU `sin`/`cos`/`pow` approximations: same orientation, same
/// cell triangulation, same texel offset.
#[test]
fn gpu_warp_vertex_stage_matches_cpu_uv() {
    let (device, queue) = need_gpu!();
    let (w, h) = (64, 48);
    let img = gradient(w, h);
    let run = |gpu_vs: bool, shader: bool| -> Vec<u8> {
        let mut r = Renderer::new(&device, &queue, cfg(w, h)).unwrap();
        let (_t, view) = target(&device, w, h, wgpu::TextureFormat::Rgba8Unorm);
        if shader {
            let iface = ShaderInterface {
                textures: vec![binding(
                    "sampler_main",
                    TexSource::Main,
                    TexDim::D2,
                    Address::Clamp,
                )],
                texsize_slots: vec![TexSource::Main],
                max_blur_level: 0,
            };
            let wgsl = tex_shader_wgsl(
                "return vec4<f32>(textureSample(t0, s0, in.uv.xy).rgb * in.diffuse.rgb, 1.0);",
                "@group(1) @binding(1) var t0: texture_2d<f32>;\n@group(1) @binding(2) var s0: sampler;",
            );
            r.set_warp_shader(Some(CompiledShader {
                wgsl,
                interface: iface,
            }))
            .unwrap();
        }
        r.inject_main(&img).unwrap();
        let g = r.mesh_geometry().clone();
        let anim = lactea_render::warp_mesh::WarpAnim::projectm(0.7, 1.0, 1.0);
        let n = g.vertex_count();
        // non-trivial motion: zoom, rotation, translation, stretch, warp, per-vertex variation in zoom
        let vars: Vec<lactea_render::warp_mesh::VertexVars> = (0..n)
            .map(|i| lactea_render::warp_mesh::VertexVars {
                zoom: 1.0 + 0.1 * (i % 5) as f32 / 5.0,
                zoomexp: 1.3,
                rot: 0.15,
                warp: 0.8,
                cx: 0.45,
                cy: 0.55,
                dx: 0.02,
                dy: -0.03,
                sx: 1.02,
                sy: 0.98,
            })
            .collect();
        let uv: Vec<[f32; 2]> = vars
            .iter()
            .enumerate()
            .map(|(i, v)| lactea_render::warp_mesh::warp_vertex_uv(&g, i, v, &anim, [0.0; 2]))
            .collect();
        let mut f = quiet_input();
        f.post.decay = 0.9;
        f.warp_uv = Some(uv);
        if gpu_vs {
            f.warp_gpu = Some(lactea_render::WarpGpu { vars, anim });
        }
        render(&mut r, &device, &queue, &f, &view);
        r.read_stage(Stage::Warped).unwrap().2
    };
    for shader in [false, true] {
        let cpu = run(false, shader);
        let gpu = run(true, shader);
        let d = max_diff(&cpu, &gpu);
        assert!(
            d <= 3,
            "shader={shader}: GPU vertex stage vs CPU UVs differ by {d} (orientation / triangulation bug?)"
        );
        // the transform really moved the image
        assert!(
            max_diff(&gpu, &img) > 40,
            "shader={shader}: warp had no effect"
        );
    }
}
