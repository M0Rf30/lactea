//! `fdcompare` — fidelity comparison CLI. See the crate README.

use clap::{Args, Parser, Subcommand};
use lactea_abi::dump::Stage;
use lactea_compare::corpus::{
    CorpusOptions, DEFAULT_CORPUS_GATE, Status, find_regressions, merge_reports, read_corpus,
    render_table, run_corpus, write_corpus,
};
use lactea_compare::dump::parse_stage;
use lactea_compare::metrics::{MetricOpts, resize_box};
use lactea_compare::report::{
    DiffOptions, FrameRange, Gate, ImagesMode, Mode, compare_vars, fmt_f, render_console, run_diff,
    write_report,
};
use lactea_compare::util::{default_jobs, par_map};
use lactea_compare::vars::VarTol;
use lactea_compare::{DumpReader, Error, Result, contact_sheet};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "fdcompare", version, about = "lactea fidelity comparison tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct Common {
    /// Comma-separated stages (final,main,warped,blur1,blur2,blur3). Default: all present in both.
    #[arg(long)]
    stages: Option<String>,
    /// Frame range: A..B (half-open), A..=B, A.., ..B or N.
    #[arg(long)]
    frames: Option<String>,
    /// Gate, e.g. `psnr=40,ssim=0.98`. A frame below either fails; exit code 1 if any does.
    #[arg(long)]
    gate: Option<String>,
    /// Which diff images to write: worst | all | none.
    #[arg(long, default_value = "worst")]
    images: String,
    /// Number of worst frames per stage to list/render.
    #[arg(long)]
    worst: Option<usize>,
    /// Worker threads (default: all cores).
    #[arg(long)]
    jobs: Option<usize>,
    /// Count alpha as a fourth channel in mean/RMSE/PSNR/fractions.
    #[arg(long)]
    alpha: bool,
    /// Skip SSIM (faster).
    #[arg(long)]
    no_ssim: bool,
    /// Skip variable comparison.
    #[arg(long)]
    no_vars: bool,
    /// Variable relative tolerance.
    #[arg(long, default_value_t = 1e-6)]
    var_rel_tol: f64,
    /// Variable absolute tolerance.
    #[arg(long, default_value_t = 1e-9)]
    var_abs_tol: f64,
}

impl Common {
    fn to_opts(&self, mode: Mode, default_gate: Gate, default_worst: usize) -> Result<DiffOptions> {
        let stages = match &self.stages {
            None => None,
            Some(s) => Some(
                s.split(',')
                    .map(|x| {
                        parse_stage(x).ok_or_else(|| Error::Msg(format!("unknown stage {x:?}")))
                    })
                    .collect::<Result<Vec<Stage>>>()?,
            ),
        };
        Ok(DiffOptions {
            stages,
            frames: self
                .frames
                .as_deref()
                .map(FrameRange::parse)
                .transpose()?
                .unwrap_or_default(),
            gate: self
                .gate
                .as_deref()
                .map(Gate::parse)
                .transpose()?
                .unwrap_or(default_gate),
            images: self.images.parse::<ImagesMode>()?,
            worst_n: self.worst.unwrap_or(default_worst),
            mode,
            jobs: self.jobs.unwrap_or_else(default_jobs),
            metric: MetricOpts {
                include_alpha: self.alpha,
                ssim: !self.no_ssim,
            },
            var_tol: VarTol {
                rel: self.var_rel_tol,
                abs: self.var_abs_tol,
            },
            vars: !self.no_vars,
        })
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Compare two dumps (free run). Writes report.json, summary.md, images.
    Diff {
        reference: PathBuf,
        candidate: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[command(flatten)]
        common: Common,
    },
    /// Same as diff, labelled as a teacher-forced single-step comparison (`mode: single_step`).
    Step {
        reference: PathBuf,
        candidate: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[command(flatten)]
        common: Common,
    },
    /// Aggregate over matching preset dumps below two roots.
    Corpus {
        ref_root: PathBuf,
        cand_root: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Previous corpus.json; regressions make the exit code non-zero.
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Only presets whose relative path contains this text.
        #[arg(long)]
        filter: Option<String>,
        /// Only the presets listed in this file (one relative path / id per line; exact match).
        #[arg(long)]
        ids_file: Option<PathBuf>,
        /// Max-PSNR-drop (dB) allowed vs baseline before flagging a regression.
        #[arg(long, default_value_t = 0.5)]
        regress_psnr: f64,
        /// Max SSIM drop allowed vs baseline.
        #[arg(long, default_value_t = 0.005)]
        regress_ssim: f64,
        /// Also exit non-zero when any preset fails its gate.
        #[arg(long)]
        strict: bool,
        /// Max table rows printed to stdout (failing rows first-come).
        #[arg(long, default_value_t = 60)]
        rows: usize,
        #[command(flatten)]
        common: Common,
    },
    /// Merge the corpus.json of disjoint chunks (same `--out` tree) into one corpus.json + index.html.
    CorpusMerge {
        #[arg(long)]
        out: PathBuf,
        /// Previous corpus.json to check the merged result against (regressions -> exit 1).
        #[arg(long)]
        baseline: Option<PathBuf>,
        #[arg(long, default_value_t = 0.5)]
        regress_psnr: f64,
        #[arg(long, default_value_t = 0.005)]
        regress_ssim: f64,
        /// Chunk reports to merge.
        parts: Vec<PathBuf>,
    },
    /// Contact sheet of one stage of one dump.
    Sheet {
        dir: PathBuf,
        #[arg(long, default_value = "final")]
        stage: String,
        #[arg(long)]
        out: PathBuf,
        /// Take every Nth frame (default: auto, at most 48 cells).
        #[arg(long)]
        every: Option<usize>,
        #[arg(long)]
        frames: Option<String>,
        #[arg(long)]
        cols: Option<u32>,
        #[arg(long, default_value_t = 160)]
        thumb_width: u32,
    },
    /// Text table of the worst-diverging variables.
    Vars {
        reference: PathBuf,
        candidate: PathBuf,
        #[arg(long, default_value_t = 20)]
        top: usize,
        #[arg(long)]
        frames: Option<String>,
        /// Include variables that never diverge.
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 1e-6)]
        rel_tol: f64,
        #[arg(long, default_value_t = 1e-9)]
        abs_tol: f64,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fdcompare: error: {e}");
            ExitCode::from(2)
        }
    }
}

fn run_pair(
    mode: Mode,
    reference: PathBuf,
    candidate: PathBuf,
    out: PathBuf,
    common: Common,
) -> Result<ExitCode> {
    let opts = common.to_opts(mode, Gate::default(), 5)?;
    let report = run_diff(&reference, &candidate, Some(&out), &opts)?;
    write_report(&report, &out)?;
    print!("{}", render_console(&report));
    println!("report: {}", out.join("report.json").display());
    Ok(if report.passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Diff {
            reference,
            candidate,
            out,
            common,
        } => run_pair(Mode::FreeRun, reference, candidate, out, common),
        Cmd::Step {
            reference,
            candidate,
            out,
            common,
        } => run_pair(Mode::SingleStep, reference, candidate, out, common),
        Cmd::Corpus {
            ref_root,
            cand_root,
            out,
            baseline,
            filter,
            ids_file,
            regress_psnr,
            regress_ssim,
            strict,
            rows,
            common,
        } => {
            let diff = common.to_opts(Mode::FreeRun, DEFAULT_CORPUS_GATE, 1)?;
            let ids = match &ids_file {
                Some(p) => Some(
                    std::fs::read_to_string(p)
                        .map_err(|e| Error::io(p, e))?
                        .lines()
                        .filter(|l| !l.is_empty())
                        .map(String::from)
                        .collect(),
                ),
                None => None,
            };
            let opts = CorpusOptions {
                ids,
                jobs: common.jobs.unwrap_or_else(default_jobs),
                diff,
                filter,
                baseline,
                regress_psnr_db: regress_psnr,
                regress_ssim,
            };
            let rep = run_corpus(&ref_root, &cand_root, &out, &opts)?;
            print!("{}", render_table(&rep, rows, false));
            println!(
                "corpus: {}  (open {})",
                out.join("corpus.json").display(),
                out.join("index.html").display()
            );
            let bad = !rep.regressions.is_empty()
                || (strict && rep.entries.iter().any(|e| e.status != Status::Pass));
            Ok(if bad {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Cmd::CorpusMerge {
            out,
            baseline,
            regress_psnr,
            regress_ssim,
            parts,
        } => {
            let reps = parts
                .iter()
                .map(|p| read_corpus(p))
                .collect::<Result<Vec<_>>>()?;
            let mut rep = merge_reports(reps)
                .ok_or_else(|| Error::Msg("corpus-merge: no parts given".into()))?;
            if let Some(bp) = &baseline {
                let base = read_corpus(bp)?;
                rep.baseline = Some(bp.display().to_string());
                rep.regressions = find_regressions(&base, &rep, regress_psnr, regress_ssim);
            }
            std::fs::create_dir_all(&out).map_err(|e| Error::io(&out, e))?;
            write_corpus(&rep, &out)?;
            println!(
                "merged {} parts: {} presets -> {}",
                parts.len(),
                rep.entries.len(),
                out.join("corpus.json").display()
            );
            for r in &rep.regressions {
                println!("REGRESSION {} {}", r.preset, r.reason);
            }
            Ok(if rep.regressions.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Cmd::Sheet {
            dir,
            stage,
            out,
            every,
            frames,
            cols,
            thumb_width,
        } => {
            let st = parse_stage(&stage)
                .ok_or_else(|| Error::Msg(format!("unknown stage {stage:?}")))?;
            let r = DumpReader::open(&dir)?;
            let range = frames
                .as_deref()
                .map(FrameRange::parse)
                .transpose()?
                .unwrap_or_default();
            let avail: Vec<u32> = r
                .frames(st)
                .iter()
                .copied()
                .filter(|f| range.contains(*f))
                .collect();
            if avail.is_empty() {
                return Err(Error::Msg(format!(
                    "{}: no frames for stage {stage}",
                    dir.display()
                )));
            }
            let every = every.unwrap_or_else(|| avail.len().div_ceil(48)).max(1);
            let picked: Vec<u32> = avail.iter().copied().step_by(every).collect();
            let thumbs = par_map(&picked, default_jobs(), |&f| {
                r.load_stage(st, f).map(|img| {
                    let tw = thumb_width.min(img.width).max(1);
                    let th =
                        (img.height as u64 * tw as u64 / img.width.max(1) as u64).max(1) as u32;
                    (f, resize_box(&img, tw, th))
                })
            });
            let thumbs: Vec<_> = thumbs.into_iter().collect::<Result<_>>()?;
            let cols = cols
                .unwrap_or_else(|| ((picked.len() as f64).sqrt().ceil() as u32).max(1) * 4 / 3)
                .max(1);
            let sheet = contact_sheet(&thumbs, cols, thumb_width);
            sheet.save_png(&out)?;
            println!(
                "{} frames -> {} ({}x{})",
                picked.len(),
                out.display(),
                sheet.width,
                sheet.height
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Vars {
            reference,
            candidate,
            top,
            frames,
            all,
            rel_tol,
            abs_tol,
        } => {
            let r = DumpReader::open(&reference)?;
            let c = DumpReader::open(&candidate)?;
            let opts = DiffOptions {
                frames: frames
                    .as_deref()
                    .map(FrameRange::parse)
                    .transpose()?
                    .unwrap_or_default(),
                var_tol: VarTol {
                    rel: rel_tol,
                    abs: abs_tol,
                },
                ..Default::default()
            };
            let mut warnings = Vec::new();
            let Some(d) = compare_vars(&r, &c, &opts, &mut warnings) else {
                return Err(Error::Msg("vars/ missing in one or both dumps".into()));
            };
            for w in warnings.iter().take(10) {
                eprintln!("warning: {w}");
            }
            println!(
                "{} frames compared; {} vars compared, {} diverging (first diverging frame: {})",
                d.frames_compared,
                d.vars.len(),
                d.diverging_vars,
                d.first_diverging_frame
                    .map_or("-".into(), |f| f.to_string())
            );
            if d.frames_only_in_ref + d.frames_only_in_cand > 0 {
                println!(
                    "frames only in ref: {}, only in cand: {}",
                    d.frames_only_in_ref, d.frames_only_in_cand
                );
            }
            println!(
                "{:<28} {:>9} {:>11} {:>22} {:>22}",
                "var", "1st div", "div/frames", "max abs (frame)", "max rel (frame)"
            );
            for v in d
                .vars
                .iter()
                .filter(|v| all || v.frames_diverging > 0)
                .take(top)
            {
                println!(
                    "{:<28} {:>9} {:>11} {:>22} {:>22}",
                    v.name,
                    v.first_diverging_frame
                        .map_or("-".into(), |f| f.to_string()),
                    format!("{}/{}", v.frames_diverging, v.frames_compared),
                    format!("{} ({})", sci(v.max_abs), v.max_abs_frame),
                    format!("{} ({})", sci(v.max_rel), v.max_rel_frame)
                );
            }
            if !d.only_in_ref.is_empty() {
                println!(
                    "only in reference ({}): {}",
                    d.only_in_ref.len(),
                    d.only_in_ref.join(" ")
                );
            }
            if !d.only_in_cand.is_empty() {
                println!(
                    "only in candidate ({}): {}",
                    d.only_in_cand.len(),
                    d.only_in_cand.join(" ")
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn sci(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.3e}")
    } else {
        fmt_f(v, 0)
    }
}
