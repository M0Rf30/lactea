//! Expression engine selection and compile-failure handling of `Compat::MilkDrop3` through the engine
//! (`CompatRules::md3_ns_eel2`, `md3_drop_failed_code`). Skipped (with a note) when there is no GPU adapter.

use lactea::{Compat, Engine, EngineConfig, PresetReport, Transition};
use lactea_testutil::gpu;

fn load(compat: Compat, preset: &str) -> Option<Result<PresetReport, String>> {
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
    )
    .unwrap();
    Some(
        engine
            .load_preset(preset.as_bytes(), "t", Transition::Cut)
            .map_err(|e| e.to_string()),
    )
}

/// Every EEL part has a `c ? x` ternary without a false branch: ns-eel2 syntax that projectm-eval rejects.
const TERNARY: &str = "MILKDROP_PRESET_VERSION=201\n\
    per_frame_init_1=my_val=1;\n\
    per_frame_1=q1=my_val?5;\n\
    per_pixel_1=zoom=1+(my_val?0);\n\
    wavecode_0_enabled=1\n\
    wave_0_init1=my_t=1;\n\
    wave_0_per_frame1=my_t=my_t?2;\n\
    wave_0_per_point1=r=my_t?1;\n\
    shapecode_0_enabled=1\n\
    shape_0_per_frame1=r=1?1;\n";

#[test]
fn ns_eel2_syntax_loads_under_milkdrop3_without_warnings() {
    match load(Compat::MilkDrop3, TERNARY) {
        None => eprintln!("skipping: no wgpu adapter"),
        Some(r) => {
            let r = r.expect("MD3 accepts ns-eel2 syntax");
            assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        }
    }
}

#[test]
fn ns_eel2_syntax_is_rejected_by_the_other_modes() {
    for compat in [Compat::ProjectM, Compat::MilkDrop2] {
        match load(compat, TERNARY) {
            None => return eprintln!("skipping: no wgpu adapter"),
            Some(r) => assert!(r.is_err(), "{compat:?}"),
        }
    }
}

/// MD3 3.37 has no '.' in identifiers and no exponent literals (measured): the part is dropped.
#[test]
fn mouse_dot_x_and_exponents_drop_the_part_under_milkdrop3() {
    let src = "MILKDROP_PRESET_VERSION=201\n\
        per_frame_1=q1=mouse.x*2;\n\
        per_pixel_1=zoom=1+1e-3;\n";
    match load(Compat::MilkDrop3, src) {
        None => eprintln!("skipping: no wgpu adapter"),
        Some(r) => {
            let r = r.expect("the preset still loads");
            assert!(
                r.warnings
                    .iter()
                    .any(|m| m.starts_with("per-frame code dropped"))
            );
            assert!(
                r.warnings
                    .iter()
                    .any(|m| m.starts_with("per-pixel code dropped"))
            );
        }
    }
}

/// A broken part of each kind next to good ones.
const BROKEN: &str = "MILKDROP_PRESET_VERSION=201\n\
    per_frame_init_1=q1=(;\n\
    per_frame_1=q2=(;\n\
    per_pixel_1=zoom=(;\n\
    wavecode_0_enabled=1\n\
    wave_0_init1=q3=(;\n\
    wave_0_per_frame1=r=(;\n\
    wave_0_per_point1=g=(;\n\
    shapecode_0_enabled=1\n\
    shape_0_init1=t1=(;\n\
    shape_0_per_frame1=r=(;\n";

#[test]
fn failing_parts_are_dropped_under_milkdrop3() {
    match load(Compat::MilkDrop3, BROKEN) {
        None => eprintln!("skipping: no wgpu adapter"),
        Some(r) => {
            let r = r.expect("the preset still loads");
            let want = [
                "per-frame init code",
                "per-frame code",
                "per-pixel code",
                "custom wave 0 init code",
                "custom wave 0 per-frame code",
                "custom wave 0 per-point code",
                "custom shape 0 init code",
                "custom shape 0 per-frame code",
            ];
            for w in want {
                assert!(
                    r.warnings
                        .iter()
                        .any(|m| m.starts_with(&format!("{w} dropped"))),
                    "{w}: {:?}",
                    r.warnings
                );
            }
        }
    }
}

#[test]
fn failing_parts_reject_the_preset_under_the_other_modes() {
    for compat in [Compat::ProjectM, Compat::MilkDrop2] {
        match load(compat, BROKEN) {
            None => return eprintln!("skipping: no wgpu adapter"),
            Some(r) => assert!(r.is_err(), "{compat:?}"),
        }
    }
}
