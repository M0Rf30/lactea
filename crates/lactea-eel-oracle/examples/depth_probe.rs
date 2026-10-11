//! Reports the largest expression-tree depth needed by the preset corpus.
use lactea_eel::{Context, GlobalMemory};
use lactea_eel_oracle::preset_code::{extract_blocks, list_presets, read_preset};
use std::path::Path;
use std::sync::Arc;

fn main() {
    // The recursive parser at depth 100_000 needs far more than the default 8 MiB main stack.
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(probe)
        .expect("spawn probe thread")
        .join()
        .expect("probe thread panicked");
}

fn probe() {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/usr/share/projectM/presets".into());
    let mut max = 0u32;
    let mut worst = String::new();
    let mut hist = std::collections::BTreeMap::new();
    for p in list_presets(Path::new(&root)) {
        let Some(t) = read_preset(&p) else { continue };
        for b in extract_blocks(&t) {
            let mut c = Context::new(Arc::new(GlobalMemory::new()));
            c.set_max_depth(100_000);
            if c.compile(&b.code).is_err() {
                continue;
            }
            // binary search the minimal limit that still compiles
            let (mut lo, mut hi) = (1u32, 100_000u32);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let mut c = Context::new(Arc::new(GlobalMemory::new()));
                c.set_max_depth(mid);
                if c.compile(&b.code).is_ok() {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            *hist.entry(lo / 10 * 10).or_insert(0usize) += 1;
            if lo > max {
                max = lo;
                worst = format!("{} {:?}", p.display(), b.kind);
            }
        }
    }
    println!("max depth {max} in {worst}");
    println!("histogram (bucket of 10): {hist:?}");
}
