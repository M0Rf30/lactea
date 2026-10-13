//! `.wave` / `.shape` import and export through the engine. Skipped (with a note) when there is
//! no GPU adapter.

use lactea::{Compat, Engine, EngineConfig, EngineError, FrameClock, Transition};
use lactea_preset::ParseError;
use lactea_testutil::{gpu, read_rgba8};

const W: u32 = 64;
const H: u32 = 48;

/// No motion, no decay, no main wave; slot 4 holds a disabled wave with `a = 0.7`.
const PRESET: &str = "MILKDROP_PRESET_VERSION=100\nfDecay=1\nfWaveAlpha=0\n\
wavecode_4_enabled=0\nwavecode_4_samples=64\nwavecode_4_a=0.7\n\
wave_4_per_point1=x=0.1;y=0.1;\n";

/// A horizontal line across the middle, white, no `a` key.
const WAVE: &str = "wavecode_0_enabled=1\nwavecode_0_samples=64\nwavecode_0_bDrawThick=1\n\
wavecode_0_r=1\nwavecode_0_g=1\nwavecode_0_b=1\n\
wave_0_per_point1=x=sample;\nwave_0_per_point2=y=0.5;\n";

const SHAPE: &str = "shapecode_0_enabled=1\nshapecode_0_sides=4\nshapecode_0_x=0.5\n\
shapecode_0_y=0.5\nshapecode_0_rad=0.4\nshapecode_0_r=1\nshapecode_0_g=1\nshapecode_0_b=1\n\
shapecode_0_a=1\nshapecode_0_r2=1\nshapecode_0_g2=1\nshapecode_0_b2=1\nshapecode_0_a2=1\n\
shape_0_per_frame1=ang=0.7;\n";

struct Rig {
    device: wgpu::Device,
    queue: wgpu::Queue,
    engine: Engine,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    frame: u32,
}

impl Rig {
    fn new() -> Option<Rig> {
        let (device, queue) = gpu()?;
        let engine = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: W,
                height: H,
                mesh: [16, 12],
                start_clean: true,
                // projectM has 4 custom slots; slots 4 and 8 exist in the MilkDrop family
                compat: Compat::MilkDrop2,
                ..Default::default()
            },
        )
        .unwrap();
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
        Some(Rig {
            device,
            queue,
            engine,
            target,
            view,
            frame: 0,
        })
    }

    fn step(&mut self) -> Vec<u8> {
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.engine.render(
            &mut enc,
            &self.view,
            FrameClock {
                time: f64::from(self.frame) / 60.0,
                frame: self.frame,
            },
        );
        self.frame += 1;
        self.queue.submit(Some(enc.finish()));
        self.read()
    }

    fn read(&self) -> Vec<u8> {
        read_rgba8(&self.device, &self.queue, &self.target, W, H)
    }
}

/// Brightest channel value in the window around the frame centre.
fn centre_max(img: &[u8]) -> u8 {
    let mut m = 0;
    for y in H / 2 - 3..H / 2 + 3 {
        for x in W / 2 - 3..W / 2 + 3 {
            let i = ((y * W + x) * 4) as usize;
            m = m.max(img[i]).max(img[i + 1]).max(img[i + 2]);
        }
    }
    m
}

#[test]
fn no_preset_is_an_error() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    assert!(rig.engine.export_custom_wave(0).is_none());
    assert!(matches!(
        rig.engine.import_custom_wave(0, WAVE.as_bytes()),
        Err(EngineError::NoPreset)
    ));
    assert!(matches!(
        rig.engine.import_custom_shape(0, SHAPE.as_bytes()),
        Err(EngineError::NoPreset)
    ));
}

#[test]
fn wave_import_enables_and_draws_the_slot() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    rig.engine
        .load_preset(PRESET.as_bytes(), "p", Transition::Cut)
        .unwrap();
    for _ in 0..3 {
        assert_eq!(centre_max(&rig.step()), 0, "disabled wave draws nothing");
    }
    let before = rig.engine.export_custom_wave(4).unwrap();
    assert!(before.starts_with("wavecode_0_enabled=0\n"), "{before}");

    let report = rig.engine.import_custom_wave(4, WAVE.as_bytes()).unwrap();
    assert_eq!(report.name, "p");

    let exported = rig.engine.export_custom_wave(4).unwrap();
    assert!(exported.starts_with("wavecode_0_enabled=1\nwavecode_0_samples=64\n"));
    // the file had no `a`: the slot keeps its 0.7
    assert!(exported.contains("wavecode_0_a=0.700\n"), "{exported}");
    // the old per-point code is replaced
    assert!(!exported.contains("0.1"), "{exported}");
    // other slots untouched
    assert!(
        rig.engine
            .export_custom_wave(3)
            .unwrap()
            .starts_with("wavecode_0_enabled=0\n")
    );

    let img = rig.step();
    assert!(centre_max(&img) > 100, "imported wave runs and draws");

    // the text the engine exports imports again, into another slot
    rig.engine
        .import_custom_wave(8, exported.as_bytes())
        .unwrap();
    assert_eq!(rig.engine.export_custom_wave(8).unwrap(), exported);
    assert!(rig.engine.export_custom_wave(16).is_none());
}

#[test]
fn shape_import_enables_and_draws_the_slot() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    rig.engine
        .load_preset(PRESET.as_bytes(), "p", Transition::Cut)
        .unwrap();
    assert_eq!(centre_max(&rig.step()), 0);
    rig.engine.import_custom_shape(8, SHAPE.as_bytes()).unwrap();
    let t = rig.engine.export_custom_shape(8).unwrap();
    assert!(
        t.starts_with("shapecode_0_enabled=1\nshapecode_0_sides=4\n"),
        "{t}"
    );
    assert!(t.contains("shape_0_per_frame1=ang=0.7;\n"), "{t}");
    assert!(centre_max(&rig.step()) > 100, "imported shape draws");
}

#[test]
fn failed_imports_leave_the_preset_alone() {
    let Some(mut rig) = Rig::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    rig.engine
        .load_preset(PRESET.as_bytes(), "p", Transition::Cut)
        .unwrap();
    let before = rig.engine.export_custom_wave(4).unwrap();
    assert!(matches!(
        rig.engine.import_custom_wave(16, WAVE.as_bytes()),
        Err(EngineError::Parse(ParseError::SlotOutOfRange {
            slot: 16,
            ..
        }))
    ));
    assert!(matches!(
        rig.engine.import_custom_wave(4, b""),
        Err(EngineError::Parse(ParseError::Empty))
    ));
    // enabled wave whose code does not compile
    let bad = "wavecode_0_enabled=1\nwave_0_per_point1=x=(;\n";
    assert!(rig.engine.import_custom_wave(4, bad.as_bytes()).is_err());
    assert_eq!(rig.engine.export_custom_wave(4).unwrap(), before);
    assert_eq!(centre_max(&rig.step()), 0);
}
