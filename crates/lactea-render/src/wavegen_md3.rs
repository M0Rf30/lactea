// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Main waveform of MilkDrop 3 (3.37): the 17 modes `0..=16` (`docs/md3.md`, "Main waveform").
//!
//! MD3 keeps the BeatDrop `DrawWave` structure (per-pass set-up, blend of the old and the new mode, colour,
//! `SmoothWave`, thick / dots passes) and differs in the modes themselves: every formula reads *one* channel (like the
//! original MilkDrop 2 and unlike BeatDrop, which averages left and right), mode 14 really halves `rad` and `ang`,
//! mode 13 reads past the end of the right channel into the spectrum, mode 16 is BeatDrop's triangle spinning the other
//! way, and BeatDrop's mode 17 does not exist. The shared tail (`WaveGeom` consumers in the renderer) is the MD2 one.
//!
//! Output convention as in [`crate::wavegen`]: clip space, x right, y up, before `SmoothWave`.

use crate::frame::MainWave;
use crate::wavegen::{
    NUM_WAVE_INPUT, NUM_WAVEFORM_SAMPLES, WaveGeom, finish_alpha, line_edges, line_strip,
    size_gain, wave_rgb, wrap_param2,
};

/// `NUM_WAVES` of MilkDrop 3: valid modes are `0..=16`; the drawn mode is `(int)wave_mode % 17`.
pub const MD3_NUM_WAVES: i32 = 17;

/// The mode MilkDrop 3 draws for the per-frame variable `wave_mode`: C truncation to `int`
/// (`cvttsd2si`: NaN and out-of-range values give `INT_MIN`), then the C remainder by 17, which keeps the sign of the
/// dividend. A negative result is not a mode: [`main_wave_geometry_md3_raw`] then yields 480 zero vertices.
pub fn md3_wave_mode(wave_mode: f64) -> i32 {
    let t = wave_mode.trunc();
    let i = if t.is_nan() || !(-2147483648.0..2147483648.0).contains(&t) {
        i32::MIN
    } else {
        t as i32
    };
    i % MD3_NUM_WAVES
}

/// Samples read by mode 13 beyond the right channel: MD3's `fWave[1]` is followed in memory by `fSpecLeft`, so
/// index `k >= 576` of the right channel is `spec_l[k - 576]`.
fn extended_right(wr: &[f32], spec_l: &[f32], k: usize) -> f32 {
    match wr.get(k) {
        Some(&v) => v,
        None => spec_l.get(k - wr.len()).copied().unwrap_or(0.0),
    }
}

/// One pass of MD3's `DrawWave` loop: the vertices and the alpha of the wave number `mw.mode` *before* the
/// "alpha < 0.004" test (which applies to the possibly blended result).
///
/// * `wl`, `wr`: the output of [`crate::wavegen::prepare_main_waveform`]; `spec_l`: `fSpecLeft` (modes 8 and 13).
/// * `mw.mode` is used as given: the caller applies [`md3_wave_mode`] for the new wave and passes the old preset's
///   mode unchanged for the second pass of a blend. A mode outside `0..=16` gives 480 zero vertices with the plain
///   `wave_a` alpha, like MD3 (`docs/md3.md`).
/// * `blending` is `m_bBlending`: the modes that close their loop by repeating the first vertex (0, 13, 14, 16) do
///   not while blending.
///
/// `None` only when a strip would have no vertices (texture narrower than 3 pixels).
pub fn main_wave_geometry_md3_raw(
    mw: &MainWave,
    wl: &[f32],
    wr: &[f32],
    spec_l: &[f32],
    time: f32,
    imm_rel: [f32; 3],
    imm_abs: [f32; 3],
    tex_w: u32,
    aspect: (f32, f32),
    blending: bool,
) -> Option<WaveGeom> {
    let (asp_x, asp_y) = aspect;
    if wl.len() < NUM_WAVE_INPUT || wr.len() < NUM_WAVE_INPUT {
        return None;
    }
    let (fl, fr) = (wl, wr);
    let rgb = wave_rgb(mw);
    let pos_x = mw.x * 2.0 - 1.0;
    let pos_y = mw.y * 2.0 - 1.0;
    let param = mw.mystery;
    let treble = imm_abs[2];
    let wave = mw.mode;
    let t = time;

    // `param2` differs from `param` only for the modes that wrap it into [-1, 1)
    let param2 = wrap_param2(wave, param);

    let mut alpha = mw.rgba[3];
    let finish = |alpha: f32| finish_alpha(mw, imm_rel, alpha);
    // modes 6..=11: vertices per strip (`min(240, W/3)`) and the sample offset that centres them in the 480 samples
    let line_n = (NUM_WAVEFORM_SAMPLES / 2).min(tex_w as usize / 3);
    let line_off = (NUM_WAVEFORM_SAMPLES - line_n) / 2;

    let mut brk: Option<usize> = None;
    let mut v: Vec<[f32; 2]>;
    match wave {
        // circle, right channel only; the first 24 vertices cross-fade into the next 240 samples
        0 | 13 | 14 => {
            alpha = finish(alpha);
            let o = 120usize;
            v = Vec::with_capacity(241);
            for i in 0..240usize {
                let fi = i as f32;
                let ang = 0.2 * t + fi * (1.0 / 239.0) * 6.28;
                let (x, y) = match wave {
                    0 => {
                        let mut rad = 0.5 + 0.4 * fr[o + i] + param2;
                        if i < 24 {
                            let m = 0.5 - 0.5 * (fi / 24.0 * 3.1416).cos();
                            let rad2 = 0.5 + 0.4 * fr[o + 240 + i] + param2;
                            rad = rad2 * (1.0 - m) + rad * m;
                        }
                        (
                            rad * ang.cos() * asp_y + pos_x,
                            rad * ang.sin() * asp_x + pos_y,
                        )
                    }
                    13 => {
                        let mut rad = 0.7 + 0.4 * extended_right(fr, spec_l, o + i) + param;
                        if fi < 240.0 / rad {
                            let m = 0.5 - 0.5 * (fi / 24.0 * 3.1416).cos();
                            // index 360 + i runs past the 576 samples for i >= 216 (into the spectrum)
                            let rad2 = 0.5 + 0.4 * extended_right(fr, spec_l, 360 + i) + param;
                            rad = rad2 * (1.0 - m) + rad * m;
                        }
                        (
                            rad * ang.cos() * asp_y + pos_x,
                            rad * ang.sin() * asp_x + pos_y,
                        )
                    }
                    _ => {
                        // MD3 really halves `rad` and `ang`; the cross-fade reuses the same sample
                        let rad0 = 0.7 + 0.7 * fr[o + i] + param;
                        let mut rad = rad0 / 2.0;
                        let ang = ang / 2.0;
                        if fi < 240.0 / rad {
                            let m = 0.7 - 0.7 * (fi / 24.0 * 3.1416).cos();
                            rad = rad0 * (1.0 - m) + 0.25 * m * rad;
                        }
                        let c = 3.1416f32.cos();
                        (
                            rad * (ang * 3.1416).cos() * asp_y / 1.5 + pos_x * c,
                            rad * (ang - t / 3.0).sin() * asp_x / 1.5 + pos_y * c,
                        )
                    }
                };
                v.push([x, y]);
            }
            if !blending {
                v.push(v[0]);
            }
        }
        // x-y oscillator spiral; mode 12 is the skewed variant whose angle also carries the final alpha
        1 | 12 => {
            alpha = finish(alpha * 1.25);
            v = (0..240usize)
                .map(|i| {
                    if wave == 1 {
                        let rad = 0.53 + 0.43 * fr[i] + param2;
                        let ang = 1.57 * fl[i + 32] + 2.3 * t;
                        [
                            rad * ang.cos() * asp_y + pos_x,
                            rad * ang.sin() * asp_x + pos_y,
                        ]
                    } else {
                        let rad = 0.63 + 0.23 * fr[i] + param;
                        let ang = 0.9 * fl[i + 32] + 3.3 * t;
                        [
                            rad * (ang + alpha).cos() * asp_y + pos_x,
                            rad * ang.sin() * asp_x + pos_y,
                        ]
                    }
                })
                .collect();
        }
        // spiro: faint (2) / treble-driven (3)
        2 | 3 => {
            if wave == 2 {
                alpha *= size_gain(tex_w);
            } else {
                alpha = match tex_w {
                    256 => 0.075,
                    512 => 0.15,
                    1024 => 0.22,
                    2048 => 0.33,
                    _ => alpha,
                };
                alpha *= 1.3 * treble * treble;
            }
            alpha = finish(alpha);
            v = (0..NUM_WAVEFORM_SAMPLES)
                .map(|i| [fr[i] * asp_y + pos_x, fl[i + 32] * asp_x + pos_y])
                .collect();
        }
        // script with momentum, left channel for y
        4 => {
            alpha = finish(alpha);
            let n = NUM_WAVEFORM_SAMPLES.min(tex_w as usize / 3);
            let o = (NUM_WAVEFORM_SAMPLES - n) / 2;
            let w1 = (param2 * 0.5 + 0.5) * 0.5 + 0.45;
            v = vec![[0.0; 2]; n];
            for i in 0..n {
                let x0 = -1.0 + 2.0 * i as f32 / n as f32 + pos_x + 0.44 * fr[o + i + 25];
                let y0 = 0.47 * fl[o + i] + pos_y;
                if i > 1 {
                    v[i] = [
                        w1 * (2.0 * v[i - 1][0] - v[i - 2][0]) + (1.0 - w1) * x0,
                        w1 * (2.0 * v[i - 1][1] - v[i - 2][1]) + (1.0 - w1) * y0,
                    ];
                } else {
                    v[i] = [x0, y0];
                }
            }
        }
        // explosive hash
        5 => {
            alpha = finish(alpha * size_gain(tex_w));
            let (c, s) = ((0.3 * t).cos(), (0.3 * t).sin());
            v = (0..NUM_WAVEFORM_SAMPLES)
                .map(|i| {
                    let x0 = fr[i] * fl[i + 32] + fl[i] * fr[i + 32];
                    let y0 = fr[i] * fr[i] - fl[i + 32] * fl[i + 32];
                    [
                        (x0 * c - y0 * s) * asp_y + pos_x,
                        (x0 * s + y0 * c) * asp_x + pos_y,
                    ]
                })
                .collect();
        }
        // angled lines: 6 left only, 7 left over right, 8 spectrum (left bins only), 9 "large" line (second strip
        // stays zero), 10 X, 11 two vertical lines
        6..=11 => {
            alpha = finish(alpha);
            let n = if wave == 8 { 256 } else { line_n };
            if n == 0 {
                return None;
            }
            let o = line_off;
            let (ex, ey) = line_edges(pos_x, 1.57 * param2);
            v = match wave {
                6 => line_strip(ex, ey, n, 0.0, |i| 0.25 * fl[o + i]),
                7 => {
                    let sep = (pos_y * 0.5 + 0.5).powf(2.0);
                    let mut a = line_strip(ex, ey, n, 0.0, |i| 0.25 * fl[o + i] + sep);
                    a.extend(line_strip(ex, ey, n, 0.0, |i| 0.25 * fr[o + i] - sep));
                    brk = Some(n);
                    a
                }
                8 => line_strip(ex, ey, n, 0.0, |i| {
                    let s = |k: usize| spec_l.get(k).copied().unwrap_or(0.0);
                    0.1 * (s(2 * i) + s(2 * i + 1)).ln()
                }),
                9 => {
                    let mut a = line_strip(ex, ey, n, 0.0, |i| fl[o + i]);
                    a.resize(2 * n, [0.0; 2]);
                    brk = Some(n);
                    a
                }
                10 => {
                    // fixed angles: `wave_mystery` has no effect
                    let (ax, ay) = line_edges(pos_x, -0.75);
                    let (bx, by) = line_edges(pos_x, 0.75);
                    let mut a = line_strip(ax, ay, n, 0.0, |i| 0.35 * fl[o + i]);
                    a.extend(line_strip(bx, by, n, 0.0, |i| 0.35 * fr[o + i]));
                    brk = Some(n);
                    a
                }
                _ => {
                    let (ex, ey) = line_edges(pos_x, 1.57);
                    let mut a = line_strip(ex, ey, n, -0.45, |i| 0.35 * fl[o + i]);
                    a.extend(line_strip(ex, ey, n, 0.45, |i| 0.35 * fr[o + i]));
                    brk = Some(n);
                    a
                }
            };
        }
        // lasso: left channel only, no zero guard on the angle, no posX / aspect in x
        15 => {
            alpha = finish(alpha * 1.25);
            v = (0..240usize)
                .map(|i| {
                    let ang = 2.0 * t + 1.57 * fl[i + 32];
                    [
                        0.5 * t.cos() + (2.0 * ang + (t / ang).tan()).cos(),
                        2.0 * t.sin() * (3.14 * ang).sin() * asp_x / 2.8 + pos_y,
                    ]
                })
                .collect();
        }
        // triangle (BeatDrop's mode 16 with the spin reversed), right channel only, no clamps
        16 => {
            alpha = finish(alpha);
            v = (0..240usize)
                .map(|i| {
                    let p = i as f32 / 240.0;
                    let angle = 6.28 * p - 0.2 * t;
                    let phi = ((3.0 * p).floor() + 0.5) / 3.0 * 6.28 - 0.2 * t;
                    let e = (angle - phi).cos();
                    let r = (0.7 + e * fr[i] + param) / (2.0 * e);
                    [
                        r * angle.cos() * asp_y + pos_x,
                        r * angle.sin() * asp_x + pos_y,
                    ]
                })
                .collect();
            if !blending {
                v.push(v[0]);
            }
        }
        // not a mode (negative after the C remainder, or an old preset's value >= 17): MD3 still tessellates and
        // draws 480 zero vertices
        _ => {
            v = vec![[0.0; 2]; NUM_WAVEFORM_SAMPLES];
        }
    }
    Some(WaveGeom {
        verts: v,
        brk,
        alpha,
        rgb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOL: f32 = 1e-4;

    /// Annex-B-style fixed input: 576 samples per channel, a synthetic left-spectrum ramp.
    fn input() -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let fl = (0..576)
            .map(|i| (i as f32 - 288.0) / 576.0 + 0.1 * (0.37 * i as f32).sin())
            .collect();
        let fr = (0..576)
            .map(|i| 0.1 + (i as f32 - 288.0) / 1152.0 + 0.05 * (0.23 * i as f32).cos())
            .collect();
        let sp = (0..512).map(|k| 1.0 + 0.01 * k as f32).collect();
        (fl, fr, sp)
    }

    fn geom(mode: i32, time: f32, tweak: impl FnOnce(&mut MainWave), blending: bool) -> WaveGeom {
        let (fl, fr, sp) = input();
        let mut mw = MainWave::default();
        mw.mode = mode;
        mw.rgba = [1.0, 1.0, 1.0, 0.8];
        mw.x = 0.5;
        mw.y = 0.5;
        mw.mystery = 0.0;
        mw.mod_alpha_by_volume = false;
        tweak(&mut mw);
        main_wave_geometry_md3_raw(
            &mw,
            &fl,
            &fr,
            &sp,
            time,
            [1.0; 3],
            [1.0; 3],
            1024,
            (1.0, 0.75),
            blending,
        )
        .unwrap()
    }

    fn near(g: &WaveGeom, i: usize, want: [f32; 2], tol: f32) {
        let p = g.verts[i];
        assert!(
            (p[0] - want[0]).abs() < tol && (p[1] - want[1]).abs() < tol,
            "v{i} = {p:?}, want {want:?}"
        );
    }

    #[test]
    fn mode_wraps_modulo_17_with_c_semantics() {
        for (x, want) in [
            (0.0, 0),
            (5.0, 5),
            (16.0, 16),
            (17.0, 0),
            (20.0, 3),
            (33.0, 16),
            (34.0, 0),
            (16.9, 16),
            (17.9, 0),
            (-1.0, -1),
            (-17.0, 0),
            (-18.0, -1),
        ] {
            assert_eq!(md3_wave_mode(x), want, "wave_mode {x}");
        }
        // cvttsd2si overflow / NaN: INT_MIN, a negative remainder, so no mode
        assert!(md3_wave_mode(1e12) < 0);
        assert!(md3_wave_mode(f64::NAN) < 0);
    }

    #[test]
    fn circle_modes_0_13_14_16() {
        let g = geom(0, 0.0, |_| {}, false);
        assert_eq!(g.verts.len(), 241);
        assert_eq!(g.brk, None);
        assert!((g.alpha - 0.8).abs() < TOL);
        near(&g, 0, [0.43030, 0.0], TOL);
        near(&g, 1, [0.42683, 0.01496], TOL);
        near(&g, 120, [-0.39578, -0.00609], TOL);
        near(&g, 239, [0.43294, -0.00184], TOL);
        assert_eq!(g.verts[240], g.verts[0]);
        let g = geom(0, 1.0, |_| {}, false);
        near(&g, 0, [0.42173, 0.11398], TOL);
        near(&g, 120, [-0.38698, -0.11081], TOL);
        // blending: no closing vertex
        assert_eq!(geom(0, 0.0, |_| {}, true).verts.len(), 240);

        let g = geom(13, 0.0, |_| {}, false);
        assert_eq!(g.verts.len(), 241);
        near(&g, 0, [0.43030, 0.0], TOL);
        near(&g, 1, [0.42747, 0.01498], TOL);
        near(&g, 120, [-0.54577, -0.00840], TOL);
        // the tail reads the spectrum (indices >= 576)
        near(&g, 239, [0.74331, -0.00316], TOL);
        let g = geom(13, 1.0, |_| {}, false);
        near(&g, 120, [-0.53364, -0.15280], TOL);
        near(&g, 239, [0.72896, 0.19380], TOL);

        let g = geom(14, 0.0, |_| {}, false);
        near(&g, 0, [0.32029, 0.0], TOL);
        near(&g, 1, [0.31652, 0.00555], TOL);
        near(&g, 120, [-0.02006, -0.11228], TOL);
        near(&g, 239, [-0.37588, 0.00088], TOL);
        let g = geom(14, 1.0, |_| {}, false);
        near(&g, 0, [0.30461, -0.09874], TOL);
        near(&g, 239, [-0.30282, 0.12893], TOL);

        let g = geom(16, 0.0, |_| {}, false);
        assert_eq!(g.verts.len(), 241);
        near(&g, 0, [0.48702, 0.0], 1e-3);
        near(&g, 1, [0.46414, 0.01620], 1e-3);
        near(&g, 120, [-0.23067, 0.00049], 1e-3);
        near(&g, 239, [0.52315, -0.02048], 1e-3);
        let g = geom(16, 1.0, |_| {}, false);
        // the spin term is negative: BeatDrop's +0.2t gives another first vertex
        near(&g, 0, [0.47731, -0.12901], 1e-3);
        near(&g, 120, [-0.22600, 0.06158], 1e-3);
    }

    #[test]
    fn spirals_and_spiros_1_2_3_4_5_12_15() {
        let g = geom(1, 0.0, |_| {}, false);
        assert_eq!(g.verts.len(), 240);
        assert!((g.alpha - 1.0).abs() < TOL);
        near(&g, 0, [0.25393, -0.35005], TOL);
        near(&g, 120, [0.33820, -0.20050], TOL);
        near(&g, 239, [0.41434, -0.04810], TOL);

        let g = geom(2, 0.0, |_| {}, false);
        assert_eq!(g.verts.len(), 480);
        assert!((g.alpha - 0.088).abs() < TOL);
        near(&g, 0, [-0.07500, -0.51086], TOL);
        near(&g, 240, [0.05201, -0.01690], TOL);
        near(&g, 479, [0.16271, 0.44149], TOL);
        // mode 3: alpha = table * 1.3 * treble^2, same vertices
        let g3 = geom(3, 0.0, |_| {}, false);
        assert!((g3.alpha - 0.286).abs() < TOL);
        assert_eq!(g3.verts, g.verts);

        let g = geom(4, 0.0, |_| {}, false);
        assert_eq!(g.verts.len(), 341);
        near(&g, 0, [-1.05060, -0.16051], TOL);
        near(&g, 170, [0.01836, -0.02024], TOL);
        near(&g, 340, [1.11230, 0.12270], TOL);

        let g = geom(5, 0.0, |_| {}, false);
        assert_eq!(g.verts.len(), 480);
        near(&g, 0, [0.07526, -0.25098], TOL);
        near(&g, 479, [0.16163, -0.14785], TOL);
        let g = geom(5, 1.0, |_| {}, false);
        near(&g, 0, [0.12752, -0.21012], TOL);

        let g = geom(12, 0.0, |_| {}, false);
        assert!((g.alpha - 1.0).abs() < TOL);
        near(&g, 0, [0.39042, -0.26935], TOL);
        near(&g, 239, [0.28049, -0.03200], TOL);
        let g = geom(12, 1.0, |_| {}, false);
        near(&g, 0, [-0.34860, 0.18017], TOL);

        let g = geom(15, 0.0, |_| {}, false);
        assert_eq!(g.verts.len(), 240, "no closing vertex");
        near(&g, 0, [0.46670, 0.0], 1e-3);
        near(&g, 120, [1.16986, 0.0], 1e-3);
        let g = geom(15, 1.0, |_| {}, false);
        near(&g, 0, [-0.66636, -0.34922], 1e-3);
        near(&g, 239, [-0.03484, -0.16372], 1e-3);
    }

    #[test]
    fn angled_line_modes_6_to_11() {
        let g = geom(6, 0.0, |_| {}, false);
        assert_eq!(g.verts.len(), 240);
        near(&g, 0, [-1.10005, -0.06278], TOL);
        near(&g, 120, [0.0, -0.00229], TOL);
        near(&g, 239, [1.09087, 0.05013], TOL);

        let g = geom(7, 0.0, |_| {}, false);
        assert_eq!((g.verts.len(), g.brk), (480, Some(240)));
        near(&g, 0, [-1.09985, 0.18722], TOL);
        near(&g, 240, [-1.10022, -0.27122], TOL);
        near(&g, 479, [1.09067, -0.20171], TOL);

        let g = geom(8, 0.0, |_| {}, false);
        assert_eq!((g.verts.len(), g.brk), (256, None));
        near(&g, 0, [-1.09994, 0.06981], TOL);
        near(&g, 255, [1.09161, 0.25023], TOL);

        // mode 9: only strip A is written, the second strip stays at the origin
        let g = geom(9, 0.0, |_| {}, false);
        assert_eq!((g.verts.len(), g.brk), (480, Some(240)));
        near(&g, 0, [-1.10020, -0.25110], TOL);
        near(&g, 239, [1.09099, 0.20054], TOL);
        assert!(g.verts[240..].iter().all(|p| *p == [0.0, 0.0]));

        let g = geom(10, 0.0, |_| {}, false);
        assert_eq!((g.verts.len(), g.brk), (480, Some(240)));
        near(&g, 0, [-1.15996, 0.96050], 1e-3);
        near(&g, 240, [-1.07976, -1.04651], 1e-3);
        near(&g, 479, [1.04479, 1.06572], 1e-3);
        // ignores wave_mystery
        let g2 = geom(10, 0.0, |m| m.mystery = 0.5, false);
        assert_eq!(g.verts, g2.verts);

        let g = geom(11, 0.0, |_| {}, false);
        assert_eq!((g.verts.len(), g.brk), (480, Some(240)));
        near(&g, 0, [-0.36299, -1.10014], TOL);
        near(&g, 240, [0.47884, -1.10005], TOL);
        near(&g, 479, [0.38326, 1.09094], TOL);
    }

    #[test]
    fn single_point_checks_and_param_wrap() {
        // mode 9 with wave_x/y moved: the line start is clipped on the left edge, y does not move the line
        let (fl, fr, sp) = input();
        let ramp: Vec<f32> = (0..576).map(|i| (i as f32 - 288.0) / 576.0).collect();
        let mut mw = MainWave::default();
        mw.mode = 9;
        mw.rgba = [1.0, 1.0, 1.0, 0.8];
        mw.x = 0.7;
        mw.y = 0.4;
        let run = |mw: &MainWave| {
            main_wave_geometry_md3_raw(
                mw,
                &ramp,
                &fr,
                &sp,
                0.0,
                [1.0; 3],
                [1.0; 3],
                1024,
                (1.0, 0.75),
                false,
            )
            .unwrap()
        };
        near(&run(&mw), 0, [-1.10023, 0.10833], TOL);
        mw.mode = 6;
        near(&run(&mw), 0, [-1.10006, 0.32708], TOL);
        mw.mode = 15;
        let g = main_wave_geometry_md3_raw(
            &mw,
            &fl,
            &fr,
            &sp,
            0.0,
            [1.0; 3],
            [1.0; 3],
            1024,
            (1.0, 0.75),
            false,
        )
        .unwrap();
        assert!(g.verts.iter().all(|p| (p[1] + 0.2).abs() < 1e-6));
        // param 0.5 on a ramp-only input
        mw.mode = 6;
        mw.mystery = 0.5;
        mw.x = 0.5;
        mw.y = 0.5;
        near(&run(&mw), 0, [-1.04850, -1.15075], 1e-3);
        mw.mode = 9;
        near(&run(&mw), 0, [-0.89401, -1.30561], 1e-3);

        // wrap of `wave_mystery` into [-1, 1) applies to modes 0, 1, 4 only
        let r = |p: f32, mode: i32| geom(mode, 0.0, |m| m.mystery = p, false).verts[0];
        let base = geom(1, 0.0, |m| m.mystery = -1.0, false).verts[0];
        // -3 wraps to -1 (mode 1): same first vertex as param -1
        assert_eq!(r(-3.0, 1), base);
        // mode 2 does not use the parameter at all
        assert_eq!(r(-3.0, 2), r(0.0, 2));
        // mode 13 uses the raw value: 2.0 is not wrapped to 0.0
        assert_ne!(r(2.0, 13), r(0.0, 13));
    }

    #[test]
    fn alpha_tables_volume_and_cut() {
        // mode 3, treble 0.5, 1024 wide: 0.22 * 1.3 * 0.25
        let (fl, fr, sp) = input();
        let mut mw = MainWave::default();
        mw.mode = 3;
        mw.rgba = [1.0, 1.0, 1.0, 0.8];
        let alpha = |mw: &MainWave, w: u32, imm_rel: [f32; 3], treb: f32| {
            main_wave_geometry_md3_raw(
                mw,
                &fl,
                &fr,
                &sp,
                0.0,
                imm_rel,
                [1.0, 1.0, treb],
                w,
                (1.0, 0.75),
                false,
            )
            .unwrap()
            .alpha
        };
        assert!((alpha(&mw, 1024, [1.0; 3], 0.5) - 0.0715).abs() < 1e-5);
        // a width off the table keeps wave_a: 0.8 * 1.3 * 0.25
        assert!((alpha(&mw, 1920, [1.0; 3], 0.5) - 0.26).abs() < 1e-5);
        mw.mode = 2;
        assert!((alpha(&mw, 512, [1.0; 3], 1.0) - 0.072).abs() < 1e-5);
        assert!((alpha(&mw, 1920, [1.0; 3], 1.0) - 0.8).abs() < 1e-5);
        // alpha by volume: 0.8 * (0.999 - 0.75) / 0.5
        mw.mode = 0;
        mw.mod_alpha_by_volume = true;
        mw.mod_alpha_start = 0.75;
        mw.mod_alpha_end = 1.25;
        assert!((alpha(&mw, 1024, [1.2, 1.0, 0.8], 1.0) - 0.3984).abs() < 1e-4);
        // below Start: clamped to 0
        assert_eq!(alpha(&mw, 1024, [0.1; 3], 1.0), 0.0);
    }

    #[test]
    fn invalid_mode_draws_480_zero_vertices() {
        for mode in [-1, 17, 40] {
            let g = geom(mode, 0.0, |_| {}, false);
            assert_eq!(g.verts.len(), 480);
            assert!(g.verts.iter().all(|p| *p == [0.0, 0.0]));
            assert_eq!(g.brk, None);
            assert!((g.alpha - 0.8).abs() < TOL);
        }
    }
}
