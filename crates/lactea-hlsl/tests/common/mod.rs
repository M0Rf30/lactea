#![allow(dead_code)]
//! Shared test helpers: corpus extraction and naga validation.

pub mod diffrun;
pub mod gpu;
pub mod oracle;
use lactea_abi::PassKind;
use std::path::{Path, PathBuf};

pub const CORPUS_DIR: &str = "/usr/share/projectM/presets";

#[derive(Clone, Debug)]
pub struct Case {
    pub path: PathBuf,
    pub kind: PassKind,
    pub text: String,
}

impl Case {
    pub fn label(&self) -> String {
        let k = if self.kind == PassKind::Warp {
            "warp"
        } else {
            "comp"
        };
        let p = self.path.strip_prefix(CORPUS_DIR).unwrap_or(&self.path);
        format!("{k}:{}", p.display())
    }
}

/// MD2 `ReadCode`: keys `<prefix>1`, `<prefix>2`, ... until one is missing; strip ONE leading
/// backtick from each line; join with `\n`.
pub fn extract_code(preset: &str, prefix: &str) -> Option<String> {
    let mut lines: Vec<(usize, String)> = Vec::new();
    for l in preset.lines() {
        let l = l.trim_end_matches('\r');
        if let Some(rest) = l.strip_prefix(prefix)
            && let Some((num, val)) = rest.split_once('=')
            && let Ok(n) = num.parse::<usize>()
        {
            lines.push((n, val.to_string()));
        }
    }
    let mut out = String::new();
    let mut i = 1;
    let mut any = false;
    while let Some((_, v)) = lines.iter().find(|(n, _)| *n == i) {
        out.push_str(v.strip_prefix('`').unwrap_or(v));
        out.push('\n');
        any = true;
        i += 1;
    }
    any.then_some(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().map(|x| x == "milk").unwrap_or(false) {
                out.push(p);
            }
        }
    }
}

pub fn corpus_cases() -> Vec<Case> {
    let mut files = Vec::new();
    walk(Path::new(CORPUS_DIR), &mut files);
    files.sort();
    let mut cases = Vec::new();
    for f in files {
        let bytes = match std::fs::read(&f) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let text: String = bytes.iter().map(|&b| b as char).collect();
        if let Some(w) = extract_code(&text, "warp_") {
            cases.push(Case {
                path: f.clone(),
                kind: PassKind::Warp,
                text: w,
            });
        }
        if let Some(c) = extract_code(&text, "comp_") {
            cases.push(Case {
                path: f.clone(),
                kind: PassKind::Comp,
                text: c,
            });
        }
    }
    cases
}

pub fn naga_validate(wgsl: &str) -> Result<naga::Module, String> {
    let module = naga::front::wgsl::parse_str(wgsl).map_err(|e| e.emit_to_string(wgsl))?;
    let mut v = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    );
    v.validate(&module).map_err(|e| format!("{:?}", e))?;
    Ok(module)
}

pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16);
    let chunk = items.len().div_ceil(n).max(1);
    let f = &f;
    std::thread::scope(|s| {
        let hs: Vec<_> = items
            .chunks(chunk)
            .map(|c| s.spawn(move || c.iter().map(f).collect::<Vec<R>>()))
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    })
}
