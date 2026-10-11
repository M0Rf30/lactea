use lactea_preset::*;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("milk"))
        {
            out.push(p);
        }
    }
}

#[test]
fn corpus_parses_and_roundtrips() {
    let root = std::env::var("LACTEA_PRESETS").unwrap_or("/usr/share/projectM/presets".into());
    let mut files = Vec::new();
    walk(Path::new(&root), &mut files);
    files.sort();
    if files.is_empty() {
        eprintln!("corpus not present at {root}; skipping");
        return;
    }
    let (mut ok, mut rt_ok, mut with_shaders, mut v200, mut v201, mut v_old) = (0, 0, 0, 0, 0, 0);
    let mut fails = Vec::new();
    let mut rt_fails = Vec::new();
    let (mut ff, mut bad_utf8, mut bom, mut long_lines, mut bare_cr, mut extra_files) =
        (0, 0, 0, 0, 0, 0);
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        ff += bytes.contains(&0xFF) as i32;
        bad_utf8 += std::str::from_utf8(&bytes).is_err() as i32;
        bom += bytes.starts_with(&[0xEF, 0xBB, 0xBF]) as i32;
        long_lines += bytes.split(|&b| b == b'\n').any(|l| l.len() > 256) as i32;
        bare_cr += (bytes.contains(&b'\r') && !bytes.windows(2).any(|w| w == b"\r\n")) as i32;
        match Preset::parse(&bytes) {
            Ok(p) => {
                ok += 1;
                if p.warp_shader.is_some() || p.comp_shader.is_some() {
                    with_shaders += 1;
                }
                match p.milkdrop_preset_version {
                    200 => v200 += 1,
                    201 => v201 += 1,
                    _ => v_old += 1,
                }
                extra_files += (!p.extra.is_empty()) as i32;
                let t = p.to_milk();
                match Preset::parse(t.as_bytes()) {
                    Ok(q) if q == p && q.to_milk() == t => rt_ok += 1,
                    _ => rt_fails.push(f.clone()),
                }
            }
            Err(e) => fails.push((f.clone(), e)),
        }
    }
    println!(
        "corpus: {} files, parsed {ok}, round-trip {rt_ok}, with shader text {with_shaders}, v200 {v200}, v201 {v201}, other {v_old}",
        files.len()
    );
    println!(
        "quirk exposure: files with 0xFF byte {ff}, invalid UTF-8 {bad_utf8}, BOM {bom}, line>256B {long_lines}, bare-CR {bare_cr}, files with unknown keys {extra_files}"
    );
    for f in fails.iter().take(10) {
        println!("PARSE FAIL {f:?}");
    }
    for f in rt_fails.iter().take(10) {
        println!("RT FAIL {f:?}");
    }
    assert!(fails.is_empty(), "{} parse failures", fails.len());
    assert!(
        rt_fails.is_empty(),
        "{} round-trip failures",
        rt_fails.len()
    );
}

/// How often projectM's reader and MD2's reader disagree on the corpus, per section.  The
/// disagreement list is informational (printed); the invariant is that the projectM flavor
/// parses every file MD2's does.
#[test]
fn projectm_flavor_vs_md2_on_corpus() {
    let root = std::env::var("LACTEA_PRESETS").unwrap_or("/usr/share/projectM/presets".into());
    let mut files = Vec::new();
    walk(Path::new(&root), &mut files);
    files.sort();
    if files.is_empty() {
        eprintln!("corpus not present at {root}; skipping");
        return;
    }
    let mut diff: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let (Ok(a), Ok(b)) = (
            Preset::parse_with(&bytes, Flavor::Md2),
            Preset::parse_with(&bytes, Flavor::ProjectM),
        ) else {
            panic!("{f:?} does not parse in both flavors");
        };
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let mut note = |k: &'static str, d: bool| {
            if d {
                diff.entry(k).or_default().push(name.clone());
            }
        };
        note("base", a.base != b.base);
        // projectM only has 4 shape/wave slots: compare those, enabled ones for shapes
        let en = |p: &Preset| {
            p.shapes
                .iter()
                .take(4)
                .filter(|s| s.enabled != 0)
                .cloned()
                .collect::<Vec<_>>()
        };
        note("waves (first 4)", a.waves[..4] != b.waves[..4]);
        note("enabled shapes (first 4)", en(&a) != en(&b));
        note("per_frame_init", a.per_frame_init != b.per_frame_init);
        note("per_frame", a.per_frame != b.per_frame);
        note("per_pixel", a.per_pixel != b.per_pixel);
        note("warp", a.warp_shader != b.warp_shader);
        note("comp", a.comp_shader != b.comp_shader);
    }
    println!("flavor differences over {} files:", files.len());
    for (k, v) in &diff {
        println!("  {k}: {} files, e.g. {:?}", v.len(), &v[..v.len().min(5)]);
    }
}
