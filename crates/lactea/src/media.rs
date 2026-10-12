//! Animated images and MD3 resource lookup.
//!
//! The engine never touches the filesystem: hosts list their files into a [`MediaFiles`] index (which only does
//! the path arithmetic and the search order of MilkDrop 3), read the winner and hand the bytes back as [`Media`].
//! [`decode_gif`] turns a GIF into frames with the delay rule MD3 uses.

use crate::textures::Image;
use lactea_render::gif_frame_index;
use std::collections::HashMap;

/// One frame of an animated image.
#[derive(Clone, Debug)]
pub struct AnimFrame {
    /// `width * height * 4` bytes, straight (non-premultiplied) alpha, row 0 = top: the whole canvas as it looks
    /// while this frame is shown.
    pub rgba8: Vec<u8>,
    /// Seconds the frame stays on screen; GIF delays are in 1/100 s and `<= 0` means 0.1 s.
    pub delay: f64,
}

/// An animated image (a GIF): every frame is a full canvas.
#[derive(Clone, Debug)]
pub struct AnimatedImage {
    /// Canvas width in pixels.
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
    /// At least one frame.
    pub frames: Vec<AnimFrame>,
}

impl AnimatedImage {
    /// The frame delays in seconds.
    pub fn delays(&self) -> Vec<f64> {
        self.frames.iter().map(|f| f.delay).collect()
    }

    /// Index of the frame shown `t` seconds after the animation started: `t` modulo the total duration, the first
    /// frame whose cumulative delay is strictly greater, else the last (frame 0 for a single frame).
    pub fn frame_index(&self, t: f64) -> usize {
        gif_frame_index(&self.delays(), t)
    }
}

/// What a [`TextureProvider`](crate::TextureProvider) found for an MD3 resource.
#[derive(Clone, Debug)]
pub enum Media {
    /// A still image.
    Still(Image),
    /// An animated GIF.
    Animated(AnimatedImage),
}

/// Largest canvas side accepted by [`decode_gif`].
const MAX_GIF_SIDE: u32 = 16384;

/// Decode a GIF into full-canvas frames.
///
/// Frames are composed the GIF89a way: the canvas starts transparent, each frame is drawn over it (transparent
/// pixels leave the canvas as it is), and the frame's disposal method is applied before the next one is drawn
/// (`Background` clears the frame's rectangle to transparent, `Previous` restores the canvas from before the
/// frame, `Keep` / unspecified leave it). The delay of a frame is its Graphic Control Extension delay in 1/100 s;
/// zero (or a missing extension) means 0.1 s. A stream that breaks after at least one frame keeps the frames read
/// so far.
pub fn decode_gif(bytes: &[u8]) -> Result<AnimatedImage, String> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    let mut dec = opts.read_info(bytes).map_err(|e| e.to_string())?;
    let (mut w, mut h) = (u32::from(dec.width()), u32::from(dec.height()));
    let mut canvas: Vec<u8> = Vec::new();
    let mut frames: Vec<AnimFrame> = Vec::new();
    // disposal still to apply before the next frame: (method, rect, canvas before that frame was drawn)
    let mut pending: Option<(gif::DisposalMethod, [u32; 4], Option<Vec<u8>>)> = None;
    loop {
        let frame = match dec.read_next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) if frames.is_empty() => return Err(e.to_string()),
            Err(_) => break,
        };
        let (fl, ft) = (u32::from(frame.left), u32::from(frame.top));
        let (fw, fh) = (u32::from(frame.width), u32::from(frame.height));
        if canvas.is_empty() {
            // a logical screen of size 0 is seen in the wild: fall back to the first frame's extent
            if w == 0 || h == 0 {
                w = fl + fw;
                h = ft + fh;
            }
            if w == 0 || h == 0 || w > MAX_GIF_SIDE || h > MAX_GIF_SIDE {
                return Err(format!("GIF canvas {w}x{h} is unusable"));
            }
            canvas = vec![0; w as usize * h as usize * 4];
        }
        if let Some((method, rect, backup)) = pending.take() {
            match method {
                gif::DisposalMethod::Background => {
                    for y in rect[1]..(rect[1] + rect[3]).min(h) {
                        let row = (y * w) as usize * 4;
                        let (x0, x1) = (
                            rect[0] as usize * 4,
                            (rect[0] + rect[2]).min(w) as usize * 4,
                        );
                        canvas[row + x0..row + x1].fill(0);
                    }
                }
                gif::DisposalMethod::Previous => {
                    if let Some(b) = backup {
                        canvas = b;
                    }
                }
                gif::DisposalMethod::Any | gif::DisposalMethod::Keep => {}
            }
        }
        let backup = (frame.dispose == gif::DisposalMethod::Previous).then(|| canvas.clone());
        let buf = &frame.buffer;
        for y in 0..fh {
            let cy = ft + y;
            if cy >= h {
                break;
            }
            for x in 0..fw {
                let cx = fl + x;
                if cx >= w {
                    break;
                }
                let s = ((y * fw + x) * 4) as usize;
                if buf[s + 3] == 0 {
                    continue;
                }
                let d = ((cy * w + cx) * 4) as usize;
                canvas[d..d + 4].copy_from_slice(&buf[s..s + 4]);
            }
        }
        let delay = f64::from(frame.delay) / 100.0;
        frames.push(AnimFrame {
            rgba8: canvas.clone(),
            delay: if delay > 0.0 { delay } else { 0.1 },
        });
        pending = Some((frame.dispose, [fl, ft, fw, fh], backup));
    }
    if frames.is_empty() {
        return Err("GIF without frames".into());
    }
    Ok(AnimatedImage {
        width: w,
        height: h,
        frames,
    })
}

/// Extensions MD3 looks for when a shader names a texture, in its order (`.jpeg` is accepted right after `.jpg`).
pub const TEXTURE_EXTENSIONS: [&str; 8] =
    ["jpg", "jpeg", "jfif", "png", "tga", "bmp", "dib", "gif"];

/// Folders of the MD3 data root searched for a shader texture, in order. The empty entry is the folder the host
/// handed over (MD3's preset folder search has no equivalent: the engine does not know where the preset came from).
const TEXTURE_DIRS: [&str; 4] = ["textures/", "", "sprites/", "gifs/"];

/// `\` or `/` separated path, lower case, without empty and `.` components. `None` for a path with `..`.
fn normalize_path(path: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for c in path.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => return None,
            c => parts.push(c.to_lowercase()),
        }
    }
    Some(parts.join("/"))
}

/// Index of the media files a host can read, with the lookups of MilkDrop 3.
///
/// `F` is whatever identifies a file for the host (a path). Files are registered under their path relative to
/// the MD3 data root (`sprites/dmt.gif`, `textures/clouds.jpg`); when several are registered under the same
/// path the first wins, so register higher priority roots first. Case is ignored.
#[derive(Clone, Debug)]
pub struct MediaFiles<F> {
    by_path: HashMap<String, F>,
    by_file: HashMap<String, F>,
}

impl<F> Default for MediaFiles<F> {
    fn default() -> Self {
        Self {
            by_path: HashMap::new(),
            by_file: HashMap::new(),
        }
    }
}

impl<F: Clone> MediaFiles<F> {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of registered paths.
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    /// Whether nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    /// Register `file` under `rel_path` (relative to the data root, either separator). A path with `..` is ignored.
    pub fn add(&mut self, rel_path: &str, file: F) {
        let Some(key) = normalize_path(rel_path) else {
            return;
        };
        if let Some(name) = key.rsplit('/').next() {
            self.by_file
                .entry(name.to_owned())
                .or_insert_with(|| file.clone());
        }
        self.by_path.entry(key).or_insert(file);
    }

    /// The file of a preset's `SpriteName` (`sprites\x.png`): the path is relative to the data root. A name that
    /// matches no registered path falls back to the first registered file of that name in any folder, so a
    /// host that was only given a flat texture folder still finds its sprites (MD3 itself reports an error).
    pub fn find_sprite(&self, path: &str) -> Option<&F> {
        let key = normalize_path(path)?;
        if let Some(f) = self.by_path.get(&key) {
            return Some(f);
        }
        self.by_file.get(key.rsplit('/').next()?)
    }

    /// The file of a shader texture called `name` (`sampler_<name>`; any case, folder prefix and a known extension
    /// are ignored): for each extension of [`TEXTURE_EXTENSIONS`] in order, the `textures`, root, `sprites` and
    /// `gifs` folders in that order.
    pub fn find_texture(&self, name: &str) -> Option<&F> {
        let lower = name.to_lowercase();
        let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
        let stem = TEXTURE_EXTENSIONS
            .iter()
            .find_map(|e| base.strip_suffix(&format!(".{e}")))
            .unwrap_or(base);
        TEXTURE_EXTENSIONS.iter().find_map(|ext| {
            TEXTURE_DIRS
                .iter()
                .find_map(|dir| self.by_path.get(&format!("{dir}{stem}.{ext}")))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny GIF89a writer for the tests: global palette red, green, blue, white.
    fn gif_bytes(frames: &[(u16, u16, u16, u16, Vec<u8>, u8, u16, Option<u8>)]) -> Vec<u8> {
        // (left, top, width, height, indices, disposal, delay, transparent)
        let (w, h) = (4u16, 4u16);
        let mut out = Vec::new();
        {
            let palette = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
            let mut enc = gif::Encoder::new(&mut out, w, h, &palette).unwrap();
            for (l, t, fw, fh, idx, disp, delay, trans) in frames {
                let mut f = gif::Frame::default();
                f.left = *l;
                f.top = *t;
                f.width = *fw;
                f.height = *fh;
                f.buffer = std::borrow::Cow::Owned(idx.clone());
                f.dispose = gif::DisposalMethod::from_u8(*disp).unwrap();
                f.delay = *delay;
                f.transparent = *trans;
                enc.write_frame(&f).unwrap();
            }
        }
        out
    }

    fn px(img: &AnimatedImage, frame: usize, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        img.frames[frame].rgba8[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn gif_delays_follow_the_md3_rule() {
        let full = |c: u8| vec![c; 16];
        let g = gif_bytes(&[
            (0, 0, 4, 4, full(0), 1, 10, None),
            (0, 0, 4, 4, full(1), 1, 20, None),
            (0, 0, 4, 4, full(2), 1, 0, None),
        ]);
        let a = decode_gif(&g).unwrap();
        assert_eq!((a.width, a.height, a.frames.len()), (4, 4, 3));
        assert_eq!(a.delays(), vec![0.1, 0.2, 0.1], "0 means 0.1 s");
        assert_eq!(a.frame_index(0.05), 0);
        assert_eq!(a.frame_index(0.12), 1);
        assert_eq!(a.frame_index(0.31), 2);
        assert_eq!(a.frame_index(0.41), 0, "0.41 mod 0.4");
    }

    #[test]
    fn gif_disposal_follows_gif89a() {
        // frame 0: whole canvas red, kept; frame 1: a 2x2 green square at (1,1), disposed to background;
        // frame 2: a 1x1 blue pixel at (0,0), restored to the previous canvas; frame 3: nothing drawn
        let g = gif_bytes(&[
            (0, 0, 4, 4, vec![0; 16], 1, 5, None),
            (1, 1, 2, 2, vec![1; 4], 2, 5, None),
            (0, 0, 1, 1, vec![2], 3, 5, None),
            (3, 3, 1, 1, vec![3], 1, 5, Some(3)),
        ]);
        let a = decode_gif(&g).unwrap();
        let (red, green, blue, clear) =
            ([255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [0; 4]);
        assert_eq!(px(&a, 0, 2, 2), red);
        assert_eq!(px(&a, 1, 2, 2), green, "frame 1 is drawn over frame 0");
        assert_eq!(px(&a, 1, 0, 0), red);
        // frame 2: the green square was disposed to *background* (transparent), not to the old red
        assert_eq!(px(&a, 2, 2, 2), clear);
        assert_eq!(px(&a, 2, 0, 0), blue);
        // frame 3: disposal "previous" of frame 2 brings back the canvas from before frame 2 (blue gone);
        // the transparent pixel of frame 3 draws nothing
        assert_eq!(px(&a, 3, 0, 0), red);
        assert_eq!(px(&a, 3, 3, 3), red);
        assert_eq!(px(&a, 3, 2, 2), clear);
    }

    #[test]
    fn broken_gif_is_an_error_and_a_truncated_one_keeps_its_frames() {
        assert!(decode_gif(b"not a gif").is_err());
        let g = gif_bytes(&[
            (0, 0, 4, 4, vec![0; 16], 1, 5, None),
            (0, 0, 4, 4, vec![1; 16], 1, 5, None),
        ]);
        let cut = &g[..g.len() - 12];
        if let Ok(a) = decode_gif(cut) {
            assert!(!a.frames.is_empty() && a.frames.len() <= 2);
        }
    }

    #[test]
    fn sprite_paths_are_root_relative_with_a_basename_fallback() {
        let mut m: MediaFiles<&str> = MediaFiles::new();
        m.add("sprites/Jello1.PNG", "root/sprites/Jello1.PNG");
        m.add("textures/clouds.jpg", "root/textures/clouds.jpg");
        assert_eq!(
            m.find_sprite("sprites\\jello1.png"),
            Some(&"root/sprites/Jello1.PNG")
        );
        // not found under that path: the same file name elsewhere is used
        assert_eq!(
            m.find_sprite("other\\clouds.jpg"),
            Some(&"root/textures/clouds.jpg")
        );
        assert_eq!(m.find_sprite("sprites\\nope.png"), None);
        assert_eq!(m.find_sprite("..\\secret.png"), None);
    }

    #[test]
    fn shader_textures_follow_the_md3_order() {
        let mut m: MediaFiles<&str> = MediaFiles::new();
        m.add("sprites/a.png", "sprites/a.png");
        m.add("gifs/a.gif", "gifs/a.gif");
        m.add("gifs/b.gif", "gifs/b.gif");
        m.add("textures/c.png", "textures/c.png");
        m.add("sprites/c.jpg", "sprites/c.jpg");
        m.add("c.png", "root/c.png");
        // the extension is the outer loop: a jpg in a later folder beats a png in an earlier one
        assert_eq!(m.find_texture("C"), Some(&"sprites/c.jpg"));
        // `textures` first, then the root, then `sprites`, then `gifs`
        assert_eq!(m.find_texture("a"), Some(&"sprites/a.png"));
        assert_eq!(m.find_texture("b.gif"), Some(&"gifs/b.gif"));
        assert_eq!(m.find_texture("textures/c.png"), Some(&"sprites/c.jpg"));
        assert_eq!(m.find_texture("zzz"), None);
    }
}
