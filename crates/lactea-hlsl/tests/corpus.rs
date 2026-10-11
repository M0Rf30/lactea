//! Corpus gate: every warp/comp shader of /usr/share/projectM/presets must translate and pass
//! naga validation; glslang (DX9-compatible HLSL front end) provides the "is this even valid
//! HLSL" baseline. Run with `--nocapture` to see the numbers and failure categories.
mod common;

use common::oracle::{glslang_validity, have_tools};
use common::*;
use lactea_hlsl::{Options, translate};
use std::collections::BTreeMap;

fn normalize(msg: &str) -> String {
    // collapse identifiers/numbers so similar failures group together
    let mut s = String::new();
    let mut in_tick = false;
    for c in msg.chars() {
        if c == '`' {
            in_tick = !in_tick;
            s.push(c);
            continue;
        }
        if in_tick {
            continue;
        }
        s.push(c);
    }
    s.chars().take(90).collect()
}

#[test]
fn corpus_translates_and_validates() {
    let cases = corpus_cases();
    if cases.is_empty() {
        eprintln!("corpus not present, skipping");
        return;
    }
    let tools = have_tools();
    let gl_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("gl-corpus");
    let indexed: Vec<(usize, Case)> = cases.into_iter().enumerate().collect();
    let results = par_map(&indexed, |(i, c)| {
        let r = translate(c.kind, &c.text, &Options::default());
        let (te, ne) = match r {
            Err(e) => (Some(e), None),
            Ok(t) => (None, naga_validate(&t.wgsl).err()),
        };
        let gl = if tools {
            Some(glslang_validity(c, &gl_dir, *i).map_err(|e| e.category()))
        } else {
            None
        };
        (te, ne, gl)
    });
    let total = indexed.len();
    let mut tr_fail = 0;
    let mut naga_fail = 0;
    let mut cats: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut report = String::new();
    let (mut gl_ok, mut gl_bad) = (0, 0);
    let mut gl_cats: BTreeMap<String, usize> = BTreeMap::new();
    let (mut fail_gl_invalid, mut fail_gl_valid) = (0, 0);
    for ((_, c), (te, ne, gl)) in indexed.iter().zip(&results) {
        let ours_failed = te.is_some() || ne.is_some();
        if let Some(e) = te {
            tr_fail += 1;
            let key = format!("translate/{}: {}", e.category(), normalize(e.message()));
            cats.entry(key).or_default().push(c.label());
            report.push_str(&format!("TRANSLATE {} :: {}\n", c.label(), e));
        } else if let Some(e) = ne {
            naga_fail += 1;
            let first = e.lines().next().unwrap_or("").to_string();
            let key = format!("naga: {}", normalize(&first));
            cats.entry(key).or_default().push(c.label());
            report.push_str(&format!(
                "NAGA {} :: {}\n",
                c.label(),
                e.replace('\n', " | ")
            ));
        }
        if let Some(g) = gl {
            match g {
                Ok(()) => {
                    gl_ok += 1;
                    if ours_failed {
                        fail_gl_valid += 1;
                    }
                }
                Err(cat) => {
                    gl_bad += 1;
                    *gl_cats.entry(cat.clone()).or_default() += 1;
                    if ours_failed {
                        fail_gl_invalid += 1;
                    }
                    report.push_str(&format!("GLSLANG-INVALID {} :: {}\n", c.label(), cat));
                }
            }
        }
    }
    let ok = total - tr_fail - naga_fail;
    println!("shaders: {total}");
    println!(
        "translated: {} ({:.2}%)",
        total - tr_fail,
        100.0 * (total - tr_fail) as f64 / total as f64
    );
    println!(
        "naga-valid: {} ({:.2}% of all)",
        ok,
        100.0 * ok as f64 / total as f64
    );
    let mut cv: Vec<_> = cats.iter().collect();
    cv.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    for (k, v) in cv.iter().take(40) {
        println!("  {:4}  {}   e.g. {}", v.len(), k, v[0]);
    }
    if tools {
        println!(
            "glslang-valid HLSL baseline: {gl_ok} ({:.2}%), invalid/unsupported by glslang: {gl_bad}",
            100.0 * gl_ok as f64 / total as f64
        );
        let mut gv: Vec<_> = gl_cats.iter().collect();
        gv.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (k, n) in gv.iter().take(15) {
            println!("  {n:4}  {k}");
        }
        println!(
            "our failures that glslang also rejects: {fail_gl_invalid}; that glslang accepts: {fail_gl_valid}"
        );
        let gl_valid_total = gl_ok;
        let ours_on_valid = gl_ok - fail_gl_valid;
        println!(
            "ours translate+naga success on glslang-valid shaders: {ours_on_valid}/{gl_valid_total} ({:.2}%)",
            100.0 * ours_on_valid as f64 / gl_valid_total.max(1) as f64
        );
    } else {
        println!("glslangValidator/spirv-opt not found: baseline skipped");
    }
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("corpus-failures.txt");
    let _ = std::fs::write(&out, report);
    println!("failure list: {}", out.display());
    // regression gate
    assert!(
        ok as f64 / total as f64 >= 0.995,
        "corpus translation+naga validity regressed: {ok}/{total}"
    );
}

/// The projectM flavor (`Options::projectm_flavor`: priority-reset parser, 6-digit literals, asin/acos, zero
/// divisors) must not make shaders untranslatable that translate in the textbook mode.
#[test]
fn corpus_translates_with_projectm_flavor() {
    let cases = corpus_cases();
    if cases.is_empty() {
        eprintln!("corpus not present, skipping");
        return;
    }
    let mut flavor = Options::default();
    flavor.projectm_flavor = true;
    let results = par_map(&cases, |c| {
        let base = translate(c.kind, &c.text, &Options::default());
        let pm = translate(c.kind, &c.text, &flavor);
        let pm_bad = match &pm {
            Err(e) => Some(e.to_string()),
            Ok(t) => naga_validate(&t.wgsl).err(),
        };
        (base.is_ok(), pm_bad)
    });
    let regressed: Vec<String> = cases
        .iter()
        .zip(&results)
        .filter(|(_, (base_ok, pm_bad))| *base_ok && pm_bad.is_some())
        .map(|(c, (_, e))| format!("{} :: {}", c.label(), e.as_deref().unwrap_or("")))
        .collect();
    for r in regressed.iter().take(20) {
        println!("FLAVOR-REGRESSION {r}");
    }
    assert!(
        regressed.is_empty(),
        "{} shaders translate in the textbook mode but not with the hlslparser flavor",
        regressed.len()
    );
}
