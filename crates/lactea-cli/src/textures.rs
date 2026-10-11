//! Directory-backed [`TextureProvider`] (the engine never touches the filesystem; the CLI does).
//! Scanning mirrors projectM's `FileScanner` + `TextureManager`: recursive, extensions
//! `.jpg .jpeg .dds .png .tga .bmp .dib`, keyed by lowercase base name; the random pool is sorted
//! by (name, path) like the instrumented oracle.

use lactea::{Image, TextureProvider};
use std::path::{Path, PathBuf};

const EXTENSIONS: [&str; 7] = ["jpg", "jpeg", "dds", "png", "tga", "bmp", "dib"];

pub struct DirTextures {
    /// (lowercase base name, path), sorted.
    files: Vec<(String, PathBuf)>,
}

impl DirTextures {
    pub fn scan(root: &Path) -> Self {
        let mut files = Vec::new();
        scan_dir(root, &mut files, 0);
        files.sort();
        Self { files }
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }
}

fn scan_dir(dir: &Path, out: &mut Vec<(String, PathBuf)>, depth: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 8 {
                scan_dir(&p, out, depth + 1);
            }
            continue;
        }
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase);
        if let (Some(ext), Some(stem)) = (ext, p.file_stem().and_then(|s| s.to_str()))
            && EXTENSIONS.contains(&ext.as_str())
        {
            out.push((stem.to_ascii_lowercase(), p));
        }
    }
}

/// projectM decodes textures with `stbi_load(.., 4)`, sniffing the content. JPEG goes through the stb-exact
/// decoder (libjpeg-family decoders differ by up to 4 code values, which comp shaders amplify); the lossless
/// formats go through the `image` crate. A JPEG that stb rejects falls back to `image` (the oracle would have
/// used its 1x1 placeholder).
fn decode_file(path: &Path) -> Option<Image> {
    let bytes = std::fs::read(path).ok()?;
    if lactea_jpeg::is_jpeg(&bytes)
        && let Ok(img) = lactea_jpeg::decode_rgba(&bytes)
    {
        return Some(Image {
            width: img.width,
            height: img.height,
            rgba8: img.rgba,
        });
    }
    let rgba = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(Image {
        width,
        height,
        rgba8: rgba.into_raw(),
    })
}

impl TextureProvider for DirTextures {
    fn load(&mut self, name: &str) -> Option<Image> {
        let lower = name.to_ascii_lowercase();
        for (n, path) in self.files.iter().filter(|(n, _)| *n == lower) {
            let _ = n;
            if let Some(img) = decode_file(path) {
                return Some(img);
            }
        }
        None
    }

    fn random_pool(&self) -> Vec<String> {
        self.files.iter().map(|(n, _)| n.clone()).collect()
    }
}
