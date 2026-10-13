//! User textures: the host supplies pixels through [`TextureProvider`]; the engine never touches
//! the filesystem.

use crate::media::Media;
use crate::rng::StreamRng;
use lactea_render::CompatRules;

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

/// The random-texture slot of the sampler `root` (its name without `sampler_` and without the quality prefix,
/// lowercase), or `None` for an ordinary texture. One parser serves the sampler binding and `texsize_randNN`
/// ([`lactea_hlsl::rand_slot`]); the compat rules say which slots and which spellings count.
pub(crate) fn rand_slot(root: &str, rules: &CompatRules) -> Option<usize> {
    lactea_hlsl::rand_slot(root, rules.max_rand_slot, !rules.rand_filter_raw_name)
}

/// The pick of a random texture for the sampler `raw` (text after `sampler_`, e.g. `fw_rand03_smalltiled`; `root` is
/// the same name without its quality prefix) from the sorted `pool`.
///
/// The `_prefix` of `randNN_prefix` filters the pool by file-name prefix. projectM
/// (`CompatRules::rand_filter_raw_name`) looks for it in the raw name (`TextureManager::GetRandomTexture` tests byte 6
/// for `_`), so a quality prefix such as `fw_` moves it out of place and the whole pool is used; MD2 filters on the
/// prefix-stripped name. The pick uses the named stream `randtex_<name>` of the run seed.
pub(crate) fn pick_for_sampler(
    seed: u64,
    rules: &CompatRules,
    raw: &str,
    root: &str,
    pool: &[String],
) -> Option<String> {
    pick_random(
        seed,
        if rules.rand_filter_raw_name {
            raw
        } else {
            root
        },
        pool,
    )
}

/// `name`: the name whose byte 6 may carry the pool filter (`randNN_prefix`), see [`pick_for_sampler`].
fn pick_random(seed: u64, name: &str, pool: &[String]) -> Option<String> {
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

    use lactea_render::Compat;

    #[test]
    fn slot_range_and_spelling_follow_the_compat_rules() {
        let (pm, md2) = (Compat::ProjectM.rules(), Compat::MilkDrop2.rules());
        assert_eq!(rand_slot("rand15_smalltiled", &pm), Some(15));
        assert_eq!(rand_slot("rand16", &pm), None);
        assert_eq!(rand_slot("rand16", &md2), Some(16));
        assert_eq!(rand_slot("rand99", &md2), Some(99));
        // projectM takes any name that starts with `randNN`, MD2 only `randNN` / `randNN_...`
        assert_eq!(rand_slot("rand03x", &pm), Some(3));
        assert_eq!(rand_slot("rand03x", &md2), None);
        assert_eq!(rand_slot("randx1", &pm), None);
        assert_eq!(rand_slot("clouds", &md2), None);
    }

    #[test]
    fn pool_filter_survives_a_quality_prefix_only_in_md2() {
        let pool: Vec<String> = ["clouds", "smalltiled_a", "smalltiled_b", "zzz"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (pm, md2) = (Compat::ProjectM.rules(), Compat::MilkDrop2.rules());
        let (raw, root) = ("fw_rand03_smalltiled", "rand03_smalltiled");
        // MD2 peels `fw_` before it looks for the filter
        for seed in 0..20 {
            let p = pick_for_sampler(seed, &md2, raw, root, &pool).unwrap();
            assert!(p.starts_with("smalltiled"));
        }
        // projectM reads byte 6 of the raw name: the quality prefix defeats the filter, the whole pool is used
        let mut seen = std::collections::HashSet::new();
        for seed in 0..64 {
            seen.insert(pick_for_sampler(seed, &pm, raw, root, &pool).unwrap());
        }
        assert!(seen.len() > 2);
        // without a quality prefix both filter
        for seed in 0..20 {
            let p = pick_for_sampler(seed, &pm, root, root, &pool).unwrap();
            assert!(p.starts_with("smalltiled"));
        }
        assert!(pick_for_sampler(1, &pm, "rand01", "rand01", &[]).is_none());
        assert!(pick_for_sampler(1, &pm, "rand01_nomatch", "rand01_nomatch", &pool).is_none());
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
