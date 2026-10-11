//! Generates a synthetic reference/candidate dump pair for demoing `fdcompare`.
//!
//! `cargo run -p lactea-compare --release --example gen_demo -- /tmp/fd-demo [frames] [WxH]`
//!
//! The candidate equals the reference for the first 40% of the frames, then slowly drifts
//! (brightness offset growing linearly + a small spatial shift), and `q1` starts diverging.

use lactea_abi::RunConfig;
use lactea_abi::dump::{DumpMeta, Stage};
use lactea_compare::DumpWriter;
use std::collections::BTreeMap;

fn render(w: u32, h: u32, f: u32, drift: f64) -> Vec<u8> {
    let t = f as f64 / 60.0;
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (u, vv) = (x as f64 / w as f64 + drift * 0.002, y as f64 / h as f64);
            let r = 0.5 + 0.5 * (6.0 * u + 3.0 * t).sin() * (4.0 * vv).cos();
            let g = 0.5 + 0.5 * (5.0 * vv - 2.0 * t).sin();
            let b = 0.5 + 0.5 * ((u - 0.5).hypot(vv - 0.5) * 20.0 - 4.0 * t).sin();
            let off = drift * 0.15;
            for c in [r, g, b] {
                v.push(((c + off).clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            v.push(255);
        }
    }
    v
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = std::path::PathBuf::from(args.get(1).map(String::as_str).unwrap_or("/tmp/fd-demo"));
    let frames: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120);
    let (w, h) = args
        .get(3)
        .and_then(|s| s.split_once('x'))
        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
        .unwrap_or((640u32, 360u32));
    let meta = |producer: &str| DumpMeta {
        producer: producer.into(),
        preset: "demo.milk".into(),
        audio: "synth128".into(),
        config: RunConfig {
            width: w,
            height: h,
            frames,
            ..RunConfig::default()
        },
        stages: vec![Stage::Final, Stage::Main],
        has_vars: true,
    };
    let rw = DumpWriter::create(root.join("ref"), meta("synthetic-ref")).unwrap();
    let cw = DumpWriter::create(root.join("cand"), meta("synthetic-cand")).unwrap();
    let start = frames * 2 / 5;
    for f in 0..frames {
        let drift = if f < start {
            0.0
        } else {
            (f - start) as f64 / frames as f64
        };
        for (stage, scale) in [(Stage::Final, 1.0), (Stage::Main, 0.3)] {
            rw.write_stage(stage, f, w, h, &render(w, h, f, 0.0))
                .unwrap();
            cw.write_stage(stage, f, w, h, &render(w, h, f, drift * scale))
                .unwrap();
        }
        let mut v = BTreeMap::new();
        v.insert("zoom".to_string(), 1.0 + 0.01 * (f as f64 / 10.0).sin());
        v.insert("q1".to_string(), f as f64 * 0.25);
        v.insert("bass".to_string(), if f % 7 == 0 { f64::NAN } else { 1.0 });
        rw.write_vars(f, &v).unwrap();
        if f >= start {
            v.insert("q1".to_string(), f as f64 * 0.25 + drift * 1e-3);
        }
        cw.write_vars(f, &v).unwrap();
    }
    println!(
        "wrote {} frames of {w}x{h} to {}/{{ref,cand}}",
        frames,
        root.display()
    );
}
