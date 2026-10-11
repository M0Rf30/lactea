//! Minimal extractor of the code blocks of a `.milk` preset.  It is deliberately independent
//! of the preset crate's MD2 joiner so the differential tests do not share a bug with the code
//! under test; it only needs to be good enough to feed both evaluators identical source.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BlockKind {
    FrameInit,
    Frame,
    Pixel,
    WaveInit(u8),
    WaveFrame(u8),
    WavePoint(u8),
    ShapeInit(u8),
    ShapeFrame(u8),
}

impl BlockKind {
    /// Blocks that share one execution context in the real engine.
    pub fn group(self) -> Group {
        match self {
            BlockKind::FrameInit | BlockKind::Frame => Group::Frame,
            BlockKind::Pixel => Group::Pixel,
            BlockKind::WaveInit(n) | BlockKind::WaveFrame(n) | BlockKind::WavePoint(n) => {
                Group::Wave(n)
            }
            BlockKind::ShapeInit(n) | BlockKind::ShapeFrame(n) => Group::Shape(n),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    Frame,
    Pixel,
    Wave(u8),
    Shape(u8),
}

#[derive(Debug, Clone)]
pub struct Block {
    pub kind: BlockKind,
    pub code: String,
}

fn classify(prefix: &str) -> Option<BlockKind> {
    match prefix {
        "per_frame_init_" => return Some(BlockKind::FrameInit),
        "per_frame_" => return Some(BlockKind::Frame),
        "per_pixel_" => return Some(BlockKind::Pixel),
        _ => {}
    }
    let (rest, is_wave) = match prefix.strip_prefix("wave_") {
        Some(r) => (r, true),
        None => (prefix.strip_prefix("shape_")?, false),
    };
    let (num, tail) = rest.split_once('_')?;
    let n: u8 = num.parse().ok()?;
    Some(match (is_wave, tail) {
        (true, "init") => BlockKind::WaveInit(n),
        (true, "per_frame") => BlockKind::WaveFrame(n),
        (true, "per_point") => BlockKind::WavePoint(n),
        (false, "init") => BlockKind::ShapeInit(n),
        (false, "per_frame") => BlockKind::ShapeFrame(n),
        _ => return None,
    })
}

/// Splits `key=value` lines, collects `<prefix><N>=<line>` entries by block and joins them with
/// `\n` in numeric order.
pub fn extract_blocks(text: &str) -> Vec<Block> {
    let mut map: BTreeMap<BlockKind, BTreeMap<u32, String>> = BTreeMap::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let digits = key.bytes().rev().take_while(u8::is_ascii_digit).count();
        if digits == 0 || digits == key.len() {
            continue;
        }
        let (prefix, num) = key.split_at(key.len() - digits);
        let Some(kind) = classify(prefix) else {
            continue;
        };
        let Ok(num) = num.parse::<u32>() else {
            continue;
        };
        map.entry(kind).or_default().insert(num, value.to_string());
    }
    map.into_iter()
        .map(|(kind, lines)| Block {
            kind,
            code: lines.into_values().collect::<Vec<_>>().join("\n"),
        })
        .filter(|b| !b.code.trim().is_empty())
        .collect()
}

/// Recursively lists `*.milk` files (sorted).
pub fn list_presets(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            // `file_type` does not follow symlinks, so a symlink loop cannot recurse forever.
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&p, out);
            } else if p
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("milk"))
            {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(root, &mut v);
    v.sort();
    v
}

/// Reads a preset (any byte soup becomes lossy UTF-8; both engines get identical bytes).
pub fn read_preset(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}
