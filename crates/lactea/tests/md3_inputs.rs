//! MilkDrop 3 shader inputs through the engine (`Compat::MilkDrop3`): `q33..q64`, the mouse, `get_fft`, the shader
//! `time`, and that the other compat modes do not see any of it. Skipped (with a note) when there is no GPU adapter.

use lactea::{Compat, Engine, EngineConfig, FrameClock, MouseState, Transition};

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    engine: Engine,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
    frame: u32,
}

impl Rig {
    fn new(compat: Compat, w: u32, h: u32) -> Option<Rig> {
        let (device, queue) = lactea_testutil::gpu()?;
        let engine = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: w,
                height: h,
                mesh: [16, 12],
                compat,
                ..EngineConfig::default()
            },
        )
        .unwrap();
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
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
        Some(Rig {
            device,
            queue,
            engine,
            target,
            view,
            w,
            h,
            frame: 0,
        })
    }

    fn load(&mut self, text: &str) -> lactea::PresetReport {
        self.engine
            .load_preset(text.as_bytes(), "t", Transition::Cut)
            .expect("preset loads")
    }

    /// Render the next frame at `time` seconds.
    fn render(&mut self, time: f64) {
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.engine.render(
            &mut enc,
            &self.view,
            FrameClock {
                time,
                frame: self.frame,
            },
        );
        self.queue.submit(Some(enc.finish()));
        self.frame += 1;
    }

    /// RGBA of the whole target.
    fn pixels(&self) -> Vec<u8> {
        lactea_testutil::read_rgba8(&self.device, &self.queue, &self.target, self.w, self.h)
    }

    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let p = self.pixels();
        let i = ((y * self.w + x) * 4) as usize;
        [p[i], p[i + 1], p[i + 2], p[i + 3]]
    }
}

/// A version-2 preset: no warp shader, a user composite shader whose body is `body`, `extra` lines (per-frame code
/// ...) in the `[preset00]` section.
fn preset(body: &str, extra: &str) -> String {
    let mut s = String::from(
        "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=0\nPSVERSION_COMP=2\n[preset00]\nfDecay=1\nfWaveAlpha=0\n",
    );
    s.push_str(extra);
    s.push_str("comp_1=`shader_body\ncomp_2=`{\n");
    for (i, l) in body.lines().enumerate() {
        s.push_str(&format!("comp_{}=`{l}\n", i + 3));
    }
    s.push_str(&format!("comp_{}=`}}\n", body.lines().count() + 3));
    s
}

fn near(got: u8, want: f32) -> bool {
    (f32::from(got) - want * 255.0).abs() <= 1.5
}

macro_rules! rig {
    ($compat:expr, $w:expr, $h:expr) => {
        match Rig::new($compat, $w, $h) {
            Some(r) => r,
            None => return eprintln!("skipping: no wgpu adapter"),
        }
    };
}

#[test]
fn q33_to_q64_reach_the_shader_and_reset_every_frame() {
    let mut r = rig!(Compat::MilkDrop3, 32, 24);
    // q33 comes from the init code, q64 from the per-frame code; q41 accumulates in the per-frame code but, like
    // every q, is reset to its post-init value (0) at the start of each frame
    let report = r.load(&preset(
        "ret = float3(q33, q64, q41);",
        "per_frame_init_1=q33=0.5;\nper_frame_1=q64=0.25; q41=q41+0.125;\n",
    ));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    for f in 0..4 {
        r.render(f64::from(f) / 60.0);
    }
    let p = r.pixel(16, 12);
    assert!(near(p[0], 0.5), "q33 {p:?}");
    assert!(near(p[1], 0.25), "q64 {p:?}");
    assert!(
        near(p[2], 0.125),
        "q41 shows only this frame's increment {p:?}"
    );
}

#[test]
fn q40_set_in_per_frame_code_reaches_the_comp_shader() {
    let mut r = rig!(Compat::MilkDrop3, 32, 24);
    r.load(&preset(
        "ret = float3(q40, 0, 0);",
        "per_frame_1=q40=0.75;\n",
    ));
    r.render(0.0);
    r.render(1.0 / 60.0);
    assert!(near(r.pixel(8, 8)[0], 0.75));
}

#[test]
fn custom_waves_and_shapes_get_q33_to_q64_and_t9_to_t16() {
    let mut r = rig!(Compat::MilkDrop3, 32, 24);
    // the shape fills the screen with a colour made of q50 (preset per-frame) and t12 (its own init code)
    let text = format!(
        "{}shapecode_0_enabled=1\nshapecode_0_sides=4\nshapecode_0_rad=2\nshapecode_0_a=1\nshapecode_0_a2=1\n\
         shapecode_0_r=0\nshapecode_0_g=0\nshapecode_0_b=0\nshapecode_0_r2=0\nshapecode_0_g2=0\nshapecode_0_b2=0\n\
         shape_0_init1=t12=0.5;\nshape_0_per_frame1=r=q50; g=t12; r2=q50; g2=t12;\n",
        preset(
            "ret = tex2D(sampler_main, uv).xyz;",
            "per_frame_1=q50=0.25;\n"
        )
    );
    r.load(&text);
    r.render(0.0);
    r.render(1.0 / 60.0);
    let p = r.pixel(16, 12);
    assert!(near(p[0], 0.25), "{p:?}");
    assert!(near(p[1], 0.5), "{p:?}");
}

#[test]
fn mouse_reaches_the_shader_in_the_shader_convention() {
    let mut r = rig!(Compat::MilkDrop3, 32, 24);
    r.load(&preset("ret = float3(mouse.x, mouse.y, mouse.z);", ""));
    // no mouse input yet: the cursor is outside the window, `(-1, -1)` saturates to 0
    r.render(0.0);
    assert_eq!(&r.pixel(4, 4)[..3], &[0, 0, 0]);
    r.engine.set_mouse(MouseState {
        position: Some([0.25, 0.75]),
        left_down: true,
    });
    r.render(1.0 / 60.0);
    let p = r.pixel(4, 4);
    assert!(
        near(p[0], 0.25) && near(p[1], 0.75) && near(p[2], 1.0),
        "{p:?}"
    );
    // released: z and w are one-frame events
    r.engine.set_mouse(MouseState {
        position: Some([0.25, 0.75]),
        left_down: false,
    });
    let mut r2 = r;
    r2.load(&preset("ret = float3(mouse_z, mouse_w, mouse_pos.y);", ""));
    r2.render(2.0 / 60.0);
    let p = r2.pixel(4, 4);
    // the new preset's first frame sees the button up now and down at the previous sample
    assert!(
        near(p[0], 0.0) && near(p[1], 1.0) && near(p[2], 0.75),
        "{p:?}"
    );
    r2.render(3.0 / 60.0);
    let p = r2.pixel(4, 4);
    assert!(near(p[1], 0.0), "mouse.w is one frame wide {p:?}");
}

#[test]
fn shader_time_counts_from_the_start_of_the_preset_under_md3_only() {
    for (compat, want) in [(Compat::MilkDrop3, 0.25), (Compat::MilkDrop2, 1.0)] {
        let mut r = rig!(compat, 16, 12);
        let text = preset("ret = float3(time * 0.1, 0, 0);", "");
        r.render(100.0); // the engine's clock is at 100 s when the preset is loaded
        r.load(&text);
        r.render(102.5);
        let p = r.pixel(8, 6);
        assert!(near(p[0], want), "{compat:?}: {p:?}");
    }
}

#[test]
fn get_fft_lights_the_bin_region_of_a_tone() {
    // a tone at fs/4 (576 samples of 0, 64, 0, -64 ... after the 8-bit quantiser) sits in bin 256, so the 512-wide
    // image has its bar at x = 256 (the pixel centre (x + 0.5) / 512 samples texel 256 exactly)
    let mut r = rig!(Compat::MilkDrop3, 512, 8);
    let report = r.load(&preset("ret = float3(get_fft(uv.x), 0, 0);", ""));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    let tone: Vec<f32> = (0..4410)
        .map(|n| 0.5 * (std::f64::consts::TAU * f64::from(n) / 4.0).sin() as f32)
        .collect();
    for f in 0..12 {
        r.engine.push_pcm_f32(&tone[..735], 1);
        r.render(f64::from(f) / 60.0);
    }
    let p = r.pixels();
    let red = |x: u32| p[((4 * 512 + x) * 4) as usize];
    assert!(red(256) >= 200, "peak bin {}", red(256));
    for x in [20, 100, 200, 300, 400, 500] {
        assert!(red(x) < 8, "bin {x} should be dark, got {}", red(x));
    }
}

#[test]
fn get_fft_stays_dark_without_audio() {
    let mut r = rig!(Compat::MilkDrop3, 64, 8);
    r.load(&preset(
        "ret = float3(get_fft(uv.x), get_fft_hz(1000), 0);",
        "",
    ));
    for f in 0..3 {
        r.render(f64::from(f) / 60.0);
    }
    let p = r.pixel(30, 4);
    assert_eq!((p[0], p[1]), (0, 0));
}

#[test]
fn monitor2_and_monitor3_persist_like_monitor() {
    let mut r = rig!(Compat::MilkDrop3, 16, 12);
    r.load(&preset(
        "ret = float3(q5, q6, q7);",
        "per_frame_init_1=monitor=0.1; monitor2=0.2; monitor3=0.3;\nper_frame_1=monitor=monitor+0.1; monitor2=monitor2+0.1; monitor3=monitor3+0.1; q5=monitor; q6=monitor2; q7=monitor3;\n",
    ));
    for f in 0..3 {
        r.render(f64::from(f) / 60.0);
    }
    let p = r.pixel(8, 6);
    // three frames after the init values: 0.4, 0.5, 0.6
    assert!(
        near(p[0], 0.4) && near(p[1], 0.5) && near(p[2], 0.6),
        "{p:?}"
    );
}

#[test]
fn the_other_compat_modes_have_no_md3_names() {
    for compat in [Compat::ProjectM, Compat::MilkDrop2] {
        let mut r = rig!(compat, 16, 12);
        let mut bodies = vec!["ret = float3(q33, 0, 0);"];
        // BeatDrop's own `get_fft` and `mouse` (`include.fx:120, 194`) exist under MilkDrop 2
        // (`tests/md2_inputs.rs`), not under projectM
        if compat == Compat::ProjectM {
            bodies.push("ret = float3(mouse.x, 0, 0);");
            bodies.push("ret = float3(get_fft(0.5), 0, 0);");
        }
        for body in bodies {
            let report = r.load(&preset(body, ""));
            assert_eq!(report.shader_fallbacks.len(), 1, "{compat:?}: {body}");
        }
    }
}

#[test]
fn the_outgoing_state_of_a_soft_cut_keeps_its_own_q() {
    let mut r = rig!(Compat::MilkDrop3, 32, 24);
    r.engine.set_md2_blend_pattern(Some(0)); // uniform fade: `mix2 = progress`
    r.load(&preset(
        "ret = float3(q40, 0, 0);",
        "per_frame_1=q40=0.25;\n",
    ));
    for f in 0..3 {
        r.render(f64::from(f) / 60.0);
    }
    assert!(near(r.pixel(8, 8)[0], 0.25));
    let new = preset("ret = float3(q40, 0, 0);", "per_frame_1=q40=0.75;\n");
    r.engine
        .load_preset(new.as_bytes(), "b", Transition::Soft { seconds: 1.0 })
        .unwrap();
    r.render(3.0 / 60.0 + 0.05);
    // shortly after the start the old shader (q40 = 0.25, its own value) still dominates the new one's 0.75
    let red = f32::from(r.pixel(8, 8)[0]) / 255.0;
    assert!(
        (0.22..0.45).contains(&red),
        "old pass must not see the new state's q: {red}"
    );
}
