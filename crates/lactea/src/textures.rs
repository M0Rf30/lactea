//! User textures: the host supplies pixels through [`TextureProvider`]; the engine never touches
//! the filesystem.

use crate::media::Media;
use crate::rng::StreamRng;

/// A decoded texture: RGBA8, row 0 = top.
#[derive(Clone, Debug)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba8: Vec<u8>,
}

/// Source of user textures (`sampler_<name>` in preset shaders).
///
/// Names are lowercase without the `sampler_` / `fw_`-style prefix and without file extension
/// (`sampler_fw_clouds2` asks for `"clouds2"`).
pub trait TextureProvider {
    /// Pixels of the texture called `name`, or `None` when the host has no such texture.
    fn load(&mut self, name: &str) -> Option<Image>;

    /// Names (same namespace as [`TextureProvider::load`]) the engine may pick from for
    /// `randNN` samplers. **Order is part of the determinism contract**: return the names sorted
    /// ascending (lowercase); duplicates are allowed and count as separate entries (projectM's
    /// texture scan keeps one entry per file).
    fn random_pool(&self) -> Vec<String>;

    /// MilkDrop 3 (`Compat::MilkDrop3`): the shader texture `name` (same naming as [`TextureProvider::load`]),
    /// looked up like MD3 does — `.gif` and `.jfif` files too, in the `textures`, `sprites` and `gifs` folders
    /// ([`MediaFiles::find_texture`](crate::MediaFiles::find_texture)) — and an animated GIF returned as
    /// [`Media::Animated`]. The default has no MD3 folders and no animation: it is [`TextureProvider::load`].
    fn load_md3_texture(&mut self, name: &str) -> Option<Media> {
        self.load(name).map(Media::Still)
    }

    /// MilkDrop 3: the image of a preset's `SpriteName`, a path relative to the MD3 data root with `\` or `/`
    /// separators (`sprites\logo.png`); see [`MediaFiles::find_sprite`](crate::MediaFiles::find_sprite). Any format
    /// the host can decode; a GIF comes back as [`Media::Animated`] (see [`decode_gif`](crate::decode_gif)).
    /// The default finds nothing.
    fn load_sprite(&mut self, _path: &str) -> Option<Media> {
        None
    }
}

/// projectM's `TextureManager::ExtractTextureSettings`: a name longer than 3 bytes whose third byte
/// is `_` carries a quality prefix (`fw_` / `wf_` linear+repeat, `fc_` / `cf_` linear+clamp, `pw_` /
/// `wp_` nearest+repeat, `pc_` / `cp_` nearest+clamp, any other `xx_` linear+repeat) that is removed;
/// everything else is linear+repeat. Returns `(name without prefix, linear, wrap)`.
pub(crate) fn split_qualified(full: &str) -> (&str, bool, bool) {
    let b = full.as_bytes();
    if b.len() <= 3 || b[2] != b'_' {
        return (full, true, true);
    }
    let rest = &full[3..]; // byte 2 is ASCII, so byte 3 starts a character
    let (linear, wrap) = match full.get(..3).map(str::to_ascii_lowercase).as_deref() {
        Some("fc_" | "cf_") => (true, false),
        Some("pw_" | "wp_") => (false, true),
        Some("pc_" | "cp_") => (false, false),
        _ => (true, true),
    };
    (rest, linear, wrap)
}

/// The renderer-side description of a shape's `image=` key (`None` for an empty key).
pub(crate) fn shape_image(full: &str) -> Option<lactea_render::ShapeImage> {
    if full.is_empty() {
        return None;
    }
    let (name, linear, wrap) = split_qualified(full);
    Some(lactea_render::ShapeImage {
        name: name.to_ascii_lowercase(),
        linear,
        wrap,
    })
}

/// `randNN` / `randNN_prefix` sampler names: returns the slot (0..=15) when `root` (the sampler
/// name without the `sampler_` prefix, lowercase) is one.
pub(crate) fn rand_slot(root: &str) -> Option<usize> {
    let b = root.as_bytes();
    if b.len() >= 6 && &b[..4] == b"rand" && b[4].is_ascii_digit() && b[5].is_ascii_digit() {
        let slot = usize::from(b[4] - b'0') * 10 + usize::from(b[5] - b'0');
        if slot <= 15 {
            return Some(slot);
        }
    }
    None
}

/// projectM's `TextureManager::GetRandomTexture`: `name` is the sampler name as projectM sees it
/// (text after `sampler_`, e.g. `rand03_smalltiled` or `fw_rand03`). A `_prefix` suffix filters
/// the pool by file-name prefix **only when** the name starts with `randNN_` (a `fw_` style
/// quality prefix defeats the filter in projectM, and so here). The pick uses the named stream
/// `randtex_<name>` of the run seed over the sorted pool.
pub(crate) fn pick_random(seed: u64, name: &str, pool: &[String]) -> Option<String> {
    if pool.is_empty() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let mut rng = StreamRng::new(seed, &format!("randtex_{lower}"));
    let prefix = if lower.len() > 7 && lower.as_bytes()[6] == b'_' {
        &lower[7..]
    } else {
        ""
    };
    if prefix.is_empty() {
        let i = (rng.next64() % pool.len() as u64) as usize;
        return Some(pool[i].clone());
    }
    let filtered: Vec<&String> = pool
        .iter()
        .filter(|n| n.to_ascii_lowercase().starts_with(prefix))
        .collect();
    if filtered.is_empty() {
        return None;
    }
    let i = (rng.next64() % filtered.len() as u64) as usize;
    Some(filtered[i].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots() {
        assert_eq!(rand_slot("rand03"), Some(3));
        assert_eq!(rand_slot("rand15_smalltiled"), Some(15));
        assert_eq!(rand_slot("rand16"), None);
        assert_eq!(rand_slot("randx1"), None);
        assert_eq!(rand_slot("clouds"), None);
    }

    #[test]
    fn pick_filters_by_prefix_only_without_quality_prefix() {
        let pool: Vec<String> = ["clouds", "smalltiled_a", "smalltiled_b", "zzz"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for seed in 0..20 {
            let p = pick_random(seed, "rand03_smalltiled", &pool).unwrap();
            assert!(p.starts_with("smalltiled"));
        }
        // quality prefix: the whole pool is used
        let mut seen = std::collections::HashSet::new();
        for seed in 0..64 {
            seen.insert(pick_random(seed, "fw_rand03_smalltiled", &pool).unwrap());
        }
        assert!(seen.len() > 2);
        assert!(pick_random(1, "rand01", &[]).is_none());
        assert!(pick_random(1, "rand01_nomatch", &pool).is_none());
    }

    /// `TextureManager::ExtractTextureSettings` (`TextureManager.cpp:375-421`).
    #[test]
    fn quality_prefixes_pick_filter_and_wrap() {
        assert_eq!(split_qualified("clouds"), ("clouds", true, true));
        assert_eq!(split_qualified("fw_clouds"), ("clouds", true, true));
        assert_eq!(split_qualified("WF_Clouds"), ("Clouds", true, true));
        assert_eq!(split_qualified("fc_clouds"), ("clouds", true, false));
        assert_eq!(split_qualified("cf_clouds"), ("clouds", true, false));
        assert_eq!(split_qualified("pw_clouds"), ("clouds", false, true));
        assert_eq!(split_qualified("PC_clouds"), ("clouds", false, false));
        assert_eq!(split_qualified("xx_clouds"), ("clouds", true, true)); // unknown prefix: stripped anyway
        // too short to carry a prefix; `_` not at index 2
        assert_eq!(split_qualified("a_b"), ("a_b", true, true));
        assert_eq!(split_qualified("ab_"), ("ab_", true, true));
        assert_eq!(split_qualified("abc_d"), ("abc_d", true, true));
        assert_eq!(split_qualified("éé_x"), ("éé_x", true, true)); // byte 2 is not `_`
        assert_eq!(split_qualified("é_é_x"), ("é_x", true, true)); // byte 2 is `_`: stripped, no panic
        let im = shape_image("PC_Idle").unwrap();
        assert_eq!(
            (im.name.as_str(), im.linear, im.wrap),
            ("idle", false, false)
        );
        assert!(shape_image("").is_none());
    }
}
