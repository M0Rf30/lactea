// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Port of MD2's procedural noise textures (plugin.cpp `AddNoiseTex` / `AddNoiseVol`,
//! `fCubicInterpolate`, `dwCubicInterpolate`).
//!
//! MD2 seeds its generator from `QueryPerformanceCounter`; here every texture is generated
//! from the renderer's seeded [`Rng`], so the *algorithm* (value range, 8-bit packing quirks,
//! cubic smoothing lattice) is MD2's and the *content* is deterministic per seed. To match the
//! reference render bit-for-bit, inject its dumped noise with `Renderer::inject_noise` (feature `diagnostics`).
//!
//! Sizes (plugin.cpp `Init`): `noise_lq` 256/zoom 1, `noise_lq_lite` 32/1, `noise_mq` 256/4,
//! `noise_hq` 256/8, `noisevol_lq` 32/1, `noisevol_hq` 32/4.

use crate::rng::Rng;
use lactea_abi::NoiseTex;

/// `(size, zoom_factor, is_volume)` for each built-in noise texture.
pub const fn noise_spec(tex: NoiseTex) -> (u32, u32, bool) {
    match tex {
        NoiseTex::Lq => (256, 1, false),
        NoiseTex::LqLite => (32, 1, false),
        NoiseTex::Mq => (256, 4, false),
        NoiseTex::Hq => (256, 8, false),
        NoiseTex::VolLq => (32, 1, true),
        NoiseTex::VolHq => (32, 4, true),
    }
}

/// All six textures in `NoiseTex::index()` order.
pub const ALL_NOISE: [NoiseTex; 6] = [
    NoiseTex::Lq,
    NoiseTex::LqLite,
    NoiseTex::Mq,
    NoiseTex::Hq,
    NoiseTex::VolLq,
    NoiseTex::VolHq,
];

/// The built-in 2D textures projectM's `TextureManager` has registered before it scans any directory
/// (`TextureManager::Preload`): a shape's `image=` of exactly one of these names samples it and never reaches the
/// file lookup. (`idlem` / `idleheadphones` are embedded PNGs; the 3D `noisevol_*` volumes bound to a 2D sampler are
/// undefined behaviour upstream.) `name` is compared as is: projectM's lookup is case-sensitive.
pub fn builtin_image_noise(name: &str) -> Option<NoiseTex> {
    Some(match name {
        "noise_lq" => NoiseTex::Lq,
        "noise_lq_lite" => NoiseTex::LqLite,
        "noise_mq" => NoiseTex::Mq,
        "noise_hq" => NoiseTex::Hq,
        _ => return None,
    })
}

fn cubic(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let a0 = y3 - y2 - y0 + y1;
    let a1 = y0 - y1 - a0;
    let a2 = y2 - y0;
    let a3 = y1;
    a0 * t * t2 + a1 * t2 + a2 * t + a3
}

/// MD2 `dwCubicInterpolate`: per-byte cubic on a packed A8R8G8B8 dword, clamped, truncated.
fn dw_cubic(y0: u32, y1: u32, y2: u32, y3: u32, t: f32) -> u32 {
    let mut ret = 0u32;
    let mut shift = 0;
    for _ in 0..4 {
        let f = cubic(
            ((y0 >> shift) & 0xFF) as f32 / 255.0,
            ((y1 >> shift) & 0xFF) as f32 / 255.0,
            ((y2 >> shift) & 0xFF) as f32 / 255.0,
            ((y3 >> shift) & 0xFF) as f32 / 255.0,
            t,
        );
        let f = f.clamp(0.0, 1.0);
        ret |= ((f * 255.0) as u32) << shift;
        shift += 8;
    }
    ret
}

/// One random pixel: MD2 packs `(rand(0..RANGE-1) + RANGE/2)` per channel with plain shifts and
/// ORs, so for RANGE=256 the 9th bit of each channel bleeds into the next-higher channel.
/// That quirk is reproduced on purpose.
fn random_dword(rng: &mut Rng, range: u32) -> u32 {
    let a = rng.below(range) + range / 2;
    let r = rng.below(range) + range / 2;
    let g = rng.below(range) + range / 2;
    let b = rng.below(range) + range / 2;
    (a << 24) | (r << 16) | (g << 8) | b
}

fn fill_lines(rng: &mut Rng, dst: &mut [u32], size: usize, range: u32) {
    for line in dst.chunks_exact_mut(size) {
        for p in line.iter_mut() {
            *p = random_dword(rng, range);
        }
        // "swap some pixels randomly, to improve 'randomness'"
        for _ in 0..size {
            let x1 = rng.below(size as u32) as usize;
            let x2 = rng.below(size as u32) as usize;
            line.swap(x1, x2);
        }
    }
}

fn dwords_to_rgba(d: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len() * 4);
    for &v in d {
        // A8R8G8B8 dword -> R,G,B,A bytes
        out.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8]);
    }
    out
}

/// `AddNoiseTex`: `size`x`size` RGBA8, row 0 first.
pub fn gen_noise_tex(rng: &mut Rng, size: u32, zoom: u32) -> Vec<u8> {
    let n = size as usize;
    let zoom = zoom as usize;
    let range = if zoom > 1 { 216 } else { 256 };
    let mut d = vec![0u32; n * n];
    fill_lines(rng, &mut d, n, range);

    if zoom > 1 {
        // across, only on the main lines
        for y in (0..n).step_by(zoom) {
            for x in 0..n {
                if x % zoom != 0 {
                    let base_x = (x / zoom) * zoom + n;
                    let base_y = y * n;
                    let y0 = d[base_y + ((base_x - zoom) % n)];
                    let y1 = d[base_y + (base_x % n)];
                    let y2 = d[base_y + ((base_x + zoom) % n)];
                    let y3 = d[base_y + ((base_x + zoom * 2) % n)];
                    let t = (x % zoom) as f32 / zoom as f32;
                    d[y * n + x] = dw_cubic(y0, y1, y2, y3, t);
                }
            }
        }
        // down, on every line
        for x in 0..n {
            for y in 0..n {
                if y % zoom != 0 {
                    let base_y = (y / zoom) * zoom + n;
                    let y0 = d[((base_y - zoom) % n) * n + x];
                    let y1 = d[(base_y % n) * n + x];
                    let y2 = d[((base_y + zoom) % n) * n + x];
                    let y3 = d[((base_y + zoom * 2) % n) * n + x];
                    let t = (y % zoom) as f32 / zoom as f32;
                    d[y * n + x] = dw_cubic(y0, y1, y2, y3, t);
                }
            }
        }
    }
    dwords_to_rgba(&d)
}

/// `AddNoiseVol`: `size`^3 RGBA8, slice (z) major, then row, then x.
pub fn gen_noise_vol(rng: &mut Rng, size: u32, zoom: u32) -> Vec<u8> {
    let n = size as usize;
    let zoom = zoom as usize;
    let slice = n * n;
    let range = if zoom > 1 { 216 } else { 256 };
    let mut d = vec![0u32; n * slice];
    for z in 0..n {
        fill_lines(rng, &mut d[z * slice..(z + 1) * slice], n, range);
    }

    if zoom > 1 {
        // across X, only on main lines of main slices
        for z in (0..n).step_by(zoom) {
            for y in (0..n).step_by(zoom) {
                for x in 0..n {
                    if x % zoom != 0 {
                        let base_x = (x / zoom) * zoom + n;
                        let base_y = z * slice + y * n;
                        let y0 = d[base_y + ((base_x - zoom) % n)];
                        let y1 = d[base_y + (base_x % n)];
                        let y2 = d[base_y + ((base_x + zoom) % n)];
                        let y3 = d[base_y + ((base_x + zoom * 2) % n)];
                        let t = (x % zoom) as f32 / zoom as f32;
                        d[z * slice + y * n + x] = dw_cubic(y0, y1, y2, y3, t);
                    }
                }
            }
        }
        // down Y, on main slices
        for z in (0..n).step_by(zoom) {
            for x in 0..n {
                for y in 0..n {
                    if y % zoom != 0 {
                        let base_y = (y / zoom) * zoom + n;
                        let base_z = z * slice;
                        let y0 = d[((base_y - zoom) % n) * n + base_z + x];
                        let y1 = d[(base_y % n) * n + base_z + x];
                        let y2 = d[((base_y + zoom) % n) * n + base_z + x];
                        let y3 = d[((base_y + zoom * 2) % n) * n + base_z + x];
                        let t = (y % zoom) as f32 / zoom as f32;
                        d[y * n + base_z + x] = dw_cubic(y0, y1, y2, y3, t);
                    }
                }
            }
        }
        // through Z, everywhere
        for x in 0..n {
            for y in 0..n {
                for z in 0..n {
                    if z % zoom != 0 {
                        let base_y = y * n;
                        let base_z = (z / zoom) * zoom + n;
                        let y0 = d[((base_z - zoom) % n) * slice + base_y + x];
                        let y1 = d[(base_z % n) * slice + base_y + x];
                        let y2 = d[((base_z + zoom) % n) * slice + base_y + x];
                        let y3 = d[((base_z + zoom * 2) % n) * slice + base_y + x];
                        let t = (z % zoom) as f32 / zoom as f32;
                        d[z * slice + base_y + x] = dw_cubic(y0, y1, y2, y3, t);
                    }
                }
            }
        }
    }
    dwords_to_rgba(&d)
}

/// Generate one of the six built-in textures as RGBA8 (2D: row 0 first; 3D: z-major slices).
pub fn generate(tex: NoiseTex, rng: &mut Rng) -> Vec<u8> {
    let (size, zoom, vol) = noise_spec(tex);
    if vol {
        gen_noise_vol(rng, size, zoom)
    } else {
        gen_noise_tex(rng, size, zoom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_determinism() {
        for tex in ALL_NOISE {
            let (s, _, vol) = noise_spec(tex);
            let a = generate(tex, &mut Rng::new(7));
            let b = generate(tex, &mut Rng::new(7));
            let expect = if vol {
                (s * s * s * 4) as usize
            } else {
                (s * s * 4) as usize
            };
            assert_eq!(a.len(), expect, "{tex:?}");
            assert_eq!(a, b, "{tex:?} must be deterministic");
            assert_ne!(a, generate(tex, &mut Rng::new(8)));
        }
    }

    #[test]
    fn smoothed_texture_is_smoother_than_raw() {
        // mean |horizontal neighbour diff| of the red channel
        fn rough(img: &[u8], n: usize) -> f64 {
            let mut s = 0.0;
            for y in 0..n {
                for x in 0..n - 1 {
                    s += (img[(y * n + x) * 4] as f64 - img[(y * n + x + 1) * 4] as f64).abs();
                }
            }
            s / (n * (n - 1)) as f64
        }
        let raw = gen_noise_tex(&mut Rng::new(3), 64, 1);
        let smooth = gen_noise_tex(&mut Rng::new(3), 64, 8);
        assert!(rough(&smooth, 64) < rough(&raw, 64) * 0.6);
    }

    #[test]
    fn byte_packing_quirk() {
        // RANGE=256: channel values are 128..383, so the 9th bit bleeds into the next channel;
        // the blue byte (no neighbour below) is a plain 8-bit value.
        let img = gen_noise_tex(&mut Rng::new(5), 32, 1);
        assert!(img.chunks_exact(4).any(|p| p[3] != 0));
    }

    /// `TextureManager::Preload` registers the four 2D noise textures under these exact (case-sensitive) names.
    #[test]
    fn shape_image_names_of_the_builtin_noise_textures() {
        assert_eq!(builtin_image_noise("noise_lq"), Some(NoiseTex::Lq));
        assert_eq!(builtin_image_noise("noise_lq_lite"), Some(NoiseTex::LqLite));
        assert_eq!(builtin_image_noise("noise_mq"), Some(NoiseTex::Mq));
        assert_eq!(builtin_image_noise("noise_hq"), Some(NoiseTex::Hq));
        for n in [
            "noisevol_lq",
            "noisevol_hq",
            "NOISE_LQ",
            "noise",
            "clouds",
            "",
        ] {
            assert_eq!(builtin_image_noise(n), None, "{n}");
        }
    }
}
