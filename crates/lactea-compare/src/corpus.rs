//! Corpus aggregation: many preset dumps under two roots, one table, `corpus.json`,
//! `index.html`, and regression detection against a previous `corpus.json`.

use crate::dump::{DumpReader, Image};
use crate::metrics::resize_box;
use crate::report::{DiffOptions, Gate, Report, fmt_f, run_diff, write_report};
use crate::util::{jf64, par_map};
use crate::{Error, Result};
use lactea_abi::dump::Stage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Version of the `corpus.json` schema.
pub const CORPUS_SCHEMA: u32 = 1;
/// Gate used by `fdcompare corpus` when none is given.
pub const DEFAULT_CORPUS_GATE: Gate = Gate {
    psnr: Some(40.0),
    ssim: Some(0.98),
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Outcome of comparing one preset.
#[non_exhaustive]
pub enum Status {
    /// All stages passed the gate.
    Pass,
    /// At least one stage fell below the gate.
    Fail,
    /// The comparison failed to run.
    Error,
    /// The candidate dump is missing for this preset.
    MissingCand,
    /// The reference dump is missing for this preset.
    MissingRef,
}

impl Status {
    /// Stable snake_case name matching the serialized form.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Error => "error",
            Status::MissingCand => "missing_cand",
            Status::MissingRef => "missing_ref",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// Corpus result for one stage of one preset.
pub struct CorpusStage {
    /// Pipeline stage.
    pub stage: Stage,
    /// Number of frames compared.
    pub frames: u32,
    #[serde(with = "jf64")]
    /// Lowest PSNR (dB) over compared frames.
    pub psnr_min: f64,
    #[serde(with = "jf64")]
    /// Median PSNR (dB) over compared frames.
    pub psnr_median: f64,
    #[serde(with = "jf64")]
    /// Lowest SSIM over compared frames.
    pub ssim_min: f64,
    /// First frame below the gate (or `None`).
    pub first_diverging_frame: Option<u32>,
    /// Status of this stage.
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// Corpus result for one preset.
pub struct CorpusEntry {
    /// Preset id (directory name under each root).
    pub preset: String,
    /// Overall status of the preset.
    pub status: Status,
    /// Failure message when `status` is [`Status::Error`].
    pub error: Option<String>,
    /// Per-preset report directory relative to the corpus output.
    pub report: Option<String>,
    /// Thumbnail path relative to the corpus output.
    pub thumb: Option<String>,
    /// Full-size ref | cand | diff triptych of the worst frame (relative to the corpus output).
    #[serde(default)]
    pub worst_image: Option<String>,
    /// First frame at which any variable diverges.
    pub vars_first_diverging_frame: Option<u32>,
    /// Number of variables that diverge.
    pub vars_diverging: u32,
    /// Per-stage results.
    pub stages: Vec<CorpusStage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// A preset that got worse than the baseline corpus report.
pub struct Regression {
    /// Preset id.
    pub preset: String,
    /// Stage that regressed, or `None` for a preset-level regression.
    pub stage: Option<Stage>,
    /// Human-readable description of the regression.
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// Contents of `corpus.json`.
pub struct CorpusReport {
    /// Schema version ([`CORPUS_SCHEMA`]).
    pub schema: u32,
    /// Reference dump root.
    pub ref_root: String,
    /// Candidate dump root.
    pub cand_root: String,
    /// Gate every stage was judged against.
    pub gate: Gate,
    /// One entry per preset.
    pub entries: Vec<CorpusEntry>,
    /// Entry counts per [`Status`], keyed by [`Status::as_str`].
    pub totals: BTreeMap<String, u32>,
    /// Path of the baseline `corpus.json`, when one was compared.
    pub baseline: Option<String>,
    /// Presets that regressed relative to `baseline`.
    pub regressions: Vec<Regression>,
}

#[derive(Clone, Debug)]
/// Options for [`run_corpus`].
pub struct CorpusOptions {
    /// Per-preset diff settings (gate, mode, images, jobs).
    pub diff: DiffOptions,
    /// Presets processed in parallel (each preset runs single-threaded internally).
    pub jobs: usize,
    /// Only presets whose id contains this substring.
    pub filter: Option<String>,
    /// Only presets whose id is in this set (exact match; e.g. one chunk of a huge corpus).
    pub ids: Option<std::collections::HashSet<String>>,
    /// Previous `corpus.json` to detect regressions against.
    pub baseline: Option<PathBuf>,
    /// A min-PSNR drop larger than this (dB, PSNR capped at 100) is a regression.
    pub regress_psnr_db: f64,
    /// A min-SSIM drop larger than this is a regression.
    pub regress_ssim: f64,
}

impl Default for CorpusOptions {
    fn default() -> Self {
        let diff = DiffOptions {
            gate: DEFAULT_CORPUS_GATE,
            worst_n: 1,
            ..Default::default()
        };
        Self {
            diff,
            jobs: crate::util::default_jobs(),
            filter: None,
            ids: None,
            baseline: None,
            regress_psnr_db: 0.5,
            regress_ssim: 0.005,
        }
    }
}

fn is_dump_dir(p: &Path) -> bool {
    p.join(lactea_abi::dump::META_FILE).is_file()
        || Stage::ALL.iter().any(|s| p.join(s.dir()).is_dir())
}

/// Find dump directories below `root` (a dump dir is one with `meta.json` or a stage subdir),
/// keyed by path relative to `root`. If `root` itself is a dump the key is `.`.
pub fn discover(root: &Path) -> BTreeMap<String, PathBuf> {
    fn walk(base: &Path, dir: &Path, depth: u32, out: &mut BTreeMap<String, PathBuf>) {
        if is_dump_dir(dir) {
            let id = dir
                .strip_prefix(base)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            out.insert(
                if id.is_empty() { ".".into() } else { id },
                dir.to_path_buf(),
            );
            return;
        }
        if depth == 0 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut subs: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        subs.sort();
        for s in subs {
            walk(base, &s, depth - 1, out);
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, 4, &mut out);
    out
}

fn status_of(report: &Report) -> Status {
    if report.passed {
        Status::Pass
    } else {
        Status::Fail
    }
}

fn summarize(
    preset: &str,
    report: &Report,
    out_rel: String,
    thumb: Option<String>,
    worst_image: Option<String>,
) -> CorpusEntry {
    let stages = report
        .stages
        .iter()
        .map(|s| CorpusStage {
            stage: s.stage,
            frames: s.summary.frames,
            psnr_min: s.summary.psnr_min,
            psnr_median: s.summary.psnr_median,
            ssim_min: s.summary.ssim_min,
            first_diverging_frame: s.summary.first_frame_below_gate,
            status: if s.summary.passed {
                Status::Pass
            } else {
                Status::Fail
            },
        })
        .collect();
    CorpusEntry {
        preset: preset.into(),
        status: status_of(report),
        error: None,
        report: Some(out_rel),
        thumb,
        worst_image,
        vars_first_diverging_frame: report.vars.as_ref().and_then(|v| v.first_diverging_frame),
        vars_diverging: report.vars.as_ref().map_or(0, |v| v.diverging_vars),
        stages,
    }
}

fn bare(preset: &str, status: Status, error: Option<String>) -> CorpusEntry {
    CorpusEntry {
        preset: preset.into(),
        status,
        error,
        report: None,
        thumb: None,
        worst_image: None,
        vars_first_diverging_frame: None,
        vars_diverging: 0,
        stages: vec![],
    }
}

fn thumb_name(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("thumbs/{safe}.png")
}

fn worst_triptych(report: &Report) -> Option<String> {
    let pick = report
        .stages
        .iter()
        .find(|s| s.stage == Stage::Final && !s.images.is_empty())
        .or_else(|| report.stages.iter().find(|s| !s.images.is_empty()))?;
    pick.images
        .iter()
        .find(|p| p.ends_with("_triptych.png"))
        .cloned()
}

fn make_thumb(
    out: &Path,
    id: &str,
    report: &Report,
    cand: &Path,
    preset_rel: &str,
) -> Option<String> {
    let pick = report
        .stages
        .iter()
        .find(|s| s.stage == Stage::Final && !s.images.is_empty())
        .or_else(|| report.stages.iter().find(|s| !s.images.is_empty()));
    let (img, width): (Image, u32) = if let Some(sr) = pick {
        let p = sr.images.iter().find(|p| p.ends_with("_triptych.png"))?;
        (Image::load_png(out.join(preset_rel).join(p)).ok()?, 480)
    } else {
        let r = DumpReader::open(cand).ok()?;
        let st = if !r.frames(Stage::Final).is_empty() {
            Stage::Final
        } else {
            *r.stages().first()?
        };
        let frames = r.frames(st);
        (r.load_stage(st, frames[frames.len() / 2]).ok()?, 160)
    };
    let tw = width.min(img.width);
    let th = (img.height as u64 * tw as u64 / img.width.max(1) as u64).max(1) as u32;
    let name = thumb_name(id);
    resize_box(&img, tw, th).save_png(out.join(&name)).ok()?;
    Some(name)
}

/// Run the whole corpus. Per-preset reports are written under `out/presets/<id>/`.
pub fn run_corpus(
    ref_root: &Path,
    cand_root: &Path,
    out: &Path,
    opts: &CorpusOptions,
) -> Result<CorpusReport> {
    let rmap = discover(ref_root);
    let cmap = discover(cand_root);
    if rmap.is_empty() {
        return Err(Error::Msg(format!(
            "no dumps found under reference root {}",
            ref_root.display()
        )));
    }
    std::fs::create_dir_all(out.join("thumbs")).map_err(|e| Error::io(out, e))?;
    let keep = |id: &String| {
        opts.filter.as_ref().is_none_or(|f| id.contains(f.as_str()))
            && opts.ids.as_ref().is_none_or(|set| set.contains(id))
    };
    let ids: Vec<String> = rmap
        .keys()
        .chain(cmap.keys().filter(|k| !rmap.contains_key(*k)))
        .filter(|k| keep(k))
        .cloned()
        .collect();

    let mut inner = opts.diff.clone();
    inner.jobs = 1;
    let entries = par_map(&ids, opts.jobs, |id| match (rmap.get(id), cmap.get(id)) {
        (Some(_), None) => bare(id, Status::MissingCand, None),
        (None, Some(_)) => bare(id, Status::MissingRef, None),
        (Some(r), Some(c)) => {
            let rel = format!("presets/{id}");
            let dir = out.join(&rel);
            match run_diff(r, c, Some(&dir), &inner).and_then(|rep| {
                write_report(&rep, &dir)?;
                Ok(rep)
            }) {
                Ok(rep) => {
                    let thumb = make_thumb(out, id, &rep, c, &rel);
                    let worst = worst_triptych(&rep).map(|p| format!("{rel}/{p}"));
                    summarize(id, &rep, rel, thumb, worst)
                }
                Err(e) => bare(id, Status::Error, Some(e.to_string())),
            }
        }
        (None, None) => unreachable!(),
    });

    let mut totals: BTreeMap<String, u32> = BTreeMap::new();
    totals.insert("presets".into(), entries.len() as u32);
    for e in &entries {
        *totals.entry(e.status.as_str().to_string()).or_default() += 1;
    }
    let mut rep = CorpusReport {
        schema: CORPUS_SCHEMA,
        ref_root: ref_root.display().to_string(),
        cand_root: cand_root.display().to_string(),
        gate: opts.diff.gate,
        entries,
        totals,
        baseline: None,
        regressions: vec![],
    };
    if let Some(bp) = &opts.baseline {
        let text = std::fs::read_to_string(bp).map_err(|e| Error::io(bp, e))?;
        let base: CorpusReport = serde_json::from_str(&text).map_err(|e| Error::fmt(bp, e))?;
        rep.baseline = Some(bp.display().to_string());
        rep.regressions = find_regressions(&base, &rep, opts.regress_psnr_db, opts.regress_ssim);
    }
    write_corpus(&rep, out)?;
    Ok(rep)
}

/// Compare `cur` against `base`; anything that got worse is listed.
pub fn find_regressions(
    base: &CorpusReport,
    cur: &CorpusReport,
    psnr_db: f64,
    ssim_tol: f64,
) -> Vec<Regression> {
    let cap = |p: f64| if p.is_nan() { 0.0 } else { p.min(100.0) };
    let cur_map: BTreeMap<&str, &CorpusEntry> =
        cur.entries.iter().map(|e| (e.preset.as_str(), e)).collect();
    let mut out = Vec::new();
    let mut push = |preset: &str, stage: Option<Stage>, reason: String| {
        out.push(Regression {
            preset: preset.into(),
            stage,
            reason,
        })
    };
    for b in &base.entries {
        let Some(c) = cur_map.get(b.preset.as_str()) else {
            if b.status == Status::Pass {
                push(&b.preset, None, "preset disappeared from corpus".into());
            }
            continue;
        };
        if b.status == Status::Pass
            && matches!(
                c.status,
                Status::Error | Status::MissingCand | Status::MissingRef
            )
        {
            push(
                &b.preset,
                None,
                format!(
                    "pass -> {}{}",
                    c.status.as_str(),
                    c.error
                        .as_deref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                ),
            );
            continue;
        }
        for bs in &b.stages {
            let Some(cs) = c.stages.iter().find(|s| s.stage == bs.stage) else {
                if bs.status == Status::Pass {
                    push(&b.preset, Some(bs.stage), "stage disappeared".into());
                }
                continue;
            };
            if bs.status == Status::Pass && cs.status != Status::Pass {
                push(
                    &b.preset,
                    Some(bs.stage),
                    format!(
                        "pass -> {} (min PSNR {} dB, min SSIM {})",
                        cs.status.as_str(),
                        fmt_f(cs.psnr_min, 2),
                        fmt_f(cs.ssim_min, 4)
                    ),
                );
                continue;
            }
            let dp = cap(bs.psnr_min) - cap(cs.psnr_min);
            if dp > psnr_db {
                push(
                    &b.preset,
                    Some(bs.stage),
                    format!(
                        "min PSNR {} -> {} dB (-{dp:.2})",
                        fmt_f(bs.psnr_min, 2),
                        fmt_f(cs.psnr_min, 2)
                    ),
                );
                continue;
            }
            let ds = if bs.ssim_min.is_nan() || cs.ssim_min.is_nan() {
                0.0
            } else {
                bs.ssim_min - cs.ssim_min
            };
            if ds > ssim_tol {
                push(
                    &b.preset,
                    Some(bs.stage),
                    format!(
                        "min SSIM {} -> {} (-{ds:.4})",
                        fmt_f(bs.ssim_min, 4),
                        fmt_f(cs.ssim_min, 4)
                    ),
                );
                continue;
            }
            match (bs.first_diverging_frame, cs.first_diverging_frame) {
                (None, Some(f)) => push(
                    &b.preset,
                    Some(bs.stage),
                    format!("first diverging frame: none -> {f}"),
                ),
                (Some(a), Some(f)) if f < a => push(
                    &b.preset,
                    Some(bs.stage),
                    format!("first diverging frame {a} -> {f}"),
                ),
                _ => {}
            }
        }
    }
    out
}

/// Merge reports of disjoint chunks of one corpus (entries concatenated and sorted by id, totals
/// recomputed). Regressions of the parts are kept; `baseline` is taken from the first part that has one.
pub fn merge_reports(parts: Vec<CorpusReport>) -> Option<CorpusReport> {
    let mut it = parts.into_iter();
    let mut acc = it.next()?;
    for p in it {
        acc.entries.extend(p.entries);
        acc.regressions.extend(p.regressions);
        if acc.baseline.is_none() {
            acc.baseline = p.baseline;
        }
    }
    acc.entries.sort_by(|a, b| a.preset.cmp(&b.preset));
    acc.entries.dedup_by(|a, b| a.preset == b.preset);
    acc.totals.clear();
    acc.totals
        .insert("presets".into(), acc.entries.len() as u32);
    for e in &acc.entries {
        *acc.totals.entry(e.status.as_str().to_string()).or_default() += 1;
    }
    Some(acc)
}

/// Read a `corpus.json`.
pub fn read_corpus(path: &Path) -> Result<CorpusReport> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    serde_json::from_str(&text).map_err(|e| Error::fmt(path, e))
}

/// Write `corpus.json`, `index.html` and `summary.md` into `out`.
pub fn write_corpus(rep: &CorpusReport, out: &Path) -> Result<()> {
    let jp = out.join("corpus.json");
    std::fs::write(
        &jp,
        serde_json::to_string_pretty(rep).map_err(|e| Error::fmt(&jp, e))?,
    )
    .map_err(|e| Error::io(&jp, e))?;
    let hp = out.join("index.html");
    std::fs::write(&hp, render_html(rep)).map_err(|e| Error::io(&hp, e))?;
    let mp = out.join("summary.md");
    std::fs::write(&mp, render_table(rep, usize::MAX, true)).map_err(|e| Error::io(&mp, e))
}

fn html_esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn url_enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            o.push(b as char);
        } else {
            let _ = write!(o, "%{b:02X}");
        }
    }
    o
}

/// Renders `rep` as the standalone `index.html` page.
pub fn render_html(rep: &CorpusReport) -> String {
    let mut h = String::new();
    h.push_str(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>lactea corpus</title><style>\
body{font-family:sans-serif;background:#111;color:#ddd}table{border-collapse:collapse}\
td,th{border:1px solid #444;padding:2px 6px;font-size:13px}th{background:#222;position:sticky;top:0}\
.pass{color:#6c6}.fail{color:#f66}.error,.missing_cand,.missing_ref{color:#fc4}a{color:#8cf}\
img{display:block;max-width:480px}.reg{color:#f66;font-weight:bold}</style></head><body>\n",
    );
    let _ = writeln!(h, "<h1>lactea corpus comparison</h1>");
    let _ = writeln!(
        h,
        "<p>ref: <code>{}</code><br>cand: <code>{}</code><br>gate: {}</p>",
        html_esc(&rep.ref_root),
        html_esc(&rep.cand_root),
        html_esc(&rep.gate.describe())
    );
    let tot: Vec<String> = rep
        .totals
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    let _ = writeln!(h, "<p>{}</p>", html_esc(&tot.join(" | ")));
    if rep.baseline.is_some() {
        let _ = writeln!(
            h,
            "<h2 class=\"reg\">{} regression(s) vs baseline</h2><ul>",
            rep.regressions.len()
        );
        for r in &rep.regressions {
            let _ = writeln!(
                h,
                "<li>{} {}: {}</li>",
                html_esc(&r.preset),
                r.stage.map(|s| s.dir()).unwrap_or(""),
                html_esc(&r.reason)
            );
        }
        let _ = writeln!(h, "</ul>");
    }
    h.push_str("<table><tr><th>preset</th><th>thumb (ref | cand | diff x8)</th><th>stage</th><th>frames</th><th>PSNR min</th><th>PSNR median</th><th>SSIM min</th><th>first diverging</th><th>status</th><th>vars first div</th></tr>\n");
    let mut order: Vec<&CorpusEntry> = rep.entries.iter().collect();
    let sev = |s: Status| match s {
        Status::Error => 0,
        Status::Fail => 1,
        Status::MissingCand => 2,
        Status::MissingRef => 3,
        Status::Pass => 4,
    };
    order.sort_by(|a, b| {
        sev(a.status)
            .cmp(&sev(b.status))
            .then(a.preset.cmp(&b.preset))
    });
    for e in order {
        let rows = e.stages.len().max(1);
        let link = e
            .report
            .as_ref()
            .map(|r| {
                format!(
                    "<a href=\"{}/summary.md\">{}</a> (<a href=\"{}/report.json\">json</a>)",
                    url_enc(r),
                    html_esc(&e.preset),
                    url_enc(r)
                )
            })
            .unwrap_or_else(|| html_esc(&e.preset));
        let thumb = e
            .thumb
            .as_ref()
            .map(|t| match &e.worst_image {
                Some(w) => format!(
                    "<a href=\"{}\"><img loading=\"lazy\" src=\"{}\"></a>",
                    url_enc(w),
                    url_enc(t)
                ),
                None => format!("<img loading=\"lazy\" src=\"{}\">", url_enc(t)),
            })
            .unwrap_or_default();
        let vars = e.vars_first_diverging_frame.map_or(
            if e.vars_diverging > 0 {
                "?".to_string()
            } else {
                "-".into()
            },
            |f| format!("{f} ({} vars)", e.vars_diverging),
        );
        if e.stages.is_empty() {
            let msg = e.error.as_deref().unwrap_or("");
            let _ = writeln!(
                h,
                "<tr><td>{link}</td><td></td><td colspan=\"7\" class=\"{0}\">{0} {1}</td><td></td></tr>",
                e.status.as_str(),
                html_esc(msg)
            );
            continue;
        }
        for (i, s) in e.stages.iter().enumerate() {
            h.push_str("<tr>");
            if i == 0 {
                let _ = write!(
                    h,
                    "<td rowspan=\"{rows}\">{link}</td><td rowspan=\"{rows}\">{thumb}</td>"
                );
            }
            let _ = write!(
                h,
                "<td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"{}\">{}</td>",
                s.stage.dir(),
                s.frames,
                fmt_f(s.psnr_min, 2),
                fmt_f(s.psnr_median, 2),
                fmt_f(s.ssim_min, 4),
                s.first_diverging_frame
                    .map_or("-".into(), |f| f.to_string()),
                s.status.as_str(),
                s.status.as_str()
            );
            if i == 0 {
                let _ = write!(h, "<td rowspan=\"{rows}\">{vars}</td>");
            }
            h.push_str("</tr>\n");
        }
    }
    h.push_str("</table></body></html>\n");
    h
}

/// Plain-text (markdown-compatible) table: preset, stage, min/median PSNR, min SSIM, first
/// diverging frame, status. `limit` caps the number of rows; `all` includes passing rows.
pub fn render_table(rep: &CorpusReport, limit: usize, all: bool) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "| preset | stage | PSNR min | PSNR median | SSIM min | first diverging | status |"
    );
    let _ = writeln!(s, "|---|---|---|---|---|---|---|");
    let mut n = 0usize;
    let mut skipped = 0usize;
    for e in &rep.entries {
        if e.stages.is_empty() {
            if all || e.status != Status::Pass {
                if n < limit {
                    let _ = writeln!(
                        s,
                        "| {} | - | - | - | - | - | {}{} |",
                        e.preset,
                        e.status.as_str(),
                        e.error
                            .as_deref()
                            .map(|x| format!(": {x}"))
                            .unwrap_or_default()
                    );
                    n += 1;
                } else {
                    skipped += 1;
                }
            }
            continue;
        }
        for st in &e.stages {
            if !all && st.status == Status::Pass {
                continue;
            }
            if n >= limit {
                skipped += 1;
                continue;
            }
            n += 1;
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} | {} |",
                e.preset,
                st.stage.dir(),
                fmt_f(st.psnr_min, 2),
                fmt_f(st.psnr_median, 2),
                fmt_f(st.ssim_min, 4),
                st.first_diverging_frame
                    .map_or("-".into(), |f| f.to_string()),
                st.status.as_str()
            );
        }
    }
    if skipped > 0 {
        let _ = writeln!(
            s,
            "\n... {skipped} more rows (see corpus.json / index.html)"
        );
    }
    let tot: Vec<String> = rep.totals.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let _ = writeln!(s, "\ntotals: {}", tot.join(" "));
    if rep.baseline.is_some() {
        let _ = writeln!(s, "regressions vs baseline: {}", rep.regressions.len());
        for r in &rep.regressions {
            let _ = writeln!(
                s,
                "  REGRESSION {} {}: {}",
                r.preset,
                r.stage.map(|s| s.dir()).unwrap_or("-"),
                r.reason
            );
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dump::DumpWriter;
    use crate::report::tests::{gradient, test_meta};

    fn write_preset(root: &Path, id: &str, frames: u32, shift: u8) {
        let w = DumpWriter::create(root.join(id), test_meta(32, 24, frames)).unwrap();
        for f in 0..frames {
            let mut img = gradient(32, 24, f);
            if shift > 0 && f >= 2 {
                for p in img.rgba.chunks_exact_mut(4) {
                    p[0] = p[0].saturating_add(shift);
                }
            }
            w.write_stage(Stage::Final, f, 32, 24, &img.rgba).unwrap();
        }
    }

    #[test]
    fn corpus_end_to_end_with_baseline() {
        let td = tempfile::tempdir().unwrap();
        let (r, c, c2) = (
            td.path().join("r"),
            td.path().join("c"),
            td.path().join("c2"),
        );
        for id in ["a b/good", "bad", "only_ref"] {
            write_preset(&r, id, 4, 0);
        }
        write_preset(&c, "a b/good", 4, 0);
        write_preset(&c, "bad", 4, 40);
        write_preset(&c, "only_cand", 4, 0);
        // second candidate: "a b/good" regressed
        write_preset(&c2, "a b/good", 4, 40);
        write_preset(&c2, "bad", 4, 40);

        let ids = discover(&r);
        assert_eq!(
            ids.keys().cloned().collect::<Vec<_>>(),
            vec!["a b/good", "bad", "only_ref"]
        );

        let out = td.path().join("out");
        let opts = CorpusOptions {
            jobs: 2,
            ..Default::default()
        };
        let rep = run_corpus(&r, &c, &out, &opts).unwrap();
        let by = |id: &str| rep.entries.iter().find(|e| e.preset == id).unwrap();
        assert_eq!(by("a b/good").status, Status::Pass);
        assert_eq!(by("bad").status, Status::Fail);
        assert_eq!(by("bad").stages[0].first_diverging_frame, Some(2));
        assert_eq!(by("only_ref").status, Status::MissingCand);
        assert_eq!(by("only_cand").status, Status::MissingRef);
        assert!(by("bad").thumb.is_some() && by("a b/good").thumb.is_some());
        assert!(out.join("corpus.json").exists() && out.join("index.html").exists());
        let html = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(html.contains("presets/a%20b/good") && html.contains("<img"));
        assert!(out.join("presets/bad/report.json").exists());

        // regression run
        let out2 = td.path().join("out2");
        // exact-id selection
        let out_ids = td.path().join("out_ids");
        let only = CorpusOptions {
            jobs: 1,
            ids: Some(["bad".to_string()].into_iter().collect()),
            ..Default::default()
        };
        let rep_ids = run_corpus(&r, &c, &out_ids, &only).unwrap();
        assert_eq!(rep_ids.entries.len(), 1);
        assert_eq!(rep_ids.entries[0].preset, "bad");
        // merging chunk reports reproduces the whole
        let out_rest = td.path().join("out_rest");
        let rest = CorpusOptions {
            jobs: 1,
            ids: Some(
                ["a b/good", "only_ref", "only_cand"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
            ),
            ..Default::default()
        };
        let rep_rest = run_corpus(&r, &c, &out_rest, &rest).unwrap();
        let merged = merge_reports(vec![rep_rest, rep_ids]).unwrap();
        assert_eq!(merged.entries.len(), rep.entries.len());
        assert_eq!(merged.totals, rep.totals);

        let opts2 = CorpusOptions {
            jobs: 2,
            baseline: Some(out.join("corpus.json")),
            ..Default::default()
        };
        let rep2 = run_corpus(&r, &c2, &out2, &opts2).unwrap();
        assert!(
            rep2.regressions
                .iter()
                .any(|g| g.preset == "a b/good" && g.reason.starts_with("pass -> fail")),
            "{:?}",
            rep2.regressions
        );
        assert!(rep2.regressions.iter().all(|g| g.preset != "bad"));
        // `only_ref` was never passing so is not a regression
        assert!(rep2.regressions.iter().all(|g| g.preset != "only_ref"));
        assert!(render_table(&rep2, 100, false).contains("REGRESSION"));
    }
}
