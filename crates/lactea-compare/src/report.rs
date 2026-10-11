//! The `diff` / `step` engine: compare two dumps stage by stage, frame by frame.

use crate::dump::{DumpReader, Image};
use crate::metrics::{ImageMetrics, MetricOpts, compare_images, diff_heatmap, triptych};
use crate::util::{default_jobs, jf64, median, par_map};
use crate::vars::{VarTol, VarsAccumulator, VarsDiff};
use crate::{Error, Result};
use lactea_abi::dump::{DumpMeta, Stage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

/// PSNR values are capped at this many dB when averaging (identical frames are `inf`).
pub const PSNR_MEAN_CAP: f64 = 100.0;
/// Version of the `report.json` schema.
pub const REPORT_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
/// How the candidate dump was produced relative to the reference.
pub enum Mode {
    /// Both dumps are free runs from identical inputs (`fdcompare diff`).
    #[default]
    FreeRun,
    /// Teacher-forced single-step comparison: candidate frame n was rendered from the
    /// reference's frame n-1 state (`fdcompare step`). Layout is identical; only labelling differs.
    SingleStep,
}

impl Mode {
    /// Display name for report headings.
    pub fn label(self) -> &'static str {
        match self {
            Mode::FreeRun => "free run",
            Mode::SingleStep => "single step (teacher-forced)",
        }
    }
}

/// Pass thresholds; a frame fails if PSNR < `psnr` or SSIM < `ssim`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Gate {
    /// Minimum PSNR in dB.
    pub psnr: Option<f64>,
    /// Minimum SSIM.
    pub ssim: Option<f64>,
}

impl Gate {
    /// Parse `psnr=40,ssim=0.98` (either key optional).
    pub fn parse(s: &str) -> Result<Gate> {
        let mut g = Gate::default();
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (k, v) = part
                .split_once('=')
                .ok_or_else(|| Error::Msg(format!("bad gate item {part:?} (want key=value)")))?;
            let v: f64 = v
                .trim()
                .parse()
                .map_err(|_| Error::Msg(format!("bad gate value {v:?}")))?;
            match k.trim() {
                "psnr" => g.psnr = Some(v),
                "ssim" => g.ssim = Some(v),
                other => {
                    return Err(Error::Msg(format!(
                        "unknown gate key {other:?} (psnr, ssim)"
                    )));
                }
            }
        }
        Ok(g)
    }
    /// True when neither threshold is set.
    pub fn is_empty(&self) -> bool {
        self.psnr.is_none() && self.ssim.is_none()
    }
    /// True if the metrics violate the gate.
    pub fn fails(&self, m: &ImageMetrics) -> bool {
        self.psnr.is_some_and(|t| m.psnr < t)
            || self.ssim.is_some_and(|t| !m.ssim.is_nan() && m.ssim < t)
    }
    /// Short text form such as `psnr>=40, ssim>=0.98`; `none` when empty.
    pub fn describe(&self) -> String {
        let mut v = Vec::new();
        if let Some(p) = self.psnr {
            v.push(format!("psnr>={p}"));
        }
        if let Some(s) = self.ssim {
            v.push(format!("ssim>={s}"));
        }
        if v.is_empty() {
            "none".into()
        } else {
            v.join(", ")
        }
    }
}

/// Frame selection: `A..B` (half-open), `A..=B` (inclusive), `A..`, `..B`, `..`, or `N`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameRange {
    /// Inclusive start.
    pub start: Option<u32>,
    /// Exclusive end.
    pub end: Option<u32>,
}

impl FrameRange {
    /// Parses `A..B`, `A..=B`, `A..`, `..B`, `..` or a single frame `N`.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let num = |t: &str| -> Result<Option<u32>> {
            let t = t.trim();
            if t.is_empty() {
                Ok(None)
            } else {
                t.parse()
                    .map(Some)
                    .map_err(|_| Error::Msg(format!("bad frame number {t:?}")))
            }
        };
        if let Some((a, b)) = s.split_once("..=") {
            return Ok(Self {
                start: num(a)?,
                end: num(b)?.map(|e| e + 1),
            });
        }
        if let Some((a, b)) = s.split_once("..") {
            return Ok(Self {
                start: num(a)?,
                end: num(b)?,
            });
        }
        let n = num(s)?.ok_or_else(|| Error::Msg("empty frame range".into()))?;
        Ok(Self {
            start: Some(n),
            end: Some(n + 1),
        })
    }
    /// Whether frame `f` is selected.
    pub fn contains(&self, f: u32) -> bool {
        self.start.is_none_or(|s| f >= s) && self.end.is_none_or(|e| f < e)
    }
    /// Formats as `start..end` (`all` when unbounded).
    pub fn describe(&self) -> String {
        match (self.start, self.end) {
            (None, None) => "all".into(),
            (s, e) => format!(
                "{}..{}",
                s.map(|v| v.to_string()).unwrap_or_default(),
                e.map(|v| v.to_string()).unwrap_or_default()
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Which per-frame images [`run_diff`] writes.
#[non_exhaustive]
pub enum ImagesMode {
    #[default]
    /// Write images for the worst frames only.
    Worst,
    /// Write images for every compared frame.
    All,
    /// Write no images.
    None,
}

impl std::str::FromStr for ImagesMode {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "worst" => Ok(Self::Worst),
            "all" => Ok(Self::All),
            "none" => Ok(Self::None),
            o => Err(Error::Msg(format!(
                "--images expects worst|all|none, got {o:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug)]
/// Options for [`run_diff`].
pub struct DiffOptions {
    /// `None` = every stage present in both dumps.
    pub stages: Option<Vec<Stage>>,
    /// Frames to compare.
    pub frames: FrameRange,
    /// Pass thresholds.
    pub gate: Gate,
    /// Which diff images to write.
    pub images: ImagesMode,
    /// How many worst frames to list / render in `worst` image mode.
    pub worst_n: usize,
    /// Comparison mode (labelling only).
    pub mode: Mode,
    /// Parallel workers.
    pub jobs: usize,
    /// Image metric options.
    pub metric: MetricOpts,
    /// Tolerances for variable divergence.
    pub var_tol: VarTol,
    /// Whether to compare per-frame variables.
    pub vars: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            stages: None,
            frames: FrameRange::default(),
            gate: Gate::default(),
            images: ImagesMode::Worst,
            worst_n: 5,
            mode: Mode::FreeRun,
            jobs: default_jobs(),
            metric: MetricOpts::default(),
            var_tol: VarTol::default(),
            vars: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// Metrics for one compared frame.
pub struct FrameEntry {
    /// Frame number.
    pub frame: u32,
    #[serde(flatten)]
    /// Metrics of the frame.
    pub metrics: ImageMetrics,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// A frame that could not be compared (e.g. size mismatch or unreadable PNG).
pub struct FrameError {
    /// Frame that failed to compare.
    pub frame: u32,
    /// Error message.
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// Aggregate statistics for one stage.
pub struct StageSummary {
    /// Number of frames compared.
    pub frames: u32,
    #[serde(with = "jf64")]
    /// Lowest PSNR (dB) over frames.
    pub psnr_min: f64,
    #[serde(with = "jf64")]
    /// Median PSNR (dB) over frames.
    pub psnr_median: f64,
    /// Mean with each frame capped at [`PSNR_MEAN_CAP`] dB.
    #[serde(with = "jf64")]
    pub psnr_mean: f64,
    /// Lowest SSIM over frames where it was computed.
    #[serde(with = "jf64")]
    pub ssim_min: f64,
    /// Median SSIM over frames where it was computed.
    #[serde(with = "jf64")]
    pub ssim_median: f64,
    /// Mean SSIM over frames where it was computed.
    #[serde(with = "jf64")]
    pub ssim_mean: f64,
    /// Mean of the per-frame mean absolute differences.
    #[serde(with = "jf64")]
    pub mean_abs_mean: f64,
    /// Largest per-channel RGB difference over all frames.
    pub max_abs_max: u8,
    /// First frame that fails the gate.
    pub first_frame_below_gate: Option<u32>,
    /// Number of frames that fail the gate.
    pub frames_below_gate: u32,
    /// Lowest-PSNR frames (only frames with any difference), worst first.
    pub worst_frames: Vec<u32>,
    /// Whether the stage met the gate and had no structural problems.
    pub passed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// Per-stage comparison result.
pub struct StageReport {
    /// Pipeline stage.
    pub stage: Stage,
    /// Per-frame metrics for compared frames.
    pub frames: Vec<FrameEntry>,
    /// Reference frames absent from the candidate.
    pub missing_in_cand: Vec<u32>,
    /// Candidate frames absent from the reference.
    pub missing_in_ref: Vec<u32>,
    /// Frames that exist on both sides but could not be compared.
    pub errors: Vec<FrameError>,
    /// Image files written for this stage, relative to the report directory.
    pub images: Vec<String>,
    /// Aggregate statistics for the stage.
    pub summary: StageSummary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// Full result of comparing two dumps; serialised as `report.json`.
pub struct Report {
    /// Report schema version ([`REPORT_SCHEMA`]).
    pub schema: u32,
    /// Comparison mode.
    pub mode: Mode,
    /// Tool name and version that produced the report.
    pub tool: String,
    /// Reference dump directory.
    pub ref_dir: String,
    /// Candidate dump directory.
    pub cand_dir: String,
    /// Reference `meta.json`, if it loaded.
    pub ref_meta: Option<DumpMeta>,
    /// Candidate `meta.json`, if it loaded.
    pub cand_meta: Option<DumpMeta>,
    /// Gate the stages were judged against.
    pub gate: Gate,
    /// Human-readable description of the compared frame range.
    pub frame_range: String,
    /// Per-stage results.
    pub stages: Vec<StageReport>,
    /// Variable comparison, when both dumps have variables and it was requested.
    pub vars: Option<VarsDiff>,
    /// Non-fatal issues found while comparing.
    pub warnings: Vec<String>,
    /// Overall verdict: no failures.
    pub passed: bool,
    /// One message per gate or structural failure; empty when `passed`.
    pub failures: Vec<String>,
}

fn finite_or(v: f64, d: f64) -> f64 {
    if v.is_nan() { d } else { v }
}

/// Build the summary for one stage from its per-frame entries.
pub fn summarize_stage(
    entries: &[FrameEntry],
    gate: &Gate,
    worst_n: usize,
    structural_ok: bool,
) -> StageSummary {
    let psnr: Vec<f64> = entries.iter().map(|e| e.metrics.psnr).collect();
    let ssim: Vec<f64> = entries
        .iter()
        .map(|e| e.metrics.ssim)
        .filter(|v| !v.is_nan())
        .collect();
    let n = entries.len();
    let minf = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
    let below: Vec<u32> = entries
        .iter()
        .filter(|e| gate.fails(&e.metrics))
        .map(|e| e.frame)
        .collect();
    let mut order: Vec<&FrameEntry> = entries.iter().filter(|e| !e.metrics.identical()).collect();
    order.sort_by(|a, b| {
        a.metrics
            .psnr
            .total_cmp(&b.metrics.psnr)
            .then(finite_or(a.metrics.ssim, 1.0).total_cmp(&finite_or(b.metrics.ssim, 1.0)))
            .then(a.frame.cmp(&b.frame))
    });
    StageSummary {
        frames: n as u32,
        psnr_min: if n == 0 { f64::NAN } else { minf(&psnr) },
        psnr_median: median(&psnr),
        psnr_mean: if n == 0 {
            f64::NAN
        } else {
            psnr.iter().map(|p| p.min(PSNR_MEAN_CAP)).sum::<f64>() / n as f64
        },
        ssim_min: if ssim.is_empty() {
            f64::NAN
        } else {
            minf(&ssim)
        },
        ssim_median: median(&ssim),
        ssim_mean: if ssim.is_empty() {
            f64::NAN
        } else {
            ssim.iter().sum::<f64>() / ssim.len() as f64
        },
        mean_abs_mean: if n == 0 {
            f64::NAN
        } else {
            entries.iter().map(|e| e.metrics.mean_abs).sum::<f64>() / n as f64
        },
        max_abs_max: entries
            .iter()
            .map(|e| e.metrics.max_abs_any())
            .max()
            .unwrap_or(0),
        first_frame_below_gate: below.iter().copied().min(),
        frames_below_gate: below.len() as u32,
        worst_frames: order.iter().take(worst_n).map(|e| e.frame).collect(),
        passed: structural_ok && n > 0 && below.is_empty(),
    }
}

fn rel(out: &Path, p: &Path) -> String {
    p.strip_prefix(out)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Write triptych + heat-map PNGs for one frame; returns paths relative to `out`.
fn write_frame_images(
    r: &DumpReader,
    c: &DumpReader,
    stage: Stage,
    frame: u32,
    out: &Path,
) -> Result<Vec<String>> {
    let a = r.load_stage(stage, frame)?;
    let b = c.load_stage(stage, frame)?;
    let base = out.join("images").join(stage.dir());
    let tp = base.join(format!("{frame:05}_triptych.png"));
    let hp = base.join(format!("{frame:05}_heat.png"));
    triptych(&a, &b, 8).save_png(&tp)?;
    diff_heatmap(&a, &b).save_png(&hp)?;
    Ok(vec![rel(out, &tp), rel(out, &hp)])
}

fn compare_stage(
    r: &DumpReader,
    c: &DumpReader,
    stage: Stage,
    opts: &DiffOptions,
    out: Option<&Path>,
) -> StageReport {
    let rf: BTreeSet<u32> = r
        .frames(stage)
        .iter()
        .copied()
        .filter(|f| opts.frames.contains(*f))
        .collect();
    let cf: BTreeSet<u32> = c
        .frames(stage)
        .iter()
        .copied()
        .filter(|f| opts.frames.contains(*f))
        .collect();
    let common: Vec<u32> = rf.intersection(&cf).copied().collect();
    let missing_in_cand: Vec<u32> = rf.difference(&cf).copied().collect();
    let missing_in_ref: Vec<u32> = cf.difference(&rf).copied().collect();

    let results = par_map(
        &common,
        opts.jobs,
        |&f| -> std::result::Result<ImageMetrics, String> {
            let a = r.load_stage(stage, f).map_err(|e| e.to_string())?;
            let b = c.load_stage(stage, f).map_err(|e| e.to_string())?;
            compare_images(&a, &b, opts.metric).map_err(|e| e.to_string())
        },
    );
    let mut frames = Vec::new();
    let mut errors = Vec::new();
    for (&f, res) in common.iter().zip(results) {
        match res {
            Ok(m) => frames.push(FrameEntry {
                frame: f,
                metrics: m,
            }),
            Err(e) => errors.push(FrameError { frame: f, error: e }),
        }
    }
    let structural_ok = errors.is_empty() && missing_in_cand.is_empty();
    let summary = summarize_stage(&frames, &opts.gate, opts.worst_n, structural_ok);

    let mut images = Vec::new();
    if let (Some(out), true) = (out, opts.images != ImagesMode::None) {
        let to_render: Vec<u32> = match opts.images {
            ImagesMode::All => frames
                .iter()
                .filter(|e| !e.metrics.identical())
                .map(|e| e.frame)
                .collect(),
            _ => summary.worst_frames.clone(),
        };
        let res = par_map(&to_render, opts.jobs, |&f| {
            write_frame_images(r, c, stage, f, out)
        });
        for (f, res) in to_render.iter().zip(res) {
            match res {
                Ok(p) => images.extend(p),
                Err(e) => errors.push(FrameError {
                    frame: *f,
                    error: format!("image output: {e}"),
                }),
            }
        }
    }
    let mut sr = StageReport {
        stage,
        frames,
        missing_in_cand,
        missing_in_ref,
        errors,
        images,
        summary,
    };
    sr.summary.passed = sr.summary.passed && sr.errors.is_empty();
    sr
}

/// Compares per-frame variables of two dumps; `None` when either has no `vars/` directory.
/// Non-fatal problems are appended to `warnings`.
pub fn compare_vars(
    r: &DumpReader,
    c: &DumpReader,
    opts: &DiffOptions,
    warnings: &mut Vec<String>,
) -> Option<VarsDiff> {
    if !r.has_vars() || !c.has_vars() {
        if r.has_vars() != c.has_vars() {
            warnings.push(format!(
                "vars/ present only in {}",
                if r.has_vars() {
                    "reference"
                } else {
                    "candidate"
                }
            ));
        }
        return None;
    }
    let rf: BTreeSet<u32> = r
        .var_frames()
        .iter()
        .copied()
        .filter(|f| opts.frames.contains(*f))
        .collect();
    let cf: BTreeSet<u32> = c
        .var_frames()
        .iter()
        .copied()
        .filter(|f| opts.frames.contains(*f))
        .collect();
    let common: Vec<u32> = rf.intersection(&cf).copied().collect();
    let mut acc = VarsAccumulator::new(opts.var_tol);
    for _ in rf.difference(&cf) {
        acc.add_missing_frame(true);
    }
    for _ in cf.difference(&rf) {
        acc.add_missing_frame(false);
    }
    for chunk in common.chunks(256) {
        let loaded = par_map(chunk, opts.jobs, |&f| (f, r.load_vars(f), c.load_vars(f)));
        for (f, a, b) in loaded {
            match (a, b) {
                (Ok(a), Ok(b)) => acc.add_frame(f, &a, &b),
                (a, b) => {
                    for e in [a.err(), b.err()].into_iter().flatten() {
                        warnings.push(format!("vars frame {f}: {e}"));
                    }
                }
            }
        }
    }
    Some(acc.finish())
}

fn meta_warnings(r: &DumpReader, c: &DumpReader, w: &mut Vec<String>) {
    for (name, d) in [("reference", r), ("candidate", c)] {
        if let Some(e) = d.meta_error() {
            w.push(format!("{name} meta: {e}"));
        }
    }
    if let (Some(a), Some(b)) = (r.meta(), c.meta()) {
        let (x, y) = (&a.config, &b.config);
        if (x.width, x.height) != (y.width, y.height) {
            w.push(format!(
                "resolution differs: ref {}x{} vs cand {}x{}",
                x.width, x.height, y.width, y.height
            ));
        }
        if x.fps != y.fps {
            w.push(format!("fps differs: ref {} vs cand {}", x.fps, y.fps));
        }
        if x.seed != y.seed {
            w.push(format!("seed differs: ref {} vs cand {}", x.seed, y.seed));
        }
        if x.mesh != y.mesh {
            w.push(format!(
                "mesh differs: ref {:?} vs cand {:?}",
                x.mesh, y.mesh
            ));
        }
        if a.preset != b.preset {
            w.push(format!(
                "preset differs: ref {:?} vs cand {:?}",
                a.preset, b.preset
            ));
        }
        if a.audio != b.audio {
            w.push(format!(
                "audio differs: ref {:?} vs cand {:?}",
                a.audio, b.audio
            ));
        }
    }
}

/// Compare two dump directories. Images (if enabled) are written below `out`; `report.json`
/// and `summary.md` are written by [`write_report`].
pub fn run_diff(
    ref_dir: &Path,
    cand_dir: &Path,
    out: Option<&Path>,
    opts: &DiffOptions,
) -> Result<Report> {
    let r = DumpReader::open(ref_dir)?;
    let c = DumpReader::open(cand_dir)?;
    let mut warnings = Vec::new();
    meta_warnings(&r, &c, &mut warnings);

    let mut failures = Vec::new();
    let mut stages: Vec<Stage> = Vec::new();
    match &opts.stages {
        Some(list) => {
            for &s in list {
                let (hr, hc) = (!r.frames(s).is_empty(), !c.frames(s).is_empty());
                if !hr || !hc {
                    failures.push(format!(
                        "stage {} missing in {}",
                        s.dir(),
                        if !hr && !hc {
                            "both dumps"
                        } else if !hr {
                            "reference"
                        } else {
                            "candidate"
                        }
                    ));
                } else {
                    stages.push(s);
                }
            }
        }
        None => {
            for s in Stage::ALL {
                match (!r.frames(s).is_empty(), !c.frames(s).is_empty()) {
                    (true, true) => stages.push(s),
                    (true, false) => {
                        warnings.push(format!("stage {} only in reference; skipped", s.dir()))
                    }
                    (false, true) => {
                        warnings.push(format!("stage {} only in candidate; skipped", s.dir()))
                    }
                    _ => {}
                }
            }
        }
    }
    if stages.is_empty() && failures.is_empty() {
        return Err(Error::Msg(format!(
            "no stage present in both {} and {}",
            ref_dir.display(),
            cand_dir.display()
        )));
    }

    let mut stage_reports = Vec::new();
    for s in stages {
        let sr = compare_stage(&r, &c, s, opts, out);
        if sr.summary.frames == 0 && sr.errors.is_empty() && sr.missing_in_cand.is_empty() {
            warnings.push(format!(
                "stage {}: no common frames in range {}",
                s.dir(),
                opts.frames.describe()
            ));
        }
        stage_reports.push(sr);
    }

    let vars = if opts.vars {
        compare_vars(&r, &c, opts, &mut warnings)
    } else {
        None
    };

    for sr in &stage_reports {
        let name = sr.stage.dir();
        let s = &sr.summary;
        if s.frames == 0 {
            failures.push(format!("{name}: no frames compared"));
        }
        if !sr.errors.is_empty() {
            failures.push(format!(
                "{name}: {} frame errors (first: frame {}: {})",
                sr.errors.len(),
                sr.errors[0].frame,
                sr.errors[0].error
            ));
        }
        if !sr.missing_in_cand.is_empty() {
            failures.push(format!(
                "{name}: {} frames missing in candidate (first {})",
                sr.missing_in_cand.len(),
                sr.missing_in_cand[0]
            ));
        }
        if let Some(f) = s.first_frame_below_gate {
            failures.push(format!(
                "{name}: {} frames below gate [{}], first at frame {f}, min PSNR {} dB, min SSIM {}",
                s.frames_below_gate,
                opts.gate.describe(),
                fmt_f(s.psnr_min, 2),
                fmt_f(s.ssim_min, 4)
            ));
        }
    }
    Ok(Report {
        schema: REPORT_SCHEMA,
        mode: opts.mode,
        tool: format!("fdcompare {}", env!("CARGO_PKG_VERSION")),
        ref_dir: ref_dir.display().to_string(),
        cand_dir: cand_dir.display().to_string(),
        ref_meta: r.meta().cloned(),
        cand_meta: c.meta().cloned(),
        gate: opts.gate,
        frame_range: opts.frames.describe(),
        stages: stage_reports,
        vars,
        warnings,
        passed: failures.is_empty(),
        failures,
    })
}

/// Formats `v` with `prec` decimals; non-finite values print as `nan`, `inf`, `-inf`.
pub fn fmt_f(v: f64, prec: usize) -> String {
    if v.is_nan() {
        "nan".into()
    } else if v.is_infinite() {
        if v > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        format!("{v:.prec$}")
    }
}

/// Write `report.json` and `summary.md` into `out` (created if needed).
pub fn write_report(report: &Report, out: &Path) -> Result<()> {
    std::fs::create_dir_all(out).map_err(|e| Error::io(out, e))?;
    let jp = out.join("report.json");
    let text = serde_json::to_string_pretty(report).map_err(|e| Error::fmt(&jp, e))?;
    std::fs::write(&jp, text).map_err(|e| Error::io(&jp, e))?;
    let mp = out.join("summary.md");
    std::fs::write(&mp, render_summary_md(report)).map_err(|e| Error::io(&mp, e))
}

/// Renders the Markdown summary written to `summary.md`.
pub fn render_summary_md(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# fdcompare report — {}\n", r.mode.label());
    let _ = writeln!(s, "- reference: `{}`", r.ref_dir);
    let _ = writeln!(s, "- candidate: `{}`", r.cand_dir);
    if let Some(m) = &r.ref_meta {
        let _ = writeln!(
            s,
            "- ref producer: {} (preset `{}`, audio `{}`)",
            m.producer, m.preset, m.audio
        );
    }
    if let Some(m) = &r.cand_meta {
        let _ = writeln!(
            s,
            "- cand producer: {} (preset `{}`, audio `{}`)",
            m.producer, m.preset, m.audio
        );
    }
    let _ = writeln!(
        s,
        "- frames: {}; gate: {}",
        r.frame_range,
        r.gate.describe()
    );
    let _ = writeln!(
        s,
        "- result: **{}**\n",
        if r.passed { "PASS" } else { "FAIL" }
    );
    if !r.warnings.is_empty() {
        let _ = writeln!(s, "## Warnings\n");
        for w in &r.warnings {
            let _ = writeln!(s, "- {w}");
        }
        s.push('\n');
    }
    if !r.failures.is_empty() {
        let _ = writeln!(s, "## Failures\n");
        for w in &r.failures {
            let _ = writeln!(s, "- {w}");
        }
        s.push('\n');
    }
    let _ = writeln!(s, "## Stages\n");
    let _ = writeln!(
        s,
        "| stage | frames | PSNR min | PSNR median | PSNR mean* | SSIM min | SSIM median | max abs | first below gate | below gate | status |"
    );
    let _ = writeln!(s, "|---|---|---|---|---|---|---|---|---|---|---|");
    for sr in &r.stages {
        let m = &sr.summary;
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            sr.stage.dir(),
            m.frames,
            fmt_f(m.psnr_min, 2),
            fmt_f(m.psnr_median, 2),
            fmt_f(m.psnr_mean, 2),
            fmt_f(m.ssim_min, 4),
            fmt_f(m.ssim_median, 4),
            m.max_abs_max,
            m.first_frame_below_gate
                .map_or("-".into(), |f| f.to_string()),
            m.frames_below_gate,
            if m.passed { "pass" } else { "FAIL" }
        );
    }
    let _ = writeln!(
        s,
        "\n\\* per-frame PSNR capped at {PSNR_MEAN_CAP} dB before averaging.\n"
    );
    for sr in &r.stages {
        if sr.summary.worst_frames.is_empty() {
            continue;
        }
        let _ = writeln!(s, "### Worst frames: {}\n", sr.stage.dir());
        let _ = writeln!(
            s,
            "| frame | PSNR | SSIM | mean abs | max abs (r,g,b) | >2 | >8 | >32 |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|---|---|");
        for f in &sr.summary.worst_frames {
            if let Some(e) = sr.frames.iter().find(|e| e.frame == *f) {
                let m = &e.metrics;
                let _ = writeln!(
                    s,
                    "| {} | {} | {} | {} | {},{},{} | {:.4} | {:.4} | {:.4} |",
                    f,
                    fmt_f(m.psnr, 2),
                    fmt_f(m.ssim, 4),
                    fmt_f(m.mean_abs, 3),
                    m.max_abs[0],
                    m.max_abs[1],
                    m.max_abs[2],
                    m.frac_gt2,
                    m.frac_gt8,
                    m.frac_gt32
                );
            }
        }
        s.push('\n');
        for p in sr
            .images
            .iter()
            .filter(|p| p.ends_with("_triptych.png"))
            .take(8)
        {
            let _ = writeln!(s, "![{p}]({p})\n");
        }
    }
    if let Some(v) = &r.vars {
        let _ = writeln!(s, "## Variables\n");
        let _ = writeln!(
            s,
            "{} frames compared, {} diverging vars (first at frame {}), {} only in ref, {} only in cand.\n",
            v.frames_compared,
            v.diverging_vars,
            v.first_diverging_frame
                .map_or("-".into(), |f| f.to_string()),
            v.only_in_ref.len(),
            v.only_in_cand.len()
        );
        if v.diverging_vars > 0 {
            let _ = writeln!(
                s,
                "| var | first diverging | frames diverging | max abs | max rel |"
            );
            let _ = writeln!(s, "|---|---|---|---|---|");
            for x in v.vars.iter().filter(|x| x.frames_diverging > 0).take(15) {
                let _ = writeln!(
                    s,
                    "| {} | {} | {}/{} | {:e} (f{}) | {:e} (f{}) |",
                    x.name,
                    x.first_diverging_frame.unwrap_or(0),
                    x.frames_diverging,
                    x.frames_compared,
                    x.max_abs,
                    x.max_abs_frame,
                    x.max_rel,
                    x.max_rel_frame
                );
            }
        }
    }
    s
}

/// Short human-readable console summary.
pub fn render_console(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{}: {} vs {} [{}]",
        if r.passed { "PASS" } else { "FAIL" },
        r.ref_dir,
        r.cand_dir,
        r.mode.label()
    );
    let _ = writeln!(
        s,
        "{:<8} {:>6} {:>9} {:>9} {:>9} {:>8} {:>8} {:>7} {:>6}",
        "stage",
        "frames",
        "PSNRmin",
        "PSNRmed",
        "PSNRmean",
        "SSIMmin",
        "SSIMmed",
        "1stbad",
        "status"
    );
    for sr in &r.stages {
        let m = &sr.summary;
        let _ = writeln!(
            s,
            "{:<8} {:>6} {:>9} {:>9} {:>9} {:>8} {:>8} {:>7} {:>6}",
            sr.stage.dir(),
            m.frames,
            fmt_f(m.psnr_min, 2),
            fmt_f(m.psnr_median, 2),
            fmt_f(m.psnr_mean, 2),
            fmt_f(m.ssim_min, 4),
            fmt_f(m.ssim_median, 4),
            m.first_frame_below_gate
                .map_or("-".into(), |f| f.to_string()),
            if m.passed { "pass" } else { "FAIL" }
        );
    }
    if let Some(v) = &r.vars {
        let _ = writeln!(
            s,
            "vars: {} diverging of {} (first at frame {})",
            v.diverging_vars,
            v.vars.len(),
            v.first_diverging_frame
                .map_or("-".into(), |f| f.to_string())
        );
    }
    for w in &r.warnings {
        let _ = writeln!(s, "warning: {w}");
    }
    for f in &r.failures {
        let _ = writeln!(s, "failure: {f}");
    }
    s
}

/// Load a PNG pair helper used by callers needing both images.
pub fn load_pair(
    r: &DumpReader,
    c: &DumpReader,
    stage: Stage,
    frame: u32,
) -> Result<(Image, Image)> {
    Ok((r.load_stage(stage, frame)?, c.load_stage(stage, frame)?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::dump::DumpWriter;
    use lactea_abi::RunConfig;

    pub(crate) fn test_meta(w: u32, h: u32, frames: u32) -> DumpMeta {
        DumpMeta {
            producer: "test".into(),
            preset: "p.milk".into(),
            audio: "synth128".into(),
            config: RunConfig {
                width: w,
                height: h,
                frames,
                ..RunConfig::default()
            },
            stages: vec![Stage::Final, Stage::Main],
            has_vars: true,
        }
    }

    /// Gradient test image with a per-frame phase.
    pub(crate) fn gradient(w: u32, h: u32, phase: u32) -> Image {
        let mut img = Image::solid(w, h, [0, 0, 0, 255]);
        for y in 0..h {
            for x in 0..w {
                img.set_pixel(
                    x,
                    y,
                    [
                        ((x * 255 / w) + phase) as u8,
                        (y * 255 / h) as u8,
                        ((x + y + phase) * 3) as u8,
                        255,
                    ],
                );
            }
        }
        img
    }

    /// Write a dump of `frames` frames; `perturb(frame, &mut img)` modifies candidate frames.
    pub(crate) fn make_pair(
        root: &Path,
        frames: u32,
        perturb: impl Fn(u32, &mut Image),
        cand_var_shift: f64,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let (rd, cd) = (root.join("ref"), root.join("cand"));
        let rw = DumpWriter::create(&rd, test_meta(32, 24, frames)).unwrap();
        let cw = DumpWriter::create(&cd, test_meta(32, 24, frames)).unwrap();
        for f in 0..frames {
            for stage in [Stage::Final, Stage::Main] {
                let a = gradient(32, 24, f);
                let mut b = a.clone();
                perturb(f, &mut b);
                rw.write_stage(stage, f, 32, 24, &a.rgba).unwrap();
                cw.write_stage(stage, f, 32, 24, &b.rgba).unwrap();
            }
            let mut v = std::collections::BTreeMap::new();
            v.insert("zoom".to_string(), 1.0);
            v.insert("q1".to_string(), f as f64);
            rw.write_vars(f, &v).unwrap();
            if f >= 2 {
                v.insert("q1".to_string(), f as f64 + cand_var_shift);
            }
            cw.write_vars(f, &v).unwrap();
        }
        (rd, cd)
    }

    #[test]
    fn gate_and_range_parsing() {
        let g = Gate::parse("psnr=40,ssim=0.98").unwrap();
        assert_eq!((g.psnr, g.ssim), (Some(40.0), Some(0.98)));
        assert!(Gate::parse("foo=1").is_err());
        assert!(Gate::parse("psnr").is_err());
        let r = FrameRange::parse("3..6").unwrap();
        assert!(!r.contains(2) && r.contains(3) && r.contains(5) && !r.contains(6));
        let r = FrameRange::parse("3..=6").unwrap();
        assert!(r.contains(6) && !r.contains(7));
        let r = FrameRange::parse("..4").unwrap();
        assert!(r.contains(0) && !r.contains(4));
        assert!(FrameRange::parse("5..").unwrap().contains(1000));
        assert!(FrameRange::parse("7").unwrap().contains(7));
        assert!(FrameRange::parse("a..b").is_err());
    }

    #[test]
    fn diff_identical_then_perturbed() {
        let td = tempfile::tempdir().unwrap();
        let (rd, cd) = make_pair(td.path(), 6, |_, _| {}, 0.0);
        let opts = DiffOptions {
            gate: Gate {
                psnr: Some(40.0),
                ssim: Some(0.98),
            },
            jobs: 2,
            ..Default::default()
        };
        let rep = run_diff(&rd, &cd, None, &opts).unwrap();
        assert!(rep.passed, "{:?}", rep.failures);
        assert_eq!(rep.stages.len(), 2);
        assert_eq!(rep.stages[0].summary.psnr_min, f64::INFINITY);
        assert_eq!(rep.stages[0].summary.psnr_mean, PSNR_MEAN_CAP);
        assert!(rep.stages[0].summary.worst_frames.is_empty());
        assert_eq!(rep.vars.as_ref().unwrap().diverging_vars, 0);

        // frames >= 3 get a +10 offset on every channel -> PSNR = 10log10(65025/100) = 28.13
        let td2 = tempfile::tempdir().unwrap();
        let (rd, cd) = make_pair(
            td2.path(),
            6,
            |f, img| {
                if f >= 3 {
                    for p in img.rgba.chunks_exact_mut(4) {
                        // gradient channels stay below 245 for this size
                        p[0] = p[0].saturating_sub(10);
                        p[1] = p[1].saturating_sub(10);
                        p[2] = p[2].saturating_sub(10);
                    }
                }
            },
            0.5,
        );
        let out = td2.path().join("report");
        let rep = run_diff(&rd, &cd, Some(&out), &opts).unwrap();
        assert!(!rep.passed);
        let fin = &rep.stages[0];
        assert_eq!(fin.summary.first_frame_below_gate, Some(3));
        assert_eq!(fin.summary.frames_below_gate, 3);
        assert_eq!(fin.summary.worst_frames.len(), 3);
        assert!(fin.summary.psnr_min > 20.0 && fin.summary.psnr_min < 29.0);
        assert!(!fin.images.is_empty());
        for p in &fin.images {
            assert!(out.join(p).exists(), "{p}");
        }
        let v = rep.vars.as_ref().unwrap();
        assert_eq!(v.first_diverging_frame, Some(2));
        assert_eq!(v.vars[0].name, "q1");

        write_report(&rep, &out).unwrap();
        let back: Report =
            serde_json::from_str(&std::fs::read_to_string(out.join("report.json")).unwrap())
                .unwrap();
        assert_eq!(back.stages[0].summary.frames_below_gate, 3);
        assert_eq!(back.mode, Mode::FreeRun);
        assert_eq!(back.stages[1].frames.len(), 6);
        assert_eq!(back.stages[0].frames[0].metrics.psnr, f64::INFINITY);
        let md = std::fs::read_to_string(out.join("summary.md")).unwrap();
        assert!(md.contains("FAIL") && md.contains("Worst frames"));
    }

    #[test]
    fn step_mode_flag_and_ranges_and_missing_frames() {
        let td = tempfile::tempdir().unwrap();
        let (rd, cd) = make_pair(td.path(), 5, |_, _| {}, 0.0);
        std::fs::remove_file(cd.join("final").join("00004.png")).unwrap();
        let opts = DiffOptions {
            mode: Mode::SingleStep,
            stages: Some(vec![Stage::Final]),
            jobs: 1,
            ..Default::default()
        };
        let rep = run_diff(&rd, &cd, None, &opts).unwrap();
        assert_eq!(rep.mode, Mode::SingleStep);
        assert_eq!(rep.stages.len(), 1);
        assert_eq!(rep.stages[0].missing_in_cand, vec![4]);
        assert!(!rep.passed);
        let json = serde_json::to_string(&rep).unwrap();
        assert!(json.contains("\"mode\":\"single_step\""));
        let opts = DiffOptions {
            frames: FrameRange::parse("0..4").unwrap(),
            stages: Some(vec![Stage::Final]),
            ..Default::default()
        };
        let rep = run_diff(&rd, &cd, None, &opts).unwrap();
        assert!(rep.passed && rep.stages[0].summary.frames == 4);
        // missing stage requested explicitly -> failure, absent stage by default -> warning
        let opts = DiffOptions {
            stages: Some(vec![Stage::Blur1]),
            ..Default::default()
        };
        assert!(!run_diff(&rd, &cd, None, &opts).unwrap().passed);
    }

    #[test]
    fn size_mismatch_reported_as_error() {
        let td = tempfile::tempdir().unwrap();
        let (rd, cd) = make_pair(td.path(), 2, |_, _| {}, 0.0);
        let w = DumpWriter::create(&cd, test_meta(16, 12, 2)).unwrap();
        w.write_stage(Stage::Final, 1, 16, 12, &vec![0u8; 16 * 12 * 4])
            .unwrap();
        let rep = run_diff(&rd, &cd, None, &DiffOptions::default()).unwrap();
        assert!(!rep.passed);
        assert_eq!(rep.stages[0].errors.len(), 1);
        assert!(rep.warnings.iter().any(|w| w.contains("resolution")));
    }
}
