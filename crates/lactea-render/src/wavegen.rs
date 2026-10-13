// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Geometry generation for the main waveform (`CPlugin::DrawWave`) and custom-wave helpers
//! (`DrawCustomWaves`, `SmoothWave`). Pure functions: no GPU.
//!
//! Coordinate convention of the *outputs*: **clip space, x right, y up** (so `y = 1` is the top
//! of the screen). MD2 draws through an ortho projection with a flipped Y; `DrawWave` negates
//! all y values at the end ("to stay consistent with the pre-VMS milkdrop"), which cancels
//! the projection flip — the y it computes *is* the clip-space y.

use crate::frame::MainWave;

/// `NUM_WAVEFORM_SAMPLES` (MD2: 480).
pub const NUM_WAVEFORM_SAMPLES: usize = 480;
/// Length of the raw waveform arrays the renderer reads (`mysound.fWave[ch][576]`).
pub const NUM_WAVE_INPUT: usize = 576;
/// Length of the spectrum arrays (`fSpecLeft`/`fSpecRight`).
pub const NUM_FREQ_SAMPLES: usize = 512;
/// Number of wave modes this port draws (0..=7 are MD2 classic; 8 is the spectrum line).
pub const NUM_WAVES_DRAWN: i32 = 9;

/// Result of [`main_wave_geometry`].
#[derive(Clone, Debug, PartialEq)]
pub struct WaveGeom {
    /// Polyline / point vertices in clip space, *before* `SmoothWave` and thick offsets.
    pub verts: Vec<[f32; 2]>,
    /// Index at which the line strip is broken into two strips (`nBreak`), if any.
    pub brk: Option<usize>,
    /// Final alpha (0..1) after per-mode scaling / volume modulation.
    pub alpha: f32,
    /// Colour after clamping and "maximize colour".
    pub rgb: [f32; 3],
}

/// `RenderFrame`: scale (`fWaveScale/128`) and IIR-smooth the raw 576-sample waveform.
pub fn prepare_main_waveform(mw: &MainWave) -> ([f32; NUM_WAVE_INPUT], [f32; NUM_WAVE_INPUT]) {
    let scale = mw.scale / 128.0;
    let mix2 = mw.smoothing;
    let mix1 = scale * (1.0 - mix2);
    let process = |src: &[f32]| -> [f32; NUM_WAVE_INPUT] {
        let mut w = [0.0f32; NUM_WAVE_INPUT];
        for (d, s) in w.iter_mut().zip(src.iter()) {
            *d = *s;
        }
        w[0] *= scale;
        for i in 1..NUM_WAVE_INPUT {
            w[i] = w[i] * mix1 + w[i - 1] * mix2;
        }
        w
    };
    (process(&mw.wave[0]), process(&mw.wave[1]))
}

/// `DrawWave`'s alpha tail: optional volume modulation, then clamp to 0..1.
pub(crate) fn finish_alpha(mw: &MainWave, imm_rel: [f32; 3], alpha: f32) -> f32 {
    let a = if mw.mod_alpha_by_volume {
        alpha * volume_alpha_mod(mw, imm_rel)
    } else {
        alpha
    };
    a.clamp(0.0, 1.0)
}

/// Wave modes 0, 1 and 4 wrap `wave_mystery` outside `[-1, 1]` into that range; the other modes use it as is.
pub(crate) fn wrap_param2(mode: i32, param: f32) -> f32 {
    if matches!(mode, 0 | 1 | 4) && (param < -1.0 || param > 1.0) {
        let q = param * 0.5 + 0.5;
        (q - q.floor()).abs() * 2.0 - 1.0
    } else {
        param
    }
}

/// Alpha gain of the texture-width table shared by the spiro / explosive modes (1 for other widths).
pub(crate) fn size_gain(tex_w: u32) -> f32 {
    match tex_w {
        256 => 0.07,
        512 => 0.09,
        1024 => 0.11,
        2048 => 0.13,
        _ => 1.0,
    }
}

/// Vertices of one strip along the clipped line `(ex, ey)` (the "angled line" helper of modes 6..11): `n` points
/// from the first edge towards the second one (the far end is never reached), displaced along the perpendicular by
/// `amp(i)`; `x_off` shifts the whole strip in x after clipping (MD3 mode 11).
pub(crate) fn line_strip(
    ex: [f32; 2],
    ey: [f32; 2],
    n: usize,
    x_off: f32,
    amp: impl Fn(usize) -> f32,
) -> Vec<[f32; 2]> {
    let dx = (ex[1] - ex[0]) / n as f32;
    let dy = (ey[1] - ey[0]) / n as f32;
    let ang2 = dy.atan2(dx);
    let perp_dx = (ang2 + 1.57).cos();
    let perp_dy = (ang2 + 1.57).sin();
    (0..n)
        .map(|i| {
            let a = amp(i);
            [
                ex[0] + x_off + dx * i as f32 + perp_dx * a,
                ey[0] + dy * i as f32 + perp_dy * a,
            ]
        })
        .collect()
}

/// MD2's `alpha *= (vol - start) / (end - start)`. A zero-width band is a step at `start` (the C division yields
/// `+-inf` / NaN, which the following clamp turns into 0 / 1 or keeps as NaN).
pub(crate) fn volume_alpha_mod(mw: &MainWave, imm_rel: [f32; 3]) -> f32 {
    let above = (imm_rel[0] + imm_rel[1] + imm_rel[2]) * 0.333 - mw.mod_alpha_start;
    let width = mw.mod_alpha_end - mw.mod_alpha_start;
    if width == 0.0 {
        return if above > 0.0 { 1.0 } else { 0.0 };
    }
    above / width
}

/// `DrawWave`'s colour: `wave_r/g/b` clamped to 0..1, then "maximize colour" (`wave_brighten`) divides by the largest
/// component when it exceeds 0.01.
pub(crate) fn wave_rgb(mw: &MainWave) -> [f32; 3] {
    let mut c = [
        mw.rgba[0].clamp(0.0, 1.0),
        mw.rgba[1].clamp(0.0, 1.0),
        mw.rgba[2].clamp(0.0, 1.0),
    ];
    if mw.brighten {
        let amount = 1.0f32;
        // a comparison chain, not `f32::max`: a NaN red stays the maximum (`max > 0.01` is false) as in MD2's C
        let mut max = c[0];
        if max < c[1] {
            max = c[1];
        }
        if max < c[2] {
            max = c[2];
        }
        if max > 0.01 {
            for x in &mut c {
                *x = *x / max * amount + *x * (1.0 - amount);
            }
        }
    }
    c
}

/// The 2-point line used by wave modes 6, 7, 8 (clipped against +-1.1).
pub(crate) fn line_edges(pos_x: f32, ang: f32) -> ([f32; 2], [f32; 2]) {
    let dx = ang.cos();
    let dy = ang.sin();
    let mut ex = [
        pos_x * (ang + 1.57).cos() - dx * 3.0,
        pos_x * (ang + 1.57).cos() + dx * 3.0,
    ];
    let mut ey = [
        pos_x * (ang + 1.57).sin() - dy * 3.0,
        pos_x * (ang + 1.57).sin() + dy * 3.0,
    ];
    for i in 0..2 {
        for j in 0..4 {
            let mut t = 0.0;
            let mut clip = false;
            match j {
                0 => {
                    if ex[i] > 1.1 {
                        t = (1.1 - ex[1 - i]) / (ex[i] - ex[1 - i]);
                        clip = true;
                    }
                }
                1 => {
                    if ex[i] < -1.1 {
                        t = (-1.1 - ex[1 - i]) / (ex[i] - ex[1 - i]);
                        clip = true;
                    }
                }
                2 => {
                    if ey[i] > 1.1 {
                        t = (1.1 - ey[1 - i]) / (ey[i] - ey[1 - i]);
                        clip = true;
                    }
                }
                _ => {
                    if ey[i] < -1.1 {
                        t = (-1.1 - ey[1 - i]) / (ey[i] - ey[1 - i]);
                        clip = true;
                    }
                }
            }
            if clip {
                let dx = ex[i] - ex[1 - i];
                let dy = ey[i] - ey[1 - i];
                ex[i] = ex[1 - i] + dx * t;
                ey[i] = ey[1 - i] + dy * t;
            }
        }
    }
    (ex, ey)
}

/// Port of the single-waveform part of `DrawWave` (no preset blending: `its == 1`).
///
/// * `wl`, `wr`: the output of [`prepare_main_waveform`].
/// * `time`: `GetTime()` as float. `imm_rel`: `[bass, mid, treb]`. `imm_abs`: `mysound.imm`.
/// * `tex_w`: `m_nTexSizeX` (several modes switch on it). `aspect`: `(m_fAspectX, m_fAspectY)`.
///
/// Returns `None` when MD2 would skip the draw (alpha < 0.004, unsupported mode).
pub fn main_wave_geometry(
    mw: &MainWave,
    wl: &[f32],
    wr: &[f32],
    spec_l: &[f32],
    spec_r: &[f32],
    time: f32,
    imm_rel: [f32; 3],
    imm_abs: [f32; 3],
    tex_w: u32,
    aspect: (f32, f32),
) -> Option<WaveGeom> {
    main_wave_geometry_raw(
        mw, wl, wr, spec_l, spec_r, time, imm_rel, imm_abs, tex_w, aspect, false,
    )
    .filter(|g| g.alpha >= 0.004)
}

/// The per-wave part of `DrawWave`'s loop (`for it in 0..its`): the vertices and the alpha of one wave *before*
/// MD2's "don't draw wave if alpha < 0.004" test, which only applies to the (possibly blended) final alpha.
/// `blending` is `m_pState->m_bBlending`: the wave modes that close their loop by duplicating the first vertex
/// do not while blending. `None` for an unsupported mode.
pub fn main_wave_geometry_raw(
    mw: &MainWave,
    wl: &[f32],
    wr: &[f32],
    spec_l: &[f32],
    spec_r: &[f32],
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
    let fl = wl;
    let fr = wr;

    let [cr, cg, cb] = wave_rgb(mw);

    let pos_x = mw.x * 2.0 - 1.0;
    let pos_y = mw.y * 2.0 - 1.0;
    let treble_rel = imm_abs[2];

    if mw.mode >= NUM_WAVES_DRAWN || mw.mode < 0 {
        return None;
    }
    let wave = mw.mode;
    let mut n_verts = NUM_WAVEFORM_SAMPLES;
    let mut brk: Option<usize> = None;
    let mut sample_offset = 0usize;

    let param2 = wrap_param2(wave, mw.mystery);

    let mut alpha = mw.rgba[3];
    let mut v: Vec<[f32; 2]> = Vec::new();
    let finish = |alpha: f32| finish_alpha(mw, imm_rel, alpha);

    match wave {
        0 => {
            // circular wave
            n_verts /= 2;
            sample_offset = (NUM_WAVEFORM_SAMPLES - n_verts) / 2;
            alpha = finish(alpha);
            let inv = 1.0 / (n_verts as f32 - 1.0);
            v.resize(n_verts, [0.0; 2]);
            for i in 0..n_verts {
                let mut rad =
                    0.5 + 0.4 * (fl[i + sample_offset] + fr[i + sample_offset]) * 0.5 + param2;
                let ang = i as f32 * inv * 6.28 + time * 0.2;
                if i < n_verts / 10 {
                    let mut mix = i as f32 / (n_verts as f32 * 0.1);
                    mix = 0.5 - 0.5 * (mix * 3.1416).cos();
                    let rad2 = 0.5
                        + 0.4
                            * (fl[i + n_verts + sample_offset] + fr[i + n_verts + sample_offset])
                            * 0.5
                        + param2;
                    rad = rad2 * (1.0 - mix) + rad * mix;
                }
                v[i] = [
                    rad * ang.cos() * asp_y + pos_x,
                    rad * ang.sin() * asp_x + pos_y,
                ];
            }
            // dupe last vertex to connect the lines; skip if blending
            if !blending {
                let first = v[0];
                v.push(first);
            }
        }
        1 => {
            // x-y oscillator in a spiral, in time
            alpha = finish(alpha * 1.25);
            n_verts /= 2;
            v.resize(n_verts, [0.0; 2]);
            for i in 0..n_verts {
                let rad = 0.53 + 0.43 * fr[i] + param2;
                let ang = fl[i + 32] * 1.57 + time * 2.3;
                v[i] = [
                    rad * ang.cos() * asp_y + pos_x,
                    rad * ang.sin() * asp_x + pos_y,
                ];
            }
        }
        2 => {
            // centered spiro (constant faint alpha)
            alpha = finish(alpha * size_gain(tex_w));
            v.resize(n_verts, [0.0; 2]);
            for i in 0..n_verts {
                v[i] = [fr[i] * asp_y + pos_x, fl[i + 32] * asp_x + pos_y];
            }
        }
        3 => {
            // centered spiro (alpha tied to treble)
            alpha = match tex_w {
                256 => 0.075,
                512 => 0.150,
                1024 => 0.220,
                2048 => 0.330,
                _ => alpha,
            };
            alpha *= 1.3;
            alpha *= treble_rel.powf(2.0);
            alpha = finish(alpha);
            v.resize(n_verts, [0.0; 2]);
            for i in 0..n_verts {
                v[i] = [fr[i] * asp_y + pos_x, fl[i + 32] * asp_x + pos_y];
            }
        }
        4 => {
            // horizontal "script", left channel, with momentum
            let cap = (tex_w / 3) as usize;
            if n_verts > cap {
                n_verts = cap;
            }
            sample_offset = (NUM_WAVEFORM_SAMPLES - n_verts) / 2;
            alpha = finish(alpha);
            let w1 = 0.45 + 0.5 * (param2 * 0.5 + 0.5);
            let w2 = 1.0 - w1;
            let inv = 1.0 / n_verts as f32;
            v.resize(n_verts, [0.0; 2]);
            for i in 0..n_verts {
                v[i][0] = -1.0 + 2.0 * (i as f32 * inv) + pos_x;
                v[i][1] = 0.5 * (fl[i + sample_offset] + fr[i + sample_offset]) * 0.47 + pos_y;
                v[i][0] += fr[i + 25 + sample_offset] * 0.44;
                if i > 1 {
                    v[i][0] = v[i][0] * w2 + w1 * (v[i - 1][0] * 2.0 - v[i - 2][0]);
                    v[i][1] = v[i][1] * w2 + w1 * (v[i - 1][1] * 2.0 - v[i - 2][1]);
                }
            }
        }
        5 => {
            // weird explosive complex-number thingy
            alpha = finish(alpha * size_gain(tex_w));
            let cos_rot = (time * 0.3).cos();
            let sin_rot = (time * 0.3).sin();
            v.resize(n_verts, [0.0; 2]);
            for i in 0..n_verts {
                let x0 = fr[i] * fl[i + 32] + fl[i] * fr[i + 32];
                let y0 = fr[i] * fr[i] - fl[i + 32] * fl[i + 32];
                v[i] = [
                    (x0 * cos_rot - y0 * sin_rot) * asp_y + pos_x,
                    (x0 * sin_rot + y0 * cos_rot) * asp_x + pos_y,
                ];
            }
        }
        6..=8 => {
            n_verts /= 2;
            let cap = (tex_w / 3) as usize;
            if n_verts > cap {
                n_verts = cap;
            }
            if wave == 8 {
                n_verts = 256;
            } else {
                sample_offset = (NUM_WAVEFORM_SAMPLES - n_verts) / 2;
            }
            alpha = finish(alpha);

            let (ex, ey) = line_edges(pos_x, 1.57 * param2);
            let so = sample_offset;
            v = match wave {
                6 => line_strip(ex, ey, n_verts, 0.0, |i| {
                    0.25 * ((fl[i + so] + fr[i + so]) * 0.5)
                }),
                8 => line_strip(ex, ey, n_verts, 0.0, |i| {
                    0.1 * ((spec_l.get(i * 2).copied().unwrap_or(0.0)
                        + spec_l.get(i * 2 + 1).copied().unwrap_or(0.0)
                        + spec_r.get(i * 2).copied().unwrap_or(0.0)
                        + spec_r.get(i * 2 + 1).copied().unwrap_or(0.0))
                        * 0.5)
                        .ln()
                }),
                _ => {
                    let sep = (pos_y * 0.5 + 0.5).powf(2.0);
                    let mut a = line_strip(ex, ey, n_verts, 0.0, |i| 0.25 * fl[i + so] + sep);
                    a.extend(line_strip(ex, ey, n_verts, 0.0, |i| {
                        0.25 * fr[i + so] - sep
                    }));
                    brk = Some(n_verts);
                    a
                }
            };
        }
        _ => return None,
    }

    if v.is_empty() {
        return None;
    }
    Some(WaveGeom {
        verts: v,
        brk,
        alpha,
        rgb: [cr, cg, cb],
    })
}

// ---------------------------------------------------------------------------------------------
// projectM `MilkdropPreset/Waveform.cpp` + `Waveforms/*.cpp` (behaviour reference; the maths below
// is re-derived from reading the source, not copied). Differences to the MD2 port above:
//  * 480-sample input buffers, copied into 512-entry arrays whose tail (480..511) is zero, and the IIR
//    smoothing runs over all 512 entries (so `buf[i + 32]` of the last samples is a decaying tail);
//  * line modes use `m_samples = 240`, divided by 3 when that exceeds `viewport_x / 3` (-> 80 at 640 px),
//    derivative mode 480 -> 160; mode 4/6/7 read the left channel only, circle/spiral read the right one;
//  * circle and XY spiral are `GL_LINE_LOOP`s (no duplicated vertex), the 2-wave modes smooth each wave;
//  * alpha: `wave_a`, x texsize factor (modes 2/3/5), mode 3 x `1.3 * treb^2`, then "modulate by volume"
//    *replaces* it with the piecewise linear `wave_a * (vol-a)/(b-a)`; texsize = max(width, height);
//  * modes 9..=15 are the Milkdrop2077 additions.
// ---------------------------------------------------------------------------------------------

/// Largest wave buffer length in projectM (`WaveformMaxPoints`).
pub const PM_WAVE_POINTS: usize = 512;

/// Number of main-wave modes in projectM (`WaveformMode::Count`); the preset value is wrapped with it.
pub const PM_WAVE_MODES: i32 = 16;

/// Output of [`main_wave_geometry_projectm`].
#[derive(Clone, Debug, PartialEq)]
pub struct PmWaveGeom {
    /// Up to two smoothed vertex lists (clip space, y up), in draw order; empty ones are dropped.
    pub strips: Vec<Vec<[f32; 2]>>,
    /// Draw as `GL_LINE_LOOP` (close the strip) instead of `GL_LINE_STRIP`.
    pub is_loop: bool,
    /// Final alpha 0..1 (`m_tempAlpha`), shared by both strips.
    pub alpha: f32,
    /// Colour after "maximize colour" (not clamped: projectM hands it to `glVertexAttrib4f` raw).
    pub rgb: [f32; 3],
}

/// Slots `NUM_WAVEFORM_SAMPLES..PM_WAVE_POINTS` of projectM's per-channel wave buffers (`WaveformMath::m_pcmDataL/R`).
///
/// The buffers are members of the preset's `WaveformMath` object and every frame only the first 480 slots are
/// overwritten with fresh samples; the IIR smoothing then runs over all 512 slots *in place*, so the last 32 slots
/// keep the previous frame's smoothed values as their "input" (`s[i] = s[i]*mix1 + s[i-1]*mix2` with the old `s[i]`).
/// Modes that read `buf[i + 32]` for `i >= 448` (1, 2, 3, 5, 12, 15, ...) see that state. Zero at preset load.
pub type PmWaveTail = [[f32; PM_WAVE_POINTS - NUM_WAVEFORM_SAMPLES]; 2];

/// Reusable vertex buffers of [`main_wave_geometry_projectm`] (contents are overwritten every call).
#[derive(Default)]
pub struct PmWaveScratch {
    w1: Vec<[f32; 2]>,
    w2: Vec<[f32; 2]>,
}

/// `WaveformMath::GetVertices`' sample preparation: copy, scale by `scale/128`, IIR smoothing over 512 slots.
///
/// `tail` is the persistent state of the unwritten slots (see [`PmWaveTail`]); it is updated in place. Spectrum
/// waves copy all 512 slots and leave it alone.
pub fn prepare_main_waveform_projectm(
    mw: &MainWave,
    spectrum: bool,
    tail: &mut PmWaveTail,
) -> ([f32; PM_WAVE_POINTS], [f32; PM_WAVE_POINTS]) {
    let mut out = [[0.0f32; PM_WAVE_POINTS]; 2];
    for (ch, o) in out.iter_mut().enumerate() {
        if spectrum {
            o[..NUM_FREQ_SAMPLES].copy_from_slice(&mw.spectrum[ch][..NUM_FREQ_SAMPLES]);
        } else {
            o[..NUM_WAVEFORM_SAMPLES].copy_from_slice(&mw.wave[ch][..NUM_WAVEFORM_SAMPLES]);
            o[NUM_WAVEFORM_SAMPLES..].copy_from_slice(&tail[ch]);
        }
        let scale = mw.scale / 128.0;
        let mix2 = mw.smoothing;
        let mix1 = scale * (1.0 - mix2);
        o[0] *= scale;
        for i in 1..PM_WAVE_POINTS {
            o[i] = o[i] * mix1 + o[i - 1] * mix2;
        }
        if !spectrum {
            tail[ch].copy_from_slice(&o[NUM_WAVEFORM_SAMPLES..]);
        }
    }
    (out[0], out[1])
}

/// `LineBase::ClipWaveformEdges`: returns `(edge_x, edge_y, dist_x, dist_y, perp_dx, perp_dy, sample_offset)`.
fn clip_waveform_edges(
    wave_x: f32,
    angle: f32,
    samples: usize,
) -> (f32, f32, f32, f32, f32, f32, usize) {
    let (ex, ey) = line_edges(wave_x, angle);
    let sample_offset = (NUM_WAVEFORM_SAMPLES - samples) / 2;
    let dist_x = (ex[1] - ex[0]) / samples as f32;
    let dist_y = (ey[1] - ey[0]) / samples as f32;
    let angle2 = dist_y.atan2(dist_x);
    (
        ex[0],
        ey[0],
        dist_x,
        dist_y,
        (angle2 + 1.57).cos(),
        (angle2 + 1.57).sin(),
        sample_offset,
    )
}

/// `Waveform::MaximizeColors` alpha part (everything but the colour): see the module notes above.
fn pm_alpha(mw: &MainWave, mode: i32, bass_mid_treb: [f32; 3], viewport: (u32, u32)) -> f32 {
    let texsize = viewport.0.max(viewport.1);
    let [bass, mid, treb] = bass_mid_treb;
    let vol = (bass + mid + treb) * 0.333;
    let mut a = mw.rgba[3];
    let by_size = |a: [f32; 5]| -> f32 {
        match texsize {
            0..=256 => a[0],
            257..=512 => a[1],
            513..=1024 => a[2],
            1025..=2048 => a[3],
            _ => a[4],
        }
    };
    if mode == 2 || mode == 5 {
        a *= by_size([0.07, 0.09, 0.11, 0.13, 0.15]);
    } else if mode == 3 {
        a *= by_size([0.075, 0.15, 0.22, 0.33, 0.44]);
        a *= 1.3;
        a *= treb.powf(2.0);
    }
    if mw.mod_alpha_by_volume {
        // `ModulateOpacityByVolume` overwrites the alpha computed above (a projectM quirk).
        a = if vol <= mw.mod_alpha_start {
            0.0
        } else if vol >= mw.mod_alpha_end {
            mw.rgba[3]
        } else {
            mw.rgba[3] * ((vol - mw.mod_alpha_start) / (mw.mod_alpha_end - mw.mod_alpha_start))
        };
    }
    a.clamp(0.0, 1.0)
}

/// Port of projectM's main waveform (`Waveform::Draw` minus the GL calls): vertex generation, `SmoothWave`,
/// alpha and colour for wave modes `0..PM_WAVE_MODES` (`mw.mode` already wrapped).
///
/// * `time`: `renderContext.time`; `bass_mid_treb`: `audioData.bass/mid/treb`; `viewport`: render target size;
///   `tail`: the persistent wave-buffer state ([`PmWaveTail`]), advanced by this call (projectM runs
///   `GetVertices` every frame, also when nothing is drawn).
///
/// Returns `None` for unknown modes. An alpha of 0 is still returned (the caller may skip drawing).
pub fn main_wave_geometry_projectm(
    mw: &MainWave,
    time: f32,
    bass_mid_treb: [f32; 3],
    viewport: (u32, u32),
    tail: &mut PmWaveTail,
    scratch: &mut PmWaveScratch,
) -> Option<PmWaveGeom> {
    let mode = mw.mode;
    if !(0..PM_WAVE_MODES).contains(&mode) {
        return None;
    }
    let (pl, pr) = prepare_main_waveform_projectm(mw, mode == 8, tail);
    let (vx, vy) = (viewport.0 as f32, viewport.1 as f32);
    let (mut asp_x, mut asp_y) = (1.0f32, 1.0f32);
    if viewport.0 > viewport.1 {
        asp_y = vy / vx;
    } else {
        asp_x = vx / vy;
    }
    let wave_x = 2.0 * mw.x - 1.0;
    let wave_y = 2.0 * mw.y - 1.0;
    let myst = wrap_param2(mode, mw.mystery);
    let vp3 = viewport.0 / 3;
    // samples for the line-like modes: 240, divided by 3 when above viewport_x / 3
    let line_samples = |base: usize| -> usize { if base as u32 > vp3 { base / 3 } else { base } };
    let (w1, w2) = (&mut scratch.w1, &mut scratch.w2);
    w1.clear();
    w2.clear();
    let n; // m_samples (the smoothing uses it for the point count)
    let mut is_loop = false;

    match mode {
        0 => {
            is_loop = true;
            n = NUM_WAVEFORM_SAMPLES / 2;
            let off = (NUM_WAVEFORM_SAMPLES - n) / 2;
            let inv = 1.0 / n as f32;
            w1.resize(n, [0.0; 2]);
            for i in 0..n {
                let mut radius = 0.5 + 0.4 * pr[i + off] + myst;
                let angle = i as f32 * inv * 6.28 + time * 0.2;
                if i < n / 10 {
                    let mut mix = i as f32 / (n as f32 * 0.1);
                    mix = 0.5 - 0.5 * (mix * 3.1416).cos();
                    let radius2 = 0.5 + 0.4 * pr[i + n + off] + myst;
                    radius = radius2 * (1.0 - mix) + radius * mix;
                }
                w1[i] = [
                    radius * angle.cos() * asp_y + wave_x,
                    radius * angle.sin() * asp_x + wave_y,
                ];
            }
        }
        1 => {
            is_loop = true;
            n = NUM_WAVEFORM_SAMPLES / 2;
            w1.resize(n, [0.0; 2]);
            for i in 0..n {
                let radius = 0.53 + 0.43 * pr[i] + myst;
                let angle = pl[i + 32] * 1.57 + time * 2.3;
                w1[i] = [
                    radius * angle.cos() * asp_y + wave_x,
                    radius * angle.sin() * asp_x + wave_y,
                ];
            }
        }
        2 | 3 => {
            n = NUM_WAVEFORM_SAMPLES;
            w1.resize(n, [0.0; 2]);
            for i in 0..n {
                w1[i] = [pr[i] * asp_y + wave_x, pl[i + 32] * asp_x + wave_y];
            }
        }
        4 => {
            n = line_samples(NUM_WAVEFORM_SAMPLES);
            w1.resize(n, [0.0; 2]);
            let off = (NUM_WAVEFORM_SAMPLES - n) / 2;
            let wa = 0.45 + 0.5 * (myst * 0.5 + 0.5);
            let wb = 1.0 - wa;
            let inv = 1.0 / n as f32;
            for i in 0..n {
                let x = -1.0 + 2.0 * (i as f32 * inv) + wave_x + pr[i + 25 + off] * 0.44;
                let y = pl[i + off] * 0.47 + wave_y;
                w1[i] = [x, y];
                if i > 1 {
                    w1[i] = [
                        x * wb + wa * (w1[i - 1][0] * 2.0 - w1[i - 2][0]),
                        y * wb + wa * (w1[i - 1][1] * 2.0 - w1[i - 2][1]),
                    ];
                }
            }
        }
        5 => {
            n = NUM_WAVEFORM_SAMPLES;
            let (c, s) = ((time * 0.3).cos(), (time * 0.3).sin());
            w1.resize(n, [0.0; 2]);
            for i in 0..n {
                let x0 = pr[i] * pl[i + 32] + pl[i] * pr[i + 32];
                let y0 = pr[i] * pr[i] - pl[i + 32] * pl[i + 32];
                w1[i] = [
                    (x0 * c - y0 * s) * asp_y + wave_x,
                    (x0 * s + y0 * c) * asp_x + wave_y,
                ];
            }
        }
        6 | 7 | 9 | 10 | 11 => {
            n = line_samples(NUM_WAVEFORM_SAMPLES / 2);
            w1.resize(n, [0.0; 2]);
            match mode {
                6 | 9 => {
                    let amp = if mode == 6 { 0.25 } else { 0.35 };
                    let (ex, ey, dx, dy, px, py, off) = clip_waveform_edges(wave_x, 1.57 * myst, n);
                    for i in 0..n {
                        w1[i] = [
                            ex + dx * i as f32 + px * amp * pl[i + off],
                            ey + dy * i as f32 + py * amp * pl[i + off],
                        ];
                    }
                    if mode == 9 {
                        w2.resize(n, [0.0; 2]); // allocated but never filled in projectM
                    }
                }
                7 => {
                    w2.resize(n, [0.0; 2]);
                    let (ex, ey, dx, dy, px, py, off) = clip_waveform_edges(wave_x, 1.57 * myst, n);
                    let sep = (wave_y * 0.5 + 0.5).powf(2.0);
                    for i in 0..n {
                        w1[i] = [
                            ex + dx * i as f32 + px * (0.25 * pl[i + off] + sep),
                            ey + dy * i as f32 + py * (0.25 * pl[i + off] + sep),
                        ];
                        w2[i] = [
                            ex + dx * i as f32 + px * (0.25 * pr[i + off] - sep),
                            ey + dy * i as f32 + py * (0.25 * pr[i + off] - sep),
                        ];
                    }
                }
                10 => {
                    w2.resize(n, [0.0; 2]);
                    let (ex, ey, dx, dy, px, py, off) =
                        clip_waveform_edges(wave_x, -0.75 + myst * 3.15, n);
                    for i in 0..n {
                        w1[i] = [
                            ex + dx * i as f32 + px * 0.35 * pl[i + off],
                            ey + dy * i as f32 + py * 0.35 * pl[i + off],
                        ];
                    }
                    let (ex, ey, dx, dy, px, py, off) =
                        clip_waveform_edges(wave_x, 0.75 + myst * 3.15, n);
                    for i in 0..n {
                        w2[i] = [
                            ex + dx * i as f32 + px * 0.35 * pr[i + off],
                            ey + dy * i as f32 + py * 0.35 * pr[i + off],
                        ];
                    }
                }
                _ => {
                    w2.resize(n, [0.0; 2]);
                    let (ex, ey, dx, dy, px, py, off) = clip_waveform_edges(wave_x, 1.57, n);
                    for i in 0..n {
                        w1[i] = [
                            ex - 0.45 + dx * i as f32 + px * 0.35 * pl[i + off],
                            ey + dy * i as f32 + py * 0.35 * pl[i + off],
                        ];
                        w2[i] = [
                            ex + 0.45 + dx * i as f32 + px * 0.35 * pr[i + off],
                            ey + dy * i as f32 + py * 0.35 * pr[i + off],
                        ];
                    }
                }
            }
        }
        8 => {
            n = 256;
            let (ex, ey, dx, dy, px, py, _) = clip_waveform_edges(wave_x, 1.57 * myst, n);
            w1.resize(n, [0.0; 2]);
            for i in 0..n {
                let f = 0.1 * (pl[i * 2] + pl[i * 2 + 1]).ln();
                w1[i] = [ex + dx * i as f32 + px * f, ey + dy * i as f32 + py * f];
            }
        }
        12 => {
            n = NUM_WAVEFORM_SAMPLES / 2;
            let mut alpha = mw.rgba[3] * 1.25;
            if mw.mod_alpha_by_volume {
                alpha *= (bass_mid_treb[0] + bass_mid_treb[1] + bass_mid_treb[2]) * 0.333;
            }
            alpha = alpha.clamp(0.0, 1.0);
            w1.resize(n, [0.0; 2]);
            for i in 0..n {
                let rad = 0.63 + 0.23 * pr[i] + myst;
                let ang = pl[i + 32] * 0.9 + time * 3.3;
                w1[i] = [
                    rad * (ang + alpha).cos() * asp_y + wave_x,
                    rad * ang.sin() * asp_x + wave_y,
                ];
            }
        }
        13 | 14 => {
            is_loop = true;
            n = NUM_WAVEFORM_SAMPLES / 2;
            let off = (NUM_WAVEFORM_SAMPLES - n) / 2;
            let inv = 1.0 / (n as f32 - 1.0);
            let tenth = n as f32 * 0.1;
            w1.resize(n, [0.0; 2]);
            for s in 0..n {
                if mode == 13 {
                    let mut radius = 0.7 + 0.4 * pr[s + off] + myst;
                    let angle = s as f32 * inv * 6.28 + time * 0.2;
                    if (s as f32) < n as f32 / radius {
                        let mut mix = s as f32 / tenth;
                        mix = 0.5 - 0.5 * (mix * 3.1416).cos();
                        let radius2 = 0.5 + 0.4 * pr[s + n - off] + myst;
                        radius = radius2 * (1.0 - mix) + radius * mix;
                    }
                    w1[s] = [
                        radius * angle.cos() * asp_y + wave_x,
                        radius * angle.sin() * asp_x + wave_y,
                    ];
                } else {
                    let mut radius = 0.7 + 0.7 * pr[s + off] + myst;
                    let angle = s as f32 * inv * 6.28 + time * 0.2;
                    if (s as f32) < n as f32 / radius {
                        let mut mix = s as f32 / tenth;
                        mix = 0.7 - 0.7 * (mix * 3.1416).cos();
                        let radius2 = 0.7 + 0.7 * pr[s + n - off] + myst;
                        radius = radius2 * (1.0 - mix) + radius * mix * 0.25;
                    }
                    w1[s] = [
                        radius * (angle * 3.1416).cos() * asp_y / 1.5 + wave_x * (3.1416f32).cos(),
                        radius * (angle - time / 3.0).sin() * asp_x / 1.5
                            + wave_y * (3.1416f32).cos(),
                    ];
                }
            }
        }
        15 => {
            n = NUM_WAVEFORM_SAMPLES / 2;
            w1.resize(n, [0.0; 2]);
            for s in 0..n {
                let angle = pl[s + 32] * 1.57 + time * 2.0;
                w1[s] = [
                    time.cos() / 2.0 + (angle * 2.0 + (time / angle).tan()).cos(),
                    time.sin() * 2.0 * (angle * 3.14).sin() * asp_x / 2.8 + wave_y,
                ];
            }
        }
        _ => return None,
    }

    // `WaveformMath::SmoothWave` (uses m_samples for the point count, ignores extra trailing vertices).
    let smooth = |v: &[[f32; 2]]| -> Vec<[f32; 2]> {
        if v.is_empty() {
            Vec::new()
        } else {
            smooth_wave(&v[..n.min(v.len())])
        }
    };
    let strips: Vec<Vec<[f32; 2]>> = [smooth(&w1[..]), smooth(&w2[..])]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();

    let alpha = pm_alpha(mw, mode, bass_mid_treb, viewport);
    let (mut cr, mut cg, mut cb) = (mw.rgba[0], mw.rgba[1], mw.rgba[2]);
    if mw.brighten {
        let max = cr.max(cg).max(cb);
        if max > 0.01 {
            // fMaximizeWaveColorAmount = 1: `c / max * 1 + c * (1 - 1)`, the second term is exactly 0
            cr /= max;
            cg /= max;
            cb /= max;
        }
    }
    Some(PmWaveGeom {
        strips,
        is_loop,
        alpha,
        rgb: [cr, cg, cb],
    })
}

/// `SmoothWave`: "better-than-linear" smoothing, roughly doubles the vertex count.
pub fn smooth_wave(vi: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut out = Vec::new();
    smooth_wave_into(&mut out, vi);
    out
}

/// [`smooth_wave`] into a reusable buffer (cleared first).
pub fn smooth_wave_into(out: &mut Vec<[f32; 2]>, vi: &[[f32; 2]]) {
    out.clear();
    let n = vi.len();
    if n < 2 {
        out.extend_from_slice(vi);
        return;
    }
    const C1: f32 = -0.15;
    const C2: f32 = 1.15;
    const C3: f32 = 1.15;
    const C4: f32 = -0.15;
    let inv_sum = 1.0 / (C1 + C2 + C3 + C4);
    out.reserve(n * 2);
    let mut i_below = 0usize;
    let mut i_above2 = 1usize;
    for i in 0..n - 1 {
        let i_above = i_above2;
        i_above2 = (n - 1).min(i + 2);
        out.push(vi[i]);
        out.push([
            (C1 * vi[i_below][0] + C2 * vi[i][0] + C3 * vi[i_above][0] + C4 * vi[i_above2][0])
                * inv_sum,
            (C1 * vi[i_below][1] + C2 * vi[i][1] + C3 * vi[i_above][1] + C4 * vi[i_above2][1])
                * inv_sum,
        ]);
        i_below = i;
    }
    out.push(vi[n - 1]);
}

/// Parameters of one custom wave's sample preparation (`CWave` fields + `fWaveScale`).
#[derive(Clone, Debug)]
pub struct CustomWaveSampling {
    /// `bSpectrum`
    pub spectrum: bool,
    /// `var_pf_samples` after per-frame code
    pub samples: i32,
    /// `sep`
    pub sep: i32,
    /// `scaling`
    pub scaling: f32,
    /// `smoothing`
    pub smoothing: f32,
    /// `m_fWaveScale.eval(-1)`
    pub wave_scale: f32,
}

/// Port of the sample-preparation block of `DrawCustomWaves`: resampling, forward + backward
/// smoothing and scaling. Fills `out` (cleared first) with `(value1, value2)` per point for the engine's per-point
/// code. `data1`/`data2` are `m_sound.fWaveform[0/1]` (480 valid samples) or `fSpectrum[0/1]` (512).
pub fn custom_wave_values_into(
    out: &mut Vec<[f32; 2]>,
    p: &CustomWaveSampling,
    data1: &[f32],
    data2: &[f32],
) {
    const MAX_OUT: usize = 512;
    out.clear();
    let max_in = if p.spectrum {
        512
    } else {
        NUM_WAVEFORM_SAMPLES as i32
    };
    let n = (MAX_OUT as i32).min(p.samples.max(0));
    if n < 1 {
        return;
    }
    let separator = (n - 1).min(p.sep.max(0));
    let n = n as usize;
    let mult = (if p.spectrum { 0.15 } else { 0.004 }) * p.scaling * p.wave_scale;
    let mut sample_scale = 1.0f32;
    if p.spectrum {
        sample_scale = (max_in - separator) as f32 / n as f32;
    } else if n as i32 > max_in {
        sample_scale = max_in as f32 / n as f32;
    }
    let mix1 = (p.smoothing * 0.98).powf(0.5);
    let mix2 = 1.0 - mix1;
    let at = |d: &[f32], i: usize| d.get(i).copied().unwrap_or(0.0);
    let mut t0 = [0.0f32; MAX_OUT];
    let mut t1 = [0.0f32; MAX_OUT];
    t0[0] = at(data1, 0);
    t1[0] = at(data2, 0);
    for j in 1..n {
        let pcm = (j as f32 * sample_scale) as usize;
        t0[j] = at(data1, pcm) * mix2 + t0[j - 1] * mix1;
        t1[j] = at(data2, pcm) * mix2 + t1[j - 1] * mix1;
    }
    for j in (0..n.saturating_sub(1)).rev() {
        t0[j] = t0[j] * mix2 + t0[j + 1] * mix1;
        t1[j] = t1[j] * mix2 + t1[j + 1] * mix1;
    }
    out.extend((0..n).map(|j| [t0[j] * mult, t1[j] * mult]));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_wave(amp: f32) -> MainWave {
        let mut mw = MainWave::default();
        for i in 0..NUM_WAVE_INPUT {
            mw.wave[0][i] = amp * (i as f32 * 0.2).sin();
            mw.wave[1][i] = amp * (i as f32 * 0.2 + 1.0).sin();
        }
        mw
    }

    #[test]
    fn maximize_colour_keeps_md2s_nan_behaviour() {
        let mut mw = MainWave::default();
        mw.brighten = true;
        mw.rgba = [f32::NAN, 0.2, 0.4, 1.0];
        let c = wave_rgb(&mw);
        // a NaN red stays the maximum, so green and blue are not rescaled
        assert!(c[0].is_nan() && c[1] == 0.2 && c[2] == 0.4);
        mw.rgba = [0.1, 0.2, 0.4, 1.0];
        assert_eq!(wave_rgb(&mw), [0.25, 0.5, 1.0]);
    }

    #[test]
    fn waveform_prep_is_iir_with_scale() {
        let mut mw = MainWave::default();
        mw.scale = 128.0; // scale factor 1
        mw.smoothing = 0.5;
        mw.wave[0] = [1.0; NUM_WAVE_INPUT];
        mw.wave[1] = [0.0; NUM_WAVE_INPUT];
        let (l, r) = prepare_main_waveform(&mw);
        assert!((l[0] - 1.0).abs() < 1e-6);
        // w[1] = 1*mix1 + 1*mix2 = 0.5 + 0.5 = 1
        assert!((l[1] - 1.0).abs() < 1e-6);
        assert!(r.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn all_modes_generate_geometry() {
        let mw0 = sine_wave(40.0);
        for mode in 0..NUM_WAVES_DRAWN {
            let mut mw = mw0.clone();
            mw.mode = mode;
            mw.spectrum = [[3.0; 512]; 2];
            let (l, r) = prepare_main_waveform(&mw);
            let g = main_wave_geometry(
                &mw,
                &l,
                &r,
                &mw.spectrum[0],
                &mw.spectrum[1],
                1.25,
                [1.0; 3],
                [1.0; 3],
                640,
                (1.0, 0.5625),
            )
            .unwrap_or_else(|| panic!("mode {mode} produced nothing"));
            assert!(g.verts.len() >= 2, "mode {mode}");
            assert!(
                g.verts.iter().all(|p| p[0].is_finite() && p[1].is_finite()),
                "mode {mode}"
            );
            assert!(g.alpha > 0.0 && g.alpha <= 1.0);
        }
    }

    #[test]
    fn mode0_circle_closed_and_vertex_counts() {
        let mut mw = sine_wave(0.0);
        mw.mode = 0;
        let (l, r) = prepare_main_waveform(&mw);
        let g = main_wave_geometry(
            &mw,
            &l,
            &r,
            &[],
            &[],
            0.0,
            [1.0; 3],
            [0.0; 3],
            640,
            (1.0, 0.5625),
        )
        .unwrap();
        assert_eq!(g.verts.len(), 241); // 240 + closing vertex
        assert_eq!(g.verts[0], g.verts[240]);
        // silent audio: rad = 0.5, ang0 = 0 -> (0.5*aspect_y, 0) + wave_pos(0,0)
        assert!((g.verts[0][0] - 0.5 * 0.5625).abs() < 1e-6 && g.verts[0][1].abs() < 1e-6);
        // mode 6 with 640 px: min(240, 213) verts; mode 7: two strips
        for (mode, n, brk) in [
            (6, 213, None),
            (7, 426, Some(213)),
            (4, 213, None),
            (2, 480, None),
            (8, 256, None),
        ] {
            let mut mw = sine_wave(10.0);
            mw.mode = mode;
            mw.spectrum = [[1.0; 512]; 2];
            let (l, r) = prepare_main_waveform(&mw);
            let g = main_wave_geometry(
                &mw,
                &l,
                &r,
                &mw.spectrum[0],
                &mw.spectrum[1],
                0.0,
                [1.0; 3],
                [1.0; 3],
                640,
                (1.0, 0.5625),
            )
            .unwrap();
            assert_eq!(g.verts.len(), n, "mode {mode}");
            assert_eq!(g.brk, brk, "mode {mode}");
        }
    }

    #[test]
    fn mode3_alpha_follows_treble_and_size() {
        let mut mw = sine_wave(10.0);
        mw.mode = 3;
        let (l, r) = prepare_main_waveform(&mw);
        let g = main_wave_geometry(
            &mw,
            &l,
            &r,
            &[],
            &[],
            0.0,
            [1.0; 3],
            [0.0, 0.0, 2.0],
            1024,
            (1.0, 0.75),
        )
        .unwrap();
        assert!((g.alpha - (0.22f32 * 1.3 * 4.0).min(1.0)).abs() < 1e-6);
        // zero treble -> alpha 0 -> skipped
        assert!(
            main_wave_geometry(
                &mw,
                &l,
                &r,
                &[],
                &[],
                0.0,
                [1.0; 3],
                [0.0; 3],
                1024,
                (1.0, 0.75)
            )
            .is_none()
        );
    }

    #[test]
    fn colour_clamp_and_maximize() {
        let mut mw = sine_wave(1.0);
        mw.rgba = [0.5, 0.25, 2.0, 0.8];
        let (l, r) = prepare_main_waveform(&mw);
        let g = main_wave_geometry(
            &mw,
            &l,
            &r,
            &[],
            &[],
            0.0,
            [1.0; 3],
            [1.0; 3],
            640,
            (1.0, 0.5625),
        )
        .unwrap();
        assert_eq!(g.rgb, [0.5, 0.25, 1.0]); // clamped to <=1, then maximised (max already 1)
        mw.rgba = [0.5, 0.25, 0.1, 0.8];
        let g = main_wave_geometry(
            &mw,
            &l,
            &r,
            &[],
            &[],
            0.0,
            [1.0; 3],
            [1.0; 3],
            640,
            (1.0, 0.5625),
        )
        .unwrap();
        assert_eq!(g.rgb, [1.0, 0.5, 0.2]);
    }

    #[test]
    fn mode1_mystery_wraps_like_md2() {
        let mut mw = sine_wave(1.0);
        mw.mode = 4;
        mw.mystery = 1.5; // -> 1.5*0.5+0.5 = 1.25 -> frac .25 -> .25*2-1 = -0.5
        let (l, r) = prepare_main_waveform(&mw);
        let a = main_wave_geometry(
            &mw,
            &l,
            &r,
            &[],
            &[],
            0.0,
            [1.0; 3],
            [1.0; 3],
            640,
            (1.0, 0.5625),
        )
        .unwrap();
        mw.mystery = -0.5;
        let b = main_wave_geometry(
            &mw,
            &l,
            &r,
            &[],
            &[],
            0.0,
            [1.0; 3],
            [1.0; 3],
            640,
            (1.0, 0.5625),
        )
        .unwrap();
        assert_eq!(a.verts, b.verts);
    }

    #[test]
    fn smooth_wave_doubles_and_keeps_endpoints() {
        let pts: Vec<[f32; 2]> = (0..5).map(|i| [i as f32, (i * i) as f32]).collect();
        let s = smooth_wave(&pts);
        assert_eq!(s.len(), 2 * 5 - 1);
        assert_eq!(s[0], pts[0]);
        assert_eq!(*s.last().unwrap(), pts[4]);
        // original points sit at even indices
        for i in 0..5 {
            assert_eq!(s[2 * i], pts[i]);
        }
        // a straight line stays straight: midpoints are exactly between (c sum = 2)
        let line: Vec<[f32; 2]> = (0..6).map(|i| [i as f32, 2.0 * i as f32]).collect();
        let s = smooth_wave(&line);
        for i in 1..4 {
            assert!((s[2 * i + 1][0] - (i as f32 + 0.5)).abs() < 1e-5);
        }
    }

    #[test]
    fn custom_wave_values_counts_and_scale() {
        let p = CustomWaveSampling {
            spectrum: false,
            samples: 64,
            sep: 0,
            scaling: 1.0,
            smoothing: 0.5,
            wave_scale: 1.0,
        };
        let d = vec![100.0f32; 480];
        let mut v = Vec::new();
        custom_wave_values_into(&mut v, &p, &d, &d);
        assert_eq!(v.len(), 64);
        // constant input -> constant output = 100 * 0.004
        assert!(v.iter().all(|x| (x[0] - 0.4).abs() < 1e-4));
        let p2 = CustomWaveSampling {
            samples: 1,
            ..p.clone()
        };
        custom_wave_values_into(&mut v, &p2, &d, &d);
        assert!(v.len() <= 1);
    }

    // ---- projectM main waveform (Waveforms/*.cpp) ------------------------------------------------

    fn pm_wave(mode: i32) -> MainWave {
        let mut mw = sine_wave(40.0);
        mw.mode = mode;
        mw.scale = 1.0;
        mw.smoothing = 0.0;
        mw.rgba = [1.0, 0.5, 0.25, 0.8];
        mw.x = 0.5;
        mw.y = 0.5;
        mw.spectrum = [[3.0; 512]; 2];
        mw
    }

    fn pm(mw: &MainWave) -> PmWaveGeom {
        main_wave_geometry_projectm(
            mw,
            1.25,
            [1.0; 3],
            (640, 360),
            &mut PmWaveTail::default(),
            &mut PmWaveScratch::default(),
        )
        .unwrap()
    }

    #[test]
    fn pm_sample_counts_and_loops() {
        // smoothed point count is 2*m_samples-1; line modes drop to 80 / derivative to 160 below 1440 px
        let want = [
            (0, 479, true),
            (1, 479, true),
            (2, 959, false),
            (3, 959, false),
            (4, 319, false),
            (5, 959, false),
            (6, 159, false),
            (8, 511, false),
            (13, 479, true),
            (14, 479, true),
        ];
        for (mode, len, lp) in want {
            let g = pm(&pm_wave(mode));
            assert_eq!(g.strips.len(), 1, "mode {mode}");
            assert_eq!(g.strips[0].len(), len, "mode {mode}");
            assert_eq!(g.is_loop, lp, "mode {mode}");
        }
        let g = pm(&pm_wave(7));
        assert_eq!(g.strips.len(), 2);
        assert!(g.strips.iter().all(|s| s.len() == 159));
        // wide viewport: no /3
        let g = main_wave_geometry_projectm(
            &pm_wave(6),
            0.0,
            [1.0; 3],
            (1920, 1080),
            &mut PmWaveTail::default(),
            &mut PmWaveScratch::default(),
        )
        .unwrap();
        assert_eq!(g.strips[0].len(), 479);
        let g = main_wave_geometry_projectm(
            &pm_wave(4),
            0.0,
            [1.0; 3],
            (1920, 1080),
            &mut PmWaveTail::default(),
            &mut PmWaveScratch::default(),
        )
        .unwrap();
        assert_eq!(g.strips[0].len(), 959);
        assert!(
            main_wave_geometry_projectm(
                &pm_wave(16),
                0.0,
                [1.0; 3],
                (640, 360),
                &mut PmWaveTail::default(),
                &mut PmWaveScratch::default()
            )
            .is_none()
        );
    }

    #[test]
    fn pm_waveform_prep_zero_tail_decays() {
        // 480 valid samples copied into 512 slots; the IIR runs over the zero tail (projectM, not MD2's 576)
        let mut mw = MainWave::default();
        mw.scale = 128.0;
        mw.smoothing = 0.5;
        mw.wave[0] = [1.0; NUM_WAVE_INPUT];
        let (l, _) = prepare_main_waveform_projectm(&mw, false, &mut PmWaveTail::default());
        assert!((l[479] - 1.0).abs() < 1e-6);
        assert!((l[480] - 0.5).abs() < 1e-6);
        assert!((l[483] - 0.0625).abs() < 1e-6);
    }

    #[test]
    fn pm_waveform_tail_persists_across_frames() {
        // projectM overwrites only slots 0..480 each frame, so slots 480..512 are smoothed *in place* from the previous
        // frame's values: frame 1 (zero tail) -> 0.5, 0.25, ..; frame 2 -> 0.5*0.5 + 1.0*0.5 = 0.75, ..; converging to 1.
        let mut mw = MainWave::default();
        mw.scale = 128.0;
        mw.smoothing = 0.5;
        mw.wave = [[1.0; NUM_WAVE_INPUT]; 2];
        let mut tail = PmWaveTail::default();
        let (l1, r1) = prepare_main_waveform_projectm(&mw, false, &mut tail);
        assert!((l1[480] - 0.5).abs() < 1e-6 && (r1[481] - 0.25).abs() < 1e-6);
        let (l2, _) = prepare_main_waveform_projectm(&mw, false, &mut tail);
        assert!((l2[480] - 0.75).abs() < 1e-6);
        assert!((l2[481] - (0.25 * 0.5 + 0.75 * 0.5)).abs() < 1e-6);
        let (l3, _) = prepare_main_waveform_projectm(&mw, false, &mut tail);
        assert!((l3[480] - 0.875).abs() < 1e-6);
        // spectrum waves copy all 512 slots and neither read nor change the tail
        let before = tail;
        mw.spectrum = [[0.25; NUM_FREQ_SAMPLES]; 2];
        let (s, _) = prepare_main_waveform_projectm(&mw, true, &mut tail);
        assert_eq!(tail, before);
        assert!((s[511] - 0.25).abs() < 1e-6);

        // the last vertices of mode 2 (`y = left[i + 32]`) see the carried-over tail
        let mut mw = pm_wave(2);
        mw.wave = [[1.0; NUM_WAVE_INPUT]; 2];
        mw.smoothing = 0.5;
        mw.scale = 128.0;
        let mut tail = PmWaveTail::default();
        let mut sc = PmWaveScratch::default();
        let a = main_wave_geometry_projectm(&mw, 0.0, [1.0; 3], (640, 360), &mut tail, &mut sc)
            .unwrap();
        let b = main_wave_geometry_projectm(&mw, 0.0, [1.0; 3], (640, 360), &mut tail, &mut sc)
            .unwrap();
        let last = a.strips[0].len() - 1;
        assert_ne!(a.strips[0][last], b.strips[0][last]);
        assert_eq!(a.strips[0][0], b.strips[0][0]);
    }

    #[test]
    fn pm_line_uses_left_channel_and_wave_x_as_offset() {
        // flat left channel at 0, right channel loud: the 80-sample line mode must stay a straight line
        let mut mw = pm_wave(6);
        mw.wave[0] = [0.0; NUM_WAVE_INPUT];
        mw.wave[1] = [100.0; NUM_WAVE_INPUT];
        mw.x = 0.75; // `wave_x` positions the line perpendicular to its direction: y = 2*0.75-1 = 0.5
        let g = pm(&mw);
        let s = &g.strips[0];
        for p in s {
            assert!((p[1] - 0.5).abs() < 1e-3, "{p:?}");
        }
        assert!((s[0][0] + 1.1).abs() < 1e-3);
        assert!(s[s.len() - 1][0] < 1.1 && s[s.len() - 1][0] > 1.0);
        // circle reads the right channel only
        let mut c = pm_wave(0);
        c.wave[0] = [0.0; NUM_WAVE_INPUT];
        c.wave[1] = [0.0; NUM_WAVE_INPUT];
        let flat = pm(&c).strips[0][100];
        c.wave[0] = [100.0; NUM_WAVE_INPUT];
        assert_eq!(pm(&c).strips[0][100], flat);
        c.wave[1] = [100.0; NUM_WAVE_INPUT];
        assert_ne!(pm(&c).strips[0][100], flat);
    }

    #[test]
    fn pm_alpha_rules() {
        let tre = |mw: &MainWave, vp: (u32, u32), bmt: [f32; 3]| {
            main_wave_geometry_projectm(
                mw,
                0.0,
                bmt,
                vp,
                &mut PmWaveTail::default(),
                &mut PmWaveScratch::default(),
            )
            .unwrap()
            .alpha
        };
        // centered spiro / hash: texsize factor with `<=` thresholds (640 -> 0.11, 256 -> 0.07, 4096 -> 0.15)
        let m = pm_wave(2);
        assert!((tre(&m, (640, 360), [1.0; 3]) - 0.8 * 0.11).abs() < 1e-6);
        assert!((tre(&m, (256, 144), [1.0; 3]) - 0.8 * 0.07).abs() < 1e-6);
        assert!((tre(&m, (4096, 2160), [1.0; 3]) - 0.8 * 0.15).abs() < 1e-6);
        // spiro-volume: * 1.3 * treb^2
        let m = pm_wave(3);
        assert!(
            (tre(&m, (640, 360), [1.0, 1.0, 2.0]) - (0.8f32 * 0.22 * 1.3 * 4.0).min(1.0)).abs()
                < 1e-6
        );
        // modulate by volume overwrites (does not multiply) and is piecewise
        let mut m = pm_wave(2);
        m.mod_alpha_by_volume = true;
        m.mod_alpha_start = 0.2;
        m.mod_alpha_end = 0.9;
        assert_eq!(tre(&m, (640, 360), [0.1, 0.1, 0.1]), 0.0); // vol 0.0999 <= start
        assert!((tre(&m, (640, 360), [3.0, 3.0, 3.0]) - 0.8).abs() < 1e-6); // vol >= end -> wave_a, not * 0.11
        let mid = tre(&m, (640, 360), [1.0, 1.0, 1.0]); // vol 0.999 -> >= end
        assert!((mid - 0.8).abs() < 1e-6);
        let mid = tre(&m, (640, 360), [0.5, 0.5, 0.5]); // vol 0.4995
        assert!((mid - 0.8 * (0.4995 - 0.2) / 0.7).abs() < 1e-5);
    }

    #[test]
    fn pm_maximize_colour_and_raw_colour() {
        let mut mw = pm_wave(6);
        mw.brighten = false;
        mw.rgba = [0.4, 0.2, 0.1, 1.0];
        assert_eq!(pm(&mw).rgb, [0.4, 0.2, 0.1]);
        mw.brighten = true;
        let c = pm(&mw).rgb;
        assert!(
            (c[0] - 1.0).abs() < 1e-6 && (c[1] - 0.5).abs() < 1e-6 && (c[2] - 0.25).abs() < 1e-6
        );
        // not clamped (GL clamps at the framebuffer)
        mw.brighten = false;
        mw.rgba = [2.0, 0.0, 0.0, 1.0];
        assert_eq!(pm(&mw).rgb[0], 2.0);
    }

    #[test]
    fn pm_derivative_line_geometry() {
        // mode 4: 160 samples, x spans -1..1 around wave_x, y = left sample * 0.47 + wave_y
        let mut mw = pm_wave(4);
        mw.wave = [[0.0; NUM_WAVE_INPUT]; 2];
        mw.scale = 128.0;
        let g = pm(&mw);
        let s = &g.strips[0];
        assert!((s[0][0] + 1.0).abs() < 1e-6 && s[0][1].abs() < 1e-6);
        // momentum extrapolation of a straight line stays a straight line (i > 1), x grows by 2/160 per sample
        let dx = s[2][0] - s[0][0]; // s[2] is the second input vertex (s[1] is its smoothing midpoint)
        assert!((dx - 2.0 / 160.0).abs() < 1e-4, "{dx}");
    }
}
