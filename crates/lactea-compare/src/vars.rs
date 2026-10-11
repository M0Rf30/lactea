//! Per-variable diffs across frames.
//!
//! Definitions (a = reference, b = candidate):
//! * `abs = |a-b|` (0 if both NaN or equal infinities; `inf` if exactly one is NaN or the
//!   infinities differ);
//! * `rel = abs / max(|a|,|b|)` (0 when both are 0);
//! * a value *diverges* when it is not equal under `abs > abs_tol + rel_tol*max(|a|,|b|)`
//!   (default `abs_tol = 1e-9`, `rel_tol = 1e-6`); NaN vs NaN is equal.

use crate::util::jf64;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug)]
/// Tolerances deciding when two variable values diverge.
pub struct VarTol {
    /// Relative tolerance factor (scaled by the larger magnitude).
    pub rel: f64,
    /// Absolute tolerance term.
    pub abs: f64,
}

impl Default for VarTol {
    fn default() -> Self {
        Self {
            rel: 1e-6,
            abs: 1e-9,
        }
    }
}

/// Absolute difference; NaN vs NaN and equal infinities give 0, a lone NaN gives infinity.
pub fn abs_diff(a: f64, b: f64) -> f64 {
    if a.is_nan() && b.is_nan() {
        0.0
    } else if a.is_nan() || b.is_nan() {
        f64::INFINITY
    } else if a == b {
        0.0
    } else {
        (a - b).abs()
    }
}

/// Relative difference `abs / max(|a|,|b|)`; 0 when equal, infinite when undefined.
pub fn rel_diff(a: f64, b: f64) -> f64 {
    let d = abs_diff(a, b);
    if d == 0.0 {
        return 0.0;
    }
    let m = a.abs().max(b.abs());
    if m.is_nan() || m.is_infinite() || d.is_infinite() {
        f64::INFINITY
    } else {
        d / m
    }
}

/// Whether `a` and `b` differ beyond `tol` (`abs > tol.abs + tol.rel * max(|a|,|b|)`).
pub fn diverges(a: f64, b: f64, tol: VarTol) -> bool {
    let d = abs_diff(a, b);
    if d == 0.0 {
        return false;
    }
    if d.is_infinite() {
        return true;
    }
    d > tol.abs + tol.rel * a.abs().max(b.abs())
}

/// Per-variable comparison statistics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VarStat {
    /// Variable name.
    pub name: String,
    /// Frames where both sides have the variable.
    pub frames_compared: u32,
    /// Frames where the variable diverges beyond tolerance.
    pub frames_diverging: u32,
    /// Earliest frame at which the variable diverges.
    pub first_diverging_frame: Option<u32>,
    #[serde(with = "jf64")]
    /// Largest absolute difference over all compared frames.
    pub max_abs: f64,
    /// Frame of the largest absolute difference.
    pub max_abs_frame: u32,
    #[serde(with = "jf64")]
    /// Largest relative difference over all compared frames.
    pub max_rel: f64,
    /// Frame of the largest relative difference.
    pub max_rel_frame: u32,
    /// Frames where only one of the two sides had it.
    pub frames_one_sided: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
/// Aggregate result of comparing two variable sequences.
pub struct VarsDiff {
    /// Frames compared (present on both sides).
    pub frames_compared: u32,
    /// Frames present in the reference dump only.
    pub frames_only_in_ref: u32,
    /// Frames present in the candidate dump only.
    pub frames_only_in_cand: u32,
    /// Variables present in both, sorted worst first (see [`VarsDiff::rank`]).
    pub vars: Vec<VarStat>,
    /// Variables that never appear on the candidate side.
    pub only_in_ref: Vec<String>,
    /// Variables that never appear on the reference side.
    pub only_in_cand: Vec<String>,
    /// Earliest frame at which any variable diverges.
    pub first_diverging_frame: Option<u32>,
    /// Number of variables with at least one diverging frame.
    pub diverging_vars: u32,
}

impl VarsDiff {
    /// Worst-first ordering: diverging vars before clean ones, then max_rel desc, max_abs desc,
    /// first diverging frame asc, name.
    pub fn rank(vars: &mut [VarStat]) {
        vars.sort_by(|a, b| {
            (b.frames_diverging > 0)
                .cmp(&(a.frames_diverging > 0))
                .then(b.max_rel.total_cmp(&a.max_rel))
                .then(b.max_abs.total_cmp(&a.max_abs))
                .then(a.first_diverging_frame.cmp(&b.first_diverging_frame))
                .then(a.name.cmp(&b.name))
        });
    }
}

/// Streaming accumulator: feed one frame pair at a time (in any frame order).
#[derive(Debug)]
pub struct VarsAccumulator {
    tol: VarTol,
    stats: BTreeMap<String, VarStat>,
    seen_ref: BTreeSet<String>,
    seen_cand: BTreeSet<String>,
    frames_compared: u32,
    only_ref_frames: u32,
    only_cand_frames: u32,
}

impl VarsAccumulator {
    /// Creates an empty accumulator that judges divergence with `tol`.
    pub fn new(tol: VarTol) -> Self {
        Self {
            tol,
            stats: BTreeMap::new(),
            seen_ref: BTreeSet::new(),
            seen_cand: BTreeSet::new(),
            frames_compared: 0,
            only_ref_frames: 0,
            only_cand_frames: 0,
        }
    }

    /// Record a frame present in only one dump.
    pub fn add_missing_frame(&mut self, in_ref: bool) {
        if in_ref {
            self.only_ref_frames += 1;
        } else {
            self.only_cand_frames += 1;
        }
    }

    /// Compares frame `frame`: `a` is the reference map, `b` the candidate map.
    pub fn add_frame(&mut self, frame: u32, a: &BTreeMap<String, f64>, b: &BTreeMap<String, f64>) {
        self.frames_compared += 1;
        for (name, &va) in a {
            self.seen_ref.insert(name.clone());
            let Some(&vb) = b.get(name) else {
                self.stat(name).frames_one_sided += 1;
                continue;
            };
            let (abs, rel) = (abs_diff(va, vb), rel_diff(va, vb));
            let div = diverges(va, vb, self.tol);
            let s = self.stat(name);
            s.frames_compared += 1;
            if div {
                s.frames_diverging += 1;
                s.first_diverging_frame =
                    Some(s.first_diverging_frame.map_or(frame, |f| f.min(frame)));
            }
            if abs > s.max_abs || s.frames_compared == 1 {
                s.max_abs = abs;
                s.max_abs_frame = frame;
            }
            if rel > s.max_rel || s.frames_compared == 1 {
                s.max_rel = rel;
                s.max_rel_frame = frame;
            }
        }
        for name in b.keys() {
            self.seen_cand.insert(name.clone());
            if !a.contains_key(name) {
                self.stat(name).frames_one_sided += 1;
            }
        }
    }

    fn stat(&mut self, name: &str) -> &mut VarStat {
        self.stats
            .entry(name.to_string())
            .or_insert_with(|| VarStat {
                name: name.to_string(),
                frames_compared: 0,
                frames_diverging: 0,
                first_diverging_frame: None,
                max_abs: 0.0,
                max_abs_frame: 0,
                max_rel: 0.0,
                max_rel_frame: 0,
                frames_one_sided: 0,
            })
    }

    /// Finalises the accumulation: ranks variables worst-first and lists one-sided names.
    pub fn finish(self) -> VarsDiff {
        let only_in_ref: Vec<String> = self.seen_ref.difference(&self.seen_cand).cloned().collect();
        let only_in_cand: Vec<String> =
            self.seen_cand.difference(&self.seen_ref).cloned().collect();
        let mut vars: Vec<VarStat> = self
            .stats
            .into_values()
            .filter(|s| s.frames_compared > 0)
            .collect();
        VarsDiff::rank(&mut vars);
        let first = vars.iter().filter_map(|s| s.first_diverging_frame).min();
        let diverging_vars = vars.iter().filter(|s| s.frames_diverging > 0).count() as u32;
        VarsDiff {
            frames_compared: self.frames_compared,
            frames_only_in_ref: self.only_ref_frames,
            frames_only_in_cand: self.only_cand_frames,
            vars,
            only_in_ref,
            only_in_cand,
            first_diverging_frame: first,
            diverging_vars,
        }
    }
}

/// Convenience: diff two in-memory sequences of (frame, vars).
pub fn diff_sequences(
    reference: &[(u32, BTreeMap<String, f64>)],
    candidate: &[(u32, BTreeMap<String, f64>)],
    tol: VarTol,
) -> VarsDiff {
    let cmap: BTreeMap<u32, &BTreeMap<String, f64>> =
        candidate.iter().map(|(f, m)| (*f, m)).collect();
    let mut acc = VarsAccumulator::new(tol);
    let mut used = BTreeSet::new();
    for (f, m) in reference {
        match cmap.get(f) {
            Some(c) => {
                acc.add_frame(*f, m, c);
                used.insert(*f);
            }
            None => acc.add_missing_frame(true),
        }
    }
    for (f, _) in candidate {
        if !used.contains(f) {
            acc.add_missing_frame(false);
        }
    }
    acc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn diff_basics() {
        assert_eq!(abs_diff(f64::NAN, f64::NAN), 0.0);
        assert_eq!(abs_diff(f64::NAN, 1.0), f64::INFINITY);
        assert_eq!(abs_diff(f64::INFINITY, f64::INFINITY), 0.0);
        assert_eq!(rel_diff(0.0, 0.0), 0.0);
        assert!((rel_diff(100.0, 101.0) - 1.0 / 101.0).abs() < 1e-15);
        let t = VarTol::default();
        assert!(!diverges(1.0, 1.0 + 5e-7, t));
        assert!(diverges(1.0, 1.0 + 5e-6, t));
        assert!(!diverges(0.0, 5e-10, t));
        assert!(diverges(0.0, 5e-9, t));
        assert!(!diverges(f64::NAN, f64::NAN, t));
        assert!(diverges(f64::NAN, 0.0, t));
    }

    #[test]
    fn sequence_diff_first_frame_and_sidedness() {
        let r: Vec<_> = (0..5)
            .map(|f| (f, m(&[("zoom", 1.0), ("q1", f as f64), ("only_r", 1.0)])))
            .collect();
        let c: Vec<_> = (0..4)
            .map(|f| {
                let q1 = if f >= 2 { f as f64 + 0.5 } else { f as f64 };
                (f, m(&[("zoom", 1.0), ("q1", q1), ("only_c", 2.0)]))
            })
            .collect();
        let d = diff_sequences(&r, &c, VarTol::default());
        assert_eq!(d.frames_compared, 4);
        assert_eq!(d.frames_only_in_ref, 1);
        assert_eq!(d.only_in_ref, vec!["only_r".to_string()]);
        assert_eq!(d.only_in_cand, vec!["only_c".to_string()]);
        assert_eq!(d.first_diverging_frame, Some(2));
        assert_eq!(d.diverging_vars, 1);
        let q1 = &d.vars[0];
        assert_eq!(q1.name, "q1");
        assert_eq!(q1.first_diverging_frame, Some(2));
        assert_eq!(q1.frames_diverging, 2);
        assert_eq!(q1.max_abs, 0.5);
        assert_eq!(q1.max_abs_frame, 2);
        assert!((q1.max_rel - 0.5 / 2.5).abs() < 1e-15);
        assert_eq!(d.vars[1].name, "zoom");
        assert_eq!(d.vars[1].frames_diverging, 0);
    }
}
