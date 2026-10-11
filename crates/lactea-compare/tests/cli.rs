//! CLI smoke tests on tiny generated dumps.

use lactea_abi::RunConfig;
use lactea_abi::dump::{DumpMeta, Stage};
use lactea_compare::DumpWriter;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

fn meta() -> DumpMeta {
    DumpMeta {
        producer: "cli-test".into(),
        preset: "t.milk".into(),
        audio: "synth128".into(),
        config: RunConfig {
            width: 48,
            height: 32,
            frames: 6,
            ..RunConfig::default()
        },
        stages: vec![Stage::Final, Stage::Main],
        has_vars: true,
    }
}

fn frame(f: u32, shift: u8) -> Vec<u8> {
    let (w, h) = (48u32, 32u32);
    let mut v = Vec::new();
    for y in 0..h {
        for x in 0..w {
            v.extend_from_slice(&[
                (x * 5 + f * 3) as u8,
                (y * 7) as u8,
                (((x ^ y) * 4) as u8).saturating_add(shift),
                255,
            ]);
        }
    }
    v
}

fn make_dump(root: &Path, name: &str, shift_from: u32, shift: u8) -> std::path::PathBuf {
    let d = root.join(name);
    let w = DumpWriter::create(&d, meta()).unwrap();
    for f in 0..6 {
        let s = if f >= shift_from { shift } else { 0 };
        for st in [Stage::Final, Stage::Main] {
            w.write_stage(st, f, 48, 32, &frame(f, s)).unwrap();
        }
        let mut vars = BTreeMap::new();
        vars.insert("zoom".to_string(), 1.0 + if s > 0 { 0.01 } else { 0.0 });
        vars.insert("rot".to_string(), if f == 3 { f64::NAN } else { 0.1 });
        w.write_vars(f, &vars).unwrap();
    }
    d
}

fn fd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fdcompare"))
}

#[test]
fn diff_step_vars_sheet_corpus() {
    let td = tempfile::tempdir().unwrap();
    let r = make_dump(td.path(), "ref", 99, 0);
    let same = make_dump(td.path(), "same", 99, 0);
    let bad = make_dump(td.path(), "bad", 3, 60);

    // identical -> exit 0 with gate
    let out = td.path().join("rep_same");
    let o = fd()
        .args(["diff"])
        .arg(&r)
        .arg(&same)
        .arg("--out")
        .arg(&out)
        .args(["--gate", "psnr=40,ssim=0.98"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(out.join("report.json").exists() && out.join("summary.md").exists());
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("PASS"), "{text}");

    // perturbed -> exit 1, images written
    let out = td.path().join("rep_bad");
    let o = fd()
        .args(["diff"])
        .arg(&r)
        .arg(&bad)
        .arg("--out")
        .arg(&out)
        .args([
            "--gate",
            "psnr=40,ssim=0.98",
            "--stages",
            "final",
            "--frames",
            "0..6",
        ])
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let rep: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("report.json")).unwrap()).unwrap();
    assert_eq!(rep["mode"], "free_run");
    assert_eq!(rep["passed"], false);
    assert_eq!(rep["stages"][0]["summary"]["first_frame_below_gate"], 3);
    assert_eq!(rep["stages"][0]["frames"][0]["psnr"], "inf");
    let img = rep["stages"][0]["images"][0].as_str().unwrap();
    assert!(out.join(img).exists());
    // without a gate: exit 0
    let o = fd()
        .args(["diff"])
        .arg(&r)
        .arg(&bad)
        .arg("--out")
        .arg(td.path().join("rep_nogate"))
        .args(["--images", "none"])
        .output()
        .unwrap();
    assert!(o.status.success());

    // step mode flag
    let out = td.path().join("rep_step");
    let o = fd()
        .args(["step"])
        .arg(&r)
        .arg(&bad)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success());
    let rep: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("report.json")).unwrap()).unwrap();
    assert_eq!(rep["mode"], "single_step");

    // vars table: zoom diverges from frame 3; rot is NaN on both sides at frame 3 -> equal
    let o = fd().args(["vars"]).arg(&r).arg(&bad).output().unwrap();
    assert!(o.status.success());
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("zoom") && !text.contains("rot "), "{text}");

    // contact sheet
    let sheet = td.path().join("sheet.png");
    let o = fd()
        .args(["sheet"])
        .arg(&r)
        .args(["--stage", "final", "--every", "2", "--thumb-width", "24"])
        .arg("--out")
        .arg(&sheet)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(sheet.exists());

    // corpus
    let (rr, cr) = (td.path().join("rroot"), td.path().join("croot"));
    std::fs::create_dir_all(&rr).unwrap();
    std::fs::create_dir_all(&cr).unwrap();
    for (n, from, sh) in [("p1", 99, 0u8), ("p2", 99, 0)] {
        make_dump(&rr, n, 99, 0);
        make_dump(&cr, n, from, sh);
    }
    let out = td.path().join("corp");
    let o = fd()
        .args(["corpus"])
        .arg(&rr)
        .arg(&cr)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(out.join("corpus.json").exists() && out.join("index.html").exists());
    // regress: candidate p2 gets worse than baseline
    let cr2 = td.path().join("croot2");
    make_dump(&cr2, "p1", 99, 0);
    make_dump(&cr2, "p2", 3, 60);
    let out2 = td.path().join("corp2");
    let o = fd()
        .args(["corpus"])
        .arg(&rr)
        .arg(&cr2)
        .arg("--out")
        .arg(&out2)
        .arg("--baseline")
        .arg(out.join("corpus.json"))
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    assert!(String::from_utf8_lossy(&o.stdout).contains("REGRESSION p2"));

    // errors -> exit 2
    let o = fd()
        .args(["diff"])
        .arg(td.path().join("nope"))
        .arg(&r)
        .arg("--out")
        .arg(td.path().join("x"))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}
