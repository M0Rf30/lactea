//! Where the app looks for textures. The provider itself is `lactea_host::DirTextures`.

use std::path::{Path, PathBuf};

/// Default search path: `oracle/build/texpack` of the repository when it can be found
/// (current directory, the executable's ancestors, the build-time source tree), then
/// `/usr/share/projectM/textures`.
pub fn default_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let rel = Path::new("oracle/build/texpack");
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().skip(1).map(Path::to_path_buf));
    }
    roots.extend(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .map(Path::to_path_buf),
    );
    for root in roots {
        let cand = root.join(rel);
        if cand.is_dir() && !dirs.contains(&cand) {
            dirs.push(cand);
            break;
        }
    }
    let sys = PathBuf::from("/usr/share/projectM/textures");
    if sys.is_dir() {
        dirs.push(sys);
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dirs_exist_and_are_unique() {
        let dirs = default_dirs();
        assert!(dirs.iter().all(|d| d.is_dir()));
        for (i, d) in dirs.iter().enumerate() {
            assert!(!dirs[..i].contains(d), "{d:?}");
        }
    }
}
