#![allow(dead_code)]
//! Shared differential runner: our WGSL vs glslang SPIR-V rendered at 64x64.
use super::gpu::*;
use super::oracle::*;
use super::*;
use lactea_hlsl::{Options, Translated, translate};
use std::collections::BTreeMap;

struct Outcome {
    label: String,
    status: Status,
}

enum Status {
    OursFailed(String),
    OracleFailed(String),
    GpuError(String),
    Diff { max: u32, at: usize, nan_like: bool },
}

pub fn run_diff(cases: Vec<Case>, tag: &str) -> Option<(usize, usize, usize)> {
    if !have_tools() {
        eprintln!("glslangValidator/spirv-opt missing, skipping");
        return None;
    }
    let Some(gpu) = Gpu::new() else {
        eprintln!("no GPU adapter, skipping");
        return None;
    };
    println!("adapter: {}", gpu.adapter_name);
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("gl-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let indexed: Vec<(usize, Case)> = cases.into_iter().enumerate().collect();
    // phase A: translate + glslang (parallel)
    let prepared = par_map(&indexed, |(i, c)| {
        let t = translate(c.kind, &c.text, &Options::default());
        match t {
            Err(e) => (Err(e.to_string()), None),
            Ok(t) => {
                let o = compile_oracle(c, &t, &dir, *i);
                (Ok(t), Some(o))
            }
        }
    });
    // phase B: render (sequential)
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut worst: Vec<(u32, usize, Vec<u8>, Vec<u8>)> = Vec::new();
    for ((i, c), (t, o)) in indexed.iter().zip(prepared) {
        let label = c.label();
        let t: Translated = match t {
            Ok(t) => t,
            Err(e) => {
                outcomes.push(Outcome {
                    label,
                    status: Status::OursFailed(e),
                });
                continue;
            }
        };
        let module = match o.unwrap() {
            Ok(m) => m,
            Err(e) => {
                if std::env::var("HLSL_DIFF_FILTER").is_ok() {
                    println!("ORACLE FAILED {label}:\n{}", e.message());
                }
                outcomes.push(Outcome {
                    label,
                    status: Status::OracleFailed(e.category()),
                });
                continue;
            }
        };
        let d = &gpu.device;
        let sv = d.push_error_scope(wgpu::ErrorFilter::Validation);
        let si = d.push_error_scope(wgpu::ErrorFilter::Internal);
        let ours_mod = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(sabotage(&t.wgsl).into()),
        });
        let ours = gpu.render(
            c.kind,
            &ours_mod,
            "fs_main",
            false,
            &t.interface,
            &t.interface.texsize_slots,
        );
        let e2 = pollster::block_on(si.pop());
        let e1 = pollster::block_on(sv.pop());
        if let Some(e) = e1.or(e2) {
            if std::env::var("HLSL_DIFF_FILTER").is_ok() {
                println!("OURS GPU ERROR {label}:\n{e}");
            }
            outcomes.push(Outcome {
                label,
                status: Status::GpuError(format!("ours: {e}")),
            });
            continue;
        }
        let sv = d.push_error_scope(wgpu::ErrorFilter::Validation);
        let si = d.push_error_scope(wgpu::ErrorFilter::Internal);
        let orc_mod = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Naga(std::borrow::Cow::Owned(module)),
        });
        let orc = gpu.render(
            c.kind,
            &orc_mod,
            "PS",
            true,
            &t.interface,
            &t.interface.texsize_slots,
        );
        let e2 = pollster::block_on(si.pop());
        let e1 = pollster::block_on(sv.pop());
        if let Some(e) = e1.or(e2) {
            let m = format!("{e}");
            if std::env::var("HLSL_DIFF_FILTER").is_ok() {
                println!("ORACLE GPU ERROR {label}:\n{m}");
            }
            let first: String = m.lines().next().unwrap_or("").chars().take(100).collect();
            outcomes.push(Outcome {
                label,
                status: Status::GpuError(format!("oracle: {first}")),
            });
            continue;
        }
        let (max, at) = max_abs_diff(&ours, &orc);
        let nan_like = ours.chunks(4).all(|p| p == &ours[..4]);
        if max > 2 {
            worst.push((max, *i, ours.clone(), orc.clone()));
        }
        outcomes.push(Outcome {
            label,
            status: Status::Diff { max, at, nan_like },
        });
    }
    // report
    let total = outcomes.len();
    let (mut ours_f, mut orc_f, mut gpu_f, mut pass, mut fail) = (0, 0, 0, 0, 0);
    let mut cats: BTreeMap<String, usize> = BTreeMap::new();
    let mut csv = String::from("label,status,maxdiff\n");
    for o in &outcomes {
        match &o.status {
            Status::OursFailed(m) => {
                ours_f += 1;
                csv.push_str(&format!(
                    "\"{}\",ours_failed:{},\n",
                    o.label,
                    m.replace('"', "'")
                ));
            }
            Status::OracleFailed(c) => {
                orc_f += 1;
                *cats.entry(c.clone()).or_default() += 1;
                csv.push_str(&format!(
                    "\"{}\",oracle_failed:{},\n",
                    o.label,
                    c.replace('"', "'")
                ));
            }
            Status::GpuError(m) => {
                gpu_f += 1;
                *cats.entry(format!("gpu: {m}")).or_default() += 1;
                csv.push_str(&format!(
                    "\"{}\",gpu_error:{},\n",
                    o.label,
                    m.replace('"', "'")
                ));
            }
            Status::Diff { max, .. } => {
                if *max <= 2 {
                    pass += 1;
                } else {
                    fail += 1;
                }
                csv.push_str(&format!("\"{}\",rendered,{}\n", o.label, max));
            }
        }
    }
    let rendered = pass + fail;
    println!("[{tag}] cases: {total}");
    println!("  ours failed to translate: {ours_f}");
    println!("  glslang/spirv/naga-spv rejected (baseline, not counted against us): {orc_f}");
    println!("  GPU pipeline errors: {gpu_f}");
    println!("  rendered on both: {rendered}");
    println!(
        "  pass (<=2/255): {pass} ({:.2}% of rendered, {:.2}% of all cases)",
        100.0 * pass as f64 / rendered.max(1) as f64,
        100.0 * pass as f64 / total.max(1) as f64
    );
    println!("  fail (>2/255): {fail}");
    let blank = outcomes
        .iter()
        .filter(|o| matches!(o.status, Status::Diff { nan_like: true, .. }))
        .count();
    println!("  constant-colour renders (weak signal): {blank}");
    let mut cv: Vec<_> = cats.iter().collect();
    cv.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    println!("  baseline-reject / gpu categories:");
    for (k, n) in cv.iter().take(25) {
        println!("    {n:4}  {k}");
    }
    worst.sort_by_key(|w| std::cmp::Reverse(w.0));
    println!("  worst mismatches:");
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    for (rank, (max, i, a, b)) in worst.iter().take(40).enumerate() {
        println!("    {max:3}  #{i:04}  {}", indexed[*i].1.label());
        if rank < 12 {
            let mut img = image::RgbaImage::new(SIZE * 2, SIZE);
            for y in 0..SIZE {
                for x in 0..SIZE {
                    let o = ((y * SIZE + x) * 4) as usize;
                    img.put_pixel(x, y, image::Rgba([a[o], a[o + 1], a[o + 2], 255]));
                    img.put_pixel(x + SIZE, y, image::Rgba([b[o], b[o + 1], b[o + 2], 255]));
                }
            }
            let _ = img.save(out.join(format!("diff-{tag}-{i:04}.png")));
        }
    }
    let _ = std::fs::write(out.join(format!("diff-{tag}.csv")), csv);
    println!("  csv: {}", out.join(format!("diff-{tag}.csv")).display());
    Some((pass, fail, total))
}

/// Mutation check of the harness itself: HLSL_SABOTAGE="from=>to" edits our WGSL before rendering.
fn sabotage(w: &str) -> String {
    match std::env::var("HLSL_SABOTAGE") {
        Ok(s) => match s.split_once("=>") {
            Some((a, b)) => w.replace(a, b),
            None => w.to_string(),
        },
        Err(_) => w.to_string(),
    }
}
