//! Caches that live as long as the [`Engine`](crate::Engine) and so outlive one preset load: decoded textures and
//! sprites, and translated shaders. Reloading a preset (playlist revisits, the two halves of a `.milk2`, soft cuts
//! back and forth) otherwise decodes the same JPEGs and GIFs and translates the same HLSL again.
//!
//! Every cache is bounded by a byte budget and evicts the least recently used entries, so a long playlist cannot
//! grow it without limit.

use crate::media::Media;
use crate::textures::TextureProvider;
use lactea_abi::PassKind;
use lactea_hlsl::{Options as HlslOptions, TranslateError, Translated};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

/// Budget for decoded media: a few dozen photographic textures, or a long GIF.
const MEDIA_BUDGET: usize = 128 << 20;
/// Budget for translated shaders (a shader is tens of KiB of WGSL).
const SHADER_BUDGET: usize = 16 << 20;

struct Entry<V> {
    value: V,
    bytes: usize,
    used: u64,
}

/// A map bounded by the summed `bytes` of its entries; inserting past the budget drops the least recently used
/// entries first, and an entry bigger than the whole budget is not kept.
struct LruCache<K, V> {
    entries: HashMap<K, Entry<V>>,
    bytes: usize,
    budget: usize,
    tick: u64,
}

impl<K: Hash + Eq + Clone, V: Clone> LruCache<K, V> {
    fn new(budget: usize) -> Self {
        LruCache {
            entries: HashMap::new(),
            bytes: 0,
            budget,
            tick: 0,
        }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        self.tick += 1;
        let e = self.entries.get_mut(key)?;
        e.used = self.tick;
        Some(e.value.clone())
    }

    fn insert(&mut self, key: K, value: V, bytes: usize) {
        if bytes > self.budget {
            return;
        }
        self.tick += 1;
        if let Some(old) = self.entries.insert(
            key,
            Entry {
                value,
                bytes,
                used: self.tick,
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        while self.bytes > self.budget {
            // every `used` stamp is unique, so the oldest entry is well defined
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(e) = self.entries.remove(&oldest) {
                self.bytes -= e.bytes;
            }
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum MediaKind {
    /// [`TextureProvider::load`]
    Still,
    /// [`TextureProvider::load_md3_texture`]
    Md3Texture,
    /// [`TextureProvider::load_sprite`]
    Sprite,
}

/// What the provider returned for a texture / sprite name, shared between loads. Only found media is kept: a miss
/// is cheap for a provider (an index lookup) and the host may add the file later.
pub(crate) struct MediaCache {
    entries: LruCache<(MediaKind, String), Arc<Media>>,
}

fn media_bytes(m: &Media) -> usize {
    match m {
        Media::Still(i) => i.rgba8.len(),
        Media::Animated(a) => a.frames.iter().map(|f| f.rgba8.len()).sum(),
    }
}

impl MediaCache {
    pub(crate) fn new() -> Self {
        MediaCache {
            entries: LruCache::new(MEDIA_BUDGET),
        }
    }

    /// Forget everything: the host changed its provider, so the same names may now mean other pixels.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    fn get_or_load(
        &mut self,
        kind: MediaKind,
        name: &str,
        load: impl FnOnce() -> Option<Media>,
    ) -> Option<Arc<Media>> {
        let key = (kind, name.to_owned());
        if let Some(hit) = self.entries.get(&key) {
            return Some(hit);
        }
        let media = Arc::new(load()?);
        self.entries
            .insert(key, Arc::clone(&media), media_bytes(&media));
        Some(media)
    }

    /// [`TextureProvider::load`] as a still image.
    pub(crate) fn still(
        &mut self,
        provider: &mut dyn TextureProvider,
        name: &str,
    ) -> Option<Arc<Media>> {
        self.get_or_load(MediaKind::Still, name, || {
            provider.load(name).map(Media::Still)
        })
    }

    /// [`TextureProvider::load_md3_texture`].
    pub(crate) fn md3_texture(
        &mut self,
        provider: &mut dyn TextureProvider,
        name: &str,
    ) -> Option<Arc<Media>> {
        self.get_or_load(MediaKind::Md3Texture, name, || {
            provider.load_md3_texture(name)
        })
    }

    /// [`TextureProvider::load_sprite`].
    pub(crate) fn sprite(
        &mut self,
        provider: &mut dyn TextureProvider,
        path: &str,
    ) -> Option<Arc<Media>> {
        self.get_or_load(MediaKind::Sprite, path, || provider.load_sprite(path))
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.entries.len()
    }
}

/// Everything a translation depends on besides the translator itself.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ShaderKey {
    kind: PassKind,
    src: String,
    derivative_uniformity_off: bool,
    floor_mod: bool,
    projectm_flavor: bool,
    md3: bool,
}

type Translation = Result<Translated, TranslateError>;

/// [`lactea_hlsl::translate`] results (also the failures: they are as deterministic as the successes).
pub(crate) struct ShaderCache {
    entries: LruCache<ShaderKey, Arc<Translation>>,
}

impl ShaderCache {
    pub(crate) fn new() -> Self {
        ShaderCache {
            entries: LruCache::new(SHADER_BUDGET),
        }
    }

    pub(crate) fn translate(
        &mut self,
        kind: PassKind,
        src: &str,
        opts: &HlslOptions,
    ) -> Arc<Translation> {
        let key = ShaderKey {
            kind,
            src: src.to_owned(),
            derivative_uniformity_off: opts.derivative_uniformity_off,
            floor_mod: opts.floor_mod,
            projectm_flavor: opts.projectm_flavor,
            md3: opts.md3,
        };
        if let Some(hit) = self.entries.get(&key) {
            return hit;
        }
        let result = Arc::new(lactea_hlsl::translate(kind, src, opts));
        let bytes = src.len() + result.as_ref().as_ref().map_or(0, |t| t.wgsl.len());
        self.entries.insert(key, Arc::clone(&result), bytes);
        result
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_the_least_recently_used_entry_over_budget() {
        let mut c: LruCache<u32, u32> = LruCache::new(10);
        c.insert(1, 1, 4);
        c.insert(2, 2, 4);
        assert_eq!(c.get(&1), Some(1)); // 2 is now the oldest
        c.insert(3, 3, 4);
        assert_eq!(c.get(&2), None);
        assert_eq!((c.get(&1), c.get(&3)), (Some(1), Some(3)));
        assert_eq!(c.bytes, 8);
    }

    #[test]
    fn an_oversized_entry_is_not_kept_and_a_replaced_one_is_recounted() {
        let mut c: LruCache<u32, u32> = LruCache::new(10);
        c.insert(1, 1, 11);
        assert_eq!((c.get(&1), c.bytes), (None, 0));
        c.insert(1, 1, 6);
        c.insert(1, 2, 3);
        assert_eq!((c.get(&1), c.bytes), (Some(2), 3));
        c.clear();
        assert_eq!((c.get(&1), c.bytes), (None, 0));
    }

    #[test]
    fn media_is_loaded_once_per_name_and_kind() {
        struct P(u32);
        impl TextureProvider for P {
            fn load(&mut self, _: &str) -> Option<crate::Image> {
                self.0 += 1;
                Some(crate::Image {
                    width: 1,
                    height: 1,
                    rgba8: vec![1, 2, 3, 4],
                })
            }
            fn random_pool(&self) -> Vec<String> {
                Vec::new()
            }
        }
        let (mut p, mut c) = (P(0), MediaCache::new());
        let a = c.still(&mut p, "a").unwrap();
        let b = c.still(&mut p, "a").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(p.0, 1);
        // the default MD3 lookup goes through `load`, but it is another kind of request: its own entry
        c.md3_texture(&mut p, "a").unwrap();
        assert_eq!((p.0, c.len()), (2, 2));
        // a miss is not cached
        assert!(c.sprite(&mut p, "a").is_none());
        assert_eq!(c.len(), 2);
        c.clear();
        c.still(&mut p, "a").unwrap();
        assert_eq!(p.0, 3);
    }

    #[test]
    fn translations_are_keyed_by_source_and_options() {
        let src = "shader_body\n{\nret = tex2D(sampler_main, uv).xyz;\n}";
        let mut c = ShaderCache::new();
        let opts = HlslOptions::default();
        let a = c.translate(PassKind::Comp, src, &opts);
        assert!(a.is_ok());
        assert!(Arc::ptr_eq(&a, &c.translate(PassKind::Comp, src, &opts)));
        let mut floor = HlslOptions::default();
        floor.floor_mod = true;
        assert!(!Arc::ptr_eq(&a, &c.translate(PassKind::Comp, src, &floor)));
        assert!(!Arc::ptr_eq(&a, &c.translate(PassKind::Warp, src, &opts)));
        assert_eq!(c.len(), 3);
    }
}
