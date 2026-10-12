// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! MilkDrop 2's preset blend (`CState::m_bBlending`), renderer side.
//!
//! MD2 has no image-space cross-fade. While a blend runs the old preset's `CState` (`m_pOldState`) lives
//! next to the new one (`m_pState`) inside **one** frame pipeline: the engine evaluates both states' equations,
//! blends the per-frame variables and the per-vertex warp UVs, and the renderer draws
//!
//! * the warp pass twice (`WarpedBlit_Shaders` / `WarpedBlit_NoShaders` pass 0 = old shader, pass 1 = new shader) on
//!   the *blended* mesh, culling mesh tiles by the per-vertex alpha `(DWORD)(mix2*255)` and alpha-blending pass 1,
//! * custom shapes / waves of both states with `alpha_mult` (`progress` for the new state, `1 - progress` for the old),
//! * the main waveform twice when the wave mode changed (`its = 2`) and blends the two polylines,
//! * the composite twice with the old and the new composite shader (`ShowToUser_Shaders` / `ShowToUser_NoShaders`).
//!
//! This module holds the data the engine hands over ([`Md2Blend`]) and the pure logic of those passes (tile culling,
//! the pass tables of `RenderFrame`, the composite alpha lookup, the two-wave blend). Everything is unit tested; the
//! GPU drawing lives in `renderer.rs`.

use crate::frame::{BlurParams, PassRandom};
use crate::geom::{FCGSX, FCGSY};

/// `CosineInterp` (vis_milk2 `utility.h`, not part of `third_party/md2`): `0.5 - 0.5 cos(pi x)`. Used for the blend of
/// the per-frame variables, the wave blend and the video-echo flip.
pub fn cosine_interp(x: f32) -> f32 {
    0.5 - 0.5 * (x * std::f32::consts::PI).cos()
}

/// What the old state contributes to the shader passes of a blend frame (its `ApplyShaderParams(.., m_pOldState)`
/// inputs that differ from the new state's: `q1..q32`, blur range, `rand_preset` / `rot_*`).
#[derive(Clone, Debug)]
pub struct Md2OldPass {
    /// `m_pOldState->var_pf_q[0..32]` after its per-frame code.
    pub q: [f32; 32],
    /// The old state's own (unblended) `var_pf_blur*` (`GetSafeBlurMinMax(m_pOldState)`).
    pub blur: BlurParams,
    /// MilkDrop 3: the old state's `_c14` / `_qi.._qp` / shader time (`ShaderExt::fft` is ignored here).
    pub ext: crate::ShaderExt,
    /// Random state of the old warp / composite pass (`m_pOldState->m_rand_preset`, `m_rot_*`; `rand_frame` is the
    /// frame's one `m_rand_frame`).
    pub warp_random: Option<PassRandom>,
    /// Random state of the old composite pass.
    pub comp_random: Option<PassRandom>,
}

/// Everything the renderer needs to draw a blend frame, on top of the new state's [`crate::FrameInput`] (whose
/// per-frame values are already the blended `m_pState->var_pf_*`).
#[derive(Clone, Debug)]
pub struct Md2Blend {
    /// `m_pState->m_fBlendProgress`: linear, `0..=1` (a blend ends once it exceeds 1).
    pub progress: f32,
    /// `m_verts[n].Diffuse >> 24` for every warp-mesh vertex (lactea mesh order): `(DWORD)(mix2 * 255)`, `mix2` being
    /// the clamped per-vertex blend factor (255 = new preset only).
    pub vert_alpha: Vec<u8>,
    /// The old state's shader-pass inputs.
    pub old: Md2OldPass,
    /// `Some(m_nOldWaveMode)` when the main wave is drawn twice (`its == 2`: blending and the wave mode changed).
    pub old_wave_mode: Option<i32>,
    /// `alpha_mult` of every entry of `FrameInput::shapes` / `FrameInput::waves` (`progress` for the new state's,
    /// `1 - progress` for the old one's). Missing entries mean 1.
    pub shape_alpha_mult: Vec<f32>,
    /// Same for `FrameInput::waves`.
    pub wave_alpha_mult: Vec<f32>,
}

/// `bIsNeeded` of the tile culling in `WarpedBlit_*` / `ShowToUser_Shaders`: with `flip_culling == false` a
/// triangle is drawn when any vertex alpha is > 0, with `true` when any is < 255 (`nAlphaTestValue`).
pub fn tile_needed(d: [u8; 3], flip_culling: bool) -> bool {
    if flip_culling {
        (d[0] & d[1] & d[2]) < 255
    } else {
        (d[0] | d[1] | d[2]) > 0
    }
}

/// Which state's shader / state parameters a pass uses (`nPass`: 0 = old, 1 = new).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Old,
    New,
}

/// One call of `WarpedBlit_*(nPass, bAlphaBlend, bFlipAlpha, bCullTiles, bFlipCulling)` /
/// `ShowToUser_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawPass {
    pub which: Which,
    /// `*_Shaders` (user / default pixel shader) rather than `*_NoShaders` (fixed function).
    pub shaders: bool,
    pub alpha_blend: bool,
    pub flip_alpha: bool,
    pub cull_tiles: bool,
    pub flip_culling: bool,
}

const fn pass(which: Which, shaders: bool, a: bool, fa: bool, c: bool, fc: bool) -> DrawPass {
    DrawPass {
        which,
        shaders,
        alpha_blend: a,
        flip_alpha: fa,
        cull_tiles: c,
        flip_culling: fc,
    }
}

/// The warp passes of `RenderFrame` while blending (`bOldPresetUsesWarpShader`, `bNewPresetUsesWarpShader`).
pub fn warp_passes(old_shader: bool, new_shader: bool) -> &'static [DrawPass] {
    use Which::{New, Old};
    const BOTH: [DrawPass; 2] = [
        pass(Old, true, false, false, true, true),
        pass(New, true, true, false, true, false),
    ];
    const NEW_ONLY: [DrawPass; 2] = [
        pass(Old, false, false, false, true, true),
        pass(New, true, true, false, true, false),
    ];
    const OLD_ONLY: [DrawPass; 2] = [
        pass(Old, true, false, false, true, true),
        pass(New, false, true, false, true, false),
    ];
    // "all the blending just happens in the vertex UV's, so just pretend there's no blend"
    const NONE: [DrawPass; 1] = [pass(New, false, false, false, false, false)];
    match (old_shader, new_shader) {
        (true, true) => &BOTH,
        (false, true) => &NEW_ONLY,
        (true, false) => &OLD_ONLY,
        (false, false) => &NONE,
    }
}

/// The composite passes of `RenderFrame` while blending (`bOldPresetUsesCompShader`, `bNewPresetUsesCompShader`);
/// `ShowToUser_NoShaders()` always draws the whole quad and comes first.
pub fn comp_passes(old_shader: bool, new_shader: bool) -> &'static [DrawPass] {
    use Which::{New, Old};
    const BOTH: [DrawPass; 2] = [
        pass(Old, true, false, false, true, true),
        pass(New, true, true, false, true, false),
    ];
    const NEW_ONLY: [DrawPass; 2] = [
        pass(New, false, false, false, false, false),
        pass(New, true, true, false, true, false),
    ];
    // "THA FUNKY REVERSAL"
    const OLD_ONLY: [DrawPass; 2] = [
        pass(New, false, false, false, false, false),
        pass(Old, true, true, true, true, true),
    ];
    const NONE: [DrawPass; 1] = [pass(New, false, false, false, false, false)];
    match (old_shader, new_shader) {
        (true, true) => &BOTH,
        (false, true) => &NEW_ONLY,
        (true, false) => &OLD_ONLY,
        (false, false) => &NONE,
    }
}

/// Texture coordinates of composite-mesh vertex `(i, j)` before the half-texel shift (`SquishToCenter` of the 32x24
/// grid, `plugin.cpp` ~2420), and the `x, y` (0..1, y up) `ShowToUser_Shaders` derives from `m_comp_verts[].x/y`
/// (`(u - 0.5/W)*2-1`, `-((v - 0.5/H)*2-1)`, then `*0.5+0.5`).
pub fn comp_vertex_xy01(i: usize, j: usize, w: u32, h: u32) -> (f32, f32) {
    let div_x = 1.0 / (FCGSX - 2) as f32;
    let div_y = 1.0 / (FCGSY - 2) as f32;
    let j2 = j - j / (FCGSY / 2);
    let i2 = i - i / (FCGSX / 2);
    let v = squish_to_center(j2 as f32 * div_y, 3.0);
    let u = squish_to_center(i2 as f32 * div_x, 3.0);
    let half_w = 0.5 / w as f32;
    let half_h = 0.5 / h as f32;
    let sy = -((v - half_h) * 2.0 - 1.0);
    let sx = (u - half_w) * 2.0 - 1.0;
    (sx * 0.5 + 0.5, sy * 0.5 + 0.5)
}

fn squish_to_center(x: f32, exp: f32) -> f32 {
    if x > 0.5 {
        return (x * 2.0 - 1.0).powf(exp) * 0.5 + 0.5;
    }
    (1.0 - (1.0 - x * 2.0).powf(exp)) * 0.5
}

/// The alpha byte of a composite vertex while blending (`ShowToUser_Shaders`): bilinear lookup of the warp mesh's
/// vertex alphas at `(x, y)` (0..1 over the mesh, the lookup point clamped to cells `0..grid-1`), then
/// `D3DCOLOR_RGBA_01`'s `(int)(alpha * 255)` of the double result.
pub fn comp_vertex_alpha(vert_alpha: &[u8], grid: [u32; 2], x01: f32, y01: f32) -> u8 {
    let (gx, gy) = (grid[0] as i32, grid[1] as i32);
    let mut x = x01 * (gx + 1) as f32;
    let mut y = y01 * (gy + 1) as f32;
    x = x.min((gx - 1) as f32).max(0.0);
    y = y.min((gy - 1) as f32).max(0.0);
    let nx = x as i32;
    let ny = y as i32;
    let dx = f64::from(x - nx as f32);
    let dy = f64::from(y - ny as f32);
    let at = |yy: i32, xx: i32| f64::from(vert_alpha[(yy * (gx + 1) + xx) as usize]);
    let mut alpha = at(ny, nx) * (1.0 - dx) * (1.0 - dy)
        + at(ny, nx + 1) * dx * (1.0 - dy)
        + at(ny + 1, nx) * (1.0 - dx) * dy
        + at(ny + 1, nx + 1) * dx * dy;
    alpha /= f64::from(255.0f32);
    ((alpha * 255.0) as i32 & 0xFF) as u8
}

/// A polyline of `DrawWave`: vertices before `SmoothWave`, and `nBreak` (`None` = -1).
#[derive(Clone, Debug, PartialEq)]
pub struct WaveLine {
    pub verts: Vec<[f32; 2]>,
    pub brk: Option<usize>,
}

/// The "blend 2 waveforms" block at the end of `DrawWave`: `new` is `v1[]` (the current wave), `old` is `v2[]`
/// (`m_nOldWaveMode`'s wave). Returns the blended line with `nVerts1 = new.verts.len()` vertices and the (possibly
/// moved) break. `mix` is `CosineInterp(progress)` (1 = new only).
pub fn blend_waves(new: &WaveLine, old: &WaveLine, mix: f32) -> WaveLine {
    let mix2 = 1.0 - mix;
    let n1 = new.verts.len();
    let n2 = old.verts.len();
    let mut out = new.clone();
    // "this won't yet handle the case where (nBreak1 > 0 && nBreak2 > 0)"
    let m = (n2 as f32 - 1.0) / n1 as f32;
    let nbreak2 = old.brk.map_or(-1, |b| b as i64);
    for i in 0..n1 {
        let f_idx = i as f32 * m;
        let n_idx = f_idx as usize;
        let t = f_idx - n_idx as f32;
        let (x, y);
        if n_idx as i64 == nbreak2 - 1 {
            x = old.verts[n_idx][0];
            y = old.verts[n_idx][1];
            out.brk = Some(i + 1);
        } else {
            // MD2 reads one vertex past the end for the very last index; its `v2` array is zero-filled there
            let b = old.verts.get(n_idx + 1).copied().unwrap_or([0.0, 0.0]);
            x = old.verts[n_idx][0] * (1.0 - t) + b[0] * t;
            y = old.verts[n_idx][1] * (1.0 - t) + b[1] * t;
        }
        out.verts[i][0] = new.verts[i][0] * mix + x * mix2;
        out.verts[i][1] = new.verts[i][1] * mix + y * mix2;
    }
    out
}

/// `alpha1 = alpha1*mix + alpha2*(1-mix)` of `DrawWave`.
pub fn blend_wave_alpha(alpha_new: f32, alpha_old: f32, mix: f32) -> f32 {
    alpha_new * mix + alpha_old * (1.0 - mix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_interp_endpoints_and_midpoint() {
        assert_eq!(cosine_interp(0.0), 0.0);
        assert!((cosine_interp(1.0) - 1.0).abs() < 1e-7);
        assert!((cosine_interp(0.5) - 0.5).abs() < 1e-7);
        // 0.5 - 0.5 cos(pi/4)
        assert!(
            (cosine_interp(0.25) - (0.5 - 0.5 * (std::f32::consts::FRAC_PI_4).cos())).abs() < 1e-6
        );
    }

    #[test]
    fn tile_culling_rules() {
        // normal culling: needed when any alpha > 0
        assert!(!tile_needed([0, 0, 0], false));
        assert!(tile_needed([0, 0, 1], false));
        assert!(tile_needed([255, 255, 255], false));
        // flipped culling: needed unless every alpha is 255
        assert!(!tile_needed([255, 255, 255], true));
        assert!(tile_needed([255, 255, 254], true));
        assert!(tile_needed([0, 0, 0], true));
        // `d1 & d2 & d3 < 255` (bitwise AND): 254 & 255 & 255 = 254
        assert!(tile_needed([254, 255, 255], true));
    }

    #[test]
    fn warp_pass_table() {
        use Which::{New, Old};
        let p = warp_passes(true, true);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0], pass(Old, true, false, false, true, true));
        assert_eq!(p[1], pass(New, true, true, false, true, false));
        let p = warp_passes(false, true);
        assert_eq!(
            (
                p[0].shaders,
                p[0].flip_culling,
                p[1].shaders,
                p[1].alpha_blend
            ),
            (false, true, true, true)
        );
        let p = warp_passes(true, false);
        assert_eq!(
            (
                p[0].shaders,
                p[1].shaders,
                p[1].alpha_blend,
                p[1].cull_tiles
            ),
            (true, false, true, true)
        );
        // neither: a single un-culled, un-blended fixed-function pass
        assert_eq!(
            warp_passes(false, false),
            vec![pass(New, false, false, false, false, false)]
        );
    }

    #[test]
    fn comp_pass_table_incl_funky_reversal() {
        use Which::{New, Old};
        assert_eq!(
            comp_passes(false, false),
            vec![pass(New, false, false, false, false, false)]
        );
        let p = comp_passes(false, true);
        assert_eq!(p[0], pass(New, false, false, false, false, false));
        assert_eq!(p[1], pass(New, true, true, false, true, false));
        // the old preset had a comp shader, the new one has not: fixed function first, then the OLD shader with flipped
        // alpha blending (src * (1 - a) + dst * a) and flipped culling
        let p = comp_passes(true, false);
        assert_eq!(p[0], pass(New, false, false, false, false, false));
        assert_eq!(p[1], pass(Old, true, true, true, true, true));
        let p = comp_passes(true, true);
        assert_eq!(p[0], pass(Old, true, false, false, true, true));
        assert_eq!(p[1], pass(New, true, true, false, true, false));
    }

    #[test]
    fn comp_vertices_cover_the_unit_square() {
        let (x, y) = comp_vertex_xy01(0, 0, 640, 360);
        // corner (0, 0): u = v = 0, so x01 = -0.5/W (the `-fHalfTexelW` shift of `m_comp_verts`), y01 = 1 + 0.5/H
        assert!(
            (x + 0.5 / 640.0).abs() < 1e-6 && (y - (1.0 + 0.5 / 360.0)).abs() < 1e-6,
            "top-left {x} {y}"
        );
        let (x, y) = comp_vertex_xy01(FCGSX - 1, FCGSY - 1, 640, 360);
        assert!(
            (x - (1.0 - 0.5 / 640.0)).abs() < 1e-6 && (y - 0.5 / 360.0).abs() < 1e-6,
            "bottom-right {x} {y}"
        );
        // symmetric about the centre (the two middle columns/rows are the duplicated centre seam)
        let (xa, ya) = comp_vertex_xy01(FCGSX / 2 - 1, FCGSY / 2 - 1, 640, 360);
        let (xb, yb) = comp_vertex_xy01(FCGSX / 2, FCGSY / 2, 640, 360);
        assert!((xa - 0.5).abs() < 2e-3 && (xb - 0.5).abs() < 2e-3);
        assert!((ya - 0.5).abs() < 2e-3 && (yb - 0.5).abs() < 2e-3);
    }

    #[test]
    fn comp_alpha_lookup_is_a_clamped_bilinear_fetch() {
        let grid = [4u32, 4];
        let n = ((grid[0] + 1) * (grid[1] + 1)) as usize;
        // constant alpha
        let a = vec![128u8; n];
        let v = comp_vertex_alpha(&a, grid, 0.37, 0.61);
        assert!(v == 127 || v == 128, "{v}");
        let full = vec![255u8; n];
        assert!(comp_vertex_alpha(&full, grid, 0.9, 0.1) >= 254);
        // alpha rises with the column index: reading further right gives a larger value
        let mut ramp = vec![0u8; n];
        for y in 0..5 {
            for x in 0..5 {
                ramp[y * 5 + x] = (x * 60) as u8;
            }
        }
        let l = comp_vertex_alpha(&ramp, grid, 0.05, 0.5);
        let r = comp_vertex_alpha(&ramp, grid, 0.6, 0.5);
        assert!(l < r, "{l} {r}");
        // hand check: (x01 = 0.4) * 5 = 2.0 -> cell 2 exactly: alpha00 = 120 (dx = 0) -> (int)(120/255*255) <= 120
        let h = comp_vertex_alpha(&ramp, grid, 0.4, 0.5);
        assert!(h == 119 || h == 120, "{h}");
        // the lookup point is clamped to the cells (x01 = 1 -> cell grid-1, dx = 0 -> alpha at column 3)
        let c = comp_vertex_alpha(&ramp, grid, 1.0, 0.5);
        assert!(c == 179 || c == 180, "{c}");
    }

    #[test]
    fn blend_waves_resamples_the_old_line_and_mixes() {
        let new = WaveLine {
            verts: vec![[0.0, 0.0]; 4],
            brk: None,
        };
        let old = WaveLine {
            verts: vec![[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]],
            brk: None,
        };
        // mix = 1: the new wave untouched; mix = 0: the old wave resampled onto 4 vertices
        assert_eq!(blend_waves(&new, &old, 1.0).verts, new.verts);
        let b = blend_waves(&new, &old, 0.0);
        // m = (3-1)/4 = 0.5: indices 0, .5, 1, 1.5
        let want = [[1.0, 2.0], [2.0, 3.0], [3.0, 4.0], [4.0, 5.0]];
        for (g, w) in b.verts.iter().zip(want) {
            assert!(
                (g[0] - w[0]).abs() < 1e-6 && (g[1] - w[1]).abs() < 1e-6,
                "{g:?} {w:?}"
            );
        }
        // halfway
        let b = blend_waves(&new, &old, 0.5);
        assert!((b.verts[2][0] - 1.5).abs() < 1e-6 && (b.verts[2][1] - 2.0).abs() < 1e-6);
    }

    #[test]
    fn blend_waves_moves_the_break_to_the_old_break() {
        let new = WaveLine {
            verts: vec![[0.0; 2]; 8],
            brk: Some(4),
        };
        let old = WaveLine {
            verts: vec![[0.0; 2]; 6],
            brk: Some(3),
        };
        let b = blend_waves(&new, &old, 0.5);
        // m = 5/8: nIdx == nBreak2 - 1 = 2 for fIdx in [2,3): i = 4 (2.5) -> break after vertex i + 1 = 5
        assert_eq!(b.brk, Some(5));
        // old wave without break keeps the new break
        let old2 = WaveLine {
            verts: vec![[0.0; 2]; 6],
            brk: None,
        };
        assert_eq!(blend_waves(&new, &old2, 0.5).brk, Some(4));
    }

    #[test]
    fn wave_alpha_blend_is_linear_in_the_cosine_mix() {
        assert_eq!(blend_wave_alpha(1.0, 0.0, 1.0), 1.0);
        assert_eq!(blend_wave_alpha(1.0, 0.0, 0.0), 0.0);
        assert!((blend_wave_alpha(0.8, 0.2, 0.25) - (0.8 * 0.25 + 0.2 * 0.75)).abs() < 1e-7);
    }
}
