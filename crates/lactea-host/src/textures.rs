//! Directory-backed [`TextureProvider`]. Scanning mirrors projectM's `FileScanner` + `TextureManager`:
//! recursive (depth 8), keyed by lowercase base name, files of one directory in path order, earlier
//! directories first. The random pool is sorted by name.

use lactea::{Image, Media, MediaFiles, TextureProvider, decode_gif};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Extensions of the plain `sampler_<name>` / `rand##` textures (projectM's list).
pub const TEXTURE_EXTENSIONS: [&str; 7] = ["jpg", "jpeg", "dds", "png", "tga", "bmp", "dib"];

/// Folders of the MD3 data root; when a scanned directory is one of them its siblings are indexed too.
const MD3_FOLDERS: [&str; 3] = ["textures", "sprites", "gifs"];

const MAX_DEPTH: u32 = 8;

fn ext_of(path: &Path) -> Option<String> {
    Some(path.extension()?.to_str()?.to_ascii_lowercase())
}

/// Every file below `dir` (recursively, each directory in path order) as `(path relative to the data root, file)`.
fn walk(dir: &Path, rel: &str, depth: u32, out: &mut Vec<(String, PathBuf)>) {
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
            name
        } else {
            format!("{rel}/{name}")
        };
        if p.is_dir() {
            if depth < MAX_DEPTH {
                walk(&p, &path, depth + 1, out);
            }
        } else {
            out.push((path, p));
        }
    }
}

/// Strip a directory prefix and a known image extension: `"textures/Clouds2.JPG"` -> `"clouds2"`.
fn normalize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    for ext in TEXTURE_EXTENSIONS {
        if let Some(stem) = lower.strip_suffix(&format!(".{ext}")) {
            return stem.to_string();
        }
    }
    lower
}

/// Decode an image the way projectM does (`stbi_load(.., 4)`, content sniffed). JPEG goes through the
/// stb-exact decoder (libjpeg-family decoders differ by up to 4 code values, which comp shaders amplify)
/// and a JPEG it rejects is an error; the other formats go through the `image` crate.
pub fn decode_image(bytes: &[u8]) -> Result<Image, String> {
    if lactea_jpeg::is_jpeg(bytes) {
        let img = lactea_jpeg::decode_rgba(bytes).map_err(|e| e.to_string())?;
        return Ok(Image {
            width: img.width,
            height: img.height,
            rgba8: img.rgba,
        });
    }
    let rgba = image::load_from_memory(bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    Ok(Image {
        width: rgba.width(),
        height: rgba.height(),
        rgba8: rgba.into_raw(),
    })
}

/// [`decode_image`] of the file at `path`.
pub fn decode_file(path: &Path) -> Result<Image, String> {
    decode_image(&std::fs::read(path).map_err(|e| e.to_string())?)
}

/// A GIF (by content) as frames, anything else as a still image.
pub fn decode_media(path: &Path) -> Result<Media, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.starts_with(b"GIF8") {
        return decode_gif(&bytes)
            .map(Media::Animated)
            .map_err(|e| e.to_string());
    }
    decode_image(&bytes).map(Media::Still)
}

/// Directories' textures for the engine: plain lookups by base name, MD3 sprites / GIFs / shader
/// textures through a [`MediaFiles`] index of the same directories.
pub struct DirTextures {
    dirs: Vec<PathBuf>,
    /// (lowercase base name, path) in priority order: earlier directories first, then path order.
    files: Vec<(String, PathBuf)>,
    /// Lowercase base names, one per file, sorted.
    pool: Vec<String>,
    media: MediaFiles<PathBuf>,
    /// Decoded plain textures by name (`None` = not found / undecodable); presets re-request the
    /// same texture on every load. Off by default: a host with many large textures pays memory for it.
    cache: Option<HashMap<String, Option<Image>>>,
}

impl DirTextures {
    /// Index `dirs` (earlier directories take priority). A directory that is one of the MD3 folders
    /// (`textures`, `sprites`, `gifs`) makes its parent the data root too, so a preset's `sprites\x.png`
    /// resolves next to the textures.
    pub fn new(dirs: &[PathBuf]) -> Self {
        let mut files = Vec::new();
        let mut media = MediaFiles::new();
        for dir in dirs {
            let mut all = Vec::new();
            walk(dir, "", 0, &mut all);
            for (rel, path) in &all {
                let Some(ext) = ext_of(path) else { continue };
                if TEXTURE_EXTENSIONS.contains(&ext.as_str())
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    files.push((stem.to_ascii_lowercase(), path.clone()));
                }
                if lactea::TEXTURE_EXTENSIONS.contains(&ext.as_str()) {
                    media.add(rel, path.clone());
                }
            }
            let is_folder = dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| MD3_FOLDERS.contains(&n.to_ascii_lowercase().as_str()));
            if let (true, Some(parent)) = (is_folder, dir.parent()) {
                for sub in MD3_FOLDERS {
                    let mut all = Vec::new();
                    walk(&parent.join(sub), sub, 0, &mut all);
                    for (rel, path) in all {
                        if ext_of(&path)
                            .is_some_and(|e| lactea::TEXTURE_EXTENSIONS.contains(&e.as_str()))
                        {
                            media.add(&rel, path);
                        }
                    }
                }
            }
        }
        let mut pool: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
        pool.sort();
        Self {
            dirs: dirs.to_vec(),
            files,
            pool,
            media,
            cache: None,
        }
    }

    /// Index the single directory `root`.
    pub fn scan(root: &Path) -> Self {
        Self::new(&[root.to_path_buf()])
    }

    /// Keep decoded plain textures in memory.
    #[must_use]
    pub fn with_cache(mut self) -> Self {
        self.cache = Some(HashMap::new());
        self
    }

    /// The directories given to [`DirTextures::new`].
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// Number of plain textures indexed.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl TextureProvider for DirTextures {
    fn load(&mut self, name: &str) -> Option<Image> {
        let key = normalize(name);
        if let Some(hit) = self.cache.as_ref().and_then(|c| c.get(&key)) {
            return hit.clone();
        }
        let img = self
            .files
            .iter()
            .filter(|(n, _)| *n == key)
            .find_map(|(_, path)| decode_file(path).ok());
        if let Some(cache) = &mut self.cache {
            cache.insert(key, img.clone());
        }
        img
    }

    fn random_pool(&self) -> Vec<String> {
        self.pool.clone()
    }

    fn load_md3_texture(&mut self, name: &str) -> Option<Media> {
        decode_media(self.media.find_texture(name)?).ok()
    }

    fn load_sprite(&mut self, path: &str) -> Option<Media> {
        decode_media(self.media.find_sprite(path)?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest GIF89a: one transparent 1x1 frame with no delay.
    const TINY_GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lactea-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn png(path: &Path, w: u32, h: u32, px: [u8; 4]) {
        image::RgbaImage::from_pixel(w, h, image::Rgba(px))
            .save(path)
            .unwrap();
    }

    /// A texture folder: sprites resolve against the parent (the MD3 data root), shader textures are
    /// found in `textures`, `sprites` and `gifs`, and a GIF comes back as frames.
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

    #[test]
    fn lookup_ignores_case_folder_and_extension_and_earlier_dir_wins() {
        let a = tmp("a");
        let b = tmp("b");
        std::fs::create_dir_all(a.join("sub")).unwrap();
        png(&a.join("sub/Clouds2.PNG"), 4, 2, [10, 20, 30, 255]);
        png(&b.join("clouds2.png"), 1, 1, [1, 1, 1, 255]);
        let mut p = DirTextures::new(&[a.clone(), b.clone()]).with_cache();
        assert_eq!(p.len(), 2);
        for name in ["clouds2", "CLOUDS2", "Clouds2.png", "x/clouds2.jpg"] {
            let img = p.load(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!((img.width, img.height), (4, 2), "{name}");
        }
        assert!(p.load("nope").is_none());
        assert_eq!(p.random_pool(), ["clouds2", "clouds2"]);
        let mut p = DirTextures::new(&[b.clone(), a.clone()]);
        assert_eq!(p.load("clouds2").unwrap().width, 1);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn a_jpeg_the_stb_decoder_rejects_is_an_error() {
        let bad = [0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0];
        assert!(decode_image(&bad).is_err());
    }
}
