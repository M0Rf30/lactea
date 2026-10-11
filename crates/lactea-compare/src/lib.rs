//! Fidelity tooling: dump reader/writer, image and variable diffs, reports.
//!
//! Layers:
//! * [`dump`]   – [`DumpWriter`] / [`DumpReader`] for the layout defined in `lactea_abi::dump`.
//! * [`metrics`] – per-image-pair metrics (max-abs, MAE, RMSE, PSNR, SSIM, fractions) and
//!   visual artefacts (heat-map, triptych, contact sheet).
//! * [`vars`]   – per-variable diffs between two sequences of per-frame variable maps.
//! * [`report`] – the `diff` / `step` engine producing `report.json` + `summary.md` + images.
//! * [`corpus`] – aggregation over many preset dumps, `corpus.json`, `index.html`, regressions.
//!
//! See the crate README for the on-disk conventions (NaN/Inf encoding in `vars/*.json`).
#![allow(clippy::chunks_exact_to_as_chunks)]
#![warn(missing_docs)]

pub mod corpus;
pub mod dump;
pub mod metrics;
pub mod report;
pub mod util;
pub mod vars;

pub use dump::{DumpReader, DumpWriter, Image, encode_var, parse_vars, vars_to_json};
pub use metrics::{
    ImageMetrics, MetricOpts, compare_images, contact_sheet, diff_heatmap, psnr_from_mse,
    ssim_luma, triptych,
};
pub use report::{DiffOptions, FrameRange, Gate, ImagesMode, Mode, Report, run_diff, write_report};
pub use vars::{VarStat, VarTol, VarsAccumulator, VarsDiff};

/// Crate error type.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An I/O operation failed.
    #[error("{path}: {source}")]
    Io {
        /// File the operation failed on.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Malformed or unexpected file content.
    #[error("{path}: {msg}")]
    Format {
        /// Offending file.
        path: std::path::PathBuf,
        /// Description of what is malformed.
        msg: String,
    },
    /// Free-form error message.
    #[error("{0}")]
    Msg(String),
}

/// Crate result alias defaulting to [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Builds an [`Error::Io`] for a failed operation on `path`.
    pub fn io(path: impl Into<std::path::PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    /// Builds an [`Error::Format`] for malformed content in `path`.
    pub fn fmt(path: impl Into<std::path::PathBuf>, msg: impl std::fmt::Display) -> Self {
        Error::Format {
            path: path.into(),
            msg: msg.to_string(),
        }
    }
}
