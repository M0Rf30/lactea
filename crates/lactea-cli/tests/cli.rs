//! End-to-end test of `fdrender`: free run, determinism, and teacher forcing from its own dump
//! (a teacher run fed with a free run's `main`/`vars` must reproduce that run exactly).
//! Skipped when there is no GPU adapter.

use std::path::{Path, PathBuf};
use std::process::Command;

const PRESET: &str = "MILKDROP_PRESET_VERSION=100\n\
fDecay=0.95\nfWaveAlpha=0.9\nnWaveMode=0\nzoom=1.02\nrot=0.01\nwarp=0.5\nfGammaAdj=1.5\n\
ob_a=0.5\nob_r=1\nob_size=0.02\nbDarkenCenter=1\n\
per_frame_1=zoom = 1.0 + 0.02*sin(time*3) + 0.01*bass;\n\
per_frame_2=q1 = q1 + 0.1; wave_r = 0.5+0.5*sin(time);\n\
per_pixel_1=rot = rot + 0.02*rad*sin(ang*3+time);\n\
wavecode_0_enabled=1\nwavecode_0_samples=64\nwavecode_0_r=1\nwavecode_0_g=0.5\nwavecode_0_b=0\nwavecode_0_a=1\n\
wave_0_per_point1=y = y + 0.1*sin(sample*6.28 + time);\n\
shapecode_0_enabled=1\nshapecode_0_sides=5\nshapecode_0_rad=0.15\nshapecode_0_a=0.8\nshapecode_0_border_a=1\n\
shape_0_per_frame1=ang = time;\n";

fn wav(path: &Path) {
    let n = 44_100 / 4;
    let mut data = Vec::new();
    for i in 0..n {
        let t = i as f32 / 44_100.0;
        let s = 0.6 * (t * 220.0 * std::f32::consts::TAU).sin()
            + 0.2 * (t * 1_900.0 * std::f32::consts::TAU).sin();
        data.extend_from_slice(&s.to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // float
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&44_100u32.to_le_bytes());
    out.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out).unwrap();
}

fn run(args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_fdrender"))
        .args(args)
        .output()
        .expect("spawn fdrender");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fdrender-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An odd mesh used to panic in `MeshGeometry::new`; it is now a usage error (exit 1, clear message).
#[test]
fn invalid_mesh_and_size_are_usage_errors_not_panics() {
    let dir = tmp("badargs");
    let preset = dir.join("p.milk");
    let audio = dir.join("a.wav");
    std::fs::write(&preset, PRESET).unwrap();
    wav(&audio);
    let (p, a, o) = (
        preset.to_str().unwrap(),
        audio.to_str().unwrap(),
        dir.join("o"),
    );
    let o = o.to_str().unwrap();
    for (flag, val) in [("--mesh", "20x15"), ("--mesh", "0x0"), ("--size", "0x10")] {
        let (code, err) = run(&[
            "--preset", p, "--audio", a, "--out", o, "--frames", "1", flag, val,
        ]);
        assert_eq!(code, 1, "{flag} {val}: {err}");
        assert!(
            err.contains(flag) && !err.contains("panicked"),
            "{flag} {val}: {err}"
        );
    }
}

#[test]
fn free_run_is_deterministic_and_teacher_reproduces_it() {
    let dir = tmp("e2e");
    let preset = dir.join("p.milk");
    let audio = dir.join("a.wav");
    std::fs::write(&preset, PRESET).unwrap();
    wav(&audio);
    let (p, a) = (preset.to_str().unwrap(), audio.to_str().unwrap());
    let common = |out: &Path| -> Vec<String> {
        [
            "--preset",
            p,
            "--audio",
            a,
            "--size",
            "96x54",
            "--frames",
            "6",
            "--stages",
            "all",
            "--vars",
            "--textures",
            "--out",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain(std::iter::once(out.to_str().unwrap().to_string()))
        .collect()
    };
    let (o1, o2, ot) = (dir.join("run1"), dir.join("run2"), dir.join("teach"));
    let args1 = common(&o1);
    let (code, err) = run(&args1.iter().map(String::as_str).collect::<Vec<_>>());
    if code == 1 && err.contains("no wgpu adapter") {
        eprintln!("skipping: no GPU");
        return;
    }
    assert_eq!(code, 0, "{err}");
    let args2 = common(&o2);
    assert_eq!(
        run(&args2.iter().map(String::as_str).collect::<Vec<_>>()).0,
        0
    );

    let meta = std::fs::read_to_string(o1.join("meta.json")).unwrap();
    assert!(meta.contains("lactea@"), "{meta}");
    for stage in ["final", "main", "warped", "blur1", "blur2", "blur3"] {
        for f in 0..6 {
            let name = format!("{f:05}.png");
            let a = std::fs::read(o1.join(stage).join(&name)).unwrap();
            let b = std::fs::read(o2.join(stage).join(&name)).unwrap();
            assert_eq!(a, b, "{stage}/{name} differs between identical runs");
        }
    }
    assert!(o1.join("vars/00005.json").exists());
    assert!(o1.join("textures/noise_lq.png").exists());
    assert!(o1.join("textures/noisevol_hq.bin").exists());

    // teacher forcing with run1 as the "oracle"
    let mut args = common(&ot);
    args.push("--teacher".into());
    args.push(o1.to_str().unwrap().into());
    let (code, err) = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(code, 0, "{err}");
    for stage in ["final", "main", "warped"] {
        for f in 0..6 {
            let name = format!("{f:05}.png");
            let a = std::fs::read(o1.join(stage).join(&name)).unwrap();
            let b = std::fs::read(ot.join(stage).join(&name)).unwrap();
            assert_eq!(
                a, b,
                "teacher {stage}/{name} differs from the run it was taught by"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_preset_exits_2_and_bad_args_exit_nonzero() {
    let dir = tmp("bad");
    let audio = dir.join("a.wav");
    wav(&audio);
    let preset = dir.join("bad.milk");
    std::fs::write(
        &preset,
        "MILKDROP_PRESET_VERSION=100\nper_frame_1=x = = 1;\n",
    )
    .unwrap();
    let out = dir.join("out");
    let (code, err) = run(&[
        "--preset",
        preset.to_str().unwrap(),
        "--audio",
        audio.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--frames",
        "1",
        "--size",
        "64x36",
    ]);
    if code == 1 && err.contains("no wgpu adapter") {
        return;
    }
    assert_eq!(code, 2, "{err}");
    assert!(out.join("fdrender.json").exists());
    let (code, _) = run(&["--preset", "x"]);
    assert_ne!(code, 0);
    let _ = std::fs::remove_dir_all(&dir);
}
