//! How preset code lines are joined before they are compiled (`CompatRules::md2_code_join`), through the engine.
//! Skipped (with a note) when there is no GPU adapter.

use lactea::{Compat, Engine, EngineConfig, Transition};

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// An identifier split over two code lines in every EEL part the engine compiles.
const SPLIT: &str = "MILKDROP_PRESET_VERSION=201\n\
    per_frame_init_1=my_\n\
    per_frame_init_2=var=1;\n\
    per_frame_1=q1=my_\n\
    per_frame_2=var;\n\
    per_pixel_1=zoom=my_\n\
    per_pixel_2=var;\n\
    wavecode_0_enabled=1\n\
    wave_0_init1=my_\n\
    wave_0_init2=var=1;\n\
    wave_0_per_frame1=my_\n\
    wave_0_per_frame2=var=2;\n\
    wave_0_per_point1=r=my_\n\
    wave_0_per_point2=var;\n\
    shapecode_0_enabled=1\n\
    shape_0_init1=my_\n\
    shape_0_init2=var=1;\n\
    shape_0_per_frame1=r=my_\n\
    shape_0_per_frame2=var;\n";

fn load(compat: Compat) -> Option<Result<(), String>> {
    let (device, queue) = gpu()?;
    let mut engine = Engine::new(
        &device,
        &queue,
        EngineConfig {
            width: 64,
            height: 48,
            compat,
            ..EngineConfig::default()
        },
    );
    Some(
        engine
            .load_preset(SPLIT.as_bytes(), "split", Transition::Cut)
            .map(|_| ())
            .map_err(|e| e.to_string()),
    )
}

#[test]
fn split_identifier_loads_under_milkdrop_2_and_3() {
    for compat in [Compat::MilkDrop2, Compat::MilkDrop3] {
        match load(compat) {
            None => return eprintln!("skipping: no wgpu adapter"),
            Some(r) => assert_eq!(r, Ok(()), "{compat:?}"),
        }
    }
}

#[test]
fn split_identifier_still_fails_under_projectm() {
    match load(Compat::ProjectM) {
        None => eprintln!("skipping: no wgpu adapter"),
        Some(r) => assert!(r.is_err(), "projectM joins lines with a newline"),
    }
}
