//! Lookups of the MilkDrop 3 shader FFT sampler (`Options::md3`, root name `fft`; MD3 3.37 `get_fft`).
//!
//! The texture is `FFT_BINS x 1` `R32Float`, which wgpu cannot filter (`FLOAT32_FILTERABLE` is an optional feature),
//! so every lookup is lowered to a WGSL helper that reproduces what D3D9 returns for R32F with `textureLoad`:
//! address mode (clamp / wrap) and filter (linear / point) of the sampler binding are applied by hand, the result is
//! `(r, 1, 1, 1)`, and the `v` coordinate is ignored (MD3's texture has two identical rows).

use super::expr::paren;
use super::*;
use lactea_abi::{Address, Filter};

/// WGSL of the lookup helpers; `@BINS@` is `lactea_abi::FFT_BINS`.
///
/// Linear filtering is the D3D9 rule for texel centres at `(i + 0.5) / N`: `x = u * N - 0.5`, blend of texels
/// `floor(x)` and `floor(x) + 1` by `fract(x)`. The two texel indices are clamped to `0..N-1` (clamp addressing: the
/// edge texel is repeated, so `u <= 0` reads texel 0 and `u >= 1` texel N-1) or wrapped modulo N. Point sampling reads
/// texel `floor(u * N)`, clamped or wrapped the same way.
const HELPER: &str = r#"
const FFT_BINS: i32 = @BINS@;

fn fft_texel(t: texture_2d<f32>, i: i32, wrapped: bool) -> f32 {
    var k = clamp(i, 0, FFT_BINS - 1);
    if (wrapped) {
        k = ((i % FFT_BINS) + FFT_BINS) % FFT_BINS;
    }
    return textureLoad(t, vec2<i32>(k, 0), 0).r;
}

fn fft_fetch(t: texture_2d<f32>, u: f32, linear: bool, wrapped: bool) -> vec4<f32> {
    let n = f32(FFT_BINS);
    var r: f32;
    if (linear) {
        let x = u * n - 0.5;
        let f = floor(x);
        let i0 = i32(f);
        r = mix(fft_texel(t, i0, wrapped), fft_texel(t, i0 + 1, wrapped), x - f);
    } else {
        r = fft_texel(t, i32(floor(u * n)), wrapped);
    }
    return vec4<f32>(r, 1.0, 1.0, 1.0);
}
"#;

/// The helper functions to append to a module that binds a [`lactea_abi::TexSource::Fft`] texture.
pub(crate) fn helper_wgsl(prelude: crate::Prelude) -> String {
    if prelude.beatdrop() {
        BD_HELPER.to_string()
    } else {
        HELPER.replace("@BINS@", &lactea_abi::FFT_BINS.to_string())
    }
}

/// WGSL of the BeatDrop lookups. `sampler_fft` (`MY_FFT_SHADER_SAMPLES` x 2) and `sampler_wave`
/// (`NUM_WAVEFORM_SAMPLES` x 2) are two separate `R32Float` textures of two rows each (`v = .25` / `.75` are the row
/// centres), so the size comes from the bound texture. Filter and address mode apply per axis as D3D9 does for an
/// `R32F` texture (read with `textureLoad`: `R32Float` is not filterable without `FLOAT32_FILTERABLE`).
const BD_HELPER: &str = r#"
fn bd_texel(t: texture_2d<f32>, x: i32, y: i32, wrapped: bool) -> f32 {
    let d = vec2<i32>(textureDimensions(t));
    let p = vec2<i32>(x, y);
    var k = clamp(p, vec2<i32>(0, 0), d - vec2<i32>(1, 1));
    if (wrapped) {
        k = ((p % d) + d) % d;
    }
    return textureLoad(t, k, 0).r;
}

fn bd_fetch(t: texture_2d<f32>, uv: vec2<f32>, linear: bool, wrapped: bool) -> vec4<f32> {
    let n = vec2<f32>(textureDimensions(t));
    var r: f32;
    if (linear) {
        let p = uv * n - vec2<f32>(0.5, 0.5);
        let f = floor(p);
        let i = vec2<i32>(f);
        let fr = p - f;
        let a = mix(bd_texel(t, i.x, i.y, wrapped), bd_texel(t, i.x + 1, i.y, wrapped), fr.x);
        let b = mix(bd_texel(t, i.x, i.y + 1, wrapped), bd_texel(t, i.x + 1, i.y + 1, wrapped), fr.x);
        r = mix(a, b, fr.y);
    } else {
        let i = vec2<i32>(floor(uv * n));
        r = bd_texel(t, i.x, i.y, wrapped);
    }
    return vec4<f32>(r, 1.0, 1.0, 1.0);
}
"#;

impl Tr<'_> {
    /// Whether lookups of sampler `samp` (HLSL name) read the engine's spectrum / waveform texture: `fft` under MD3,
    /// `fft` and `wave` under BeatDrop.
    pub fn is_fft_sampler(&self, samp: &str) -> bool {
        match crate::iface::sampler_root(samp).0.as_str() {
            "fft" => self.prelude.md3() || self.prelude.beatdrop(),
            "wave" => self.prelude.beatdrop(),
            _ => false,
        }
    }

    /// `tex2D*` of the FFT sampler `samp`. `args` are the checked arguments of the HLSL call; `kind` is the lookup
    /// flavour (`""`, `lod`, `bias`, `proj`, `grad`). Only the `x` coordinate matters; LOD, bias and gradients are
    /// evaluated (and type-checked) but do not change the result: the texture has a single level and one row.
    pub fn fft_lookup(&mut self, samp: &str, args: &[Expr], kind: &str) -> R<V> {
        let tb = crate::iface::resolve_sampler(samp, TexDim::D2, false, self.prelude);
        let linear = tb.filter == Filter::Linear;
        let wrapped = tb.address == Address::Wrap;
        let cv = self.expr(&args[1])?;
        if self.prelude.beatdrop() {
            return self.bd_lookup(samp, args, kind, cv, linear, wrapped);
        }
        let u = match kind {
            "" | "grad" => {
                let c = self.conv(&cv, &Ty::vec(Sc::F, 2))?;
                if kind == "grad" {
                    for a in &args[2..4] {
                        let g = self.expr(a)?;
                        self.conv(&g, &Ty::vec(Sc::F, 2))?;
                    }
                }
                let c = self.bind(V::new(c, Ty::vec(Sc::F, 2)));
                format!("{}.x", paren(&c.s))
            }
            _ => {
                let c4 = self.conv(&cv, &Ty::vec(Sc::F, 4))?;
                let c = self.bind(V::new(c4, Ty::vec(Sc::F, 4)));
                let c = paren(&c.s);
                if kind == "proj" {
                    format!("{c}.x / {c}.w")
                } else {
                    format!("{c}.x")
                }
            }
        };
        Ok(V::new(
            format!("fft_fetch(tex_{samp}, {u}, {linear}, {wrapped})"),
            Ty::vec(Sc::F, 4),
        ))
    }

    /// BeatDrop's `tex2D*` of `sampler_fft` / `sampler_wave`: both coordinates matter (`v` selects the row, `.25` /
    /// `.75` are the row centres); LOD, bias and gradients are evaluated but do not change the result.
    fn bd_lookup(
        &mut self,
        samp: &str,
        args: &[Expr],
        kind: &str,
        cv: V,
        linear: bool,
        wrapped: bool,
    ) -> R<V> {
        let uv = match kind {
            "" | "grad" => {
                let c = self.conv(&cv, &Ty::vec(Sc::F, 2))?;
                if kind == "grad" {
                    for a in &args[2..4] {
                        let g = self.expr(a)?;
                        self.conv(&g, &Ty::vec(Sc::F, 2))?;
                    }
                }
                let c = self.bind(V::new(c, Ty::vec(Sc::F, 2)));
                c.s
            }
            _ => {
                let c4 = self.conv(&cv, &Ty::vec(Sc::F, 4))?;
                let c = self.bind(V::new(c4, Ty::vec(Sc::F, 4)));
                let c = paren(&c.s);
                if kind == "proj" {
                    format!("({c}.xy / {c}.w)")
                } else {
                    format!("{c}.xy")
                }
            }
        };
        Ok(V::new(
            format!("bd_fetch(tex_{samp}, {uv}, {linear}, {wrapped})"),
            Ty::vec(Sc::F, 4),
        ))
    }
}
