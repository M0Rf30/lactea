//! Compare the analyzer with the projectM oracle's per-frame variables.
//!
//! Ignored by default because it needs oracle output (`out/ref/starter/*/vars/*.json`, see
//! `lactea_abi::dump`).  Run with
//!
//! ```text
//! CARGO_TARGET_DIR=target-presetaudio cargo test -p lactea-audio --release \
//!     --test oracle_vars -- --ignored --nocapture
//! ```
//!
//! Env: `LACTEA_ORACLE_OUT` (default `<workspace>/out/ref/starter`).  For every run dir
//! the test reads `meta.json` (`audio` = fixture name or path, `config.fps`), replays the
//! WAV with `pcm_range` tiling, and prints max/mean abs difference per variable.  Frame 0 is
//! driven with dt = 0 like projectM's first frame (its TimeKeeper sees ~0 elapsed).

use lactea_audio::Analyzer;
use std::path::{Path, PathBuf};

fn read_wav_f32(path: &Path) -> (u32, usize, Vec<f32>) {
    let b = std::fs::read(path).unwrap();
    assert_eq!(&b[0..4], b"RIFF");
    let (mut fmt_tag, mut ch, mut sr, mut bits) = (0u16, 0usize, 0u32, 0u16);
    let mut p = 12;
    while p + 8 <= b.len() {
        let id = &b[p..p + 4];
        let len = u32::from_le_bytes(b[p + 4..p + 8].try_into().unwrap()) as usize;
        let body = &b[p + 8..(p + 8 + len).min(b.len())];
        if id == b"fmt " {
            fmt_tag = u16::from_le_bytes(body[0..2].try_into().unwrap());
            ch = u16::from_le_bytes(body[2..4].try_into().unwrap()) as usize;
            sr = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
        } else if id == b"data" {
            let v: Vec<f32> = match (fmt_tag, bits) {
                (3, 32) => body
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_le_bytes(*c))
                    .collect(),
                (1, 16) => body
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| i16::from_le_bytes(*c) as f32 / 32768.0)
                    .collect(),
                _ => panic!("unsupported wav format {fmt_tag}/{bits}"),
            };
            return (sr, ch, v);
        }
        p += 8 + len + (len & 1);
    }
    panic!("no data chunk");
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
#[ignore = "needs oracle output under out/ref/starter"]
fn compare_with_oracle_vars() {
    let root = std::env::var("LACTEA_ORACLE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace().join("out/ref/starter"));
    let Ok(rd) = std::fs::read_dir(&root) else {
        eprintln!("no oracle output at {}; nothing to compare", root.display());
        return;
    };
    let mut worst = 0.0f64;
    for e in rd.flatten() {
        let dir = e.path();
        let (Ok(meta), true) = (
            std::fs::read_to_string(dir.join("meta.json")),
            dir.join("vars").is_dir(),
        ) else {
            continue;
        };
        let meta: serde_json::Value = serde_json::from_str(&meta).unwrap();
        let fps = meta["config"]["fps"].as_u64().unwrap_or(60);
        let audio = meta["audio"].as_str().unwrap_or("synth128");
        let mut wav = PathBuf::from(audio);
        if !wav.exists() {
            wav = workspace()
                .join("fixtures/audio")
                .join(format!("{}.wav", audio.trim_end_matches(".wav")));
        }
        let (sr, ch, pcm) = read_wav_f32(&wav);
        let mut a = Analyzer::new(sr);
        let mut files: Vec<_> = std::fs::read_dir(dir.join("vars"))
            .unwrap()
            .flatten()
            .map(|f| f.path())
            .collect();
        files.sort();
        let names = ["bass", "mid", "treb", "bass_att", "mid_att", "treb_att"];
        let mut maxd = [0.0f64; 6];
        let mut sumd = [0.0f64; 6];
        let mut n = 0usize;
        for (frame, f) in files.iter().enumerate() {
            let (s, en) = lactea_abi::pcm_range(frame as u64, sr as u64, fps);
            let (s, en) = (
                (s as usize * ch).min(pcm.len()),
                (en as usize * ch).min(pcm.len()),
            );
            a.push_pcm_f32(&pcm[s..en], ch);
            let af = if frame == 0 {
                a.update_dt(0.0)
            } else {
                a.update(fps as f32)
            };
            let mine = [
                af.bass,
                af.mid,
                af.treb,
                af.bass_att,
                af.mid_att,
                af.treb_att,
            ];
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
            for (i, k) in names.iter().enumerate() {
                if let Some(o) = v.get(*k).and_then(|x| x.as_f64()) {
                    let d = (mine[i] as f64 - o).abs();
                    maxd[i] = maxd[i].max(d);
                    sumd[i] += d;
                }
            }
            n += 1;
        }
        println!(
            "== {} ({} frames, {} fps, audio {})",
            dir.display(),
            n,
            fps,
            wav.display()
        );
        for i in 0..6 {
            println!(
                "   {:9} max|d| = {:.3e}  mean|d| = {:.3e}",
                names[i],
                maxd[i],
                sumd[i] / n.max(1) as f64
            );
            worst = worst.max(maxd[i]);
        }
    }
    println!("worst max|d| over all runs: {worst:.3e}");
}
