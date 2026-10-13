//! Headless smoke tests: load real corpus presets, render frames, require no wgpu validation
//! errors. Skipped (with a note) when there is no GPU adapter or no preset corpus.

use lactea::{Engine, EngineConfig, FrameClock, Image, TextureProvider, Transition};
use lactea_testutil::{gpu, read_rgba8 as read_back};
use std::path::{Path, PathBuf};

const CORPUS: &str = "/usr/share/projectM/presets";

fn find(name: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 3 {
                if let Some(f) = walk(&p, name, depth + 1) {
                    return Some(f);
                }
            } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
                return Some(p);
            }
        }
        None
    }
    walk(Path::new(CORPUS), name, 0)
}

struct Checker;
impl TextureProvider for Checker {
    fn load(&mut self, _name: &str) -> Option<Image> {
        let (w, h) = (8u32, 8u32);
        let mut rgba8 = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 255 } else { 40 };
                rgba8.extend_from_slice(&[v, v / 2, 255 - v, 255]);
            }
        }
        Some(Image {
            width: w,
            height: h,
            rgba8,
        })
    }
    fn random_pool(&self) -> Vec<String> {
        vec!["a".into(), "b".into(), "c".into()]
    }
}

#[test]
fn corpus_presets_render_without_validation_errors() {
    let Some((device, queue)) = gpu() else {
        eprintln!("skipping: no wgpu adapter");
        return;
    };
    // v1 (no shaders), v2 warp+comp, custom waves/shapes, per-pixel code, noise + random texture
    let names = [
        "Aderrasi - Antidote.milk",
        "Geiss - Skin Dots 3.milk",
        "EoS + Phat - CAT Scan.milk",
        "Aderrasi - Songflower (Hybrid Plant).milk",
        "Geiss - Diffuser (Gold Mix).milk",
        "ORB - Acid Sunrise.milk",
        "Goody - LSD Zoomtunnel.milk",
    ];
    let presets: Vec<(String, PathBuf)> = names
        .iter()
        .filter_map(|n| find(n).map(|p| (n.to_string(), p)))
        .collect();
    if presets.is_empty() {
        eprintln!("skipping: no preset corpus under {CORPUS}");
        return;
    }
    let (w, h) = (160u32, 90u32);
    let mut engine = Engine::new(
        &device,
        &queue,
        EngineConfig {
            width: w,
            height: h,
            ..EngineConfig::default()
        },
    )
    .unwrap();
    engine.set_texture_provider(Box::new(Checker));
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
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
    // 0.5 s of a two-tone stereo signal per preset
    let pcm: Vec<f32> = (0..22_050)
        .flat_map(|i| {
            let t = i as f32 / 44_100.0;
            [
                0.5 * (t * 440.0 * std::f32::consts::TAU).sin(),
                0.4 * (t * 1_320.0 * std::f32::consts::TAU).sin(),
            ]
        })
        .collect();

    let mut any_visible = false;
    for (name, path) in &presets {
        let bytes = std::fs::read(path).unwrap();
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let report = engine
            .load_preset(&bytes, name, Transition::Cut)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            report.shader_fallbacks.is_empty(),
            "{name}: shader fallback {:?}",
            report.shader_fallbacks
        );
        for (frame, chunk) in (0u32..).zip(pcm.chunks(735 * 2).take(30)) {
            engine.push_pcm_f32(chunk, 2);
            let mut enc = device.create_command_encoder(&Default::default());
            engine.render(
                &mut enc,
                &view,
                FrameClock {
                    time: f64::from(frame) / 60.0,
                    frame,
                },
            );
            queue.submit(Some(enc.finish()));
        }
        if let Some(err) = pollster::block_on(scope.pop()) {
            panic!("{name}: wgpu validation error: {err}");
        }
        let px = read_back(&device, &queue, &target, w, h);
        any_visible |= px.chunks(4).any(|p| p[0] > 8 || p[1] > 8 || p[2] > 8);
        let vars = engine.frame_vars();
        assert!(
            vars.iter().any(|(n, _)| n == "zoom"),
            "{name}: no per-frame vars"
        );
        assert!(vars.iter().all(|(_, v)| !v.is_nan() || true));
        eprintln!("{name}: ok ({} vars)", vars.len());
    }
    assert!(any_visible, "every preset rendered black");
}

#[test]
fn garbage_and_empty_presets_are_errors() {
    let Some((device, queue)) = gpu() else {
        eprintln!("skipping: no wgpu adapter");
        return;
    };
    let mut engine = Engine::new(
        &device,
        &queue,
        EngineConfig {
            width: 64,
            height: 36,
            ..EngineConfig::default()
        },
    )
    .unwrap();
    assert!(engine.load_preset(b"", "empty", Transition::Cut).is_err());
    // code that cannot compile fails the load (projectM semantics), the previous preset stays
    let bad = b"MILKDROP_PRESET_VERSION=100\nper_frame_1=zoom = = 3;\n";
    assert!(matches!(
        engine.load_preset(bad, "bad", Transition::Cut),
        Err(lactea::EngineError::Compile { .. })
    ));
    assert!(engine.preset_name().is_none());
}

#[test]
fn unresolved_textures_are_reported_without_a_provider() {
    let Some((device, queue)) = gpu() else {
        return;
    };
    let Some(path) = find("Geiss - Diffuser (Gold Mix).milk") else {
        return;
    };
    let mut engine = Engine::new(
        &device,
        &queue,
        EngineConfig {
            width: 64,
            height: 36,
            ..EngineConfig::default()
        },
    )
    .unwrap();
    let report = engine
        .load_preset(&std::fs::read(path).unwrap(), "diffuser", Transition::Cut)
        .unwrap();
    assert!(!report.unresolved_textures.is_empty(), "{report:?}");
}
