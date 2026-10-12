//! The frozen mix of a `.milk2` against MD3 3.37 measured under Wine: a red / blue flat-colour pair (PRESET1 red, PRESET2
//! blue, `blending_progress = 0.5`), the red channel being `1 - weight of PRESET2`. The expected contours are the ones
//! measured on the real program at 800x450 and 800x800 (`cercle` radius scales with the window *width*, a circle in
//! screen pixels rather than a 16:9 ellipse; `side` is a vertical wipe). Skipped (with a note) without a GPU adapter.

use lactea::{Compat, Engine, EngineConfig, FrameClock, Stage, Transition};

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

fn half(b: f32, r: f32) -> String {
    format!(
        "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=0\nPSVERSION_COMP=2\nfDecay=1\nfWaveAlpha=0\nfGammaAdj=1\n\
         comp_1=`shader_body\ncomp_2=`{{\ncomp_3=`  ret = float3({r}, 0, {b});\ncomp_4=`}}\n"
    )
}

fn pair(header: &str) -> String {
    format!(
        "Created with test\nwww.example.org\n\n{header}\n[PRESET1_BEGIN]\nNAME=red\n{}[PRESET1_END]\n\n[PRESET2_BEGIN]\nNAME=blue\n{}[PRESET2_END]\n",
        half(0.0, 1.0),
        half(1.0, 0.0)
    )
}

/// The red channel (0..1) of the centre row after a few frames of a hard-loaded `.milk2` at `w` x `h` (mesh 48x36).
fn centre_row(w: u32, h: u32, header: &str) -> Option<Vec<f32>> {
    let (device, queue) = gpu()?;
    let mut e = Engine::new(
        &device,
        &queue,
        EngineConfig {
            width: w,
            height: h,
            mesh: [48, 36],
            compat: Compat::MilkDrop3,
            start_clean: true,
            ..Default::default()
        },
    );
    e.load_preset(pair(header).as_bytes(), "pair.milk2", Transition::Cut)
        .unwrap();
    let view = device
        .create_texture(&wgpu::TextureDescriptor {
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
        })
        .create_view(&Default::default());
    for frame in 0..4 {
        let mut enc = device.create_command_encoder(&Default::default());
        e.render(
            &mut enc,
            &view,
            FrameClock {
                time: f64::from(frame) / 60.0,
                frame,
            },
        );
        queue.submit(Some(enc.finish()));
    }
    let (rw, rh, rgba) = e.renderer().read_stage(Stage::Final).expect("read back");
    let y = rh / 2;
    Some(
        (0..rw)
            .map(|x| f32::from(rgba[((y * rw + x) * 4) as usize]) / 255.0)
            .collect(),
    )
}

/// Positions (interpolated, in pixels) where `row` crosses `level`.
fn crossings(row: &[f32], level: f32) -> Vec<f32> {
    row.windows(2)
        .enumerate()
        .filter(|(_, p)| (p[0] - level) * (p[1] - level) < 0.0)
        .map(|(i, p)| i as f32 + (level - p[0]) / (p[1] - p[0]))
        .collect()
}

fn cercle(direction: i32) -> String {
    format!(
        "blending_pattern=cercle\nblending_progress=0.5\nblending_direction={direction}\nrandom_1=0.3\nrandom_2=0.4\nrandom_3=0.5\nrandom_4=0.5\nrandom_5=0.5\n"
    )
}

/// `cercle`, direction 1: red disc inside, blue outside. The half width of the contour where the weight of PRESET2
/// is 0.05 / 0.5 / 0.95 is about 213 / 278 / 347 px at both 800x450 and 800x800 (MD3: 213 / 278 / 347). Direction -1
/// is the exact inverse.
#[test]
fn cercle_is_a_circle_in_pixels_whose_radius_follows_the_width() {
    for (w, h) in [(800, 450), (800, 800)] {
        let Some(row) = centre_row(w, h, &cercle(1)) else {
            eprintln!("SKIP: no wgpu adapter");
            return;
        };
        // weight of PRESET2 = 1 - red
        let weight: Vec<f32> = row.iter().map(|r| 1.0 - r).collect();
        for (level, want) in [(0.05, 213.0), (0.5, 278.0), (0.95, 347.0)] {
            let c = crossings(&weight, level);
            assert_eq!(c.len(), 2, "{w}x{h} level {level}: {c:?}");
            let half_width = (c[1] - c[0]) / 2.0;
            assert!(
                (half_width - want).abs() < 6.0,
                "{w}x{h} level {level}: half width {half_width}, MD3 {want}"
            );
        }
        // the inverse for direction -1: the same contours, the weight mirrored
        let inv = centre_row(w, h, &cercle(-1)).unwrap();
        let worst = row
            .iter()
            .zip(&inv)
            .map(|(a, b)| (a + b - 1.0).abs())
            .fold(0.0, f32::max);
        assert!(
            worst < 0.03,
            "{w}x{h}: direction -1 is the inverse ({worst})"
        );
    }
}

/// `side` (angle 0, band 0.2): the weight of PRESET2 is 0.05 / 0.5 / 0.95 at x = 293 / 395 / 501 of 800 (MD3).
#[test]
fn side_is_a_vertical_wipe_centred_on_the_screen() {
    let header = "blending_pattern=side\nblending_progress=0.5\nblending_direction=1\nrandom_1=0.0\nrandom_2=0.5\nrandom_3=0.5\nrandom_4=0.5\nrandom_5=0.5\n";
    let Some(row) = centre_row(800, 450, header) else {
        eprintln!("SKIP: no wgpu adapter");
        return;
    };
    let weight: Vec<f32> = row.iter().map(|r| 1.0 - r).collect();
    for (level, want) in [(0.05, 293.0), (0.5, 395.0), (0.95, 501.0)] {
        let c = crossings(&weight, level);
        assert_eq!(c.len(), 1, "level {level}: {c:?}");
        assert!(
            (c[0] - want).abs() < 6.0,
            "level {level}: x = {}, MD3 {want}",
            c[0]
        );
    }
}
