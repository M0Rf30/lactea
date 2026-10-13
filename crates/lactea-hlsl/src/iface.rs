//! `CShaderParams::CacheParams` sampler rules: prefix parsing (`fw_ fc_ pw_ pc_`), default
//! filter/address modes per texture class, `randXX` handling, `texsize_<name>` resolution.

use crate::Prelude;
use lactea_abi::{Address, Filter, NoiseTex, TexDim, TexSource, TextureBinding};

/// Root texture name of an HLSL sampler identifier plus the explicit mode, if any.
/// `sampler_fw_clouds2` -> ("clouds2", Some((bilinear, wrap)))
pub(crate) fn sampler_root(hlsl_name: &str) -> (String, Option<(bool, bool)>) {
    let root: &str = hlsl_name.strip_prefix("sampler_").unwrap_or(hlsl_name);
    let chars: Vec<char> = root.chars().collect();
    if chars.len() > 3 && chars[2] == '_' {
        let p: String = chars[..2].iter().map(|c| c.to_ascii_uppercase()).collect();
        let mode = match p.as_str() {
            "FW" | "WF" => Some((true, true)),
            "FC" | "CF" => Some((true, false)),
            "PW" | "WP" => Some((false, true)),
            "PC" | "CP" => Some((false, false)),
            _ => None,
        };
        // the 3-char prefix is peeled off even when it is not a recognised mode
        return (chars[3..].iter().collect::<String>().to_lowercase(), mode);
    }
    (root.to_lowercase(), None)
}

pub(crate) fn noise_source(root: &str) -> Option<NoiseTex> {
    Some(match root {
        "noise_lq" => NoiseTex::Lq,
        "noise_lq_lite" => NoiseTex::LqLite,
        "noise_mq" => NoiseTex::Mq,
        "noise_hq" => NoiseTex::Hq,
        "noisevol_lq" => NoiseTex::VolLq,
        "noisevol_hq" => NoiseTex::VolHq,
        _ => return None,
    })
}

/// Source of a texture root name. With the MD3 prelude `fft` is the engine-owned shader FFT texture; with the BeatDrop
/// prelude `fft` and `wave` both are (one engine texture holds the spectrum rows and the waveform rows); without a
/// prelude `fft` is just a user texture named `fft` (which the host may or may not supply).
pub(crate) fn source_of_root(root: &str, prelude: impl Into<Prelude>) -> TexSource {
    let prelude = prelude.into();
    match root {
        "main" => TexSource::Main,
        "blur1" => TexSource::Blur(1),
        "blur2" => TexSource::Blur(2),
        "blur3" => TexSource::Blur(3),
        "fft" if prelude.md3() || prelude.beatdrop() => TexSource::Fft,
        "wave" if prelude.beatdrop() => TexSource::Fft,
        r => match noise_source(r) {
            Some(n) => TexSource::Noise(n),
            None => TexSource::User(r.to_string()),
        },
    }
}

pub(crate) fn resolve_sampler(
    hlsl_name: &str,
    declared_dim: TexDim,
    used_3d: bool,
    prelude: impl Into<Prelude>,
) -> TextureBinding {
    let (root, mode) = sampler_root(hlsl_name);
    let source = source_of_root(&root, prelude);
    // defaults: bilinear + wrap; blur/fft/wave default to bilinear + clamp unless a prefix says otherwise
    // (MD3 never uses anisotropy for fft/wave, so there is nothing else to special-case there)
    let clamp_default = matches!(source, TexSource::Blur(_)) || root == "fft" || root == "wave";
    let (bilinear, wrap) = match mode {
        Some(m) => m,
        None => (true, !clamp_default),
    };
    // the FFT texture has one row and no depth: it is always a 2D binding
    let dim = if source == TexSource::Fft {
        TexDim::D2
    } else if declared_dim == TexDim::D3 || used_3d || root.starts_with("noisevol") {
        TexDim::D3
    } else {
        TexDim::D2
    };
    TextureBinding {
        hlsl_name: hlsl_name.to_string(),
        source,
        filter: if bilinear {
            Filter::Linear
        } else {
            Filter::Point
        },
        address: if wrap { Address::Wrap } else { Address::Clamp },
        dim,
    }
}

/// The slot of a `randNN` / `randNN_prefix` texture name (`root`: the sampler name without `sampler_` and without its
/// quality prefix, lowercase), or `None` when `root` is an ordinary texture name. Shared by the sampler and the
/// `texsize_` resolution so both agree on which names are random slots.
///
/// * `max_slot`: the highest slot that is random (projectM 15, MD2 99); higher numbers are ordinary names.
/// * `strict`: MD2 (`plugin.cpp:3575-3578`) accepts only `randNN` or `randNN_...`; projectM
///   (`MilkdropShader.cpp:117-119`) accepts any name that starts with `randNN`.
pub fn rand_slot(root: &str, max_slot: u8, strict: bool) -> Option<usize> {
    let b = root.as_bytes();
    if b.len() >= 6
        && &b[..4] == b"rand"
        && b[4].is_ascii_digit()
        && b[5].is_ascii_digit()
        && (!strict || b.len() == 6 || b[6] == b'_')
    {
        let slot = usize::from(b[4] - b'0') * 10 + usize::from(b[5] - b'0');
        if slot <= usize::from(max_slot) {
            return Some(slot);
        }
    }
    None
}

/// Source for a `texsize_<name>` parameter. Mirrors CacheParams: random textures are matched by
/// slot number, everything else by name. The size of a source that no sampler of this shader
/// binds is still returned (MD2 would only warn). With `md3`, `texsize_fft` names the shader FFT texture (MD3 3.37
/// leaves it unresolved, i.e. zero; reporting the real source lets the host answer 512 x 1).
pub(crate) fn texsize_source(
    param: &str,
    bindings: &[TextureBinding],
    prelude: impl Into<Prelude>,
    (max_slot, strict): (u8, bool),
) -> TexSource {
    let name = param
        .strip_prefix("texsize_")
        .unwrap_or(param)
        .to_lowercase();
    if let Some(ri) = rand_slot(&name, max_slot, strict) {
        for b in bindings {
            if let TexSource::User(u) = &b.source
                && rand_slot(u, max_slot, strict) == Some(ri)
            {
                return b.source.clone();
            }
        }
    }
    source_of_root(&name, prelude)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_prefix_rules() {
        assert_eq!(
            sampler_root("sampler_fw_clouds2"),
            ("clouds2".to_string(), Some((true, true)))
        );
        assert_eq!(
            sampler_root("sampler_PC_main"),
            ("main".to_string(), Some((false, false)))
        );
        assert_eq!(
            sampler_root("sampler_cf_x1"),
            ("x1".to_string(), Some((true, false)))
        );
        assert_eq!(
            sampler_root("sampler_noise_lq"),
            ("noise_lq".to_string(), None)
        );
        // unknown 2-letter prefix is peeled without effect
        assert_eq!(sampler_root("sampler_ab_foo"), ("foo".to_string(), None));
        // name without `sampler_` prefix
        assert_eq!(sampler_root("base02"), ("base02".to_string(), None));

        let b = resolve_sampler("sampler_blur2", TexDim::D2, false, false);
        assert_eq!(
            (b.source.clone(), b.filter, b.address),
            (TexSource::Blur(2), Filter::Linear, Address::Clamp)
        );
        let b = resolve_sampler("sampler_pw_blur1", TexDim::D2, false, false);
        assert_eq!((b.filter, b.address), (Filter::Point, Address::Wrap));
        let m = resolve_sampler("sampler_main", TexDim::D2, false, false);
        assert_eq!(
            (m.source, m.filter, m.address),
            (TexSource::Main, Filter::Linear, Address::Wrap)
        );
        let n = resolve_sampler("sampler_pw_noise_lq", TexDim::D2, false, false);
        assert_eq!(
            (n.source, n.filter, n.address),
            (TexSource::Noise(NoiseTex::Lq), Filter::Point, Address::Wrap)
        );
        let v = resolve_sampler("sampler_fw_noisevol_hq", TexDim::D2, false, false);
        assert_eq!(
            (v.source, v.dim),
            (TexSource::Noise(NoiseTex::VolHq), TexDim::D3)
        );
        let r = resolve_sampler("sampler_rand03_smalltiled", TexDim::D2, false, false);
        assert_eq!(r.source, TexSource::User("rand03_smalltiled".into()));
        let u = resolve_sampler("sampler_fc_clouds", TexDim::D2, false, false);
        assert_eq!(
            (u.source, u.filter, u.address),
            (
                TexSource::User("clouds".into()),
                Filter::Linear,
                Address::Clamp
            )
        );
    }

    #[test]
    fn rand_names_follow_the_compat_rules() {
        // MD2: slots up to 99, only `randNN` / `randNN_...`
        assert_eq!(rand_slot("rand03", 99, true), Some(3));
        assert_eq!(rand_slot("rand42_tiled", 99, true), Some(42));
        assert_eq!(rand_slot("rand03x", 99, true), None);
        assert_eq!(rand_slot("rand100", 99, true), None);
        // projectM: slots up to 15, any suffix
        assert_eq!(rand_slot("rand15", 15, false), Some(15));
        assert_eq!(rand_slot("rand16", 15, false), None);
        assert_eq!(rand_slot("rand03x", 15, false), Some(3));
        // `texsize_randNN` finds the bound slot whichever suffix the sampler had
        let bound = resolve_sampler("sampler_rand03_smalltiled", TexDim::D2, false, false);
        assert_eq!(
            texsize_source(
                "texsize_rand03",
                std::slice::from_ref(&bound),
                false,
                (99, true)
            ),
            bound.source
        );
    }

    #[test]
    fn fft_sampler_is_a_user_texture_unless_md3() {
        // without the MD3 prelude: unchanged (BeatDrop-style user texture `fft`, bilinear + clamp)
        let f = resolve_sampler("sampler_fft", TexDim::D2, false, false);
        assert_eq!(
            (f.source, f.filter, f.address),
            (
                TexSource::User("fft".into()),
                Filter::Linear,
                Address::Clamp
            )
        );
        assert_eq!(
            texsize_source("texsize_fft", &[], false, (99, true)),
            TexSource::User("fft".into())
        );
        // MD3: the engine's FFT texture, default clamp + bilinear, prefixes pick filter / wrap
        for (name, filter, address) in [
            ("sampler_fft", Filter::Linear, Address::Clamp),
            ("sampler_FW_fft", Filter::Linear, Address::Wrap),
            ("sampler_fc_fft", Filter::Linear, Address::Clamp),
            ("sampler_pw_fft", Filter::Point, Address::Wrap),
            ("sampler_pc_fft", Filter::Point, Address::Clamp),
        ] {
            let f = resolve_sampler(name, TexDim::D2, false, true);
            assert_eq!(
                (f.source, f.filter, f.address, f.dim),
                (TexSource::Fft, filter, address, TexDim::D2),
                "{name}"
            );
        }
        // `wave` stays a user texture under MD3 (never filled there)
        let w = resolve_sampler("sampler_wave", TexDim::D2, false, true);
        assert_eq!(w.source, TexSource::User("wave".into()));
        assert_eq!(
            texsize_source("texsize_fft", &[], true, (99, true)),
            TexSource::Fft
        );
    }
}
