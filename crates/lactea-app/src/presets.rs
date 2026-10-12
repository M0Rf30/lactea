//! Preset list, navigation (next/previous/random), lock and auto-advance timing.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Directories where a projectM preset pack is usually installed, in the order they are tried.
/// The first one that exists is the default for `--dir`. Windows has no conventional location, so
/// the preset directory must be passed explicitly there.
pub(crate) fn default_preset_dirs() -> &'static [&'static str] {
    if cfg!(windows) {
        &[]
    } else {
        &[
            "/usr/share/projectM/presets",
            "/usr/local/share/projectM/presets",
            "/opt/homebrew/share/projectM/presets",
        ]
    }
}

#[derive(Clone, Debug)]
pub struct PresetEntry {
    pub path: PathBuf,
    /// Display name: path relative to its scan root, without `.milk` (a `.milk2` keeps its extension).
    pub name: String,
}

/// Recursively collect `*.milk` (any case) below `dir`, and with `milk2` also `*.milk2` (double presets, which only
/// `--compat milkdrop3` plays), sorted by path. Directories are visited once (by canonical path), so symlink loops
/// and aliases cannot blow the scan up.
pub fn scan_dir(dir: &Path, milk2: bool) -> Vec<PresetEntry> {
    fn walk(dir: &Path, milk2: bool, visited: &mut HashSet<PathBuf>, out: &mut Vec<PathBuf>) {
        let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        if !visited.insert(canon) {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, milk2, visited, out);
            } else if p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("milk"))
                || (milk2 && is_milk2(&p))
            {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, milk2, &mut HashSet::new(), &mut files);
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let rel = path.strip_prefix(dir).unwrap_or(&path);
            PresetEntry {
                name: if is_milk2(&path) {
                    rel.to_string_lossy().into_owned()
                } else {
                    rel.with_extension("").to_string_lossy().into_owned()
                },
                path,
            }
        })
        .collect()
}

/// Append `found` to `out`, skipping files already present (same path, or the same file reached
/// through another spelling/symlink). Linear: canonical paths are only computed for the few
/// entries whose file name already occurs in `out`.
pub fn append_unique(out: &mut Vec<PresetEntry>, found: Vec<PresetEntry>) {
    let mut seen: HashSet<PathBuf> = out.iter().map(|e| e.path.clone()).collect();
    let names: HashSet<OsString> = out
        .iter()
        .filter_map(|e| e.path.file_name().map(|n| n.to_owned()))
        .collect();
    let mut canon: Option<HashSet<PathBuf>> = None;
    for e in found {
        if seen.contains(&e.path) {
            continue;
        }
        if e.path.file_name().is_some_and(|n| names.contains(n)) {
            let existing = canon.get_or_insert_with(|| {
                out.iter()
                    .filter_map(|x| x.path.canonicalize().ok())
                    .collect()
            });
            if e.path
                .canonicalize()
                .ok()
                .is_some_and(|c| existing.contains(&c))
            {
                continue;
            }
        }
        seen.insert(e.path.clone());
        out.push(e);
    }
}

/// Display name of a preset file: its file name without `.milk`; a `.milk2` keeps its extension (the engine takes
/// it as a hint that the file is a double preset).
pub fn entry_for_file(path: &Path) -> PresetEntry {
    let milk2 = is_milk2(path);
    PresetEntry {
        name: if milk2 {
            path.file_name()
        } else {
            path.file_stem()
        }
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string()),
        path: path.to_path_buf(),
    }
}

fn is_milk2(path: &Path) -> bool {
    path.extension()
        .and_then(|x| x.to_str())
        .is_some_and(|x| x.eq_ignore_ascii_case("milk2"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Next,
    Prev,
    Random,
    /// Timer expiry: like `Next` (or random when shuffling), ignored while locked.
    Auto,
}

pub struct PresetBrowser {
    entries: Vec<PresetEntry>,
    current: Option<usize>,
    history: Vec<usize>,
    locked: bool,
    shuffle: bool,
    duration: Duration,
    loaded_at: Instant,
    rng: fastrand::Rng,
}

impl PresetBrowser {
    pub fn new(entries: Vec<PresetEntry>, shuffle: bool, duration: Duration, seed: u64) -> Self {
        Self {
            entries,
            current: None,
            history: Vec::new(),
            locked: false,
            shuffle,
            duration,
            loaded_at: Instant::now(),
            rng: fastrand::Rng::with_seed(seed),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry(&self, idx: usize) -> &PresetEntry {
        &self.entries[idx]
    }

    pub fn current(&self) -> Option<usize> {
        self.current
    }

    pub fn current_entry(&self) -> Option<&PresetEntry> {
        self.current.map(|i| &self.entries[i])
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    pub fn toggle_lock(&mut self) -> bool {
        self.locked = !self.locked;
        // A lock toggle restarts the timer so unlocking never flips the preset immediately.
        self.loaded_at = Instant::now();
        self.locked
    }

    /// Seconds until the next automatic switch (None when locked / nothing to switch to).
    pub fn auto_remaining(&self, now: Instant) -> Option<Duration> {
        if self.locked || self.entries.len() <= 1 || self.duration.is_zero() {
            return None;
        }
        Some(
            self.duration
                .saturating_sub(now.saturating_duration_since(self.loaded_at)),
        )
    }

    pub fn auto_due(&self, now: Instant) -> bool {
        self.auto_remaining(now).is_some_and(|d| d.is_zero())
    }

    /// Index the navigation `nav` selects from the current position (does not commit).
    pub fn pick(&mut self, nav: Nav) -> Option<usize> {
        let n = self.entries.len();
        if n == 0 {
            return None;
        }
        let Some(cur) = self.current else {
            return Some(if self.shuffle && nav != Nav::Prev {
                self.rng.usize(..n)
            } else {
                0
            });
        };
        Some(match nav {
            Nav::Next | Nav::Auto if !(self.shuffle && nav == Nav::Auto) => (cur + 1) % n,
            Nav::Next | Nav::Auto | Nav::Random => self.random_other(cur),
            Nav::Prev => self.history.last().copied().unwrap_or((cur + n - 1) % n),
        })
    }

    fn random_other(&mut self, cur: usize) -> usize {
        let n = self.entries.len();
        if n == 1 {
            return 0;
        }
        let mut i = self.rng.usize(..n - 1);
        if i >= cur {
            i += 1;
        }
        i
    }

    /// Record that `idx` is now showing (after a successful load).
    pub fn commit(&mut self, idx: usize, nav: Nav, now: Instant) {
        if let Some(cur) = self.current {
            match nav {
                Nav::Prev => {
                    if self.history.last() == Some(&idx) {
                        self.history.pop();
                    }
                }
                _ if cur != idx => {
                    self.history.push(cur);
                    if self.history.len() > 256 {
                        self.history.remove(0);
                    }
                }
                _ => {}
            }
        }
        self.current = Some(idx);
        self.loaded_at = now;
    }

    /// Index to try after a failed load of `failed` during `nav` (so a broken preset is skipped
    /// in the direction of travel). `None` once every preset has been tried.
    pub fn after_failure(&mut self, failed: usize, nav: Nav, tried: usize) -> Option<usize> {
        let n = self.entries.len();
        if tried >= n {
            return None;
        }
        Some(match nav {
            Nav::Prev => (failed + n - 1) % n,
            Nav::Random => self.random_other(failed),
            Nav::Auto if self.shuffle => self.random_other(failed),
            _ => (failed + 1) % n,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(n: usize) -> Vec<PresetEntry> {
        (0..n)
            .map(|i| PresetEntry {
                path: PathBuf::from(format!("{i}.milk")),
                name: format!("{i}"),
            })
            .collect()
    }

    fn browser(n: usize) -> PresetBrowser {
        PresetBrowser::new(entries(n), false, Duration::from_secs(30), 1)
    }

    fn go(b: &mut PresetBrowser, nav: Nav) -> usize {
        let i = b.pick(nav).unwrap();
        b.commit(i, nav, Instant::now());
        i
    }

    #[test]
    fn next_wraps_and_prev_retraces() {
        let mut b = browser(3);
        assert_eq!(go(&mut b, Nav::Next), 0);
        assert_eq!(go(&mut b, Nav::Next), 1);
        assert_eq!(go(&mut b, Nav::Next), 2);
        assert_eq!(go(&mut b, Nav::Next), 0);
        assert_eq!(go(&mut b, Nav::Prev), 2);
        assert_eq!(go(&mut b, Nav::Prev), 1);
        assert_eq!(go(&mut b, Nav::Prev), 0);
        // History exhausted: wraps backwards.
        assert_eq!(go(&mut b, Nav::Prev), 2);
    }

    #[test]
    fn random_never_repeats_current() {
        let mut b = browser(5);
        go(&mut b, Nav::Next);
        for _ in 0..200 {
            let cur = b.current().unwrap();
            assert_ne!(go(&mut b, Nav::Random), cur);
        }
    }

    #[test]
    fn lock_disables_auto_advance() {
        let mut b = browser(3);
        go(&mut b, Nav::Next);
        let later = Instant::now() + Duration::from_secs(31);
        assert!(b.auto_due(later));
        assert!(b.toggle_lock());
        assert!(!b.auto_due(later + Duration::from_secs(100)));
        assert!(!b.toggle_lock());
        // Unlocking restarts the timer.
        assert!(!b.auto_due(Instant::now()));
    }

    #[test]
    fn single_preset_never_auto_advances() {
        let mut b = browser(1);
        go(&mut b, Nav::Next);
        assert!(!b.auto_due(Instant::now() + Duration::from_secs(3600)));
    }

    #[test]
    fn failure_skips_in_direction_and_terminates() {
        let mut b = browser(3);
        let mut tried = 1;
        let mut idx = 1;
        let mut seen = vec![idx];
        while let Some(next) = b.after_failure(idx, Nav::Next, tried) {
            idx = next;
            tried += 1;
            seen.push(idx);
        }
        assert_eq!(seen, vec![1, 2, 0]);
        let mut b = browser(3);
        assert_eq!(b.after_failure(0, Nav::Prev, 1), Some(2));
    }

    #[test]
    fn symlink_loops_and_aliases_are_scanned_once() {
        let d = std::env::temp_dir().join(format!("lactea-presets-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::write(d.join("a/x.milk"), b"").unwrap();
        std::os::unix::fs::symlink(&d, d.join("a/loop")).unwrap();
        std::os::unix::fs::symlink(d.join("a"), d.join("alias")).unwrap();
        let found = scan_dir(&d, false);
        assert_eq!(found.len(), 1, "{found:?}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn append_unique_dedupes_spellings_and_is_linear_for_big_lists() {
        let d = std::env::temp_dir().join(format!("lactea-presets-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("one.milk"), b"").unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let mut out = vec![entry_for_file(&d.join("sub/../one.milk"))];
        let mut found = scan_dir(&d, false);
        // 20k extra entries must not make this quadratic.
        for i in 0..20_000 {
            found.push(PresetEntry {
                path: PathBuf::from(format!("/nonexistent/{i}.milk")),
                name: format!("{i}"),
            });
        }
        let t = Instant::now();
        append_unique(&mut out, found);
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        assert_eq!(out.len(), 20_001, "one.milk listed twice");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn scan_dir_is_recursive_and_case_insensitive() {
        let d = std::env::temp_dir().join(format!("lactea-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("x.milk"), b"").unwrap();
        std::fs::write(d.join("a/y.MILK"), b"").unwrap();
        std::fs::write(d.join("a/b/z.milk"), b"").unwrap();
        std::fs::write(d.join("a/readme.txt"), b"").unwrap();
        let e = scan_dir(&d, false);
        let names: Vec<_> = e.iter().map(|e| e.name.replace('\\', "/")).collect();
        assert_eq!(names, vec!["a/b/z", "a/y", "x"]);
        let _ = std::fs::remove_dir_all(d);
    }

    /// `.milk2` double presets are listed only on request (they need `--compat milkdrop3`) and keep their extension in
    /// the name, so the engine can tell them from plain presets.
    #[test]
    fn scan_dir_lists_milk2_on_request() {
        let d = std::env::temp_dir().join(format!("lactea-presets-m2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::write(d.join("x.milk"), b"").unwrap();
        std::fs::write(d.join("a/y.milk2"), b"").unwrap();
        std::fs::write(d.join("a/z.MILK2"), b"").unwrap();
        let names = |milk2| -> Vec<String> {
            scan_dir(&d, milk2)
                .iter()
                .map(|e| e.name.replace('\\', "/"))
                .collect()
        };
        assert_eq!(names(false), vec!["x"]);
        assert_eq!(names(true), vec!["a/y.milk2", "a/z.MILK2", "x"]);
        assert_eq!(entry_for_file(&d.join("a/y.milk2")).name, "y.milk2");
        assert_eq!(entry_for_file(&d.join("x.milk")).name, "x");
        let _ = std::fs::remove_dir_all(d);
    }
}
