//! Directory-backed [`TextureProvider`] (the engine never touches the filesystem; the CLI does).
//! Scanning mirrors projectM's `FileScanner` + `TextureManager`: recursive, extensions
//! `.jpg .jpeg .dds .png .tga .bmp .dib`, keyed by lowercase base name; the random pool is sorted
//! by (name, path) like the instrumented oracle.

use lactea::{Image, Media, MediaFiles, TEXTURE_EXTENSIONS, TextureProvider, decode_gif};
use std::path::{Path, PathBuf};

const EXTENSIONS: [&str; 7] = ["jpg", "jpeg", "dds", "png", "tga", "bmp", "dib"];

pub struct DirTextures {
    /// (lowercase base name, path), sorted.
    files: Vec<(String, PathBuf)>,
    /// Every file MD3 may look for (sprites, GIFs, `.jfif`), by path relative to the MD3 data root.
    media: MediaFiles<PathBuf>,
}

impl DirTextures {
    /// Index `root` recursively. When `root` is one of the MD3 folders (`textures`, `sprites`, `gifs`) its parent
    /// is taken for the data root too, so a preset's `sprites\x.png` resolves next to the textures.
    pub fn scan(root: &Path) -> Self {
        let mut files = Vec::new();
        scan_dir(root, &mut files, 0);
        files.sort();
        let mut media = MediaFiles::new();
        scan_media(root, "", 0, &mut media);
        let md3_folder = root.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
            ["textures", "sprites", "gifs"].contains(&n.to_ascii_lowercase().as_str())
        });
        if let (true, Some(parent)) = (md3_folder, root.parent()) {
            for sub in ["textures", "sprites", "gifs"] {
                scan_media(&parent.join(sub), sub, 0, &mut media);
            }
        }
        Self { files, media }
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

/// Register every media file under `dir` as `<rel>/<name>` (path relative to the data root).
fn scan_media(dir: &Path, rel: &str, depth: u32, out: &mut MediaFiles<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if p.is_dir() {
            if depth < 8 {
                scan_media(&p, &path, depth + 1, out);
            }
            continue;
        }
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase);
        if ext.is_some_and(|e| TEXTURE_EXTENSIONS.contains(&e.as_str())) {
            out.add(&path, p);
        }
    }
}

/// A GIF (by content) as frames, anything else as a still image.
fn decode_media(path: &Path) -> Option<Media> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(b"GIF8") {
        return decode_gif(&bytes).ok().map(Media::Animated);
    }
    decode_file(path).map(Media::Still)
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

    fn load_md3_texture(&mut self, name: &str) -> Option<Media> {
        let path = self.media.find_texture(name)?.clone();
        decode_media(&path)
    }

    fn load_sprite(&mut self, path: &str) -> Option<Media> {
        let path = self.media.find_sprite(path)?.clone();
        decode_media(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest GIF89a: one transparent 1x1 frame with no delay.
    const TINY_GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lactea-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn png(path: &Path, w: u32, h: u32, px: [u8; 4]) {
        image::RgbaImage::from_pixel(w, h, image::Rgba(px))
            .save(path)
            .unwrap();
    }

    /// `--texture-dir <root>/textures`: sprites resolve against the parent (the MD3 data root), shader textures
    /// are found in `textures`, `sprites` and `gifs`, and a GIF comes back as frames.
    #[test]
    fn md3_sprites_and_gifs_resolve_from_a_textures_folder() {
        let root = tmp("md3");
        for d in ["textures", "sprites", "gifs"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        png(&root.join("textures/clouds.png"), 2, 2, [1, 2, 3, 255]);
        png(&root.join("sprites/Logo.PNG"), 4, 2, [9, 8, 7, 255]);
        std::fs::write(root.join("gifs/blink.gif"), TINY_GIF).unwrap();
        let mut p = DirTextures::scan(&root.join("textures"));

        let Some(Media::Still(s)) = p.load_sprite("sprites\\logo.png") else {
            panic!("sprite");
        };
        assert_eq!((s.width, s.height), (4, 2));
        assert!(p.load_sprite("sprites\\missing.png").is_none());
        let Some(Media::Animated(a)) = p.load_sprite("gifs\\blink.gif") else {
            panic!("gif sprite");
        };
        assert_eq!((a.width, a.height, a.frames.len()), (1, 1, 1));
        assert_eq!(a.frames[0].delay, 0.1);
        // shader textures: `textures` first, then `sprites` / `gifs`
        assert!(matches!(
            p.load_md3_texture("clouds"),
            Some(Media::Still(_))
        ));
        assert!(matches!(p.load_md3_texture("LOGO"), Some(Media::Still(_))));
        assert!(matches!(
            p.load_md3_texture("blink"),
            Some(Media::Animated(_))
        ));
        // plain `load` and the random pool keep their projectM behaviour: no GIFs
        assert!(p.random_pool().contains(&"clouds".to_string()));
        assert!(p.load("blink").is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
