//! BeatDrop's host inputs through the engine (`Compat::MilkDrop2`): the expression variables of the mouse, the arrow
//! keys and the clock, `monitor`, the shader constants `_c14` / `_c15` / `_c16` and the shader FFT; projectM sees none
//! of it. Skipped (with a note) when there is no GPU adapter.

use lactea::{
    Compat, CtrlKeys, DateTime, Engine, EngineConfig, FrameClock, MouseState, Transition,
};

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
        let instance = wgpu::Instance::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
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

    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let bpr = (self.w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(bpr * self.h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(self.h),
                },
            },
            wgpu::Extent3d {
                width: self.w,
                height: self.h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(enc.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        rx.recv().unwrap().unwrap();
        let data = slice.get_mapped_range().unwrap();
        let i = (y * bpr + x * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    }

    fn var(&self, name: &str) -> Option<f64> {
        self.engine
            .frame_vars()
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }
}

/// A version-2 preset: no warp shader, a user composite shader whose body is `body`, `extra` lines in `[preset00]`.
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

const NOON: DateTime = DateTime {
    year: 2026,
    month: 10,
    day: 13,
    weekday: 2,
    hour: 23,
    minute: 59,
    second: 58,
    millisecond: 250,
};

#[test]
fn expressions_see_the_mouse_the_keys_the_clock_and_a_persistent_monitor() {
    let mut r = rig!(Compat::MilkDrop2, 32, 24);
    let report = r.load(&preset(
        "ret = tex2D(sampler_main, uv).xyz;",
        "per_frame_1=monitor = monitor + 1;\n",
    ));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    r.engine.set_mouse(MouseState {
        position: Some([0.25, 0.25]),
        left_down: true,
    });
    r.engine.set_ctrl_keys(CtrlKeys {
        left: true,
        up: true,
        ..CtrlKeys::default()
    });
    r.engine.set_date_time(NOON);
    r.render(0.0);

    // the cursor is 1/4 from the left and the top: BeatDrop's y is bottom-origin
    assert_eq!(r.var("mousex"), Some(0.25));
    assert_eq!(r.var("mousey"), Some(0.75));
    assert_eq!(r.var("mousedown"), Some(1.0));
    assert_eq!(r.var("mouseclick"), Some(1.0), "the press frame");
    assert_eq!(
        [
            r.var("ctrllt"),
            r.var("ctrlrt"),
            r.var("ctrldn"),
            r.var("ctrlup")
        ],
        [Some(1.0), Some(0.0), Some(0.0), Some(1.0)]
    );
    assert_eq!(r.var("year"), Some(2026.0));
    assert_eq!(r.var("weekday"), Some(2.0));
    assert_eq!(r.var("hour"), Some(23.0));
    assert_eq!(r.var("milliseconds"), Some(250.0));
    assert_eq!(r.var("totalseconds"), Some(f64::from(86_398.25f32)));
    assert_eq!(r.engine.monitor(), 1.0);

    r.render(1.0 / 60.0);
    r.render(2.0 / 60.0);
    assert_eq!(r.var("mousedown"), Some(1.0));
    assert_eq!(r.var("mouseclick"), Some(0.0), "a click lasts one frame");
    assert_eq!(r.engine.monitor(), 3.0, "monitor keeps what the code wrote");

    // without a host clock the time of day follows the frame time and the date is 0
    r.engine.clear_date_time();
    r.render(3723.5);
    assert_eq!(r.var("year"), Some(0.0));
    assert_eq!(r.var("hour"), Some(1.0));
    assert_eq!(r.var("minute"), Some(2.0));
    assert_eq!(r.var("second"), Some(3.0));
    assert_eq!(r.var("totalseconds"), Some(3723.5));
}

#[test]
fn projectm_registers_no_host_variables() {
    let mut r = rig!(Compat::ProjectM, 32, 24);
    r.load(&preset(
        "ret = tex2D(sampler_main, uv).xyz;",
        "per_frame_1=q1 = mousex + year;\n",
    ));
    r.engine.set_mouse(MouseState {
        position: Some([0.25, 0.25]),
        left_down: true,
    });
    r.engine.set_date_time(NOON);
    r.render(0.0);
    assert_eq!(r.var("mousex"), Some(0.0), "an ordinary user variable");
    assert_eq!(r.var("year"), Some(0.0));
    assert_eq!(r.var("hour"), None, "not even registered");
}

#[test]
fn shaders_read_c14_c15_and_c16() {
    let mut r = rig!(Compat::MilkDrop2, 32, 24);
    let report = r.load(&preset(
        "ret = float3(_c14.x, _c15.y / 60.0, _c16.z / 31.0);",
        "",
    ));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    r.engine.set_mouse(MouseState {
        position: Some([0.25, 0.5]),
        left_down: false,
    });
    r.engine.set_date_time(DateTime {
        minute: 30,
        day: 31,
        ..NOON
    });
    r.render(0.0);
    r.render(1.0 / 60.0);
    let p = r.pixel(16, 12);
    assert!(near(p[0], 0.25), "mouse x {p:?}");
    assert!(near(p[1], 0.5), "minute {p:?}");
    assert!(near(p[2], 1.0), "day {p:?}");
}

#[test]
fn get_fft_reads_the_beatdrop_spectrum() {
    let tone: Vec<f32> = (0..2048)
        .flat_map(|i| {
            let s = 0.5 * (std::f32::consts::FRAC_PI_2 * i as f32).sin();
            [s, s]
        })
        .collect();
    let body = "ret = float3(get_fft(0.5), get_fft_peak(0.5), 0);";

    let mut r = rig!(Compat::MilkDrop2, 32, 24);
    let report = r.load(&preset(body, ""));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    // silence: the whole texture is dark
    for f in 0..3 {
        r.render(f64::from(f) / 60.0);
    }
    assert_eq!(&r.pixel(16, 12)[..3], &[0, 0, 0]);

    // a tone at a quarter of the sample rate sits at the middle of the 1024 bins
    for f in 3..12 {
        r.engine.push_pcm_f32(&tone, 2);
        r.render(f64::from(f) / 60.0);
    }
    let p = r.pixel(16, 12);
    assert!(p[0] > 100, "spectrum row {p:?}");
    assert!(p[1] > 100, "peak row {p:?}");
}

#[test]
fn get_fft_does_not_exist_in_projectm() {
    let mut r = rig!(Compat::ProjectM, 32, 24);
    let report = r.load(&preset("ret = float3(get_fft(0.5), 0, 0);", ""));
    assert!(
        !report.shader_fallbacks.is_empty(),
        "projectM has no BeatDrop prelude"
    );
}

/// `include.fx:213-231`: `get_wave` is the mean of both rows of `sampler_wave`, `get_wave_left` / `get_wave_right`
/// read one row each; `plugin.cpp:11731-11748` fills the rows with the waveform over 128.
#[test]
fn get_wave_is_the_mono_average_of_the_two_channels() {
    // a constant signal: left 0.5, right -0.25 (so the average, 0.125, differs from either channel)
    let pcm: Vec<f32> = (0..1200).flat_map(|_| [0.5f32, -0.25]).collect();
    let mut r = rig!(Compat::MilkDrop2, 32, 24);
    let report = r.load(&preset(
        "ret = float3(get_wave_left(0.5), get_wave_right(0.5) + 0.5, get_wave(0.5) + 0.5);",
        "",
    ));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    assert!(
        !report.unresolved_textures.contains(&"wave".to_string()),
        "`sampler_wave` is the engine's own texture: {report:?}"
    );
    r.engine.push_pcm_f32(&pcm, 2);
    r.render(0.0);
    r.render(1.0 / 60.0);
    let p = r.pixel(16, 12);
    assert!(near(p[0], 0.5), "left {p:?}");
    assert!(near(p[1], 0.25), "right {p:?}");
    assert!(near(p[2], 0.625), "mono mean of the rows {p:?}");
}

/// A direct `tex2D` of `sampler_fft` is a bilinear fetch of the 1024 x 2 texture: row 0 is the spectrum `get_fft`
/// takes the square root of, row 1 the peak hold of `get_fft_peak`.
#[test]
fn direct_tex2d_of_sampler_fft_agrees_with_the_helpers() {
    let tone: Vec<f32> = (0..2048)
        .flat_map(|i| {
            let s = 0.5 * (std::f32::consts::FRAC_PI_2 * i as f32).sin();
            [s, s]
        })
        .collect();
    let mut r = rig!(Compat::MilkDrop2, 32, 24);
    let report = r.load(&preset(
        "ret = float3(get_fft(0.5), sqrt(tex2D(sampler_fft, float2(0.5, 0.25)).x), \
         sqrt(tex2D(sampler_fft, float2(0.5, 0.75)).x) - get_fft_peak(0.5) + 0.5);",
        "",
    ));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    for f in 0..8 {
        r.engine.push_pcm_f32(&tone, 2);
        r.render(f64::from(f) / 60.0);
    }
    let p = r.pixel(16, 12);
    assert!(p[0] > 100, "the tone is there {p:?}");
    assert!(p[0].abs_diff(p[1]) <= 2, "row 0 {p:?}");
    assert!(near(p[2], 0.5), "row 1 {p:?}");
}

/// `texsize_fft` / `texsize_wave` report the real texture sizes: 1024 x 2 and 480 x 2.
#[test]
fn texsize_fft_and_texsize_wave_report_the_real_sizes() {
    let mut r = rig!(Compat::MilkDrop2, 32, 24);
    let report = r.load(&preset(
        "ret = float3(texsize_fft.x / 2048.0, texsize_wave.x / 1920.0, texsize_wave.y / 8.0) \
         + float3(0, texsize_fft.w - 0.5, texsize_wave.w - 0.5);",
        "",
    ));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    r.render(0.0);
    r.render(1.0 / 60.0);
    let p = r.pixel(16, 12);
    assert!(near(p[0], 0.5), "1024 / 2048 {p:?}");
    assert!(near(p[1], 0.25), "480 / 1920, and 1 / 2 - 0.5 {p:?}");
    assert!(near(p[2], 0.25), "2 / 8, and 1 / 2 - 0.5 {p:?}");
}

/// `include.fx:120-135`: `mouse*` and `sysTime*` / `sysDate*` are macros over `_c14` / `_c15` / `_c16`.
#[test]
fn the_mouse_and_clock_macros_read_the_constants() {
    let mut r = rig!(Compat::MilkDrop2, 32, 24);
    let report = r.load(&preset(
        "ret = float3(mouse_pos.x, sysTimeMinute / 60.0, sysDateDay / 31.0) * 0.5 \
         + float3(mouse.y, sysTime.z / 120.0, sysDate.y / 24.0);",
        "",
    ));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    r.engine.set_mouse(MouseState {
        position: Some([0.5, 0.5]),
        left_down: false,
    });
    r.engine.set_date_time(DateTime {
        minute: 30,
        second: 60 - 1,
        day: 31,
        month: 12,
        ..NOON
    });
    r.render(0.0);
    r.render(1.0 / 60.0);
    let p = r.pixel(16, 12);
    // x: 0.5 * 0.5 + (top-origin) y 0.5; minute: 0.5 * 0.5 + 59 / 120; day: 1 * 0.5 + 12 / 24
    assert!(near(p[0], 0.75), "mouse {p:?}");
    assert!(near(p[1], 0.25 + 59.0 / 120.0), "time {p:?}");
    assert!(near(p[2], 1.0), "date {p:?}");
}

/// `milkdropfs.cpp:616, 838` and `state.cpp:1682-1800`: `monitor` starts from the init code's value, keeps what the
/// per-frame code wrote from frame to frame, and starts over (0, or the init code's value) when the preset is loaded
/// again.
#[test]
fn monitor_starts_from_the_init_code_persists_and_resets_on_a_new_load() {
    let mut r = rig!(Compat::MilkDrop2, 32, 24);
    let body = "ret = tex2D(sampler_main, uv).xyz;";
    let counting = preset(
        body,
        "per_frame_init_1=monitor = 5;\nper_frame_1=monitor = monitor + 1;\n",
    );
    r.load(&counting);
    assert_eq!(r.engine.monitor(), 5.0, "the init code's value");
    r.render(0.0);
    assert_eq!(
        r.engine.monitor(),
        6.0,
        "frame 0 starts from the init value"
    );
    r.render(1.0 / 60.0);
    r.render(2.0 / 60.0);
    assert_eq!(
        r.engine.monitor(),
        8.0,
        "a value written in a frame is the next frame's start"
    );

    // loading compiles again: the variable starts over from the init code, it does not carry on
    r.load(&counting);
    assert_eq!(r.engine.monitor(), 5.0);
    r.render(3.0 / 60.0);
    assert_eq!(r.engine.monitor(), 6.0);

    // another preset: its own `monitor`, reset to 0 (BeatDrop zeroes it with the other built-ins on a recompile)
    r.load(&preset(body, "per_frame_1=monitor = monitor + 0.5;\n"));
    assert_eq!(r.engine.monitor(), 0.0);
    r.render(4.0 / 60.0);
    r.render(5.0 / 60.0);
    assert_eq!(r.engine.monitor(), 1.0);
    r.load(&preset(body, ""));
    assert_eq!(r.engine.monitor(), 0.0, "never mentioned");
}

/// `state.cpp:1794`: BeatDrop loads the per-frame variables (mouse, keys, clock) before it runs the init code, so the
/// init code sees them.
#[test]
fn init_code_reads_the_host_inputs() {
    let mut r = rig!(Compat::MilkDrop2, 32, 24);
    r.engine.set_mouse(MouseState {
        position: Some([0.25, 0.25]),
        left_down: false,
    });
    r.engine.set_date_time(NOON);
    r.load(&preset(
        "ret = tex2D(sampler_main, uv).xyz;",
        "per_frame_init_1=monitor = year + hour; q1 = mousey; q2 = milliseconds;\nper_frame_1=q3 = monitor;\n",
    ));
    assert_eq!(r.engine.monitor(), 2026.0 + 23.0);
    r.render(0.0);
    assert_eq!(r.var("q1"), Some(0.75), "mousey, bottom origin");
    assert_eq!(r.var("q2"), Some(250.0));
    assert_eq!(r.var("q3"), Some(2049.0));
}

/// `include.fx:104, 142-147, 150`: `aspect_sd` is `_c1` (the aspect itself while the screen-dependent mode is off),
/// `trunc` the ceil / floor macro and `SAMPLER2D` / `texture2D` the sampler type and `tex2D`.
#[test]
fn the_remaining_include_additions_work_in_a_preset() {
    let mut r = rig!(Compat::MilkDrop2, 32, 24);
    let report = r.load(&preset(
        "ret = float3(aspect_sd.y, trunc(-1.7) * -0.5, trunc(float2(-2.9, 2.9)).y * 0.25);",
        "",
    ));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    r.render(0.0);
    r.render(1.0 / 60.0);
    let p = r.pixel(16, 12);
    assert!(near(p[0], 0.75), "aspect_sd.y of a 32 x 24 target {p:?}");
    assert!(near(p[1], 0.5), "trunc(-1.7) = -1 {p:?}");
    assert!(near(p[2], 0.5), "trunc(2.9) = 2 {p:?}");

    let report = r.load(&preset(
        "ret = texture2D(sampler_main, uv).xyz * 0 + float3(aspect_sd.x, 0.5, 0.25);",
        "",
    ));
    assert!(report.shader_fallbacks.is_empty(), "{report:?}");
    r.render(2.0 / 60.0);
    r.render(3.0 / 60.0);
    let p = r.pixel(16, 12);
    assert!(
        near(p[0], 1.0) && near(p[1], 0.5) && near(p[2], 0.25),
        "{p:?}"
    );
}
