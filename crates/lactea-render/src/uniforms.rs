// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Filling of [`MilkUniforms`] exactly like `CPlugin::ApplyShaderParams` (milkdropfs.cpp),
//! including its quirks, plus the preset-level random state (`CState::RandomizePresetVars`).

use crate::blur::safe_blur_min_max;
use crate::config::CompatRules;
use crate::frame::{BlurParams, FrameInput, PassRandom};
use crate::rng::Rng;
use crate::shader_ext::ShaderExt;
use lactea_abi::{MilkUniforms, NUM_ROT};

/// Per-preset random rotation/translation parameters (`m_rot_base`, `m_rot_speed`, `m_xlate`)
/// for the 24 `rot_*` matrices, plus `m_rand_preset`.
#[derive(Clone, Debug, PartialEq)]
pub struct RotParams {
    /// `m_rot_base`: starting angles of each matrix.
    pub base: [[f32; 3]; NUM_ROT],
    /// `m_rot_speed`: angular speed of each matrix.
    pub speed: [[f32; 3]; NUM_ROT],
    /// `m_xlate`: translation of each matrix.
    pub xlate: [[f32; 3]; NUM_ROT],
    /// `m_rand_preset` as drawn by `RandomizePresetVars` (the engine may also supply its own in
    /// [`FrameInput::rand_preset`]; the uniform always uses the `FrameInput` value).
    pub rand_preset: [f32; 4],
    /// `m_fRandStart[0..4]` / projectM `hueRandomOffsets`: phases of the hue-shader corner colours.
    pub hue_offsets: [f32; 4],
}

impl RotParams {
    /// `CState::RandomizePresetVars`, with MD2's `FRAND` replaced by the seeded generator.
    /// Call once per preset load.
    pub fn randomize(rng: &mut Rng) -> Self {
        let mut p = RotParams {
            base: [[0.0; 3]; NUM_ROT],
            speed: [[0.0; 3]; NUM_ROT],
            xlate: [[0.0; 3]; NUM_ROT],
            rand_preset: [rng.frand(), rng.frand(), rng.frand(), rng.frand()],
            hue_offsets: [
                rng.below(64841) as f32 * 0.01,
                rng.below(53751) as f32 * 0.01,
                rng.below(42661) as f32 * 0.01,
                rng.below(31571) as f32 * 0.01,
            ],
        };
        for k in 0..NUM_ROT {
            let rot_mult = 0.9 * (k as f32 / 8.0).powf(3.2);
            let xlate_mult = 1.0;
            p.xlate[k] = [
                (rng.frand() * 2.0 - 1.0) * xlate_mult,
                (rng.frand() * 2.0 - 1.0) * xlate_mult,
                (rng.frand() * 2.0 - 1.0) * xlate_mult,
            ];
            p.base[k] = [rng.frand() * 6.28, rng.frand() * 6.28, rng.frand() * 6.28];
            p.speed[k] = [
                (rng.frand() * 2.0 - 1.0) * rot_mult,
                (rng.frand() * 2.0 - 1.0) * rot_mult,
                (rng.frand() * 2.0 - 1.0) * rot_mult,
            ];
        }
        p
    }
}

impl Default for RotParams {
    fn default() -> Self {
        Self::randomize(&mut Rng::new(0))
    }
}

type M4 = [[f32; 4]; 4];

fn mat_mul(a: &M4, b: &M4) -> M4 {
    let mut r = [[0.0f32; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[i][k] * b[k][j];
            }
            r[i][j] = s;
        }
    }
    r
}

fn ident() -> M4 {
    let mut m = [[0.0; 4]; 4];
    for i in 0..4 {
        m[i][i] = 1.0;
    }
    m
}

/// `D3DXMatrixRotationX` (row-vector convention).
fn rotation_x(a: f32) -> [[f32; 4]; 4] {
    let (s, c) = a.sin_cos();
    let mut m = ident();
    m[1][1] = c;
    m[1][2] = s;
    m[2][1] = -s;
    m[2][2] = c;
    m
}

/// `D3DXMatrixRotationY`.
fn rotation_y(a: f32) -> [[f32; 4]; 4] {
    let (s, c) = a.sin_cos();
    let mut m = ident();
    m[0][0] = c;
    m[0][2] = -s;
    m[2][0] = s;
    m[2][2] = c;
    m
}

/// `D3DXMatrixRotationZ`.
fn rotation_z(a: f32) -> [[f32; 4]; 4] {
    let (s, c) = a.sin_cos();
    let mut m = ident();
    m[0][0] = c;
    m[0][1] = s;
    m[1][0] = -s;
    m[1][1] = c;
    m
}

/// `D3DXMatrixTranslation`.
fn translation(x: f32, y: f32, z: f32) -> [[f32; 4]; 4] {
    let mut m = ident();
    m[3][0] = x;
    m[3][1] = y;
    m[3][2] = z;
    m
}

/// `temp = Rx * T * Rz * Ry` (D3DXMatrixMultiply order used by `ApplyShaderParams`).
pub fn rot_matrix(ax: f32, ay: f32, az: f32, t: [f32; 3]) -> [[f32; 4]; 4] {
    let mut m = mat_mul(&rotation_x(ax), &translation(t[0], t[1], t[2]));
    m = mat_mul(&m, &rotation_z(az));
    mat_mul(&m, &rotation_y(ay))
}

/// Pack a D3DX 4x4 (row-major `m[row][col]`) into the ABI's `float4x3` -> `mat3x4` layout:
/// `out[col][row] = m[row][col]` for `col < 3`.
pub fn pack_float4x3(m: &M4) -> [[f32; 4]; 3] {
    let mut out = [[0.0f32; 4]; 3];
    for col in 0..3 {
        for row in 0..4 {
            out[col][row] = m[row][col];
        }
    }
    out
}

/// `rot_*` matrices: `[0..4)` static, `[4..20)` slow/fast/very-fast/ultra-fast dynamic
/// (`base + speed*time`), `[20..24)` random every call (`FRAND*6.28`, `FRAND` translation).
pub(crate) fn rot_matrices(rot: &RotParams, time: f64, rng: &mut Rng) -> [[[f32; 4]; 3]; NUM_ROT] {
    let mut out = [[[0.0f32; 4]; 3]; NUM_ROT];
    for i in 0..20 {
        // MD2: `m_rot_base.x + m_rot_speed.x*time` with `time` a double.
        let ang = |k: usize| (rot.base[i][k] as f64 + rot.speed[i][k] as f64 * time) as f32;
        let m = rot_matrix(ang(0), ang(1), ang(2), rot.xlate[i]);
        out[i] = pack_float4x3(&m);
    }
    for i in 20..24 {
        let ax = rng.frand() * 6.28;
        let ay = rng.frand() * 6.28;
        let az = rng.frand() * 6.28;
        let t = [rng.frand(), rng.frand(), rng.frand()];
        let m = rot_matrix(ax, ay, az, t);
        out[i] = pack_float4x3(&m);
    }
    out
}

/// projectM uploads `glm::mat3x4(mat4)`: only the three rotation columns, so the translation
/// (row 3 of the ABI's `float4x3`) is lost. Zero it (see [`crate::CompatRules::rot_translation_dropped`]).
pub fn drop_rot_translation(rot: &mut [[[f32; 4]; 3]; NUM_ROT]) {
    for m in rot.iter_mut() {
        for col in m.iter_mut() {
            col[3] = 0.0;
        }
    }
}

/// Sizes of the noise textures as `(w, h, 1/w, 1/h)` in `NoiseTex::index()` order.
pub(crate) fn noise_texsizes() -> [[f32; 4]; 6] {
    let mut out = [[0.0; 4]; 6];
    for (i, t) in crate::noise::ALL_NOISE.iter().enumerate() {
        let (s, _, _) = crate::noise::noise_spec(*t);
        let s = s as f32;
        out[i] = [s, s, 1.0 / s, 1.0 / s];
    }
    out
}

/// Everything [`fill_milk_uniforms`] reads besides the random generator.
///
/// * `width`, `height`: `GetWidth()/GetHeight()` (used for aspect and the mip registers).
/// * `tex_w`, `tex_h`: `m_nTexSizeX/Y` (`_c7`).
/// * `q`, `blur`, `ext`: the `q1..q32` values, blur range and MilkDrop 3 inputs (shader time) of the state the pass
///   belongs to (the frame's own, or the old preset's during a MilkDrop 2 / 3 blend).
pub(crate) struct MilkUniformInputs<'a> {
    pub width: u32,
    pub height: u32,
    pub tex_w: u32,
    pub tex_h: u32,
    pub frame: &'a FrameInput,
    pub q: &'a [f32; 32],
    pub blur: &'a BlurParams,
    pub ext: &'a ShaderExt,
    pub rot: &'a RotParams,
    pub rules: &'a CompatRules,
    pub pass: Option<&'a PassRandom>,
}

impl<'a> MilkUniformInputs<'a> {
    /// Inputs for a pass of `f`'s own state: texture size = `width x height`, `q`, `blur` and `ext` taken from `f`.
    pub fn for_frame(
        width: u32,
        height: u32,
        f: &'a FrameInput,
        rot: &'a RotParams,
        rules: &'a CompatRules,
        pass: Option<&'a PassRandom>,
    ) -> Self {
        Self {
            width,
            height,
            tex_w: width,
            tex_h: height,
            frame: f,
            q: &f.q,
            blur: &f.blur,
            ext: &f.ext,
            rot,
            rules,
            pass,
        }
    }
}

/// Fill [`MilkUniforms`] like `ApplyShaderParams`.
///
/// * `rot_rng`: consumed for the 4 per-call random rotation matrices.
///
/// MD2-vs-projectM choices come from `rules` (see [`crate::Compat`]):
/// * `_c3.w`/`_c4.w` (`vol`, `vol_att`): projectM `(b+m+t)*0.333`; MD2 `0.3333f*(a, b, c)` — the
///   C comma operator, i.e. `0.3333 * treb`.
/// * `mip_y`: projectM from the height; MD2 from the *width* (typo).
/// * `_c1`: projectM zeros; MD2 the "sd" aspect.
/// * `_c8.._c11` use the double-precision `time` (`time*0.329f` etc.).
/// * `_c2.x` is `ext.time` (MilkDrop 3: seconds since the pass's own preset started) when set, else `time`; both are
///   wrapped modulo 10000.
pub(crate) fn fill_milk_uniforms(inp: &MilkUniformInputs<'_>, rot_rng: &mut Rng) -> MilkUniforms {
    let MilkUniformInputs {
        width,
        height,
        tex_w,
        tex_h,
        frame: f,
        q,
        blur,
        ext,
        rot,
        rules,
        pass,
    } = *inp;
    let mut u = MilkUniforms::default();
    let w = width as f32;
    let h = height as f32;

    match pass {
        Some(p) => {
            u.rand_frame = p.rand_frame;
            u.rand_preset = p.rand_preset;
        }
        None => {
            u.rand_frame = f.rand_frame;
            u.rand_preset = f.rand_preset;
        }
    }

    // aspect (non screen-dependent mode)
    let (mut aspect_x, mut aspect_y) = (1.0f32, 1.0f32);
    if width > height {
        aspect_y = h / w;
    } else {
        aspect_x = w / h;
    }
    u.c[0] = [aspect_x, aspect_y, 1.0 / aspect_x, 1.0 / aspect_y];
    if !rules.c1_is_zero {
        u.c[1] = [aspect_x, aspect_y, 1.0 / aspect_x, 1.0 / aspect_y];
    }
    // `_c2.x`: time since preset start wrapped at 10000 s (`ApplyShaderParams` / projectM `LoadVariables`).
    let ft = ext.time.unwrap_or(f.time) as f32;
    let wrapped = ft - ((ft as f64 / 10000.0) as i32 * 10000) as f32;
    u.c[2] = [wrapped, f.fps, f.frame as f32, f.progress];
    let (vol, vol_att) = if rules.vol_is_comma_bug {
        (0.3333 * f.treb, 0.3333 * f.treb_att)
    } else {
        (
            (f.bass + f.mid + f.treb) * 0.333,
            (f.bass_att + f.mid_att + f.treb_att) * 0.333,
        )
    };
    u.c[3] = [f.bass, f.mid, f.treb, vol];
    u.c[4] = [f.bass_att, f.mid_att, f.treb_att, vol_att];

    let (bmin, bmax) = safe_blur_min_max(blur.min, blur.max);
    u.c[5] = [bmax[0] - bmin[0], bmin[0], bmax[1] - bmin[1], bmin[1]];
    u.c[6] = [bmax[2] - bmin[2], bmin[2], bmin[0], bmax[0]];
    u.c[7] = [
        tex_w as f32,
        tex_h as f32,
        1.0 / tex_w as f32,
        1.0 / tex_h as f32,
    ];

    let t = f.time;
    let cosv = |m: [f32; 4], o: [f32; 4]| -> [f32; 4] {
        let mut r = [0.0; 4];
        for k in 0..4 {
            r[k] = 0.5 + 0.5 * ((t * m[k] as f64 + o[k] as f64) as f32).cos();
        }
        r
    };
    let sinv = |m: [f32; 4], o: [f32; 4]| -> [f32; 4] {
        let mut r = [0.0; 4];
        for k in 0..4 {
            r[k] = 0.5 + 0.5 * ((t * m[k] as f64 + o[k] as f64) as f32).sin();
        }
        r
    };
    let fast_m = [0.329, 1.293, 5.070, 20.051];
    let fast_o = [1.2, 3.9, 2.5, 5.4];
    let slow_m = [0.0050, 0.0085, 0.0133, 0.0217];
    let slow_o = [2.7, 5.3, 4.5, 3.8];
    u.c[8] = cosv(fast_m, fast_o);
    u.c[9] = sinv(fast_m, fast_o);
    u.c[10] = cosv(slow_m, slow_o);
    u.c[11] = sinv(slow_m, slow_o);

    let mip_x = w.ln() / 2.0f32.ln();
    let mip_y = if rules.mip_y_from_width {
        w.ln() / 2.0f32.ln()
    } else {
        h.ln() / 2.0f32.ln()
    };
    u.c[12] = [mip_x, mip_y, 0.5 * (mip_x + mip_y), 0.0];
    u.c[13] = [bmin[1], bmax[1], bmin[2], bmax[2]];

    for i in 0..8 {
        u.q[i] = [q[i * 4], q[i * 4 + 1], q[i * 4 + 2], q[i * 4 + 3]];
    }
    u.rot = match pass.and_then(|p| p.rot.as_deref()) {
        Some(r) => *r,
        None => {
            let mut r = rot_matrices(rot, f.time, rot_rng);
            if rules.rot_translation_dropped {
                drop_rot_translation(&mut r);
            }
            r
        }
    };
    u.texsize_noise = noise_texsizes();
    u
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Compat;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    fn frame() -> FrameInput {
        let mut f = FrameInput::default();
        f.time = 12.5;
        f.fps = 60.0;
        f.frame = 750;
        f.progress = 0.25;
        f.bass = 1.1;
        f.mid = 0.9;
        f.treb = 0.6;
        f.bass_att = 1.2;
        f.mid_att = 1.0;
        f.treb_att = 0.3;
        f.blur.min = [0.1, 0.2, 0.3];
        f.blur.max = [0.9, 0.8, 0.7];
        for i in 0..32 {
            f.q[i] = i as f32 + 1.0;
        }
        f.rand_frame = [0.1, 0.2, 0.3, 0.4];
        f.rand_preset = [0.5, 0.6, 0.7, 0.8];
        f
    }

    #[test]
    fn registers_match_hand_computation() {
        let f = frame();
        let rot = RotParams::default();
        let u = fill_milk_uniforms(
            &MilkUniformInputs::for_frame(640, 360, &f, &rot, &Compat::MilkDrop2.rules(), None),
            &mut Rng::new(1),
        );
        // aspect (wide): (1, 0.5625, 1, 1/0.5625)
        assert_eq!(u.c[0], [1.0, 0.5625, 1.0, 1.0 / 0.5625]);
        assert_eq!(u.c[1], u.c[0]); // MD2 compat: sd aspect
        assert_eq!(u.c[2], [12.5, 60.0, 750.0, 0.25]);
        // vol is the comma-operator quirk: 0.3333 * treb, NOT the average
        assert_eq!(u.c[3][..3], [1.1, 0.9, 0.6]);
        assert!(close(u.c[3][3], 0.3333 * 0.6));
        assert!(close(u.c[4][3], 0.3333 * 0.3));
        // blur: window sizes progress: level2 clamped into level1 [0.1,0.9] -> [0.2,0.8]; level3 -> [0.3,0.7]
        assert!(
            close(u.c[5][0], 0.8)
                && close(u.c[5][1], 0.1)
                && close(u.c[5][2], 0.6)
                && close(u.c[5][3], 0.2)
        );
        assert!(
            close(u.c[6][0], 0.4)
                && close(u.c[6][1], 0.3)
                && close(u.c[6][2], 0.1)
                && close(u.c[6][3], 0.9)
        );
        assert!(
            close(u.c[13][0], 0.2)
                && close(u.c[13][1], 0.8)
                && close(u.c[13][2], 0.3)
                && close(u.c[13][3], 0.7)
        );
        assert_eq!(u.c[7], [640.0, 360.0, 1.0 / 640.0, 1.0 / 360.0]);
        // roam
        let c = 0.5 + 0.5 * ((12.5f64 * 0.329f32 as f64 + 1.2f32 as f64) as f32).cos();
        assert!(close(u.c[8][0], c));
        let s = 0.5 + 0.5 * ((12.5f64 * 20.051f32 as f64 + 5.4f32 as f64) as f32).sin();
        assert!(close(u.c[9][3], s));
        let sc = 0.5 + 0.5 * ((12.5f64 * 0.0085f32 as f64 + 5.3f32 as f64) as f32).cos();
        assert!(close(u.c[10][1], sc));
        // mip: both from the width
        let m = 640f32.ln() / 2f32.ln();
        assert!(close(u.c[12][0], m) && close(u.c[12][1], m) && close(u.c[12][2], m));
        // projectM compat: vol = (b+m+t)*0.333, mip_y from height, _c1 zero
        let up = fill_milk_uniforms(
            &MilkUniformInputs::for_frame(640, 360, &f, &rot, &Compat::ProjectM.rules(), None),
            &mut Rng::new(1),
        );
        assert!(close(up.c[3][3], (1.1 + 0.9 + 0.6) * 0.333));
        assert!(close(up.c[4][3], (1.2 + 1.0 + 0.3) * 0.333));
        assert!(close(up.c[12][1], 360f32.ln() / 2f32.ln()));
        assert_eq!(up.c[1], [0.0; 4]);
        // q packing
        assert_eq!(u.q[0], [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(u.q[7], [29.0, 30.0, 31.0, 32.0]);
        assert_eq!(u.rand_frame, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(u.rand_preset, [0.5, 0.6, 0.7, 0.8]);
        // noise sizes
        assert_eq!(u.texsize_noise[0], [256.0, 256.0, 1.0 / 256.0, 1.0 / 256.0]);
        assert_eq!(u.texsize_noise[1][0], 32.0);
        assert_eq!(u.texsize_noise[5][0], 32.0);
    }

    #[test]
    fn portrait_aspect() {
        let f = frame();
        let u = fill_milk_uniforms(
            &MilkUniformInputs::for_frame(
                360,
                640,
                &f,
                &RotParams::default(),
                &Compat::MilkDrop2.rules(),
                None,
            ),
            &mut Rng::new(1),
        );
        assert_eq!(u.c[0], [0.5625, 1.0, 1.0 / 0.5625, 1.0]);
    }

    #[test]
    fn rot_matrices_static_dynamic_random() {
        let rot = RotParams::randomize(&mut Rng::new(42));
        // rot_s1 has speed exactly 0; rot_s2..4 drift slowly (0.9*(k/8)^3.2 rad/s max)
        let a = rot_matrices(&rot, 0.0, &mut Rng::new(1));
        let b = rot_matrices(&rot, 100.0, &mut Rng::new(1));
        assert_eq!(a[0], b[0], "rot_s1 is static");
        assert_ne!(a[2], b[2], "rot_s3 drifts slowly (0.9*(2/8)^3.2 rad/s max)");
        // dynamic ones change
        assert_ne!(a[8], b[8]);
        // random ones depend on the rng, not time
        let c = rot_matrices(&rot, 0.0, &mut Rng::new(2));
        assert_ne!(a[20], c[20]);
        assert_eq!(a[19], c[19]);
    }

    #[test]
    fn rot_matrix_layout_matches_d3dx() {
        // pure Z rotation by 90deg with translation: D3DX row-vector math:
        // v=(1,0,0,1) * (Rx*T*Rz*Ry). Rx=I. T moves by t. Rz rotates (x,y): (x,y)->(x*c - y*s, x*s + y*c)
        let m = rot_matrix(0.0, 0.0, std::f32::consts::FRAC_PI_2, [0.0, 0.0, 0.0]);
        let p = pack_float4x3(&m);
        // WGSL `v * M` for mat3x4: result_i = dot(v, column_i)
        let v = [1.0f32, 0.0, 0.0, 1.0];
        let res: Vec<f32> = (0..3)
            .map(|c| (0..4).map(|k| v[k] * p[c][k]).sum())
            .collect();
        assert!(close(res[0], 0.0) && close(res[1], 1.0) && close(res[2], 0.0));
        // translation lives in row 3 -> last element of each column
        let m = rot_matrix(0.0, 0.0, 0.0, [0.25, 0.5, 0.75]);
        let p = pack_float4x3(&m);
        assert_eq!([p[0][3], p[1][3], p[2][3]], [0.25, 0.5, 0.75]);
    }

    #[test]
    fn randomize_matches_md2_distribution() {
        let r = RotParams::randomize(&mut Rng::new(9));
        // rot_mult = 0.9*(k/8)^3.2: exactly zero only for k = 0, tiny for the other "static" ones
        assert_eq!(r.speed[0], [0.0; 3]);
        for k in 1..4 {
            let lim = 0.9 * (k as f32 / 8.0).powf(3.2) + 1e-6;
            assert!(
                r.speed[k].iter().all(|s| s.abs() <= lim),
                "static rotation {k} speed bound"
            );
        }
        for k in 0..NUM_ROT {
            for c in 0..3 {
                assert!(r.base[k][c] >= 0.0 && r.base[k][c] < 6.28);
                assert!(r.xlate[k][c].abs() <= 1.0);
                assert!(r.speed[k][c].abs() <= 0.9 * (k as f32 / 8.0).powf(3.2) + 1e-6);
            }
        }
    }

    fn fill(f: &FrameInput, rules: &CompatRules) -> MilkUniforms {
        fill_milk_uniforms(
            &MilkUniformInputs::for_frame(640, 360, f, &RotParams::default(), rules, None),
            &mut Rng::new(1),
        )
    }

    /// `_c2.x` shows `ShaderExt::time` when set, wrapped modulo 10000 like the absolute time; `_c8.._c11` still
    /// follow `FrameInput::time`.
    #[test]
    fn shader_time_is_the_ext_time_wrapped_at_10000() {
        let rules = Compat::MilkDrop3.rules();
        // E-core Annex D T9: the preset started at 100 s and it is now 12445.5 s; the engine passes the difference
        let mut f = frame();
        f.ext.time = Some(12345.5);
        let u = fill(&f, &rules);
        assert_eq!(u.c[2][0], 2345.5);
        let base = fill(&frame(), &rules);
        assert_eq!(
            u.c[2][1..],
            base.c[2][1..],
            "fps, frame, progress untouched"
        );
        assert_eq!(
            u.c[8..12],
            base.c[8..12],
            "roam registers use FrameInput::time"
        );
        // no ext time: FrameInput::time, same wrap as before
        let mut g = frame();
        g.time = 12345.5;
        assert_eq!(fill(&g, &rules).c[2][0], 2345.5);
        // the ext time wins over a different absolute time and does not wrap below the limit
        let mut h = frame();
        h.time = 99999.0;
        h.ext.time = Some(3.25);
        assert_eq!(fill(&h, &rules).c[2][0], 3.25);
    }

    /// `ShaderExt::default()` is neutral: `_c2.x` is `FrameInput::time` and nothing of the extension (mouse, q33..,
    /// FFT) leaks into the frozen `MilkUniforms` block.
    #[test]
    fn default_ext_leaves_the_registers_alone() {
        let rules = Compat::MilkDrop2.rules();
        let bytes = |f: &FrameInput| bytemuck::bytes_of(&fill(f, &rules)).to_vec();
        let f = frame();
        assert_eq!(f.ext, ShaderExt::default());
        assert_eq!(fill(&f, &rules).c[2][0], 12.5);
        let base = bytes(&f);

        let mut g = frame();
        g.ext.mouse = [0.3, 0.6, 1.0, 1.0];
        g.ext.q_hi = [7.0; 32];
        g.ext.fft = Some(std::sync::Arc::new([0.5; lactea_abi::FFT_BINS]));
        assert_eq!(bytes(&g), base);

        let mut h = frame();
        h.ext.time = Some(h.time);
        assert_eq!(bytes(&h), base);
    }
}
