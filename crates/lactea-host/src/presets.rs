//! Finding preset files on disk and feeding them to a [`lactea::Playlist`]
//! (`projectm_playlist_add_path` / `projectm_playlist_insert_path`). The playlist itself is pure
//! data; the directory walk lives here.

use lactea::Playlist;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Does `path` name a preset file? `.milk` and `.prjm` always; the MilkDrop 3 double preset
/// `.milk2` only with `milk2` (only the MilkDrop 3 compat modes can play it). Extensions are matched
/// case-insensitively.
pub fn is_preset_file(path: &Path, milk2: bool) -> bool {
    path.extension().and_then(|x| x.to_str()).is_some_and(|x| {
        x.eq_ignore_ascii_case("milk")
            || x.eq_ignore_ascii_case("prjm")
            || (milk2 && x.eq_ignore_ascii_case("milk2"))
    })
}

/// Preset files (`.milk`, `.prjm`, `.milk2`) in `dir`, also below its subdirectories with
/// `recursive`, sorted by path so the playlist order does not depend on the filesystem. See
/// [`scan_preset_files`].
pub fn scan_presets(dir: &Path, recursive: bool) -> Vec<PathBuf> {
    scan_preset_files(dir, recursive, true)
}

/// [`scan_presets`] with `.milk2` optional. A directory is visited once (by canonical path), so
/// symlink loops and aliases cannot blow the scan up; unreadable directories are skipped. A `dir`
/// that is not a readable directory gives an empty list.
pub fn scan_preset_files(dir: &Path, recursive: bool, milk2: bool) -> Vec<PathBuf> {
    fn walk(
        dir: &Path,
        recursive: bool,
        milk2: bool,
        visited: &mut HashSet<PathBuf>,
        out: &mut Vec<PathBuf>,
    ) {
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
                if recursive {
                    walk(&p, recursive, milk2, visited, out);
                }
            } else if is_preset_file(&p, milk2) && p.is_file() {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, recursive, milk2, &mut HashSet::new(), &mut files);
    files.sort();
    files
}

/// Directory scanning for [`Playlist`] (the library crate cannot touch the filesystem).
pub trait PlaylistFs {
    /// `projectm_playlist_add_path`: appends the presets [`scan_presets`] finds in `dir`, in sorted
    /// order. Returns how many the playlist accepted (its filter and `allow_duplicates` apply).
    fn add_path(&mut self, dir: &Path, recursive: bool, allow_duplicates: bool) -> u32;

    /// `projectm_playlist_insert_path`: like [`PlaylistFs::add_path`] but the first accepted
    /// preset goes to `index` ([`lactea::INSERT_AT_END`] appends) and the rest follow it.
    fn insert_path(
        &mut self,
        dir: &Path,
        index: u32,
        recursive: bool,
        allow_duplicates: bool,
    ) -> u32;
}

impl PlaylistFs for Playlist {
    fn add_path(&mut self, dir: &Path, recursive: bool, allow_duplicates: bool) -> u32 {
        self.insert_path(dir, lactea::INSERT_AT_END, recursive, allow_duplicates)
    }

    fn insert_path(
        &mut self,
        dir: &Path,
        index: u32,
        recursive: bool,
        allow_duplicates: bool,
    ) -> u32 {
        let names: Vec<String> = scan_presets(dir, recursive)
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        self.insert_presets(&names, index, allow_duplicates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "lactea-host-presets-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        for f in ["b.milk", "a.PRJM", "c.milk2", "notes.txt", "sub/d.milk"] {
            std::fs::write(d.join(f), b"").unwrap();
        }
        d
    }

    fn names(v: &[PathBuf], root: &Path) -> Vec<String> {
        v.iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    #[test]
    fn scan_honours_recursion_and_extensions() {
        let d = tree();
        assert_eq!(
            names(&scan_presets(&d, false), &d),
            ["a.PRJM", "b.milk", "c.milk2"]
        );
        assert_eq!(
            names(&scan_presets(&d, true), &d),
            ["a.PRJM", "b.milk", "c.milk2", "sub/d.milk"]
        );
        assert_eq!(
            names(&scan_preset_files(&d, true, false), &d),
            ["a.PRJM", "b.milk", "sub/d.milk"]
        );
        assert!(scan_presets(&d.join("missing"), true).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn add_and_insert_path_follow_playlist_rules() {
        let d = tree();
        let mut p = Playlist::new(1);
        assert_eq!(p.add_path(&d, false, false), 3);
        // the same files again are duplicates
        assert_eq!(p.add_path(&d, true, false), 1);
        assert_eq!(p.len(), 4);
        assert_eq!(p.add_path(&d, false, true), 3);

        let mut q = Playlist::new(1);
        q.add_preset("first", false);
        q.add_preset("last", false);
        assert_eq!(q.insert_path(&d, 1, true, false), 4);
        let items = names(
            &q.items()
                .iter()
                .skip(1)
                .take(4)
                .map(PathBuf::from)
                .collect::<Vec<_>>(),
            &d,
        );
        assert_eq!(items, ["a.PRJM", "b.milk", "c.milk2", "sub/d.milk"]);
        assert_eq!(q.item(0), Some("first"));
        assert_eq!(q.item(5), Some("last"));

        q.set_filter(vec!["-*.milk2".into()]);
        assert_eq!(q.insert_path(&d, 0, true, true), 3);
        let _ = std::fs::remove_dir_all(d);
    }
}
