//! Filesystem texture provider for the engine.
//!
//! MilkDrop presets reference textures by bare name (`sampler_clouds2`, `tex_name = "cells"`),
//! without extension and in any letter case. The provider indexes the configured directories
//! (recursively) once, then answers `load(name)` by case-insensitive base-name match over the
//! usual image extensions and decodes the file to RGBA8.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Extensions tried, in this order of preference when several files share a base name.
const EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "tga", "bmp"];

/// Decoded texture, RGBA8, row 0 = top.
#[derive(Clone, Debug)]
pub struct TextureImage {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

/// `name -> image` lookup over directories. The engine adapter wraps this in the engine's own
/// `TextureProvider` trait (`load(&mut self, &str) -> Option<Image>` / `random_pool`).
#[derive(Debug, Default)]
pub struct FsTextureProvider {
    dirs: Vec<PathBuf>,
    /// lowercase base name -> file (first directory wins, then preferred extension).
    index: HashMap<String, PathBuf>,
    /// Lowercase file stems, one entry per indexed file, sorted ascending (the engine's
    /// `random_pool` contract; projectM's scan also keeps one entry per file).
    names: Vec<String>,
    /// Decode cache: presets re-request the same texture on every load.
    cache: HashMap<String, Option<TextureImage>>,
}

fn ext_rank(path: &Path) -> Option<usize> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    EXTENSIONS.iter().position(|e| *e == ext)
}

/// Strip a directory prefix and a known image extension: `"textures/Clouds2.JPG"` -> `"clouds2"`.
fn normalize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    for ext in EXTENSIONS {
        if let Some(stem) = lower.strip_suffix(&format!(".{ext}")) {
            return stem.to_string();
        }
    }
    lower
}

fn scan(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            scan(&p, depth + 1, out);
        } else if ext_rank(&p).is_some() {
            out.push(p);
        }
    }
}

impl FsTextureProvider {
    /// Index `dirs` (earlier directories take priority over later ones).
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        let mut index: HashMap<String, (usize, usize, PathBuf)> = HashMap::new();
        let mut names: Vec<String> = Vec::new();
        for (di, dir) in dirs.iter().enumerate() {
            let mut files = Vec::new();
            scan(dir, 0, &mut files);
            for f in files {
                let Some(stem) = f.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let key = stem.to_ascii_lowercase();
                names.push(key.clone());
                let rank = ext_rank(&f).unwrap_or(usize::MAX);
                match index.get(&key) {
                    Some((d, r, _)) if (*d, *r) <= (di, rank) => {}
                    _ => {
                        index.insert(key, (di, rank, f.clone()));
                    }
                }
            }
        }
        names.sort();
        Self {
            dirs,
            index: index.into_iter().map(|(k, (_, _, p))| (k, p)).collect(),
            names,
            cache: HashMap::new(),
        }
    }

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

    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Decode `name` (any case, with or without extension) to RGBA8.
    pub fn load(&mut self, name: &str) -> Option<TextureImage> {
        let key = normalize(name);
        if let Some(hit) = self.cache.get(&key) {
            return hit.clone();
        }
        let img = self
            .index
            .get(&key)
            .and_then(|path| match decode_file(path) {
                Ok(img) => Some(img),
                Err(e) => {
                    log::warn!("texture {}: decode failed: {e}", path.display());
                    None
                }
            });
        if img.is_none() {
            log::debug!("texture '{name}' not found in {} dir(s)", self.dirs.len());
        }
        self.cache.insert(key, img.clone());
        img
    }

    /// Names a `rand##` texture may resolve to (every indexed texture, sorted).
    pub fn random_pool(&self) -> Vec<String> {
        self.names.clone()
    }
}

/// projectM decodes textures with `stbi_load(.., 4)` and sniffs the content. JPEG goes through the stb-exact
/// decoder (`lactea-jpeg`: libjpeg-family decoders differ by up to 4 code values, which comp shaders
/// amplify); the lossless formats go through the `image` crate.
fn decode_file(path: &Path) -> Result<TextureImage, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if lactea_jpeg::is_jpeg(&bytes)
        && let Ok(img) = lactea_jpeg::decode_rgba(&bytes)
    {
        return Ok(TextureImage {
            width: img.width,
            height: img.height,
            rgba8: img.rgba,
        });
    }
    let rgba = image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    Ok(TextureImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba8: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lactea-tex-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_png(path: &Path, w: u32, h: u32, px: [u8; 4]) {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(px));
        img.save(path).unwrap();
    }

    #[test]
    fn case_insensitive_extensionless_lookup() {
        let d = tmp("case");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        write_png(&d.join("sub/Clouds2.PNG"), 4, 2, [10, 20, 30, 255]);
        let mut p = FsTextureProvider::new(vec![d.clone()]);
        assert_eq!(p.len(), 1);
        for name in ["clouds2", "CLOUDS2", "Clouds2.png", "x/clouds2.jpg"] {
            let img = p.load(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!((img.width, img.height), (4, 2));
            assert_eq!(&img.rgba8[..4], &[10, 20, 30, 255]);
            assert_eq!(img.rgba8.len(), 4 * 2 * 4);
        }
        assert!(p.load("nope").is_none());
        assert_eq!(p.random_pool(), vec!["clouds2".to_string()]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn earlier_dir_wins_and_preferred_extension() {
        let a = tmp("a");
        let b = tmp("b");
        write_png(&a.join("tex.png"), 1, 1, [1, 1, 1, 255]);
        write_png(&b.join("tex.png"), 2, 2, [2, 2, 2, 255]);
        let mut p = FsTextureProvider::new(vec![a.clone(), b.clone()]);
        assert_eq!(p.load("tex").unwrap().width, 1);
        let mut p = FsTextureProvider::new(vec![b.clone(), a.clone()]);
        assert_eq!(p.load("tex").unwrap().width, 2);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn repo_texpack_is_found_and_decodes() {
        let dirs = FsTextureProvider::default_dirs();
        let Some(dir) = dirs.iter().find(|d| d.ends_with("oracle/build/texpack")) else {
            return; // texpack not built on this machine
        };
        let mut p = FsTextureProvider::new(vec![dir.clone()]);
        assert!(p.len() > 10, "{} textures", p.len());
        let name = p.random_pool()[0].clone();
        let img = p.load(&name.to_ascii_uppercase()).expect("decode");
        assert_eq!(img.rgba8.len(), (img.width * img.height * 4) as usize);
    }
}
