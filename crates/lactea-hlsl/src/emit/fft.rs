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
pub(crate) fn helper_wgsl() -> String {
    HELPER.replace("@BINS@", &lactea_abi::FFT_BINS.to_string())
}

impl Tr<'_> {
    /// Whether lookups of sampler `samp` (HLSL name) are the MD3 shader FFT.
    pub fn is_fft_sampler(&self, samp: &str) -> bool {
        self.md3 && crate::iface::sampler_root(samp).0 == "fft"
    }

    /// `tex2D*` of the FFT sampler `samp`. `args` are the checked arguments of the HLSL call; `kind` is the lookup
    /// flavour (`""`, `lod`, `bias`, `proj`, `grad`). Only the `x` coordinate matters; LOD, bias and gradients are
    /// evaluated (and type-checked) but do not change the result: the texture has a single level and one row.
    pub fn fft_lookup(&mut self, samp: &str, args: &[Expr], kind: &str) -> R<V> {
        let tb = crate::iface::resolve_sampler(samp, TexDim::D2, false, true);
        let linear = tb.filter == Filter::Linear;
        let wrapped = tb.address == Address::Wrap;
        let cv = self.expr(&args[1])?;
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
}
