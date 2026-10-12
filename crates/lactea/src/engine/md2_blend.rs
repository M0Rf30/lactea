//! MilkDrop 2's preset blend, CPU side (`Compat::MilkDrop2` / `Compat::MilkDrop3`, `CompatRules::md2_preset_blend`).
//!
//! Port of the parts of `third_party/md2` that make `m_bBlending` work:
//!
//! | MD2 | here |
//! |---|---|
//! | `CBlendableFloat::eval` / `StartBlendFrom` (state.cpp) | [`BlendFloat`] |
//! | the `CBlendableFloat` members of a `CState` | [`BlendBase`] (`Loaded::floats`) |
//! | `CState::StartBlendFrom` | [`start_blend_from`] |
//! | `RunPerFrameEquations(code)` snap point + variable blend | [`snap_point`], [`blend_vars`] |
//! | `ComputeGridAlphaValues` (`mix2`, vertex alpha) | [`blend_mesh`] |
//! | `RandomizeBlendPattern` / `GenPlasma` (plugin.cpp) | [`randomize_blend_pattern`] |
//! | `RenderFrame` / `DrawCustomShapes` / `DrawWave` ... inputs | [`build_blend_frame`] |
//! | `LoadPreset` / `LoadPresetTick` with a blend | `Engine::load_preset_md2_soft` |
//!
//! The renderer half (warp / composite pass pairs, tile culling, two-wave blend) lives in `lactea-render`.
//!
//! The literals of the MD2 pattern generators (`6.28`, `1.41421`, `3.14159`, ...) are MD2's own
//! rounded constants and must stay as written for bit-identical vertex values.
#![allow(
    clippy::approx_constant,
    clippy::excessive_precision,
    clippy::manual_clamp
)]

use super::md3_blend::{BlendLaunch, ProgressMode};
use super::{
    FrameCtx, LastFrame, Loaded, Slot, eval_per_frame, eval_vertices, finish_frame, frame_env,
    frame_head, push_shapes, push_waves, warp_anim,
};
use crate::rng::StreamRng;
use crate::runtime::PerFrame;
use crate::{EngineConfig, EngineError, ForcedFrame, PresetReport};
use lactea_preset::BaseValues;
use lactea_render::warp_mesh;
use lactea_render::{BlurParams, FrameInput, Md2OldPass, PassRandom, cosine_interp};

// ---------------------------------------------------------------------------------------------
// CBlendableFloat
// ---------------------------------------------------------------------------------------------

/// The blend half of a `CBlendableFloat`: the value it blends *from*, the window and whether it is still running.
/// The target value (`val`) is the preset's own base value, passed in by the caller.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct BlendFloat {
    blending: bool,
    start: f32,
    dur: f32,
    from: f32,
}

impl BlendFloat {
    /// `CBlendableFloat::eval(fTime)`: `fTime < 0` returns `val` unblended; a blend ends (for good) once
    /// `fTime > start + duration` (or time runs backwards before `start`), otherwise it is linear.
    pub fn eval(&mut self, val: f32, t: f32) -> f32 {
        if t < 0.0 {
            return val;
        }
        if (self.blending && t > self.start + self.dur) || t < self.start {
            self.blending = false;
        }
        if !self.blending {
            return val;
        }
        let mix = (t - self.start) / self.dur;
        self.from * (1.0 - mix) + val * mix
    }

    /// `CBlendableFloat::StartBlendFrom`: durations below 1 ms do not start a blend.
    pub fn start_blend_from(&mut self, from: &mut BlendFloat, from_val: f32, t: f32, dur: f32) {
        if dur < 0.001 {
            return;
        }
        self.from = from.eval(from_val, t);
        self.blending = true;
        self.start = t;
        self.dur = dur;
    }

    /// `operator = (float)`: assigning a value stops the blend.
    pub fn stop(&mut self) {
        self.blending = false;
    }
}

macro_rules! blend_base {
    (motion: [$($m:ident),* $(,)?], timed: [$($t:ident),* $(,)?]) => {
        /// Every `CBlendableFloat` member of a `CState` (named like [`BaseValues`]).
        #[derive(Clone, Debug, Default)]
        pub(super) struct BlendBase {
            $(pub $m: BlendFloat,)*
            $(pub $t: BlendFloat,)*
        }

        impl BlendBase {
            /// The state's base values as MD2 reads them at time `t`: `m_f*.eval(GetTime())` for everything
            /// except the ten pixel-motion variables, which `LoadPerFrameEvallibVars` reads with `eval(-1)`
            /// (the unblended value; the motion is blended later, as two UV meshes).
            pub fn eval(&mut self, base: &BaseValues, t: f32) -> BaseValues {
                let mut o = base.clone();
                $( o.$t = self.$t.eval(base.$t, t); )*
                o
            }

            /// `CState::StartBlendFrom`: every `CBlendableFloat` of this state starts from `from`'s value at `t`.
            pub fn start_blend_from(
                &mut self,
                from: &mut BlendBase,
                from_base: &BaseValues,
                t: f32,
                dur: f32,
            ) {
                $( self.$m.start_blend_from(&mut from.$m, from_base.$m, t, dur); )*
                $( self.$t.start_blend_from(&mut from.$t, from_base.$t, t, dur); )*
            }
        }
    };
}

blend_base! {
    motion: [zoom, zoom_exponent, rot, cx, cy, dx, dy, warp, sx, sy],
    timed: [
        gamma_adj, video_echo_zoom, video_echo_alpha, decay, fft_attack, fft_decay, wave_alpha, wave_scale,
        wave_smoothing, wave_param, warp_scale, shader, mod_wave_alpha_start, mod_wave_alpha_end,
        wave_r, wave_g, wave_b, wave_x, wave_y, ob_size, ob_r, ob_g, ob_b, ob_a, ib_size, ib_r, ib_g, ib_b,
        ib_a, mv_x, mv_y, mv_dx, mv_dy, mv_l, mv_r, mv_g, mv_b, mv_a, blur1_min, blur2_min, blur3_min,
        blur1_max, blur2_max, blur3_max, blur1_edge_darken
    ]
}

/// What `CState::StartBlendFrom` remembers beyond the floats (`m_fVideoEchoAlphaOld`,
/// `m_nVideoEchoOrientationOld`, `m_nOldWaveMode`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BlendStatics {
    pub echo_alpha_old: f32,
    pub echo_orient_old: i32,
    pub old_wave_mode: i32,
}

/// `CState::StartBlendFrom(s_from, fAnimTime, fTimespan)` for `to = new`, `from = old`.
pub(super) fn start_blend_from(
    new: &mut Loaded,
    old: &mut Loaded,
    t: f32,
    dur: f32,
) -> BlendStatics {
    // bools, ints, and strings instantly change
    let statics = BlendStatics {
        echo_alpha_old: old.preset.base.video_echo_alpha, // `.eval(-1)`
        echo_orient_old: old.preset.base.video_echo_orientation,
        old_wave_mode: old.preset.base.wave_mode,
    };
    // CBlendableFloats blend over time
    new.floats
        .start_blend_from(&mut old.floats, &old.preset.base, t, dur);

    // if motion vectors were transparent before, don't morph the # in X and Y - just start in the right place, and
    // fade them in. (Both conditions below are the same in MD2: `!old && new` twice; ported as written.)
    let old_transparent = old.preset.base.mv_a < 0.001;
    let new_transparent = new.preset.base.mv_a < 0.001;
    macro_rules! mv_fields {
        ($($f:ident),*) => {
            if !old_transparent && new_transparent {
                $( {
                    let v = new.floats.$f.eval(new.preset.base.$f, t);
                    old.preset.base.$f = v;
                    old.floats.$f.stop();
                } )*
            }
            if new_transparent && !old_transparent {
                $( {
                    let v = old.floats.$f.eval(old.preset.base.$f, t);
                    new.preset.base.$f = v;
                    new.floats.$f.stop();
                } )*
            }
        };
    }
    mv_fields!(mv_x, mv_y, mv_dx, mv_dy, mv_l, mv_r, mv_g, mv_b);
    statics
}

// ---------------------------------------------------------------------------------------------
// RunPerFrameEquations: snap point and variable blend
// ---------------------------------------------------------------------------------------------

/// The 4-bit `code` of `RenderFrame`: which of the old / new preset use a warp / composite shader.
pub(super) fn shader_code(old_warp: bool, old_comp: bool, new_warp: bool, new_comp: bool) -> u8 {
    (if old_warp { 8 } else { 0 })
        | (if old_comp { 4 } else { 0 })
        | (if new_warp { 2 } else { 0 })
        | (if new_comp { 1 } else { 0 })
}

/// `m_fSnapPoint` while blending: where the booleans (darken, invert, wrap ...) of pre-shader presets flip. Blending
/// to / from a comp-shader preset moves the flip to the end / start of the blend for a less jumpy transition.
pub(super) fn snap_point(code: u8) -> f32 {
    match code {
        // old preset (only) had a comp shader
        4 | 6 | 12 | 14 => -0.01,
        // new preset (only) has a comp shader
        1 | 3 | 9 | 11 => 1.01,
        // neither, or both
        _ => 0.5,
    }
}

/// The second half of `RunPerFrameEquations`: for all variables that do not affect pixel motion, blend the new
/// state's (`new`, after its per-frame code) with the old one's by `mix = CosineInterp(progress)` — so later the
/// frame can read `m_pState->var_pf_*` — and snap the booleans at `snap`.
pub(super) fn blend_vars(new: &mut PerFrame, old: &PerFrame, mix: f64, snap: f64) {
    let mix2 = 1.0 - mix;
    macro_rules! mix_vars {
        ($($f:ident),* $(,)?) => {$({
            let n = new.ctx.get(new.v.$f);
            let o = old.ctx.get(old.v.$f);
            new.ctx.set(new.v.$f, mix * n + mix2 * o);
        })*};
    }
    macro_rules! snap_vars {
        ($($f:ident),* $(,)?) => {$({
            let o = old.ctx.get(old.v.$f);
            if mix < snap {
                new.ctx.set(new.v.$f, o);
            }
        })*};
    }
    mix_vars!(
        decay,
        wave_a,
        wave_r,
        wave_g,
        wave_b,
        wave_x,
        wave_y,
        wave_mystery, // wave_mode: exempt (integer)
        ob_size,
        ob_r,
        ob_g,
        ob_b,
        ob_a,
        ib_size,
        ib_r,
        ib_g,
        ib_b,
        ib_a,
        mv_x,
        mv_y,
        mv_dx,
        mv_dy,
        mv_l,
        mv_r,
        mv_g,
        mv_b,
        mv_a,
        echo_zoom,
        echo_alpha
    );
    snap_vars!(
        echo_orient,
        wave_usedots,
        wave_thick,
        wave_additive,
        wave_brighten,
        darken_center
    );
    mix_vars!(gamma);
    snap_vars!(wrap, invert, brighten, darken, solarize);
    mix_vars!(
        blur1_min,
        blur2_min,
        blur3_min,
        blur1_max,
        blur2_max,
        blur3_max,
        blur1_edge_darken
    );
    // MD3: `mouse.x` / `mouse.y` blend like the other per-frame outputs; `mouse.z` / `mouse.w` are neither blended
    // nor snapped
    if let (Some(n), Some(o)) = (new.mouse, old.mouse) {
        for (&nv, &ov) in n.iter().zip(&o).take(2) {
            let (a, b) = (new.ctx.get(nv), old.ctx.get(ov));
            new.ctx.set(nv, mix * a + mix2 * b);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// ComputeGridAlphaValues
// ---------------------------------------------------------------------------------------------

/// `mix2` of one vertex: `m_vertinfo[n].a * fBlend + m_vertinfo[n].c`, clamped like `max(0, min(1, mix2))`
/// (MD2's macros, so a NaN stays NaN).
pub(super) fn vertex_mix(a: f32, c: f32, f_blend: f32) -> f32 {
    let mix2 = a * f_blend + c;
    let m = if 1.0 < mix2 { 1.0 } else { mix2 };
    if 0.0 > m { 0.0 } else { m }
}

/// Blend the new state's warp UVs with the old state's at every vertex (`m_verts[n].tu = tu*mix2 + u*(1-mix2)`) and
/// the alpha byte `(DWORD)(mix2*255)` the two draw passes cull and blend by. `pattern[n]` is `(a, c)`.
pub(super) fn blend_mesh(
    new_uv: &[[f32; 2]],
    old_uv: &[[f32; 2]],
    pattern: &[[f32; 2]],
    f_blend: f32,
) -> (Vec<[f32; 2]>, Vec<u8>) {
    let mut uv = Vec::with_capacity(new_uv.len());
    let mut alpha = Vec::with_capacity(new_uv.len());
    for n in 0..new_uv.len() {
        let mix2 = vertex_mix(pattern[n][0], pattern[n][1], f_blend);
        uv.push([
            new_uv[n][0] * mix2 + old_uv[n][0] * (1.0 - mix2),
            new_uv[n][1] * mix2 + old_uv[n][1] * (1.0 - mix2),
        ]);
        // `0x00FFFFFF | (((DWORD)(mix2*255))<<24)`: only the low byte survives the shift
        alpha.push(((mix2 * 255.0) as u32 & 0xFF) as u8);
    }
    (uv, alpha)
}

// ---------------------------------------------------------------------------------------------
// RandomizeBlendPattern
// ---------------------------------------------------------------------------------------------

/// `int TransitionLimit = 18` (plugin.cpp): the pattern kinds are `0..=TRANSITION_LIMIT`.
pub const TRANSITION_LIMIT: u32 = 18;

/// `FRAND` of plugin.cpp (`uniform_real_distribution<float>(0, 1)`): the seeded stream's next `[0, 1)` float.
fn frand(r: &mut StreamRng) -> f32 {
    (r.next32() >> 8) as f32 / 16_777_216.0
}

/// `rand()` of the C runtime (0..=32767) drawn from the seeded stream.
fn irand(r: &mut StreamRng) -> i32 {
    (r.next32() >> 17) as i32
}

/// The `static float` time accumulators of the animated patterns (one per pattern, each `+= 1/GetFps()` per call).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct PatternClocks {
    checker: f32,
    disco: f32,
    fire: f32,
    drain: f32,
}

const TAU: f32 = 6.2831853;

/// `RandomizeBlendPattern()`: fills `vertinfo[n] = (a, c)` of the `(gx+1) x (gy+1)` mesh (row 0 first) with one of
/// the 19 blend patterns (`forced`, else drawn like `uniform_int_distribution(0, TransitionLimit)`) and returns the
/// kind. `aspect` is `(m_fAspectX, m_fAspectY)`; `vertinfo` persists between calls like `m_vertinfo` (`GenPlasma`
/// reads the entries it has not written yet).
pub(super) fn randomize_blend_pattern(
    vertinfo: &mut [[f32; 2]],
    grid: [u32; 2],
    aspect: (f32, f32),
    fps: f32,
    rng: &mut StreamRng,
    clocks: &mut PatternClocks,
    forced: Option<u32>,
) -> u32 {
    let (gx, gy) = (grid[0] as usize, grid[1] as usize);
    let (gxf, gyf) = (gx as f32, gy as f32);
    let (ax, ay) = aspect;
    // `m_nTransitionBlendPattern > -1 ? m_nTransitionBlendPattern : dist(gen)`
    let mixtype = match forced {
        Some(k) => k,
        None => rng.next32() % (TRANSITION_LIMIT + 1),
    };
    // `fx`, `fy` of the patterns that centre the mesh (and apply the aspect)
    let cxy = |x: usize, y: usize| ((x as f32 / gxf - 0.5) * ax, (y as f32 / gyf - 0.5) * ay);
    let uxy = |x: usize, y: usize| (x as f32 / gxf, y as f32 / gyf);
    let set = |vi: &mut [[f32; 2]], n: usize, inv_band: f32, band_mul: f32, c: f32| {
        vi[n][0] = inv_band * band_mul;
        vi[n][1] = c;
    };
    // a = inv_band * (1 + band), c = -inv_band + inv_band * t
    macro_rules! each {
        ($band:expr, |$x:ident, $y:ident| $t:expr) => {{
            let band: f32 = $band;
            let inv_band = 1.0 / band;
            for $y in 0..=gy {
                for $x in 0..=gx {
                    let t: f32 = $t;
                    set(
                        vertinfo,
                        $y * (gx + 1) + $x,
                        inv_band,
                        1.0 + band,
                        -inv_band + inv_band * t,
                    );
                }
            }
        }};
    }

    match mixtype {
        0 => {
            // constant, uniform blend
            for v in vertinfo.iter_mut() {
                *v = [1.0, 0.0];
            }
        }
        1 => {
            // directional wipe
            let ang = frand(rng) * 6.28;
            let (vx, vy) = (ang.cos(), ang.sin());
            let band = 0.1 + 0.2 * frand(rng);
            each!(band, |x, y| {
                let fy = (y as f32 / gyf) * ay;
                let fx = (x as f32 / gxf) * ax;
                let t = (fx - 0.5) * vx + (fy - 0.5) * vy + 0.5;
                (t - 0.5) / 2.0f32.sqrt() + 0.5
            });
        }
        2 => {
            // plasma transition
            let band = 0.12 + 0.13 * frand(rng);
            let inv_band = 1.0 / band;
            vertinfo[0][1] = frand(rng);
            vertinfo[gx][1] = frand(rng);
            vertinfo[gy * (gx + 1)][1] = frand(rng);
            vertinfo[gy * (gx + 1) + gx][1] = frand(rng);
            gen_plasma(vertinfo, gx, aspect, rng, 0, gx, 0, gy, 0.25);
            let mut minc = vertinfo[0][1];
            let mut maxc = vertinfo[0][1];
            for v in vertinfo.iter() {
                if minc > v[1] {
                    minc = v[1];
                }
                if maxc < v[1] {
                    maxc = v[1];
                }
            }
            let mult = 1.0 / (maxc - minc);
            for v in vertinfo.iter_mut() {
                let t = (v[1] - minc) * mult;
                v[0] = inv_band * (1.0 + band);
                v[1] = -inv_band + inv_band * t;
            }
        }
        3 => {
            // radial blend
            let band = 0.02 + 0.14 * frand(rng) + 0.34 * frand(rng);
            let dir = ((irand(rng) % 2) * 2 - 1) as f32; // 1=outside-in, -1=inside-out
            each!(band, |x, y| {
                let (dx, dy) = cxy(x, y);
                let mut t = (dx * dx + dy * dy).sqrt() * 1.41421;
                if dir == -1.0 {
                    t = 1.0 - t;
                }
                t
            });
        }
        4 => {
            // seamless clock transition
            let band = 0.08 + 0.14 * frand(rng);
            let dir = if irand(rng) % 2 != 0 { 1.0f32 } else { -1.0 };
            let start_angle = frand(rng) * TAU;
            each!(band, |x, y| {
                let (fx, fy) = cxy(x, y);
                let mut angle = fy.atan2(fx);
                let dist = (fx * fx + fy * fy).sqrt() * 1.41421356;
                if angle < 0.0 {
                    angle += TAU;
                }
                angle = (angle * dir + start_angle + 10.0 * TAU) % TAU;
                let t = angle / TAU;
                let t_adjusted = if t < band { t + 1.0 } else { t };
                t_adjusted - dist * 0.1
            });
        }
        5 => {
            // spiral / snail
            let band = 0.07 + 0.1 * frand(rng);
            let loops = 2 + (irand(rng) % 7);
            let rotation_speed = frand(rng) * 0.5;
            let inward = irand(rng) % 2 == 0;
            each!(band, |x, y| {
                let (fx, fy) = cxy(x, y);
                let mut angle = fy.atan2(fx);
                let radius = (fx * fx + fy * fy).sqrt() * 1.41421356;
                if angle < 0.0 {
                    angle += TAU;
                }
                let mut p = (angle / TAU + loops as f32 * radius + rotation_speed) % 1.0;
                if inward {
                    p = 1.0 - p;
                }
                p
            });
        }
        6 => {
            // rhombus / diamond
            let band = 0.07 + 0.12 * frand(rng);
            let angle = frand(rng) * TAU;
            let asp = 0.8 + frand(rng) * 2.4;
            let reverse = irand(rng) % 2 == 0;
            let (cos_a, sin_a) = (angle.cos(), angle.sin());
            let norm_factor = 1.0 / (1.0 + asp);
            each!(band, |x, y| {
                let (fx, fy) = cxy(x, y);
                let rx = fx * cos_a - fy * sin_a;
                let ry = fx * sin_a + fy * cos_a;
                let diamond = (rx.abs() * asp + ry.abs()) * norm_factor;
                if reverse { 1.0 - diamond } else { diamond }
            });
        }
        7 => {
            // nuclear clock wipe
            let band = 0.05 + 0.15 * frand(rng);
            let exact_repeats = 3.0f32;
            let reverse = irand(rng) % 2 == 0;
            let glow_intensity = 0.5 + frand(rng) * 1.5;
            let center_x = 0.5 + (frand(rng) - 0.5) * 0.1;
            let center_y = 0.5 + (frand(rng) - 0.5) * 0.1;
            each!(band, |x, y| {
                let fy = (y as f32 / gyf - center_y) * ay;
                let fx = (x as f32 / gxf - center_x) * ax;
                let mut angle = fy.atan2(fx);
                let dist = (fx * fx + fy * fy).sqrt() * 1.41421356;
                if angle < 0.0 {
                    angle += TAU;
                }
                let mut clock_pos = angle / TAU * exact_repeats;
                if reverse {
                    clock_pos = exact_repeats - clock_pos;
                }
                clock_pos -= clock_pos.floor();
                let glow = (1.0 - dist) * glow_intensity;
                clock_pos + glow * 0.3
            });
        }
        8 => {
            // square / diamond
            let band = 0.08 + 0.12 * frand(rng);
            let diagonal = irand(rng) % 2 == 0;
            let center_bias = 0.3 + frand(rng) * 0.4;
            let softness = 0.1 + frand(rng) * 0.2;
            let reverse = irand(rng) % 2 == 0;
            each!(band, |x, y| {
                let (fx, fy) = cxy(x, y);
                let mut t = if diagonal {
                    let d1 = (fx + fy) * 0.7071;
                    let d2 = (fx - fy) * 0.7071;
                    if d1.abs() > d2.abs() {
                        d1.abs()
                    } else {
                        d2.abs()
                    }
                } else if fx.abs() > fy.abs() {
                    fx.abs()
                } else {
                    fy.abs()
                };
                t = t.powf(center_bias);
                t = t * (1.0 + softness) - softness * 0.5;
                t = if t < 0.0 {
                    0.0
                } else if t > 1.0 {
                    1.0
                } else {
                    t
                };
                if reverse { 1.0 - t } else { t }
            });
        }
        9 => {
            // animated checkerboard
            let band = 0.05 + 0.15 * frand(rng);
            let checker_size = 4 + (irand(rng) % 12);
            let anim_speed = 0.5 + frand(rng) * 2.0;
            let diagonal_anim = irand(rng) % 2 == 0;
            let reverse = irand(rng) % 2 == 0;
            clocks.checker += 1.0 / fps;
            let time = clocks.checker;
            each!(band, |x, y| {
                let (fx, fy) = uxy(x, y);
                let cx = (fx * checker_size as f32) as i32;
                let cy = (fy * checker_size as f32 * ay) as i32;
                let checker = (cx + cy) % 2;
                let mut anim_progress = if diagonal_anim {
                    (fx + fy) * 0.5 + time * anim_speed
                } else {
                    fx + time * anim_speed
                };
                anim_progress %= 2.0;
                if anim_progress > 1.0 {
                    anim_progress = 2.0 - anim_progress;
                }
                if reverse {
                    anim_progress = 1.0 - anim_progress;
                }
                if checker == 0 {
                    anim_progress - 0.3
                } else {
                    anim_progress + 0.3
                }
            });
        }
        10 => {
            // curtain
            let band = 0.05 + 0.15 * frand(rng);
            let opening = irand(rng) % 2 == 0;
            let vertical = irand(rng) % 2 == 0;
            let curtain_wrinkles = 0.5 + frand(rng) * 2.0;
            let center_gap = 0.05 + frand(rng) * 0.15;
            let reverse_motion = irand(rng) % 2 == 0;
            let repeats = 1 + (irand(rng) % 4);
            let repeat_variation = 0.3 * frand(rng);
            let alternate_direction = irand(rng) % 2 == 0;
            each!(band, |x, y| {
                let (fx, fy) = uxy(x, y);
                let (pos, other) = if vertical { (fx, fy) } else { (fy, fx) };
                let segment_pos = pos * repeats as f32;
                let segment_idx = segment_pos.floor() as i32;
                let segment_local = segment_pos - segment_idx as f32;
                let mut center_dist = (segment_local - 0.5).abs() - center_gap / 2.0;
                if center_dist < 0.0 {
                    center_dist = 0.0;
                }
                let mut curtain_side = if segment_local < 0.5 { -1.0f32 } else { 1.0 };
                let mut t = center_dist * 2.0;
                t += (segment_idx as f32 * 1.618).sin() * repeat_variation;
                let wrinkles = (other * 3.14159 * curtain_wrinkles).sin() * 0.1;
                t += wrinkles * (1.0 - t);
                if opening {
                    t = 1.0 - t;
                }
                if alternate_direction && segment_idx % 2 == 1 {
                    curtain_side *= -1.0;
                }
                if reverse_motion {
                    if curtain_side > 0.0 { t } else { 1.0 - t }
                } else if curtain_side > 0.0 {
                    1.0 - t
                } else {
                    t
                }
            });
        }
        11 => {
            // bubbles
            let band = 0.05 + 0.15 * frand(rng);
            let bubble_count = 10 + (irand(rng) % 30);
            let size_min = 0.05 + frand(rng) * 0.1;
            let size_max = 0.15 + frand(rng) * 0.2;
            let growing = irand(rng) % 2 == 0;
            let mut bubbles = Vec::with_capacity(bubble_count as usize);
            for _ in 0..bubble_count {
                let bx = frand(rng);
                let by = frand(rng);
                let size = size_min + frand(rng) * (size_max - size_min);
                let _speed = 0.5 + frand(rng) * 1.5;
                bubbles.push((bx, by, size));
            }
            each!(band, |x, y| {
                let (fx, fy) = uxy(x, y);
                let mut max_influence = 0.0f32;
                for &(bx, by, size) in &bubbles {
                    let dx = (fx - bx) * ax;
                    let dy = (fy - by) * ay;
                    let dist = (dx * dx + dy * dy).sqrt();
                    let mut influence = 1.0 - (dist / size);
                    if influence < 0.0 {
                        influence = 0.0;
                    }
                    influence = influence * influence * (3.0 - 2.0 * influence);
                    if influence > max_influence {
                        max_influence = influence;
                    }
                }
                if growing {
                    max_influence
                } else {
                    1.0 - max_influence
                }
            });
        }
        12 => {
            // kaleidoscope wipe
            let band = 0.06 + 0.14 * frand(rng);
            let segments = 3 + (irand(rng) % 9);
            let segment_angle = TAU / segments as f32;
            let rotation = frand(rng) * TAU;
            let mirror = irand(rng) % 2 == 0;
            let radial_factor = 0.5 + frand(rng);
            each!(band, |x, y| {
                let (fx, fy) = cxy(x, y);
                let mut angle = fy.atan2(fx) + rotation;
                let radius = (fx * fx + fy * fy).sqrt() * 1.41421356;
                if angle < 0.0 {
                    angle += TAU;
                }
                if angle >= TAU {
                    angle -= TAU;
                }
                let segment = (angle / segment_angle) as i32;
                let mut segment_offset = angle - segment as f32 * segment_angle;
                if mirror && segment_offset > segment_angle * 0.5 {
                    segment_offset = segment_angle - segment_offset;
                }
                let normalized_angle = segment_offset / segment_angle;
                normalized_angle * 0.7 + radius * 0.3 * radial_factor
            });
        }
        13 => {
            // moebius strip
            let band = 0.07 + 0.13 * frand(rng);
            let twist_factor = 1.0 + frand(rng) * 2.0;
            let reverse_twist = irand(rng) % 2 == 0;
            let strip_width = 0.3 + frand(rng) * 0.4;
            let progress_offset = frand(rng) * 0.5;
            each!(band, |x, y| {
                let (fx, fy) = cxy(x, y);
                let angle = fy.atan2(fx);
                let radius = (fx * fx + fy * fy).sqrt() * 1.41421356;
                let normalized_angle = (angle + 3.14159265) / TAU;
                let mut twist_progress = (normalized_angle + progress_offset) * twist_factor;
                if reverse_twist {
                    twist_progress = -twist_progress;
                }
                let moebius_value = radius + 0.3 * (twist_progress * 3.14159265).sin();
                let t = (moebius_value * (1.0 / strip_width)) % 1.0;
                1.0 - t
            });
        }
        14 => {
            // star wipe
            let band = 0.05 + 0.15 * frand(rng);
            let points = 5 + (irand(rng) % 2);
            let inner_radius = 0.3 + frand(rng) * 0.4;
            let rotation = frand(rng) * TAU;
            let reverse = irand(rng) % 2 == 0;
            each!(band, |x, y| {
                let (fx, fy) = cxy(x, y);
                let mut angle = fy.atan2(fx) + rotation;
                let radius = (fx * fx + fy * fy).sqrt() * 1.41421356;
                if angle < 0.0 {
                    angle += TAU;
                }
                if angle >= TAU {
                    angle -= TAU;
                }
                let segment = TAU / points as f32;
                let point_angle = (angle % segment) / segment;
                let star_radius = if point_angle < 0.5 {
                    inner_radius + (1.0 - inner_radius) * point_angle * 2.0
                } else {
                    1.0 - (1.0 - inner_radius) * (point_angle - 0.5) * 2.0
                };
                let t = radius / star_radius;
                if reverse { 1.0 - t } else { t }
            });
        }
        15 => {
            // disco floor
            let band = 0.08 + 0.12 * frand(rng);
            let tile_size = 8 + (irand(rng) % 25);
            let beat_sync = 0.5 + frand(rng) * 1.5;
            let diagonal_pattern = irand(rng) % 2 == 0;
            let color_cycling = irand(rng) % 2 == 0;
            let speed_factor = 0.5 + frand(rng) * 2.0;
            clocks.disco += 1.0 / fps;
            let time = clocks.disco * speed_factor;
            let beat = ((time * 3.0).sin() * 0.5 + 0.5).powf(beat_sync);
            each!(band, |x, y| {
                let (fx, fy) = uxy(x, y);
                let tile_x = (fx * gxf / tile_size as f32) as i32;
                let tile_y = (fy * gyf / tile_size as f32) as i32;
                let pattern = if diagonal_pattern {
                    ((tile_x + tile_y) % 2) as f32 * 0.8 + 0.1
                } else {
                    f32::from(u8::from((tile_x % 2) == (tile_y % 2))) * 0.8 + 0.1
                };
                let anim =
                    (time * 2.0 + tile_x as f32 * 0.3 + tile_y as f32 * 0.7).sin() * 0.5 + 0.5;
                let mut t = (pattern * 0.7 + anim * 0.3) * beat;
                if color_cycling {
                    let hue = (time * 0.2 + tile_x as f32 * 0.1 + tile_y as f32 * 0.15) % 1.0;
                    t = (t + hue * 0.3) % 1.0;
                }
                t
            });
        }
        16 => {
            // fire / flame
            let band = 0.08 + 0.04 * frand(rng);
            let flame_speed = 0.7 + frand(rng) * 0.6;
            let seed1 = frand(rng) * 10.0;
            let seed2 = frand(rng) * 20.0;
            let seed3 = frand(rng) * 30.0;
            clocks.fire += 1.0 / fps;
            let time = clocks.fire;
            each!(band, |x, y| {
                let (fx, fy) = uxy(x, y);
                let random_flame = (fx * 15.0 + seed1 + time * 2.0).sin() * 0.4
                    + (fx * 30.0 + seed2 + time * 3.7).sin() * 0.2
                    + (fx * 45.0 + seed3 + time * 5.3).sin() * 0.1;
                let flame_shape = (1.0 - fy) * (0.3 + random_flame * 0.7);
                let flame_front = (time * flame_speed) % 1.5;
                let t = 1.0 - (fy - flame_front + flame_shape);
                if t < 0.0 {
                    0.0
                } else if t > 1.0 {
                    1.0
                } else {
                    t
                }
            });
        }
        17 => {
            // drain swirl
            let band = 0.05 + 0.15 * frand(rng);
            let swirl_intensity = 2.0 + frand(rng) * 3.0;
            let drain_speed = 0.5 + frand(rng) * 1.5;
            let clockwise = irand(rng) % 2 == 0;
            let center_pull = 0.7 + frand(rng) * 0.6;
            let invert = irand(rng) % 2 == 0;
            clocks.drain += 1.0 / fps;
            let time = clocks.drain * drain_speed;
            each!(band, |x, y| {
                let (fx, fy) = cxy(x, y);
                let radius = (fx * fx + fy * fy).sqrt() * 1.41421356;
                let angle = fy.atan2(fx);
                let mut swirl_factor = (1.0 - radius) * swirl_intensity;
                if clockwise {
                    swirl_factor = -swirl_factor;
                }
                let swirled_angle = angle + swirl_factor + time * 2.0;
                let t = radius * center_pull
                    + (1.0 - center_pull)
                        * (0.5 + 0.5 * (swirled_angle * 2.0 + radius * 5.0).sin());
                if invert { 1.0 - t } else { t }
            });
        }
        _ => {
            // smooth Julia set fractal
            let band = 0.08 + 0.12 * frand(rng);
            let inv_band = 1.0 / band;
            let julia_real = -0.8 + frand(rng) * 1.6;
            let julia_imag = -0.8 + frand(rng) * 1.6;
            let max_iterations = 20 + (irand(rng) % 20);
            let zoom = 0.7 + frand(rng) * 1.6;
            let rotation = frand(rng) * TAU;
            let edge_softness = 0.3 + frand(rng) * 0.5;
            let contrast = 0.7 + frand(rng) * 0.6;
            let (cos_rot, sin_rot) = (rotation.cos(), rotation.sin());
            let mut min_val = f32::MAX;
            let mut max_val = -f32::MAX;
            let mut values = vec![0.0f32; (gx + 1) * (gy + 1)];
            let mut n = 0;
            for y in 0..=gy {
                for x in 0..=gx {
                    let (fx, fy) = cxy(x, y);
                    let mut zx = (fx * cos_rot - fy * sin_rot) * zoom;
                    let mut zy = (fx * sin_rot + fy * cos_rot) * zoom;
                    let (cx, cy) = (julia_real, julia_imag);
                    let mut i = 0;
                    while i < max_iterations {
                        let tmp = zx * zx - zy * zy + cx;
                        zy = 2.0 * zx * zy + cy;
                        zx = tmp;
                        if zx * zx + zy * zy > 4.0 {
                            break;
                        }
                        i += 1;
                    }
                    let mut t = if i < max_iterations {
                        let log_zn = (zx * zx + zy * zy).ln() / 2.0;
                        let nu = (log_zn / 2.0f32.ln()).ln() / 2.0f32.ln();
                        ((i + 1) as f32 - nu) / max_iterations as f32
                    } else {
                        1.0
                    };
                    t = t.powf(contrast);
                    values[n] = t;
                    if t < min_val {
                        min_val = t;
                    }
                    if t > max_val {
                        max_val = t;
                    }
                    n += 1;
                }
            }
            let mut range = max_val - min_val;
            if range < 0.0001 {
                range = 1.0;
            }
            for (n, v) in values.iter().enumerate() {
                let mut t = (v - min_val) / range;
                t = t * t * (3.0 - 2.0 * t) * (1.0 - edge_softness) + t * edge_softness;
                vertinfo[n][0] = inv_band * (1.0 + band * 1.5);
                let c = -inv_band + inv_band * t * 1.1;
                // `max(-10, min(10, c))`
                let m = if 10.0 < c { 10.0 } else { c };
                vertinfo[n][1] = if -10.0 > m { -10.0 } else { m };
            }
        }
    }
    mixtype
}

/// `CPlugin::GenPlasma`: midpoint displacement over the `c` values of the mesh.
#[allow(clippy::too_many_arguments)]
fn gen_plasma(
    vi: &mut [[f32; 2]],
    gx: usize,
    aspect: (f32, f32),
    rng: &mut StreamRng,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    dt: f32,
) {
    let (ax, ay) = aspect;
    let w = gx + 1;
    let midx = (x0 + x1) / 2;
    let midy = (y0 + y1) / 2;
    let mut t00 = vi[y0 * w + x0][1];
    let mut t01 = vi[y0 * w + x1][1];
    let mut t10 = vi[y1 * w + x0][1];
    let mut t11 = vi[y1 * w + x1][1];

    if y1 - y0 >= 2 {
        if x0 == 0 {
            vi[midy * w + x0][1] = 0.5 * (t00 + t10) + (frand(rng) * 2.0 - 1.0) * dt * ay;
        }
        vi[midy * w + x1][1] = 0.5 * (t01 + t11) + (frand(rng) * 2.0 - 1.0) * dt * ay;
    }
    if x1 - x0 >= 2 {
        if y0 == 0 {
            vi[y0 * w + midx][1] = 0.5 * (t00 + t01) + (frand(rng) * 2.0 - 1.0) * dt * ax;
        }
        vi[y1 * w + midx][1] = 0.5 * (t10 + t11) + (frand(rng) * 2.0 - 1.0) * dt * ax;
    }

    if y1 - y0 >= 2 && x1 - x0 >= 2 {
        // do midpoint & recurse:
        t00 = vi[midy * w + x0][1];
        t01 = vi[midy * w + x1][1];
        t10 = vi[y0 * w + midx][1];
        t11 = vi[y1 * w + midx][1];
        vi[midy * w + midx][1] = 0.25 * (t10 + t11 + t00 + t01) + (frand(rng) * 2.0 - 1.0) * dt;

        gen_plasma(vi, gx, aspect, rng, x0, midx, y0, midy, dt * 0.5);
        gen_plasma(vi, gx, aspect, rng, midx, x1, y0, midy, dt * 0.5);
        gen_plasma(vi, gx, aspect, rng, x0, midx, midy, y1, dt * 0.5);
        gen_plasma(vi, gx, aspect, rng, midx, x1, midy, y1, dt * 0.5);
    }
}

// ---------------------------------------------------------------------------------------------
// the running blend
// ---------------------------------------------------------------------------------------------

/// `m_pOldState` and the blend bookkeeping of `m_pState` (`m_bBlending`, `m_fBlendStartTime`, ...) while a blend runs.
pub(super) struct BlendState {
    /// The outgoing preset: its contexts keep running every frame.
    pub old: Loaded,
    pub old_last: LastFrame,
    /// `m_fBlendStartTime`, `m_fBlendDuration`, `m_fBlendProgress` (all `float` like MD2's).
    pub start: f32,
    pub duration: f32,
    pub progress: f32,
    pub statics: BlendStatics,
    /// `m_vertinfo` as `RandomizeBlendPattern` left it, and the pattern kind that did.
    pub pattern: Vec<[f32; 2]>,
    pub pattern_kind: u32,
    /// `m_fPresetStartTime` and `m_fNextPresetTime` (set on the first frame after the load) for `progress`.
    pub preset_start: f64,
    pub next_preset: Option<f64>,
    /// MilkDrop 3 `progressMode` (`Normal` = MD2's ramp), `blending_progress` (the target of the held modes) and
    /// whether borders are suppressed (`.milk2` loaded). MD2 blends use `Normal`, `1.0`, `false`.
    pub mode: ProgressMode,
    pub target: f32,
    pub no_borders: bool,
}

/// `m_fBlendProgress = (GetTime() - m_fBlendStartTime) / m_fBlendDuration`, in `float` like MD2.
pub(super) fn blend_progress(time: f32, start: f32, duration: f32) -> f32 {
    (time - start) / duration
}

impl BlendState {
    /// `RenderFrame`'s blend update: `m_fBlendProgress = (GetTime() - start) / duration`, the blend ends once it is
    /// above 1. Returns whether it is still running. The MilkDrop 3 modes follow `progressMode` (see
    /// [`ProgressMode`]); only `Normal` ever ends here, the others end through the engine's double-preset events.
    pub fn update(&mut self, time: f32) -> bool {
        let ramp = blend_progress(time, self.start, self.duration);
        match self.mode {
            ProgressMode::Normal => {
                self.progress = ramp;
                self.progress <= 1.0
            }
            ProgressMode::Frozen => {
                self.progress = self.target;
                true
            }
            ProgressMode::SoftInSecond => {
                if self.progress < self.target {
                    self.progress = ramp.min(self.target);
                }
                true
            }
            ProgressMode::SoftInFirst | ProgressMode::SoftOut => {
                self.progress = ramp;
                true
            }
        }
    }

    /// `progress` of the preset variable: `(GetTime() - m_fPresetStartTime) / (m_fNextPresetTime - m_fPresetStartTime)`.
    pub fn preset_progress(&mut self, time: f64, preset_duration: f64) -> f32 {
        let next = *self.next_preset.get_or_insert(time + preset_duration);
        (time as f32 - self.preset_start as f32) / (next as f32 - self.preset_start as f32)
    }
}

/// Everything of one blend frame: both states' per-frame equations, the blended variables, both per-vertex passes
/// blended into one warp mesh, shapes / waves of both states with their `alpha_mult`, the second wave mode, and the
/// old state's shader-pass inputs. `new` is `m_pState`; the result is `new`'s [`FrameInput`] with
/// `md2_blend` set.
pub(super) fn build_blend_frame(
    new: &mut Loaded,
    new_last: &mut LastFrame,
    m: &mut BlendState,
    cx: &FrameCtx,
    forced: Option<&ForcedFrame>,
) -> FrameInput {
    let time = cx.clock.time as f32;
    let g = cx.g;
    let cfg: &EngineConfig = cx.cfg;
    let env = frame_env(cx);

    // ---- RunPerFrameEquations(code): new state first, then the old one ----------------------------------
    let code = shader_code(m.old.has_warp, m.old.has_comp, new.has_warp, new.has_comp);
    let snap = snap_point(code);
    let base_new = new.floats.eval(&new.preset.base, time);
    let base_old = m.old.floats.eval(&m.old.preset.base, time);
    eval_per_frame(new, cx, &env, &base_new, forced, new_last);
    eval_per_frame(&mut m.old, cx, &env, &base_old, None, &mut m.old_last);
    let mix = f64::from(cosine_interp(m.progress));
    blend_vars(&mut new.pf, &m.old.pf, mix, f64::from(snap));

    // ---- ComputeGridAlphaValues: both per-vertex passes, then the UVs blended --------------------------
    // (the old state's UVs use the *new* state's warp animation terms, like MD2)
    let toff = warp_mesh::texel_offset_uv(cfg.texel_offset, cfg.width, cfg.height);
    let anim = warp_anim(cx, &base_new);
    let (_vv_new, uv_new) = eval_vertices(new, g, &anim, toff);
    let (_, uv_old) = eval_vertices(&mut m.old, g, &anim, toff);
    let (uv, vert_alpha) = blend_mesh(&uv_new, &uv_old, &m.pattern, m.progress);

    let mut fi = frame_head(new, cx, &base_new, Vec::new(), uv, anim, false, Some(snap));
    // MilkDrop 3 (`DisableBox`): no ob / ib borders while a blend is running
    fi.borders.hidden |= cx.cfg.md3_disable_box && cx.cfg.compat.rules().md3_border_gate;

    // ShowToUser_NoShaders: the video echo flips its orientation / fades across the blend
    if base_new.video_echo_alpha > 0.01
        && m.statics.echo_alpha_old > 0.01
        && new.preset.base.video_echo_orientation != m.statics.echo_orient_old
    {
        if m.progress < snap {
            fi.post.echo_orient = m.statics.echo_orient_old;
            fi.post.echo_alpha *= 1.0 - 2.0 * cosine_interp(m.progress);
        } else {
            fi.post.echo_alpha *= 2.0 * cosine_interp(m.progress) - 1.0;
        }
    }

    // ---- DrawCustomShapes / DrawCustomWaves: the new state, then the old one, alpha_mult = progress / 1 - progress
    let (am_new, am_old) = (m.progress, 1.0 - m.progress);
    let mut shape_alpha = Vec::new();
    let mut wave_alpha = Vec::new();
    push_shapes(new, &env, &mut fi.shapes);
    shape_alpha.resize(fi.shapes.len(), am_new);
    push_shapes(&mut m.old, &env, &mut fi.shapes);
    shape_alpha.resize(fi.shapes.len(), am_old);
    push_waves(new, cx, &env, &mut fi.waves);
    wave_alpha.resize(fi.waves.len(), am_new);
    push_waves(&mut m.old, cx, &env, &mut fi.waves);
    wave_alpha.resize(fi.waves.len(), am_old);

    finish_frame(new, cx, forced, new_last, &mut fi);
    if m.no_borders {
        // MD3 never draws the outer / inner borders while a `.milk2` is loaded (`fromMilk2`)
        fi.borders.ob_rgba[3] = 0.0;
        fi.borders.ib_rgba[3] = 0.0;
    }

    // ---- the old state's shader-pass inputs: its own q / blur / random state; `m_rand_frame` is shared ----------
    let shared_rand_frame = fi
        .warp_random
        .as_ref()
        .or(fi.comp_random.as_ref())
        .map(|p| p.rand_frame);
    let (mut old_warp, mut old_comp): (Option<PassRandom>, Option<PassRandom>) = (None, None);
    if let Some(s) = m.old.warp_stream.as_mut() {
        let mut p = s.frame(time, false);
        if let Some(rf) = shared_rand_frame {
            p.rand_frame = rf;
        }
        if m.old.has_warp {
            old_warp = Some(p.clone());
        }
        if m.old.has_comp {
            old_comp = Some(p);
        }
    }
    let opf = &m.old.pf;
    let f32v = |v| opf.ctx.get(v) as f32;
    let mut q = [0.0f32; 32];
    for (i, q) in q.iter_mut().enumerate() {
        *q = opf.q(i) as f32;
    }
    let ext = match cx.md3 {
        Some(md3) => crate::md3_inputs::shader_ext(
            &cfg.compat.rules(),
            md3,
            opf,
            m.old.start_time,
            cx.clock.time,
            false,
        ),
        None => Default::default(),
    };
    fi.md2_blend = Some(lactea_render::Md2Blend {
        progress: m.progress,
        vert_alpha,
        old: Md2OldPass {
            q,
            ext,
            blur: BlurParams {
                min: [
                    f32v(opf.v.blur1_min),
                    f32v(opf.v.blur2_min),
                    f32v(opf.v.blur3_min),
                ],
                max: [
                    f32v(opf.v.blur1_max),
                    f32v(opf.v.blur2_max),
                    f32v(opf.v.blur3_max),
                ],
                edge_darken: f32v(opf.v.blur1_edge_darken),
            },
            warp_random: old_warp,
            comp_random: old_comp,
        },
        // `its = 2` when the wave mode changed: `new_wavemode != m_nOldWaveMode`
        old_wave_mode: (fi.main_wave.mode != m.statics.old_wave_mode)
            .then_some(m.statics.old_wave_mode),
        shape_alpha_mult: shape_alpha,
        wave_alpha_mult: wave_alpha,
    });
    fi
}

// ---------------------------------------------------------------------------------------------
// loading a preset while the engine runs MilkDrop 2 blends
// ---------------------------------------------------------------------------------------------

impl super::Engine {
    /// `CPlugin::LoadPreset(.., fBlendTime > 0)` + `LoadPresetTick` state 8: the running preset becomes `m_pOldState`
    /// (its shaders `m_OldShaders`), the new preset `m_pState` blending from it. One renderer, the feedback images
    /// continue. Loading during a blend replaces the old state by the current (half-blended) one and drops the
    /// previous old state and its shaders.
    pub(super) fn load_preset_md2_soft(
        &mut self,
        bytes: &[u8],
        name: &str,
        seconds: f64,
    ) -> Result<PresetReport, EngineError> {
        let launch = BlendLaunch::ordinary(self.cfg.compat.rules().md3_blend_patterns);
        self.load_preset_blend(bytes, name, seconds, launch)
    }

    /// [`Engine::load_preset_md2_soft`] with an explicit progress mode, pattern source and border rule (the MilkDrop 3
    /// double-preset legs). The new preset is built next to the running one; on error nothing changes.
    pub(super) fn load_preset_blend(
        &mut self,
        bytes: &[u8],
        name: &str,
        seconds: f64,
        launch: BlendLaunch,
    ) -> Result<PresetReport, EngineError> {
        let mut slot: Slot = self
            .active
            .take()
            .expect("a soft cut needs a running preset");
        // build the new preset's shaders next to the running ones, which stay safe in `prev`
        let prev = slot.renderer.take_shader_state();
        match self.build_loaded(bytes, name, &mut slot.renderer) {
            Err(e) => {
                slot.renderer.restore_shader_state(prev);
                self.active = Some(slot);
                Err(e)
            }
            Ok((new_loaded, report)) => {
                let mut old = std::mem::replace(&mut slot.loaded, new_loaded);
                let old_last = std::mem::take(&mut slot.last);
                let t = self.clock.now as f32;
                let dur = seconds as f32;
                let geom = slot.renderer.mesh_geometry();
                let aspect = (geom.aspect_x(), geom.aspect_y());
                let pattern_kind = self.fill_blend_pattern(&launch.pattern, aspect);
                let statics = start_blend_from(&mut slot.loaded, &mut old, t, dur);
                slot.renderer.set_old_shader_state(Some(prev));
                self.clock.start_preset();
                self.md2 = Some(self.new_blend_state(
                    old,
                    old_last,
                    statics,
                    (t, dur),
                    pattern_kind,
                    &launch,
                ));
                self.active = Some(slot);
                Ok(report)
            }
        }
    }

    /// The bookkeeping of a blend that starts now: progress 0, `m_vertinfo` as the pattern left it.
    pub(super) fn new_blend_state(
        &self,
        old: Loaded,
        old_last: LastFrame,
        statics: BlendStatics,
        (start, duration): (f32, f32),
        pattern_kind: u32,
        launch: &BlendLaunch,
    ) -> BlendState {
        BlendState {
            old,
            old_last,
            start,
            duration,
            progress: 0.0,
            statics,
            pattern: self.md2_vertinfo.clone(),
            pattern_kind,
            preset_start: self.clock.now,
            next_preset: None,
            mode: launch.mode,
            target: launch.target,
            no_borders: launch.no_borders,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lactea_eel::GlobalMemory;
    use std::sync::Arc;

    const G: [u32; 2] = [48, 36];

    fn aspect() -> (f32, f32) {
        (1.0, 360.0 / 640.0)
    }

    fn fresh(n: usize) -> Vec<[f32; 2]> {
        vec![[1.0, 0.0]; n]
    }

    fn n_verts() -> usize {
        ((G[0] + 1) * (G[1] + 1)) as usize
    }

    // ---- CBlendableFloat ------------------------------------------------------------------------

    #[test]
    fn blend_float_is_linear_from_the_old_value_and_ends_after_the_duration() {
        let mut f = BlendFloat::default();
        let mut from = BlendFloat::default();
        // blend from 2.0 to 10.0 over 4 s, starting at t = 1
        f.start_blend_from(&mut from, 2.0, 1.0, 4.0);
        assert_eq!(f.eval(10.0, 1.0), 2.0);
        assert_eq!(f.eval(10.0, 3.0), 6.0); // halfway
        assert_eq!(f.eval(10.0, 5.0), 10.0); // `fTime > start + dur` is strict: t = 5 is still mix = 1
        assert_eq!(f.eval(10.0, 5.5), 10.0);
        assert!(!f.blending, "the blend ended for good");
        assert_eq!(f.eval(10.0, 3.0), 10.0);
        // eval(-1) is always the unblended value, and does not touch the blend
        let mut g = BlendFloat::default();
        g.start_blend_from(&mut BlendFloat::default(), 0.0, 0.0, 10.0);
        assert_eq!(g.eval(7.0, -1.0), 7.0);
        assert!(g.blending);
    }

    #[test]
    fn blend_float_too_short_does_not_start_and_assignment_stops() {
        let mut f = BlendFloat::default();
        f.start_blend_from(&mut BlendFloat::default(), 1.0, 0.0, 0.0005);
        assert_eq!(f.eval(5.0, 0.0), 5.0);
        f.start_blend_from(&mut BlendFloat::default(), 1.0, 0.0, 2.0);
        assert_eq!(f.eval(5.0, 1.0), 3.0);
        f.stop();
        assert_eq!(f.eval(5.0, 1.0), 5.0);
        // time before the start stops it as well
        let mut h = BlendFloat::default();
        h.start_blend_from(&mut BlendFloat::default(), 1.0, 5.0, 2.0);
        assert_eq!(h.eval(9.0, 1.0), 9.0);
    }

    #[test]
    fn blending_from_a_blend_in_progress_uses_its_current_value() {
        // MD2 loads a preset during a blend: the new one blends from the half-blended old state
        let mut a = BlendFloat::default();
        a.start_blend_from(&mut BlendFloat::default(), 0.0, 0.0, 2.0); // 0 -> 8 over 2 s
        let mut b = BlendFloat::default();
        b.start_blend_from(&mut a, 8.0, 1.0, 1.0); // at t = 1 `a` is at 4: blend 4 -> 20 over 1 s
        assert_eq!(b.eval(20.0, 1.0), 4.0);
        assert_eq!(b.eval(20.0, 1.5), 12.0);
    }

    // ---- snap point & variable blend ---------------------------------------------------------------

    #[test]
    fn snap_points_follow_the_code_table() {
        // code bits: old warp 8, old comp 4, new warp 2, new comp 1
        assert_eq!(shader_code(true, true, true, true), 15);
        assert_eq!(shader_code(false, false, false, false), 0);
        assert_eq!(shader_code(false, true, true, false), 6);
        for c in [4, 6, 12, 14] {
            assert_eq!(snap_point(c), -0.01, "old comp only: {c}");
        }
        for c in [1, 3, 9, 11] {
            assert_eq!(snap_point(c), 1.01, "new comp only: {c}");
        }
        for c in [0, 2, 8, 10, 5, 7, 13, 15] {
            assert_eq!(snap_point(c), 0.5, "{c}");
        }
        // derived from the shader flags of the two presets
        assert_eq!(snap_point(shader_code(false, true, false, false)), -0.01);
        assert_eq!(snap_point(shader_code(false, false, false, true)), 1.01);
        assert_eq!(snap_point(shader_code(true, true, true, true)), 0.5);
    }

    fn pf(gm: &Arc<GlobalMemory>) -> PerFrame {
        PerFrame::new(gm)
    }

    #[test]
    fn variables_mix_by_the_cosine_progress_and_booleans_snap() {
        let gm = Arc::new(GlobalMemory::new());
        let (mut new, mut old) = (pf(&gm), pf(&gm));
        new.ctx.set(new.v.decay, 1.0);
        old.ctx.set(old.v.decay, 0.5);
        new.ctx.set(new.v.ob_r, 1.0);
        old.ctx.set(old.v.ob_r, 0.0);
        new.ctx.set(new.v.invert, 1.0);
        old.ctx.set(old.v.invert, 0.0);
        new.ctx.set(new.v.wave_mode, 3.0);
        old.ctx.set(old.v.wave_mode, 5.0);
        new.ctx.set(new.v.zoom, 2.0);
        old.ctx.set(old.v.zoom, 1.0);
        // mix = 0.25: before the 0.5 snap point the booleans are the old ones
        blend_vars(&mut new, &old, 0.25, 0.5);
        assert!((new.ctx.get(new.v.decay) - (0.25 * 1.0 + 0.75 * 0.5)).abs() < 1e-12);
        assert!((new.ctx.get(new.v.ob_r) - 0.25).abs() < 1e-12);
        assert_eq!(new.ctx.get(new.v.invert), 0.0);
        // `wave_mode` is exempt, pixel-motion variables are not blended at all
        assert_eq!(new.ctx.get(new.v.wave_mode), 3.0);
        assert_eq!(new.ctx.get(new.v.zoom), 2.0);

        // after the snap point the new booleans stay
        let (mut new, old2) = (pf(&gm), pf(&gm));
        new.ctx.set(new.v.invert, 1.0);
        blend_vars(&mut new, &old2, 0.75, 0.5);
        assert_eq!(new.ctx.get(new.v.invert), 1.0);

        // a snap point of 1.01 (new preset only has a comp shader) keeps the old booleans for the whole blend, -0.01
        // switches to the new ones at once
        let (mut new, mut old3) = (pf(&gm), pf(&gm));
        new.ctx.set(new.v.darken, 1.0);
        old3.ctx.set(old3.v.darken, 0.0);
        blend_vars(&mut new, &old3, 0.999, 1.01);
        assert_eq!(new.ctx.get(new.v.darken), 0.0);
        let (mut new, mut old4) = (pf(&gm), pf(&gm));
        new.ctx.set(new.v.darken, 1.0);
        old4.ctx.set(old4.v.darken, 0.0);
        blend_vars(&mut new, &old4, 0.0, -0.01);
        assert_eq!(new.ctx.get(new.v.darken), 1.0);
    }

    // ---- mix2 / mesh blend -------------------------------------------------------------------------

    #[test]
    fn vertex_mix_clamps_to_the_unit_interval() {
        assert_eq!(vertex_mix(1.0, 0.0, 0.3), 0.3);
        assert_eq!(vertex_mix(2.0, 0.5, 0.9), 1.0);
        assert_eq!(vertex_mix(2.0, -3.0, 0.1), 0.0);
        assert_eq!(vertex_mix(10.0, -2.0, 0.5), 1.0);
        assert!(vertex_mix(f32::NAN, 0.0, 0.5).is_nan());
    }

    #[test]
    fn blend_mesh_mixes_uvs_and_truncates_the_alpha_byte() {
        let new_uv = [[1.0, 0.0], [0.5, 0.5], [0.0, 1.0]];
        let old_uv = [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]];
        // uniform blend: a = 1, c = 0 -> mix2 = progress
        let pat = [[1.0, 0.0]; 3];
        let (uv, a) = blend_mesh(&new_uv, &old_uv, &pat, 0.25);
        assert_eq!(uv[0], [0.25, 0.0]);
        assert_eq!(uv[1], [0.5, 0.5]);
        assert_eq!(uv[2], [0.75, 1.0]);
        assert_eq!(a, vec![63, 63, 63]); // (DWORD)(0.25 * 255) = 63
        let (_, a) = blend_mesh(&new_uv, &old_uv, &pat, 1.0);
        assert_eq!(a, vec![255; 3]);
        let (uv, a) = blend_mesh(&new_uv, &old_uv, &pat, 0.0);
        assert_eq!((uv[0], a[0]), ([0.0, 0.0], 0), "mix2 = 0: the old UVs");
        // a wipe: per-vertex a / c
        let pat = [[4.0, -2.0], [4.0, -1.0], [4.0, 0.0]];
        let (_, a) = blend_mesh(&new_uv, &old_uv, &pat, 0.5);
        assert_eq!(a, vec![0, 255, 255]); // mix2 = 0, 1, 2 -> clamped to 0, 1, 1
    }

    // ---- RandomizeBlendPattern ------------------------------------------------------------------------

    #[test]
    fn uniform_pattern_is_a_1_c_0() {
        let mut vi = vec![[3.0, 4.0]; n_verts()];
        let mut rng = StreamRng::new(1, "md2_blend_pattern");
        let k = randomize_blend_pattern(
            &mut vi,
            G,
            aspect(),
            60.0,
            &mut rng,
            &mut PatternClocks::default(),
            Some(0),
        );
        assert_eq!(k, 0);
        assert!(vi.iter().all(|v| *v == [1.0, 0.0]));
    }

    #[test]
    fn directional_wipe_matches_the_formula() {
        let mut vi = fresh(n_verts());
        let mut rng = StreamRng::new(7, "md2_blend_pattern");
        let mut probe = rng.clone();
        let ang = frand(&mut probe) * 6.28;
        let band = 0.1 + 0.2 * frand(&mut probe);
        let k = randomize_blend_pattern(
            &mut vi,
            G,
            aspect(),
            60.0,
            &mut rng,
            &mut PatternClocks::default(),
            Some(1),
        );
        assert_eq!(k, 1);
        let (ax, ay) = aspect();
        for (x, y) in [
            (0usize, 0usize),
            (48, 0),
            (0, 36),
            (48, 36),
            (24, 18),
            (10, 30),
        ] {
            let fx = (x as f32 / 48.0) * ax;
            let fy = (y as f32 / 36.0) * ay;
            let mut t = (fx - 0.5) * ang.cos() + (fy - 0.5) * ang.sin() + 0.5;
            t = (t - 0.5) / 2.0f32.sqrt() + 0.5;
            let inv = 1.0 / band;
            let v = vi[y * 49 + x];
            assert_eq!(v[0], inv * (1.0 + band));
            assert_eq!(v[1], -inv + inv * t);
        }
    }

    #[test]
    fn radial_pattern_centre_and_corner() {
        let mut vi = fresh(n_verts());
        let mut rng = StreamRng::new(3, "md2_blend_pattern");
        let mut probe = rng.clone();
        let band = 0.02 + 0.14 * frand(&mut probe) + 0.34 * frand(&mut probe);
        let dir = ((irand(&mut probe) % 2) * 2 - 1) as f32;
        randomize_blend_pattern(
            &mut vi,
            G,
            aspect(),
            60.0,
            &mut rng,
            &mut PatternClocks::default(),
            Some(3),
        );
        let inv = 1.0 / band;
        let centre = vi[18 * 49 + 24];
        let corner = vi[0];
        let t_centre = if dir == -1.0 { 1.0 } else { 0.0 };
        assert_eq!(centre[1], -inv + inv * t_centre);
        // corner: distance sqrt(0.5^2 + (0.5*aspect)^2) * 1.41421
        let (ax, ay) = aspect();
        let d = ((0.5 * ax) * (0.5 * ax) + (0.5 * ay) * (0.5 * ay)).sqrt() * 1.41421;
        let t_corner = if dir == -1.0 { 1.0 - d } else { d };
        assert!(
            (corner[1] - (-inv + inv * t_corner)).abs() < 1e-3,
            "{} {}",
            corner[1],
            -inv + inv * t_corner
        );
    }

    #[test]
    fn plasma_is_normalised_to_the_band_range() {
        let mut vi = fresh(n_verts());
        let mut rng = StreamRng::new(5, "md2_blend_pattern");
        let mut probe = rng.clone();
        let band = 0.12 + 0.13 * frand(&mut probe);
        randomize_blend_pattern(
            &mut vi,
            G,
            aspect(),
            60.0,
            &mut rng,
            &mut PatternClocks::default(),
            Some(2),
        );
        let inv = 1.0 / band;
        let min = vi.iter().map(|v| v[1]).fold(f32::MAX, f32::min);
        let max = vi.iter().map(|v| v[1]).fold(f32::MIN, f32::max);
        assert!(
            (min + inv).abs() < 1e-3,
            "smallest t is 0 -> c = -inv_band: {min}"
        );
        assert!(max.abs() < 1e-3, "largest t is 1 -> c = 0: {max}");
        assert!(vi.iter().all(|v| v[0] == inv * (1.0 + band)));
    }

    #[test]
    fn every_pattern_fills_the_whole_mesh_with_finite_values_deterministically() {
        for kind in 0..=TRANSITION_LIMIT {
            let run = || {
                let mut vi = fresh(n_verts());
                let mut rng = StreamRng::new(11, "md2_blend_pattern");
                let mut clocks = PatternClocks::default();
                let k = randomize_blend_pattern(
                    &mut vi,
                    G,
                    aspect(),
                    60.0,
                    &mut rng,
                    &mut clocks,
                    Some(kind),
                );
                assert_eq!(k, kind);
                (vi, clocks)
            };
            let (a, clocks) = run();
            let (b, _) = run();
            assert_eq!(a, b, "kind {kind} is deterministic");
            assert!(
                a.iter().all(|v| v[0].is_finite() && v[1].is_finite()),
                "kind {kind}"
            );
            if kind != 0 {
                assert!(
                    a.iter().all(|v| v[0] > 0.0),
                    "kind {kind}: a = inv_band * (1 + band) > 0"
                );
            }
            // the animated ones advanced their `static` clock by 1/fps once
            let want = |on: bool| if on { 1.0 / 60.0 } else { 0.0 };
            assert_eq!(clocks.checker, want(kind == 9));
            assert_eq!(clocks.disco, want(kind == 15));
            assert_eq!(clocks.fire, want(kind == 16));
            assert_eq!(clocks.drain, want(kind == 17));
        }
    }

    #[test]
    fn unforced_kind_is_drawn_from_the_stream_in_range() {
        let mut seen = std::collections::BTreeSet::new();
        let mut rng = StreamRng::new(2, "md2_blend_pattern");
        let mut vi = fresh(n_verts());
        let mut clocks = PatternClocks::default();
        for _ in 0..200 {
            let k =
                randomize_blend_pattern(&mut vi, G, aspect(), 60.0, &mut rng, &mut clocks, None);
            assert!(k <= TRANSITION_LIMIT);
            seen.insert(k);
        }
        assert!(seen.len() >= 15, "all kinds appear: {seen:?}");
    }

    #[test]
    fn wipes_start_with_the_old_preset_and_end_with_the_new_one() {
        // at progress 0 every mix2 is <= 0 and at progress 1 every mix2 is >= 1 for the band patterns whose t is in [0, 1]
        for kind in [0u32, 1, 3, 6, 8, 11, 14] {
            let mut vi = fresh(n_verts());
            let mut rng = StreamRng::new(9, "md2_blend_pattern");
            randomize_blend_pattern(
                &mut vi,
                G,
                aspect(),
                60.0,
                &mut rng,
                &mut PatternClocks::default(),
                Some(kind),
            );
            let slack = 1e-4;
            let at0: Vec<f32> = vi.iter().map(|v| vertex_mix(v[0], v[1], 0.0)).collect();
            let at1: Vec<f32> = vi.iter().map(|v| vertex_mix(v[0], v[1], 1.0)).collect();
            if kind == 0 {
                assert!(at0.iter().all(|m| *m == 0.0) && at1.iter().all(|m| *m == 1.0));
                continue;
            }
            // the band patterns put t in a slightly wider range for some kinds: most vertices are fully old / new
            let old_frac = at0.iter().filter(|m| **m <= slack).count() as f32 / at0.len() as f32;
            let new_frac =
                at1.iter().filter(|m| **m >= 1.0 - slack).count() as f32 / at1.len() as f32;
            assert!(old_frac > 0.9, "kind {kind}: {old_frac}");
            assert!(new_frac > 0.9, "kind {kind}: {new_frac}");
        }
    }

    #[test]
    fn gen_plasma_visits_every_vertex() {
        let mut vi = vec![[1.0, f32::NAN]; n_verts()];
        let mut rng = StreamRng::new(4, "md2_blend_pattern");
        let (gx, gy) = (48usize, 36usize);
        vi[0][1] = 0.1;
        vi[gx][1] = 0.2;
        vi[gy * (gx + 1)][1] = 0.3;
        vi[gy * (gx + 1) + gx][1] = 0.4;
        gen_plasma(&mut vi, gx, aspect(), &mut rng, 0, gx, 0, gy, 0.25);
        assert!(vi.iter().all(|v| v[1].is_finite()), "every c written");
    }

    // ---- BlendState -----------------------------------------------------------------------------------

    #[test]
    fn progress_window_ends_when_it_exceeds_one() {
        // frame times n/60 (f32), 1 s blend loaded before frame 10 (start = 9/60): drawn on frames 10..=69, frame 70 ends it
        let running = |start: f32, dur: f32, t: f32| blend_progress(t, start, dur) <= 1.0;
        let start = 9.0f32 / 60.0;
        assert!(running(start, 1.0, 10.0 / 60.0));
        assert!(running(start, 1.0, 69.0 / 60.0));
        assert!(!running(start, 1.0, 70.0 / 60.0));
        // exactly 1.0 still blends (`> 1.0f` ends it)
        assert!(running(0.0, 2.0, 2.0));
        assert!(!running(0.0, 2.0, 2.001));
        assert_eq!(blend_progress(1.5, 1.0, 2.0), 0.25);
    }
}
