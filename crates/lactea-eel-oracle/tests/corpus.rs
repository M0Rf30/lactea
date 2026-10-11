//! Corpus differential: every code block of every preset under /usr/share/projectM/presets is
//! compiled in lactea-eel and projectm-eval and run K=20 iterations with identical seeded
//! inputs; every variable (plus return value, regs, megabuf, gmegabuf) is compared bitwise.
//!
//! Override the corpus root with `LACTEA_CORPUS=<dir>`; `LACTEA_CORPUS_LIMIT=<n>` limits
//! the number of presets.

use lactea_eel_oracle::diff::{CompileOutcome, Compiled, Pair, Rng, hash_str};
use lactea_eel_oracle::preset_code::{
    Block, BlockKind, Group, extract_blocks, list_presets, read_preset,
};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

const K: usize = 20;

#[derive(Default)]
struct Report {
    presets: usize,
    blocks: usize,
    groups: usize,
    groups_run: usize,
    groups_pass: usize,
    both_compiled: usize,
    both_rejected: usize,
    compile_disagree: usize,
    runs: usize,
    mismatches: usize,
    /// category -> (count, first example)
    cats: BTreeMap<String, (usize, String)>,
    rejected_samples: Vec<String>,
}

impl Report {
    fn note(&mut self, cat: String, example: String) {
        let e = self.cats.entry(cat).or_insert((0, example));
        e.0 += 1;
    }

    fn merge(&mut self, o: Report) {
        self.presets += o.presets;
        self.blocks += o.blocks;
        self.groups += o.groups;
        self.groups_run += o.groups_run;
        self.groups_pass += o.groups_pass;
        self.both_compiled += o.both_compiled;
        self.both_rejected += o.both_rejected;
        self.compile_disagree += o.compile_disagree;
        self.runs += o.runs;
        self.mismatches += o.mismatches;
        for (k, (n, ex)) in o.cats {
            let e = self.cats.entry(k).or_insert((0, ex));
            e.0 += n;
        }
        self.rejected_samples.extend(o.rejected_samples);
    }
}

fn is_init(k: BlockKind) -> bool {
    matches!(
        k,
        BlockKind::FrameInit | BlockKind::WaveInit(_) | BlockKind::ShapeInit(_)
    )
}

fn run_group(path: &str, group: Group, blocks: &[&Block], rep: &mut Report) {
    rep.groups += 1;
    let mut p = Pair::new();
    let mut progs: Vec<(BlockKind, Compiled)> = Vec::new();
    let mut ok = true;
    for b in blocks {
        rep.blocks += 1;
        match p.compile(&b.code) {
            CompileOutcome::Both(c) => {
                rep.both_compiled += 1;
                progs.push((b.kind, c));
            }
            CompileOutcome::BothFailed { rust, c } => {
                rep.both_rejected += 1;
                if rep.rejected_samples.len() < 8 {
                    rep.rejected_samples
                        .push(format!("{path} {:?}: rust: {rust} | C: {c}", b.kind));
                }
            }
            CompileOutcome::RustOnly(m) => {
                rep.compile_disagree += 1;
                ok = false;
                rep.note(
                    "compile: rust rejects, C accepts".into(),
                    format!("{path} {:?}: {m}", b.kind),
                );
            }
            CompileOutcome::COnly(m) => {
                rep.compile_disagree += 1;
                ok = false;
                rep.note(
                    "compile: C rejects, rust accepts".into(),
                    format!("{path} {:?}: {m}", b.kind),
                );
            }
        }
    }
    if progs.is_empty() {
        if ok {
            rep.groups_pass += 1;
        }
        return;
    }
    rep.groups_run += 1;
    let mut rng = Rng::new(hash_str(path) ^ hash_str(&format!("{group:?}")));

    let fail =
        |rep: &mut Report, kind: BlockKind, it: usize, m: lactea_eel_oracle::diff::Mismatch| {
            rep.mismatches += 1;
            let cat = format!("{}/{}", m.kind, m.relation());
            rep.note(cat, format!("{path} {kind:?} iter {it}: {m}"));
        };

    // init blocks once
    let mut good = ok;
    'outer: {
        for (kind, prog) in progs.iter().filter(|(k, _)| is_init(*k)) {
            p.set_inputs(&mut rng, 0);
            let (r, c) = p.run(prog);
            rep.runs += 1;
            if let Err(m) = p.compare(r, c) {
                fail(rep, *kind, 0, m);
                good = false;
                break 'outer;
            }
        }
        for it in 0..K {
            for (kind, prog) in progs.iter().filter(|(k, _)| !is_init(*k)) {
                let reps = if matches!(kind, BlockKind::WavePoint(_)) {
                    3
                } else {
                    1
                };
                for _ in 0..reps {
                    p.set_inputs(&mut rng, it);
                    let (r, c) = p.run(prog);
                    rep.runs += 1;
                    if let Err(m) = p.compare(r, c) {
                        fail(rep, *kind, it, m);
                        good = false;
                        break 'outer;
                    }
                }
            }
        }
    }
    if good {
        rep.groups_pass += 1;
    }
}

fn run_preset(path: &Path, rep: &mut Report) {
    let Some(text) = read_preset(path) else {
        return;
    };
    rep.presets += 1;
    let blocks = extract_blocks(&text);
    let mut groups: HashMap<Group, Vec<&Block>> = HashMap::new();
    for b in &blocks {
        groups.entry(b.kind.group()).or_default().push(b);
    }
    let mut keys: Vec<_> = groups.keys().copied().collect();
    keys.sort();
    let name = path.display().to_string();
    for g in keys {
        run_group(&name, g, &groups[&g], rep);
    }
}

#[test]
fn corpus_differential() {
    let root =
        std::env::var("LACTEA_CORPUS").unwrap_or_else(|_| "/usr/share/projectM/presets".into());
    let mut presets = list_presets(Path::new(&root));
    if presets.is_empty() {
        eprintln!("corpus {root} not found; skipping");
        return;
    }
    if let Ok(l) = std::env::var("LACTEA_CORPUS_LIMIT") {
        presets.truncate(
            l.parse()
                .unwrap_or_else(|e| panic!("LACTEA_CORPUS_LIMIT={l:?} is not a count: {e}")),
        );
    }
    let t0 = std::time::Instant::now();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let r = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut rep = Report::default();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(p) = presets.get(i) else { break };
                        run_preset(p, &mut rep);
                    }
                    rep
                })
            })
            .collect();
        let mut total = Report::default();
        for h in handles {
            total.merge(h.join().expect("worker panicked"));
        }
        total
    });
    eprintln!(
        "== corpus differential ({root}) in {:.1}s ==",
        t0.elapsed().as_secs_f64()
    );
    eprintln!("presets: {}", r.presets);
    eprintln!(
        "code blocks: {}  (compiled in both: {}, rejected by both: {}, compile disagreement: {})",
        r.blocks, r.both_compiled, r.both_rejected, r.compile_disagree
    );
    eprintln!(
        "groups (shared-context block sets): {}  run: {}  bit-exact: {}  ({:.3}%)",
        r.groups,
        r.groups_run,
        r.groups_pass,
        100.0 * r.groups_pass as f64 / r.groups.max(1) as f64
    );
    eprintln!(
        "program runs compared: {} (each vs all variables, return, regs, megabuf[0..1024), gmegabuf[0..1024))",
        r.runs
    );
    eprintln!("value mismatches: {}", r.mismatches);
    for (cat, (n, ex)) in &r.cats {
        eprintln!("  [{n}] {cat}  e.g. {ex}");
    }
    for s in &r.rejected_samples {
        eprintln!("  both-reject sample: {s}");
    }
    assert_eq!(
        r.mismatches + r.compile_disagree,
        0,
        "differential mismatches (see above)"
    );
}
