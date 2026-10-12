//! MilkDrop 3 blend patterns and double-preset playback (`Compat::MilkDrop3`, `docs/md3.md` "Double presets").
//!
//! * the 30 MD3 blend patterns: [`Md3Pattern`], [`Md3PatternInstance`] (the `.milk2` header's
//!   `blending_pattern` / `blending_direction` / `random_1..5` / `blending_progress`) and the pure generator
//!   [`md3_fill_vertinfo`] that turns an instance into the `(a, c)` mesh `mix2 = clamp(a * progress + c, 0, 1)` of
//!   MD2's per-vertex blend (`md2_blend.rs`). All randomness is drawn up front ([`md3_random_instance`]), so a
//!   generator is a pure function of `(instance, grid, aspect)`;
//! * `.milk2` playback on top of MD2's soft-cut machinery: the progress modes, soft in / out, hard cuts and the
//!   frozen blend ([`ProgressMode`], `impl Engine`).
//!
//! The float literals of the generators (`6.28`, `1.41421`, `3.14159274`, ...) are MD3's own and stay as written.
#![allow(
    clippy::approx_constant,
    clippy::excessive_precision,
    clippy::manual_clamp
)]

use super::md2_blend::{randomize_blend_pattern, start_blend_from};
use super::{LastFrame, Slot};
use crate::rng::StreamRng;
use crate::{EngineError, PresetReport, Transition};
use lactea_preset::milk2::{
    Milk2, Milk2Error, Milk2Header, is_double_preset, is_milk2_path, parse_milk2,
};
use lactea_preset::{Flavor, Preset};

// ---------------------------------------------------------------------------------------------
// the 30 patterns
// ---------------------------------------------------------------------------------------------

macro_rules! patterns {
    ($($variant:ident $name:literal $on:literal ($pa:literal, $pb:literal);)*) => {
        /// The 30 MD3 blend patterns, in MD3's order (`index()` is the discriminant).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[allow(missing_docs)]
        pub enum Md3Pattern {
            $($variant,)*
        }

        impl Md3Pattern {
            /// All patterns in index order.
            pub const ALL: [Md3Pattern; 30] = [$(Md3Pattern::$variant,)*];

            /// The pattern name as `blending_pattern=` and the `[transitions]` settings write it.
            pub fn name(self) -> &'static str {
                match self {
                    $(Md3Pattern::$variant => $name,)*
                }
            }

            /// Enabled for random soft cuts when `settings.ini` has no `[transitions]` entry for it.
            pub fn default_enabled(self) -> bool {
                match self {
                    $(Md3Pattern::$variant => $on,)*
                }
            }

            /// `(A, B)` of the draw of `blending_progress` in double-preset mode: the progress is
            /// `round((u * A + B) * 100) / 100` for a uniform `u`, i.e. in `[B, B + A]`.
            pub fn progress_range(self) -> (f32, f32) {
                match self {
                    $(Md3Pattern::$variant => ($pa, $pb),)*
                }
            }
        }
    };
}

patterns! {
    Zoom "zoom" true (0.7, 0.2);
    Side "side" true (0.3, 0.3);
    Plasma "plasma" true (0.4, 0.2);
    Plasma2 "plasma2" true (0.5, 0.1);
    Plasma3 "plasma3" true (0.3, 0.1);
    Cercle "cercle" true (0.3, 0.3);
    Square "square" false (0.3, 0.5);
    Snail "snail" true (0.3, 0.3);
    Snail2 "snail2" false (0.3, 0.3);
    Snail3 "snail3" false (0.3, 0.3);
    Triangle "triangle" true (0.4, 0.2);
    Donuts "donuts" false (0.3, 0.3);
    Corner "corner" true (0.4, 0.4);
    Patches "patches" true (0.4, 0.4);
    Checkerboard "checkerboard" false (0.4, 0.2);
    Bubbles "bubbles" false (0.4, 0.2);
    Stars "stars" false (0.5, 0.1);
    Stars2 "stars2" false (0.5, 0.2);
    Clock "clock" true (0.5, 0.1);
    Nuclear "nuclear" false (0.5, 0.1);
    Arrow "arrow" true (0.3, 0.15);
    Cisor "cisor" false (0.4, 0.2);
    Wave "wave" false (0.4, 0.2);
    Curtain "curtain" true (0.2, 0.1);
    Vertical "vertical" false (0.3, 0.3);
    Horizontal "horizontal" false (0.3, 0.3);
    LinesVertical "linesvertical" false (0.4, 0.2);
    LinesHorizontal "lineshorizontal" false (0.4, 0.2);
    Cross "cross" false (0.4, 0.5);
    Cross2 "cross2" false (0.25, 0.7);
}

impl Md3Pattern {
    /// `0..30`: the pattern's index in MD3's name table.
    pub fn index(self) -> usize {
        self as usize
    }

    /// The pattern with this index.
    pub fn from_index(i: usize) -> Option<Md3Pattern> {
        Self::ALL.get(i).copied()
    }

    /// The pattern called `name` (exact, case-sensitive, like MD3's name match).
    pub fn from_name(name: &str) -> Option<Md3Pattern> {
        Self::ALL.iter().copied().find(|p| p.name() == name)
    }

    /// Whether the pattern reads `blending_direction` (only `-1` is special): cercle, triangle, corner, patches.
    pub fn uses_direction(self) -> bool {
        matches!(
            self,
            Md3Pattern::Cercle | Md3Pattern::Triangle | Md3Pattern::Corner | Md3Pattern::Patches
        )
    }
}

/// The 13 patterns random soft cuts may pick when `settings.ini` has no `[transitions]` section.
pub fn default_enabled_patterns() -> Vec<Md3Pattern> {
    Md3Pattern::ALL
        .into_iter()
        .filter(|p| p.default_enabled())
        .collect()
}

/// Everything a generator needs: the values of the `.milk2` header keys.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Md3PatternInstance {
    /// `blending_pattern`.
    pub pattern: Md3Pattern,
    /// `blending_direction`: only `-1` is special.
    pub direction: i32,
    /// `random_1` .. `random_5`.
    pub r: [f32; 5],
    /// `blending_progress`: where a frozen blend stands (1.0 for ordinary soft cuts); used raw.
    pub progress: f32,
}

// ---------------------------------------------------------------------------------------------
// the generators (pure)
// ---------------------------------------------------------------------------------------------

/// `GenPlasma` with `FRAND` replaced by the five stored randoms: midpoint displacement of `h[y][x]` (row-major,
/// `gx + 1` wide). The edge midpoints use `r1..r4` by position, the centre `r5`, at every level.
#[allow(clippy::too_many_arguments)]
fn gen_plasma(
    h: &mut [f32],
    gx: usize,
    aspect: (f32, f32),
    r: &[f32; 5],
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    dt: f32,
) {
    let w = gx + 1;
    let (mx, my) = ((x0 + x1) / 2, (y0 + y1) / 2);
    let (t00, t01, t10, t11) = (
        h[y0 * w + x0],
        h[y0 * w + x1],
        h[y1 * w + x0],
        h[y1 * w + x1],
    );
    if y1 - y0 >= 2 {
        if x0 == 0 {
            h[my * w + x0] = 0.5 * (t00 + t10) + (r[0] * 2.0 - 1.0) * dt * aspect.1;
        }
        h[my * w + x1] = 0.5 * (t01 + t11) + (r[1] * 2.0 - 1.0) * dt * aspect.1;
    }
    if x1 - x0 >= 2 {
        if y0 == 0 {
            h[y0 * w + mx] = 0.5 * (t00 + t01) + (r[2] * 2.0 - 1.0) * dt * aspect.0;
        }
        h[y1 * w + mx] = 0.5 * (t10 + t11) + (r[3] * 2.0 - 1.0) * dt * aspect.0;
    }
    if y1 - y0 < 2 || x1 - x0 < 2 {
        return;
    }
    h[my * w + mx] = 0.25 * (h[y0 * w + mx] + h[y1 * w + mx] + h[my * w + x0] + h[my * w + x1])
        + (r[4] * 2.0 - 1.0) * dt;
    let half = dt * 0.5;
    gen_plasma(h, gx, aspect, r, x0, mx, y0, my, half);
    gen_plasma(h, gx, aspect, r, mx, x1, y0, my, half);
    gen_plasma(h, gx, aspect, r, x0, mx, my, y1, half);
    gen_plasma(h, gx, aspect, r, mx, x1, my, y1, half);
}

/// Fill the `(gx + 1) * (gy + 1)` mesh `vi` (`vi[y * (gx + 1) + x] = (a, c)`) with the pattern. `aspect` is
/// `(m_fAspectX, m_fAspectY)`. Pure: the same instance gives the same mesh, nothing is read from `vi`.
///
/// `horizontal` fills the mesh with transposed loops and a running index (MD3's own quirk, which scrambles the
/// pattern on grids that are not square); `plasma3` seeds its min / max with `0.5 * h(0, 0)`; `cross2` zeroes
/// the vertices that never blend.
pub fn md3_fill_vertinfo(
    inst: &Md3PatternInstance,
    gx: usize,
    gy: usize,
    aspect: (f32, f32),
    vi: &mut [[f32; 2]],
) {
    assert_eq!(
        vi.len(),
        (gx + 1) * (gy + 1),
        "vertinfo has one entry per mesh vertex"
    );
    let (ax, ay) = aspect;
    let (gxf, gyf) = (gx as f32, gy as f32);
    let [r1, r2, r3, r4, r5] = inst.r;
    let flip = inst.direction == -1;
    let sqrt2 = 2.0f32.sqrt();
    let w = gx + 1;

    // the common band `B` of most patterns
    let band = r1 * 0.14 + 0.02 + r2 * 0.34;

    // one entry per vertex from `f(X, Y)` (`X = x / gx`, `Y = y / gy`)
    let each = |vi: &mut [[f32; 2]], f: &dyn Fn(f32, f32) -> [f32; 2]| {
        for y in 0..=gy {
            for x in 0..=gx {
                vi[y * w + x] = f(x as f32 / gxf, y as f32 / gyf);
            }
        }
    };
    // `emit(t, k)`: `a = (B + k) / B`, `c = t / B - 1 / B`
    let banded = |vi: &mut [[f32; 2]], band: f32, k: f32, t: &dyn Fn(f32, f32) -> f32| {
        let ib = 1.0 / band;
        each(vi, &|x, y| [(band + k) * ib, t(x, y) * ib - ib]);
    };
    // `(dx, dy)` of the centred patterns
    let centred = |x: f32, y: f32| ((x - 0.5) * ax, (y - 0.5) * ay);
    let radius = |dx: f32, dy: f32| (dx * dx + dy * dy).sqrt();
    let flipped = |t: f32| if flip { 1.0 - t } else { t };
    // `side`, `vertical` and `horizontal`: the projection on a direction
    let wipe = |u: f32, v: f32, vx: f32, vy: f32| {
        (((u - 0.5) * vx + (v - 0.5) * vy + 0.5) - 0.5) / sqrt2 + 0.5
    };

    match inst.pattern {
        Md3Pattern::Zoom => vi.fill([1.0, 0.0]),
        Md3Pattern::Side => {
            let ang = r1 * 6.28;
            let (vx, vy) = (ang.cos(), ang.sin());
            let b = 0.1 + 0.2 * r2;
            let ib = 1.0 / b;
            each(vi, &|x, y| {
                let t = wipe(x * ax, y * ay, vx, vy);
                [(1.0 + b) * ib, t * ib - ib]
            });
        }
        Md3Pattern::Plasma | Md3Pattern::Plasma2 | Md3Pattern::Plasma3 => {
            let (b, dt) = match inst.pattern {
                Md3Pattern::Plasma => (0.12 + 0.13 * r1, 0.25),
                Md3Pattern::Plasma2 => (0.25 + 0.15 * r1, 0.25),
                _ => (0.25 + 0.15 * r1, 0.75),
            };
            let ib = 1.0 / b;
            let mut h = vec![0.0f32; w * (gy + 1)];
            h[0] = r2;
            h[gx] = r3;
            h[gy * w] = r4;
            h[gy * w + gx] = r5;
            gen_plasma(&mut h, gx, aspect, &inst.r, 0, gx, 0, gy, dt);
            // `plasma3` seeds both extremes with half of h(0, 0)
            let seed = if inst.pattern == Md3Pattern::Plasma3 {
                0.5 * h[0]
            } else {
                h[0]
            };
            let (mut mn, mut mx) = (seed, seed);
            for &v in &h {
                if mn > v {
                    mn = v;
                }
                if mx < v {
                    mx = v;
                }
            }
            for (v, out) in h.iter().zip(vi.iter_mut()) {
                let c = if inst.pattern == Md3Pattern::Plasma {
                    (v - mn) * (1.0 / (mx - mn)) * ib - ib
                } else {
                    ((v - mn) * 2.0 / (mx - mn)).sin() * ib - ib
                };
                *out = [(1.0 + b) / b, c];
            }
        }
        Md3Pattern::Cercle => banded(vi, band, 1.0, &|x, y| {
            let (dx, dy) = centred(x, y);
            flipped(radius(dx, dy) * 1.41421)
        }),
        Md3Pattern::Square => banded(vi, band, 1.0, &|x, y| {
            let (dx, dy) = centred(x, y);
            (dx.abs() * 1.41421).max(dy.abs() * 1.41421)
        }),
        Md3Pattern::Snail | Md3Pattern::Snail2 | Md3Pattern::Snail3 => {
            let pattern = inst.pattern;
            banded(vi, band, 2.0, &|x, y| {
                let (dx, dy) = centred(x, y);
                let (rho, th) = (radius(dx, dy), dy.atan2(dx));
                match pattern {
                    Md3Pattern::Snail => (th + 10.0 * rho - 0.2).cos() * rho,
                    Md3Pattern::Snail2 => (th + 25.0 * rho - 0.2).cos() * rho,
                    _ => (th + 50.0 * rho + 0.5).sin() * rho,
                }
            })
        }
        Md3Pattern::Triangle => banded(vi, band, 1.0, &|x, y| {
            let (dx, dy) = centred(x, y);
            flipped(dx.abs() + dy.abs())
        }),
        Md3Pattern::Donuts => banded(vi, band, 3.0, &|x, y| {
            let (dx, dy) = centred(x, y);
            (10.0 * radius(dx, dy) + 90.0).sin()
        }),
        Md3Pattern::Corner => banded(vi, band, 1.0, &|x, y| {
            let dx = ((x - 0.5) + 0.25) * ax;
            let dy = ((y - 0.5) + 0.35) * ay;
            let t = (radius(dx, dy) * 1.41421).min(1.0).max(0.0);
            flipped(t)
        }),
        Md3Pattern::Patches => banded(vi, band, 1.0, &|x, y| {
            let (dx, dy) = centred(x, y);
            flipped(((10.0 * dy).cos() + (10.0 * dx).sin()) * 0.25 + 0.5)
        }),
        Md3Pattern::Checkerboard => banded(vi, band, 3.0, &|x, y| {
            // the origin is the (1, 0) corner, not the centre
            let xs = (x - 1.0) * ax * 10.0 + 90.0;
            let ys = (y - 0.0) * ay * 10.0 + 90.0;
            xs.sin() * ys.cos()
        }),
        Md3Pattern::Bubbles => each(vi, &|x, y| {
            [
                7.0,
                2.0 * ((x - 10.0 * r1) * ax * 10.0 + 0.5 * r3).sin()
                    * (0.5 * r4 + (y - 0.5) * ay * 10.0).cos()
                    - 2.0,
            ]
        }),
        Md3Pattern::Stars | Md3Pattern::Stars2 | Md3Pattern::Clock | Md3Pattern::Nuclear => {
            let pattern = inst.pattern;
            banded(vi, band, 3.0, &|x, y| {
                let (dx, dy) = centred(x, y);
                let th = dy.atan2(dx);
                match pattern {
                    Md3Pattern::Stars => (5.0 * th + 90.0).sin(),
                    Md3Pattern::Stars2 => (10.0 * th + 90.0).sin(),
                    Md3Pattern::Clock => ((th + th) * 0.5).sin(),
                    _ => ((th + th) * 1.5).sin(),
                }
            })
        }
        Md3Pattern::Arrow => banded(vi, band, 1.0, &|x, y| {
            let (dx, dy) = centred(x, y);
            // |dx|, |dy| <= 0.5 * aspect stay below 1 for any window the engine uses
            let lim = |v: f32| v.clamp(-0.9999, 0.9999);
            let (ax_, ay_) = (lim(dx).atanh(), lim(dy).atanh());
            ((4.0 * ay_).cos() + ax_).sin() * 0.5 + 0.5
        }),
        Md3Pattern::Cisor => banded(vi, band, 3.0, &|x, y| {
            let th = ((y - 0.0) * ay).atan2((x - 1.0) * ax);
            (5.0 * th + 90.0).sin()
        }),
        Md3Pattern::Wave => banded(vi, band, 3.0, &|x, y| {
            let (dx, dy) = ((x - 1.0) * ax, (y - 0.0) * ay);
            (10.0 * radius(dx, dy) + 90.0).sin()
        }),
        Md3Pattern::Curtain => {
            // constants, unlike `side`
            let (vx, vy) = (6.28f32.cos(), 6.28f32.sin());
            let ten_thirds = 3.33333325f32;
            let a = f32::from_bits(0x408a_aaaa);
            each(vi, &|x, y| {
                let s = ((x * ax - 0.5) * vx + (y * ay - 0.5) * vy + 0.5) * 3.14159274;
                let h = s.sin() * 0.5 + 0.5;
                [a, h * ten_thirds - ten_thirds]
            });
        }
        Md3Pattern::Vertical => {
            let (vx, vy) = (6.28f32.cos(), 6.28f32.sin());
            each(vi, &|x, y| [6.0, 5.0 * wipe(x * ax, y * ay, vx, vy) - 5.0]);
        }
        Md3Pattern::Horizontal => {
            // the same value as `vertical`, but the loops are transposed (outer `x`, inner `y`) and the entry is
            // written at a running index: row-major reads of a non-square grid see scrambled values
            let (vx, vy) = (6.28f32.cos(), 6.28f32.sin());
            let mut k = 0;
            for i in 0..=gx {
                for j in 0..=gy {
                    let (u, v) = (i as f32 / gxf * ax, j as f32 / gyf * ay);
                    let t = ((((v - 0.5) * vy + (u - 0.5) * vx + 0.5) - 0.5) / sqrt2) + 0.5;
                    vi[k] = [6.0, 5.0 * t - 5.0];
                    k += 1;
                }
            }
        }
        Md3Pattern::LinesVertical => each(vi, &|x, _| {
            [7.0, 2.0 * ((x - 0.5) * ax * r1 * 60.0).sin() - 2.0]
        }),
        Md3Pattern::LinesHorizontal => each(vi, &|_, y| {
            [7.0, 2.0 * (r1 * 60.0 * (y - 0.5) * ay).sin() - 2.0]
        }),
        Md3Pattern::Cross | Md3Pattern::Cross2 => {
            let cut = inst.pattern == Md3Pattern::Cross2;
            let ib = 1.0 / band;
            each(vi, &|x, y| {
                let (dx, dy) = centred(x, y);
                let t = (dx.abs() + dx.abs()).min(dy.abs() + dy.abs());
                if cut && t >= 0.3 {
                    // never blends, for any progress
                    [0.0, 0.0]
                } else {
                    [(band + 1.0) * ib, t * ib - ib]
                }
            });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the random block
// ---------------------------------------------------------------------------------------------

/// A source of `rand()` values (`0..=32767`, MSVC's range): the engine's seeded stream, or an exact MSVC `rand()`
/// in tests.
pub trait Rand15 {
    /// The next `rand()`.
    fn rand15(&mut self) -> u32;
}

impl Rand15 for StreamRng {
    /// The top 15 bits of the stream's next 32-bit draw.
    fn rand15(&mut self) -> u32 {
        self.next32() >> 17
    }
}

/// `FRAND` of MD3: `(rand() % 7381) / 7380`.
fn frand7380(rng: &mut impl Rand15) -> f32 {
    (rng.rand15() % 7381) as f32 / 7380.0
}

/// The user locks of the random block (`forcePattern`, `forceProgress`, `dirLock`), all off by default.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Md3Locks {
    /// `forcePattern >= 0`: use this pattern instead of the random one.
    pub pattern: Option<Md3Pattern>,
    /// `forceProgress > 0`: use this progress instead of the drawn one.
    pub progress: Option<f32>,
    /// `dirLock != 0`: use this direction (no draw is consumed).
    pub direction: Option<i32>,
}

/// The progress of a double preset drawn for `pattern`: `round((rand() / 32767 * A + B) * 100) / 100`.
pub fn md3_draw_progress(pattern: Md3Pattern, rng: &mut impl Rand15) -> f32 {
    let (a, b) = pattern.progress_range();
    (((rng.rand15() as f32 / 32767.0) * a + b) * 100.0).round() / 100.0
}

/// The random block of `RandomizeBlendPattern`, once per preset change: draws, in this order, the pattern (a uniform
/// element of `enabled`, `{zoom}` if empty), the direction (`+-1`, unless locked), `r1..r5` and, in double-preset
/// mode, the pattern again if `auto_hardcut` (one of the first six patterns, ignoring `enabled`) and the progress.
/// Ordinary soft cuts have `progress = 1`.
pub fn md3_random_instance(
    rng: &mut impl Rand15,
    enabled: &[Md3Pattern],
    double_preset: bool,
    auto_hardcut: bool,
    locks: &Md3Locks,
) -> Md3PatternInstance {
    let zoom = [Md3Pattern::Zoom];
    let list = if enabled.is_empty() {
        &zoom[..]
    } else {
        enabled
    };
    let mut pattern = list[rng.rand15() as usize % list.len()];
    let direction = match locks.direction {
        Some(d) => d,
        None => 2 * (rng.rand15() & 1) as i32 - 1,
    };
    let mut r = [0.0f32; 5];
    for v in &mut r {
        *v = frand7380(rng);
    }
    if double_preset && auto_hardcut {
        pattern = Md3Pattern::ALL[rng.rand15() as usize % 6];
    }
    if let Some(p) = locks.pattern {
        pattern = p;
    }
    let mut progress = 1.0;
    if double_preset {
        progress = md3_draw_progress(pattern, rng);
    }
    if let Some(p) = locks.progress.filter(|&p| p > 0.0) {
        progress = p;
    }
    Md3PatternInstance {
        pattern,
        direction,
        r,
        progress,
    }
}

// ---------------------------------------------------------------------------------------------
// progress modes and how a blend is launched
// ---------------------------------------------------------------------------------------------

/// MD3's `progressMode` of the running blend (`m_pState->m_fBlendProgress` source).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProgressMode {
    /// 0: an ordinary soft cut, `(now - start) / duration`, over once it exceeds 1.
    Normal,
    /// 3: first leg of a soft transition *into* a double preset: the running preset blends to PRESET1 with the
    /// double preset's own pattern; when the ramp passes 1 PRESET2 is loaded on top of PRESET1.
    SoftInFirst,
    /// 4: second leg: PRESET1 to PRESET1 + PRESET2, the ramp is capped at `blending_progress` and held there.
    SoftInSecond,
    /// 2: the steady double preset: progress is `blending_progress` for as long as it is on screen.
    Frozen,
    /// 5: leaving a double preset: the ramp continues from `blending_progress` to 1, then the next preset loads.
    SoftOut,
}

/// Where the `(a, c)` mesh of a blend comes from.
#[derive(Clone, Copy)]
pub(super) enum LaunchPattern {
    /// BeatDrop's 19 patterns, drawn from the engine's seeded stream (`RandomizeBlendPattern`).
    Md2,
    /// An ordinary MD3 soft cut: a pattern drawn from the enabled list.
    Md3Random,
    /// A `.milk2` header. `None` is a name no generator knows: `m_vertinfo` stays as it was.
    Md3(Option<Md3PatternInstance>),
}

/// How a blend starts ([`Engine::load_preset_blend`]).
#[derive(Clone, Copy)]
pub(super) struct BlendLaunch {
    pub mode: ProgressMode,
    /// `blending_progress`: the value `Frozen` holds and `SoftInSecond` rises to.
    pub target: f32,
    pub pattern: LaunchPattern,
    /// A `.milk2` is loaded: no borders.
    pub no_borders: bool,
    /// The preset this blend loads honours its sprite block: ordinary presets always, a `.milk2` only for PRESET2 and
    /// only with the header's `sprite=1`.
    pub sprite: bool,
}

impl BlendLaunch {
    /// A plain soft cut: MD3's patterns under `CompatRules::md3_blend_patterns`, else MD2's.
    pub fn ordinary(md3_patterns: bool) -> Self {
        Self {
            mode: ProgressMode::Normal,
            target: 1.0,
            pattern: if md3_patterns {
                LaunchPattern::Md3Random
            } else {
                LaunchPattern::Md2
            },
            no_borders: false,
            sprite: true,
        }
    }
}

/// The next preset asked for while a double preset was on screen (`savedPath` / `savedBlendTime`).
struct Pending {
    bytes: Vec<u8>,
    name: String,
    seconds: f64,
}

/// The double preset on screen.
pub(super) struct DoubleState {
    name: String,
    milk2: Milk2,
    /// `None`: the file's pattern name is not one of the 30.
    inst: Option<Md3PatternInstance>,
    /// Set while leaving (`progressMode` 5).
    pending: Option<Pending>,
}

/// Where a double preset is in its life ([`DoublePresetInfo::mode`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleMode {
    /// A soft transition into the double preset, first leg: the running preset blends to PRESET1.
    SoftInPreset1,
    /// Second leg: PRESET2 is wiped in over PRESET1 up to `blending_progress`.
    SoftInPreset2,
    /// The steady frozen blend.
    Steady,
    /// Leaving: the blend runs on to PRESET2 alone before the next preset loads.
    SoftOut,
}

/// What the host can show about the double preset on screen ([`crate::Engine::double_preset_info`]).
#[derive(Clone, Debug, PartialEq)]
pub struct DoublePresetInfo {
    /// Name given to [`crate::Engine::load_preset`].
    pub name: String,
    /// `blending_pattern` as the file says it.
    pub pattern: String,
    /// The pattern, `None` if its name is not one of the 30 (the previous mesh is kept).
    pub known_pattern: Option<Md3Pattern>,
    /// `blending_progress`: where the blend is held.
    pub progress: f32,
    /// `blending_direction`.
    pub direction: i32,
    /// `random_1` .. `random_5`.
    pub random: [f32; 5],
    /// Names of the two halves (`NAME=` lines), `None` when the half has none.
    pub name1: Option<String>,
    /// See `name1`.
    pub name2: Option<String>,
    /// The file carries a sprite block (belongs to PRESET2).
    pub has_sprite: bool,
    /// The phase of the double preset.
    pub mode: DoubleMode,
    /// The blend's current progress (`m_fBlendProgress`).
    pub blend_progress: f32,
}

/// What the double-preset layer decided about a load request.
pub(super) enum Md3Dispatch {
    /// Handled (or refused).
    Done(Result<PresetReport, EngineError>),
    /// An ordinary load with this transition.
    Plain(Transition),
}

impl super::Engine {
    /// The double preset on screen, `None` for ordinary presets. Only `Compat::MilkDrop3` loads `.milk2` files.
    pub fn double_preset_info(&self) -> Option<DoublePresetInfo> {
        let (d, m) = (self.double.as_ref()?, self.md2.as_ref()?);
        let mode = match m.mode {
            ProgressMode::SoftInFirst => DoubleMode::SoftInPreset1,
            ProgressMode::SoftInSecond if m.progress < m.target => DoubleMode::SoftInPreset2,
            ProgressMode::SoftOut => DoubleMode::SoftOut,
            _ => DoubleMode::Steady,
        };
        Some(DoublePresetInfo {
            name: d.name.clone(),
            pattern: d.milk2.pattern.clone(),
            known_pattern: d.inst.map(|i| i.pattern),
            progress: d.milk2.progress,
            direction: d.milk2.direction,
            random: d.milk2.random,
            name1: d.milk2.name1.clone(),
            name2: d.milk2.name2.clone(),
            has_sprite: d.milk2.has_sprite,
            mode,
            blend_progress: m.progress,
        })
    }

    /// The patterns random soft cuts of `Compat::MilkDrop3` may pick (`[transitions]` of MD3's settings); an empty
    /// list means `zoom` only.
    pub fn set_md3_transition_patterns(&mut self, patterns: &[Md3Pattern]) {
        self.cfg.md3_transition_patterns = patterns.to_vec();
    }

    /// `RandomizeBlendPattern`: fill `m_vertinfo` for a blend that starts now and return the pattern kind.
    pub(super) fn fill_blend_pattern(&mut self, launch: &LaunchPattern, aspect: (f32, f32)) -> u32 {
        let (gx, gy) = (self.cfg.mesh[0], self.cfg.mesh[1]);
        let n = ((gx + 1) * (gy + 1)) as usize;
        if self.md2_vertinfo.len() != n {
            // `m_vertinfo` initial values: a = 1, c = 0
            self.md2_vertinfo = vec![[1.0, 0.0]; n];
        }
        let inst = match *launch {
            LaunchPattern::Md2 => {
                return randomize_blend_pattern(
                    &mut self.md2_vertinfo,
                    [gx, gy],
                    aspect,
                    self.cfg.fps as f32,
                    &mut self.md2_pattern_rng,
                    &mut self.md2_clocks,
                    self.md2_forced_pattern,
                );
            }
            LaunchPattern::Md3Random => {
                let locks = Md3Locks {
                    pattern: self
                        .md2_forced_pattern
                        .and_then(|k| Md3Pattern::from_index(k as usize)),
                    ..Default::default()
                };
                let inst = md3_random_instance(
                    &mut self.md3_pattern_rng,
                    &self.cfg.md3_transition_patterns,
                    false,
                    false,
                    &locks,
                );
                // a soft cut overwrites the globals a later `.milk2` without those keys falls back on
                self.md3_carry = Milk2Header {
                    pattern: inst.pattern.name().to_owned(),
                    progress: inst.progress,
                    direction: inst.direction,
                    random: inst.r,
                };
                inst
            }
            LaunchPattern::Md3(Some(inst)) => inst,
            LaunchPattern::Md3(None) => return 0,
        };
        md3_fill_vertinfo(
            &inst,
            gx as usize,
            gy as usize,
            aspect,
            &mut self.md2_vertinfo,
        );
        inst.pattern.index() as u32
    }

    /// A resize changes the aspect the pattern was generated for: a frozen double preset regenerates it from its
    /// header values (no re-roll).
    pub(super) fn md3_refresh_pattern(&mut self) {
        let (Some(d), Some(m), Some(act)) = (
            self.double.as_ref(),
            self.md2.as_mut(),
            self.active.as_ref(),
        ) else {
            return;
        };
        if let Some(inst) = d.inst {
            let geom = act.renderer.mesh_geometry();
            let (gx, gy) = (self.cfg.mesh[0] as usize, self.cfg.mesh[1] as usize);
            md3_fill_vertinfo(
                &inst,
                gx,
                gy,
                (geom.aspect_x(), geom.aspect_y()),
                &mut m.pattern,
            );
        }
    }

    /// `LoadPreset` for `Compat::MilkDrop3` and `.milk2`: recognises double presets, enters them (hard load or soft
    /// transition in) and defers the next preset while one is on screen (the soft-out of `progressMode` 5). Other
    /// requests continue as ordinary loads.
    pub(super) fn load_preset_md3(
        &mut self,
        bytes: &[u8],
        name: &str,
        transition: Transition,
    ) -> Md3Dispatch {
        if !self.cfg.compat.rules().md3_double_preset {
            if is_milk2_path(name) && is_double_preset(bytes) {
                return Md3Dispatch::Done(Err(EngineError::Unsupported(
                    "a .milk2 double preset needs Compat::MilkDrop3".to_owned(),
                )));
            }
            return Md3Dispatch::Plain(transition);
        }
        // a name that says `.milk` is a plain preset; otherwise (also for names without extension) the marker scan
        // decides, like MD3 does for `.milk2` files, and a failed scan means a plain preset
        let milk2 = if name.ends_with(".milk") {
            None
        } else {
            match parse_milk2(bytes, &self.md3_carry) {
                Ok(m) => Some(m),
                Err(Milk2Error::NotDouble) => None,
                Err(e) => return Md3Dispatch::Done(Err(e.into())),
            }
        };
        if self.double.is_none() {
            return match milk2 {
                None => Md3Dispatch::Plain(transition),
                Some(m) => Md3Dispatch::Done(self.enter_double(m, name, transition)),
            };
        }
        // leaving a double preset: `BlendMilk2 = 0` makes it a hard cut
        let transition = if self.cfg.blend_milk2 {
            transition
        } else {
            Transition::Cut
        };
        match transition {
            Transition::Soft { seconds } if seconds > 0.0 => {
                Md3Dispatch::Done(self.defer_leaving(bytes, name, seconds, milk2.as_ref()))
            }
            // the hard cut: `commit` drops the double preset once the new one is in
            _ => match milk2 {
                None => Md3Dispatch::Plain(Transition::Cut),
                Some(m) => Md3Dispatch::Done(self.enter_double(m, name, Transition::Cut)),
            },
        }
    }

    /// `LoadRandomPreset(T > 0)` while a double preset is on screen: remember the request, start the soft-out ramp
    /// (`progressMode` 5) and load it when the ramp ends. During the first soft-in leg the ramp just continues.
    /// A request while already leaving is ignored, like MD3. The report is the request's parsed header only: the
    /// preset itself is built when the ramp ends (a failure then goes to the switch-failed callback).
    fn defer_leaving(
        &mut self,
        bytes: &[u8],
        name: &str,
        seconds: f64,
        milk2: Option<&Milk2>,
    ) -> Result<PresetReport, EngineError> {
        let probe = milk2.map_or(bytes, |m| m.preset1.as_slice());
        let parsed = Preset::parse_with(probe, Flavor::Md2)?;
        let report = PresetReport {
            name: name.to_owned(),
            preset_version: parsed.milkdrop_preset_version,
            ..Default::default()
        };
        let now = self.clock.now as f32;
        let (Some(d), Some(m)) = (self.double.as_mut(), self.md2.as_mut()) else {
            return Ok(report);
        };
        if d.pending.is_some() {
            return Ok(report);
        }
        d.pending = Some(Pending {
            bytes: bytes.to_vec(),
            name: name.to_owned(),
            seconds,
        });
        if m.mode != ProgressMode::SoftInFirst {
            // the next frame's progress is the target, even if the second leg had not reached it yet
            m.start = now - m.target * m.duration;
        }
        m.mode = ProgressMode::SoftOut;
        Ok(report)
    }

    /// Load a `.milk2`: the hard load (nothing running, `BlendMilk2` off, a cut) or the soft transition in.
    fn enter_double(
        &mut self,
        milk2: Milk2,
        name: &str,
        transition: Transition,
    ) -> Result<PresetReport, EngineError> {
        let inst = Md3Pattern::from_name(&milk2.pattern).map(|pattern| Md3PatternInstance {
            pattern,
            direction: milk2.direction,
            r: milk2.random,
            progress: milk2.progress,
        });
        // PRESET2 is only built when the first leg ends: refuse an unusable one now
        Preset::parse_with(&milk2.preset2, Flavor::Md2)?;
        let mut launch = BlendLaunch {
            mode: ProgressMode::Frozen,
            target: milk2.progress,
            pattern: LaunchPattern::Md3(inst),
            no_borders: true,
            // leg A loads PRESET1, which never carries the sprite; the hard load builds PRESET2 with it
            sprite: false,
        };
        let user = self.cfg.blend_time_user;
        let soft = matches!(transition, Transition::Soft { seconds } if seconds > 0.0)
            && self.active.is_some()
            && self.cfg.blend_milk2
            && user > 0.0;
        let report = if soft {
            // the duration of both legs is `BlendTimeUser`, the request's time only has to be positive
            launch.mode = ProgressMode::SoftInFirst;
            self.load_preset_blend(&milk2.preset1, name, user, launch)?
        } else {
            self.load_double_hard(&milk2, name, launch)?
        };
        self.md3_carry = milk2.header();
        self.double = Some(DoubleState {
            name: name.to_owned(),
            milk2,
            inst,
            pending: None,
        });
        Ok(report)
    }

    /// The hard load: PRESET1 and PRESET2 are built into one renderer (PRESET1 as `m_pOldState`), the pattern is
    /// generated and the frozen blend is on screen from the first frame. MD3 shows PRESET1 alone for about seven
    /// frames first (its staged load); this port does not.
    fn load_double_hard(
        &mut self,
        milk2: &Milk2,
        name: &str,
        launch: BlendLaunch,
    ) -> Result<PresetReport, EngineError> {
        let mut renderer = match self.spare.take() {
            Some(r) => r,
            None => self
                .active
                .as_ref()
                .expect("engine always owns a renderer")
                .renderer
                .sibling(),
        };
        renderer.reset_images();
        let (mut old, rep1) = match self.build_loaded(&milk2.preset1, name, &mut renderer, false) {
            Ok(v) => v,
            Err(e) => {
                self.spare = Some(renderer);
                return Err(e);
            }
        };
        let prev = renderer.take_shader_state();
        let (mut new, mut report) =
            match self.build_loaded(&milk2.preset2, name, &mut renderer, milk2.has_sprite) {
                Ok(v) => v,
                Err(e) => {
                    self.spare = Some(renderer);
                    return Err(e);
                }
            };
        let t = self.clock.now as f32;
        let geom = renderer.mesh_geometry();
        let aspect = (geom.aspect_x(), geom.aspect_y());
        let kind = self.fill_blend_pattern(&launch.pattern, aspect);
        // `m_fLoadingPresetBlendTime := 1.0` for the base values' slide
        let statics = start_blend_from(&mut new, &mut old, t, 1.0);
        renderer.set_old_shader_state(Some(prev));
        self.commit(
            Slot {
                renderer,
                loaded: new,
                last: LastFrame::default(),
            },
            Transition::Cut,
        );
        self.md2 =
            Some(self.new_blend_state(old, LastFrame::default(), statics, (t, 1.0), kind, &launch));
        report.warnings = rep1
            .warnings
            .into_iter()
            .map(|w| format!("PRESET1: {w}"))
            .chain(report.warnings.into_iter().map(|w| format!("PRESET2: {w}")))
            .collect();
        report.shader_fallbacks.extend(rep1.shader_fallbacks);
        report.unresolved_textures.extend(rep1.unresolved_textures);
        report.resolved_textures.extend(rep1.resolved_textures);
        Ok(report)
    }

    /// The events of `progressMode` 3 and 5 after the frame's progress update; `true` when the blend state was
    /// replaced (the caller refreshes its preset progress).
    pub(super) fn double_tick(&mut self) -> bool {
        let (mode, progress) = match (&self.double, &self.md2) {
            (Some(_), Some(m)) => (m.mode, m.progress),
            _ => return false,
        };
        match mode {
            // leg A ended: PRESET2 is loaded on top of PRESET1, progress restarts at 0
            ProgressMode::SoftInFirst if progress > 1.0 => {
                let d = self.double.as_ref().expect("checked above");
                let (bytes, name, inst) = (d.milk2.preset2.clone(), d.name.clone(), d.inst);
                let launch = BlendLaunch {
                    mode: ProgressMode::SoftInSecond,
                    target: d.milk2.progress,
                    pattern: LaunchPattern::Md3(inst),
                    no_borders: true,
                    sprite: d.milk2.has_sprite,
                };
                let user = self.cfg.blend_time_user;
                if let Err(e) = self.load_preset_blend(&bytes, &name, user, launch) {
                    // PRESET1 stays alone on screen
                    self.on_switch_failed
                        .fire(&crate::callbacks::SwitchFailure {
                            name,
                            message: e.to_string(),
                        });
                    self.end_double();
                }
                true
            }
            // the ramp reached 1: PRESET2 is on screen alone, the saved request loads (`savedBlendTime`, 0 for
            // `BlendMilk2 = 0`)
            ProgressMode::SoftOut if progress >= 1.0 => {
                let pending = self.double.as_mut().and_then(|d| d.pending.take());
                self.end_double();
                if let Some(p) = pending {
                    let t = if self.cfg.blend_milk2 && p.seconds > 0.0 {
                        Transition::Soft { seconds: p.seconds }
                    } else {
                        Transition::Cut
                    };
                    // a failure reaches the switch-failed callback; the engine stays on PRESET2
                    let _ = self.load_preset(&p.bytes, &p.name, t);
                }
                true
            }
            _ => false,
        }
    }

    /// The double preset is over and the (new) state stays alone: the blend ends, old shaders are released.
    fn end_double(&mut self) {
        self.double = None;
        self.md2 = None;
        if let Some(a) = self.active.as_mut() {
            a.renderer.set_old_shader_state(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSVC UCRT `rand()` after `srand(seed)`.
    struct Msvc(u32);
    impl Rand15 for Msvc {
        fn rand15(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(214013).wrapping_add(2531011);
            (self.0 >> 16) & 0x7fff
        }
    }

    const GX: usize = 4;
    const GY: usize = 2;
    const ASPECT: (f32, f32) = (1.0, 0.5625);
    const R: [f32; 5] = [0.25, 0.5, 0.75, 0.125, 0.875];
    /// Vertices the table lists: v0 = (0, 0), v3 = (3, 0), v7 = (2, 1), v14 = (4, 2).
    const PICK: [usize; 4] = [0, 3, 7, 14];

    /// `(pattern index, direction, [(a, c) at v0, v3, v7, v14])` of the reference table (4x2 mesh, aspect
    /// `(1, 0.5625)`, `r = [0.25, 0.5, 0.75, 0.125, 0.875]`).
    #[rustfmt::skip]
    #[allow(clippy::type_complexity)]
    const TABLE: &[(usize, i32, [(f32, f32); 4])] = &[
        (0, 1, [(1.0000, 0.0000), (1.0000, 0.0000), (1.0000, 0.0000), (1.0000, 0.0000)]),
        (1, 1, [(6.0000, -4.2692), (6.0000, -4.2671), (6.0000, -3.2734), (6.0000, -2.2776)]),
        (2, 1, [(7.5574, -3.2787), (7.5574, -0.5464), (7.5574, -1.3832), (7.5574, 0.0000)]),
        (3, 1, [(4.4783, -0.5514), (4.4783, -0.1192), (4.4783, -0.0001), (4.4783, -0.3155)]),
        (4, 1, [(4.4783, -0.3137), (4.4783, -0.3155), (4.4783, -0.2387), (4.4783, -0.0357)]),
        (5, 1, [(5.4444, -0.8387), (5.4444, -2.0793), (5.4444, -4.4444), (5.4444, -0.8387)]),
        (5, -1, [(5.4444, -3.6058), (5.4444, -2.3652), (5.4444, 0.0000), (5.4444, -3.6058)]),
        (6, 1, [(5.4444, -1.3018), (5.4444, -2.6767), (5.4444, -4.4444), (5.4444, -1.3018)]),
        (7, 1, [(9.8889, -6.9246), (9.8889, -5.9697), (9.8889, -4.4444), (9.8889, -1.9643)]),
        (8, 1, [(9.8889, -3.1841), (9.8889, -5.2600), (9.8889, -4.4444), (9.8889, -5.7048)]),
        (9, 1, [(9.8889, -1.9231), (9.8889, -5.0628), (9.8889, -4.4444), (9.8889, -6.9658)]),
        (10, 1, [(5.4444, -0.9722), (5.4444, -2.0833), (5.4444, -4.4444), (5.4444, -0.9722)]),
        (10, -1, [(5.4444, -3.4722), (5.4444, -2.3611), (5.4444, 0.0000), (5.4444, -3.4722)]),
        (11, 1, [(14.3333, -0.0149), (14.3333, -6.5156), (14.3333, -0.4711), (14.3333, -0.0149)]),
        (12, 1, [(5.4444, -2.7860), (5.4444, -1.2573), (5.4444, -2.4444), (5.4444, 0.0000)]),
        (12, -1, [(5.4444, -1.6584), (5.4444, -3.1871), (5.4444, -2.0001), (5.4444, -4.4444)]),
        (13, 1, [(5.4444, -2.2082), (5.4444, -2.6087), (5.4444, -1.1111), (5.4444, -4.3392)]),
        (13, -1, [(5.4444, -2.2362), (5.4444, -1.8357), (5.4444, -3.3333), (5.4444, -0.1053)]),
        (14, 1, [(14.3333, -2.4652), (14.3333, -3.5522), (14.3333, -4.5502), (14.3333, -3.6801)]),
        (15, 1, [(7.0000, -2.8988), (7.0000, -3.8268), (7.0000, -3.3973), (7.0000, -0.2957)]),
        (16, 1, [(14.3333, -0.0294), (14.3333, -8.0757), (14.3333, -0.4711), (14.3333, -8.8595)]),
        (17, 1, [(14.3333, -1.0300), (14.3333, -4.9895), (14.3333, -0.4711), (14.3333, -1.0300)]),
        (18, 1, [(14.3333, -6.6234), (14.3333, -7.7663), (14.3333, -4.4444), (14.3333, -2.2655)]),
        (19, 1, [(14.3333, -8.8864), (14.3333, -6.9874), (14.3333, -4.4444), (14.3333, -0.0025)]),
        (20, 1, [(5.4444, -2.5465), (5.4444, -0.8628), (5.4444, -0.3523), (5.4444, -0.4118)]),
        (21, 1, [(14.3333, -8.4178), (14.3333, -8.4178), (14.3333, -2.2109), (14.3333, -6.4359)]),
        (22, 1, [(14.3333, -6.6950), (14.3333, -8.8195), (14.3333, -0.0149), (14.3333, -0.0830)]),
        (23, 1, [(4.3333, -1.6583), (4.3333, -0.4941), (4.3333, -0.0000), (4.3333, -1.6656)]),
        (24, 1, [(6.0000, -4.2621), (6.0000, -1.6105), (6.0000, -2.4975), (6.0000, -0.7329)]),
        (25, 1, [(6.0000, -4.2621), (6.0000, -3.3782), (6.0000, -2.4975), (6.0000, -0.7329)]),
        (26, 1, [(7.0000, -3.8760), (7.0000, -3.1431), (7.0000, -2.0000), (7.0000, -0.1240)]),
        (27, 1, [(7.0000, -0.2388), (7.0000, -0.2388), (7.0000, -2.0000), (7.0000, -3.7612)]),
        (28, 1, [(5.4444, -1.9444), (5.4444, -2.2222), (5.4444, -4.4444), (5.4444, -1.9444)]),
        (29, 1, [(0.0000, 0.0000), (0.0000, 0.0000), (5.4444, -4.4444), (0.0000, 0.0000)]),
    ];

    fn fill(
        pattern: Md3Pattern,
        direction: i32,
        r: [f32; 5],
        gx: usize,
        gy: usize,
    ) -> Vec<[f32; 2]> {
        let mut vi = vec![[f32::NAN; 2]; (gx + 1) * (gy + 1)];
        md3_fill_vertinfo(
            &Md3PatternInstance {
                pattern,
                direction,
                r,
                progress: 0.5,
            },
            gx,
            gy,
            ASPECT,
            &mut vi,
        );
        vi
    }

    #[test]
    fn names_indices_and_defaults() {
        assert_eq!(Md3Pattern::ALL.len(), 30);
        for (i, p) in Md3Pattern::ALL.iter().enumerate() {
            assert_eq!(p.index(), i);
            assert_eq!(Md3Pattern::from_index(i), Some(*p));
            assert_eq!(Md3Pattern::from_name(p.name()), Some(*p));
        }
        assert_eq!(Md3Pattern::from_index(30), None);
        assert_eq!(
            Md3Pattern::from_name("Zoom"),
            None,
            "names are case-sensitive"
        );
        assert_eq!(Md3Pattern::from_name("cercle "), None);
        assert_eq!(Md3Pattern::Wave.index(), 22);
        assert_eq!(Md3Pattern::Cross2.name(), "cross2");
        let on: Vec<&str> = default_enabled_patterns()
            .iter()
            .map(|p| p.name())
            .collect();
        assert_eq!(
            on,
            [
                "zoom", "side", "plasma", "plasma2", "plasma3", "cercle", "snail", "triangle",
                "corner", "patches", "clock", "arrow", "curtain"
            ]
        );
        let dir: Vec<&str> = Md3Pattern::ALL
            .iter()
            .filter(|p| p.uses_direction())
            .map(|p| p.name())
            .collect();
        assert_eq!(dir, ["cercle", "triangle", "corner", "patches"]);
    }

    /// The reference table of every pattern (both directions where the direction matters), 1e-3.
    #[test]
    fn all_thirty_patterns_match_the_reference_table() {
        let mut seen = [false; 30];
        for (idx, dir, cells) in TABLE {
            let p = Md3Pattern::from_index(*idx).unwrap();
            seen[*idx] = true;
            let vi = fill(p, *dir, R, GX, GY);
            for (k, (a, c)) in PICK.iter().zip(cells) {
                let [va, vc] = vi[*k];
                assert!(
                    (va - a).abs() <= 1e-3 && (vc - c).abs() <= 1e-3,
                    "{} dir {dir} v{k}: got ({va:.4}, {vc:.4}), want ({a:.4}, {c:.4})",
                    p.name()
                );
            }
        }
        assert!(seen.iter().all(|&s| s), "every pattern has a row");
    }

    #[test]
    fn generators_are_pure() {
        // the old contents of the mesh never matter, only the instance does
        for p in Md3Pattern::ALL {
            let a = fill(p, -1, [0.3, 0.6, 0.2, 0.9, 0.4], 8, 6);
            let mut b = vec![[123.0, -77.0]; 9 * 7];
            md3_fill_vertinfo(
                &Md3PatternInstance {
                    pattern: p,
                    direction: -1,
                    r: [0.3, 0.6, 0.2, 0.9, 0.4],
                    progress: 0.1,
                },
                8,
                6,
                ASPECT,
                &mut b,
            );
            assert_eq!(a, b, "{}", p.name());
            assert!(
                a.iter().all(|v| v[0].is_finite() && v[1].is_finite()),
                "{} is finite",
                p.name()
            );
        }
    }

    #[test]
    fn direction_matters_for_four_patterns_only() {
        for p in Md3Pattern::ALL {
            let (up, down, other) = (
                fill(p, 1, R, GX, GY),
                fill(p, -1, R, GX, GY),
                fill(p, 7, R, GX, GY),
            );
            assert_eq!(up, other, "{}: only -1 is special", p.name());
            assert_eq!(p.uses_direction(), up != down, "{}", p.name());
        }
        // `t -> 1 - t`: the centre vertex of cercle is the first region where the second preset appears
        let c = fill(Md3Pattern::Cercle, -1, R, GX, GY)[7];
        assert!(
            c[1].abs() < 1e-6,
            "centre: c = 0 with direction -1, got {}",
            c[1]
        );
    }

    #[test]
    fn horizontal_fills_the_mesh_transposed() {
        // grid 4x2: entry k belongs to the transposed loop nest (outer i in 0..=4, inner j in 0..=2, k = i*3 + j)
        let h = fill(Md3Pattern::Horizontal, 1, R, GX, GY);
        let v = fill(Md3Pattern::Vertical, 1, R, GX, GY);
        assert_eq!(h[0], v[0]);
        assert_ne!(h[3], v[3], "k = 3 is (i = 1, j = 0), not (x = 3, y = 0)");
        let (vx, vy) = (6.28f32.cos(), 6.28f32.sin());
        for i in 0..=GX {
            for j in 0..=GY {
                let (u, w) = (
                    i as f32 / GX as f32 * ASPECT.0,
                    j as f32 / GY as f32 * ASPECT.1,
                );
                let t = (((w - 0.5) * vy + (u - 0.5) * vx + 0.5) - 0.5) / 2.0f32.sqrt() + 0.5;
                let got = h[i * (GY + 1) + j];
                assert!((got[1] - (5.0 * t - 5.0)).abs() < 1e-5 && got[0] == 6.0);
            }
        }
        // on a square grid the transposition is a plain row/column swap of the same value function
        let sq = fill(Md3Pattern::Horizontal, 1, R, 4, 4);
        assert!(sq.iter().all(|e| e[0] == 6.0));
    }

    /// With every random 0.5 the displacement vanishes and the plasma field is flat (`h = 0.5`). `plasma2` then has
    /// `mn == mx` (0 / 0); `plasma3` seeds both extremes with `0.5 * h(0, 0) = 0.25`, so its range is 0.25..0.5
    /// and every vertex gets `sin(2)`.
    #[test]
    fn plasma3_seeds_min_and_max_with_half_the_corner() {
        let r = [0.5; 5];
        let b = 0.25 + 0.15 * r[0];
        let ib = 1.0 / b;
        let want = 2.0f32.sin() * ib - ib;
        for e in fill(Md3Pattern::Plasma3, 1, r, GX, GY) {
            assert!((e[1] - want).abs() < 1e-4, "{} vs {want}", e[1]);
        }
        for e in fill(Md3Pattern::Plasma2, 1, r, GX, GY) {
            assert!(e[1].is_nan(), "plasma2 has no seed offset: 0 / 0");
        }
    }

    #[test]
    fn cross2_zeroes_the_vertices_that_never_blend() {
        let v = fill(Md3Pattern::Cross2, 1, R, GX, GY);
        assert_eq!(v[0], [0.0, 0.0]);
        assert!((v[7][0] - 5.4444).abs() < 1e-3 && (v[7][1] + 4.4444).abs() < 1e-3);
        // zeroed vertices keep mix 0 at any progress
        for p in [0.0f32, 0.5, 1.0, 3.0] {
            assert_eq!((v[0][0] * p + v[0][1]).clamp(0.0, 1.0), 0.0);
        }
    }

    #[test]
    fn mix_ramp_of_the_wipe_patterns_covers_zero_to_one() {
        // `c <= 0` and `a + c >= 1`: mix is 0 at progress 0 and 1 at progress 1 for every vertex of the patterns
        // that fully blend
        for p in Md3Pattern::ALL {
            if matches!(p, Md3Pattern::Cross2 | Md3Pattern::Horizontal) {
                continue;
            }
            for e in fill(p, 1, R, 16, 12) {
                let (m0, m1) = ((e[1]).clamp(0.0, 1.0), (e[0] + e[1]).clamp(0.0, 1.0));
                assert!(
                    m0 <= 0.0 + 1e-6 || p == Md3Pattern::Zoom,
                    "{} start {m0}",
                    p.name()
                );
                assert!(m1 >= 1.0 - 1e-3, "{} end {m1}", p.name());
            }
        }
    }

    #[test]
    fn random_block_follows_msvc_rand() {
        // MSVC srand(1): rand() = 41, 18467, 6334, 26500, 19169, 15724, 11478, 29358, ...
        let mut rng = Msvc(1);
        assert_eq!(
            [rng.rand15(), rng.rand15(), rng.rand15()],
            [41, 18467, 6334]
        );
        let enabled = default_enabled_patterns();
        let inst = md3_random_instance(&mut Msvc(1), &enabled, false, false, &Md3Locks::default());
        assert_eq!(inst.pattern, Md3Pattern::Plasma, "41 % 13 = 2");
        assert_eq!(inst.direction, 1, "18467 is odd");
        let want = [0.85827, 0.59038, 0.59715, 0.13035, 0.55515];
        for (g, w) in inst.r.iter().zip(want) {
            assert!((g - w).abs() < 1e-5, "{g} vs {w}");
        }
        assert_eq!(inst.progress, 1.0);
        // double-preset mode draws one more value: 29358 -> 0.56
        let d = md3_random_instance(&mut Msvc(1), &enabled, true, false, &Md3Locks::default());
        assert_eq!(d.pattern, Md3Pattern::Plasma);
        assert_eq!(d.progress, 0.56);
        // and the mesh of that instance: a = 5.3183 everywhere, mix at P = 0.56
        let mut vi = vec![[0.0; 2]; 15];
        md3_fill_vertinfo(&d, GX, GY, ASPECT, &mut vi);
        assert!(vi.iter().all(|e| (e[0] - 5.3183).abs() < 1e-3));
        let c = [-0.3912, -0.0247, -0.0123, 0.0];
        for (k, w) in c.iter().enumerate() {
            assert!((vi[k][1] - w).abs() < 1e-3, "c[{k}] = {} vs {w}", vi[k][1]);
        }
    }

    #[test]
    fn random_block_locks_and_lists() {
        let enabled = [Md3Pattern::Square, Md3Pattern::Wave];
        let locks = Md3Locks {
            pattern: Some(Md3Pattern::Cross),
            progress: Some(0.33),
            direction: Some(-1),
        };
        let inst = md3_random_instance(&mut Msvc(5), &enabled, true, false, &locks);
        assert_eq!(inst.pattern, Md3Pattern::Cross);
        assert_eq!(inst.direction, -1);
        assert_eq!(inst.progress, 0.33);
        // a locked direction consumes no draw: the five randoms are the next five values
        let mut a = Msvc(9);
        let locked = md3_random_instance(
            &mut a,
            &enabled,
            false,
            false,
            &Md3Locks {
                direction: Some(1),
                ..Default::default()
            },
        );
        let mut b = Msvc(9);
        let _ = b.rand15();
        let want: Vec<f32> = (0..5).map(|_| frand7380(&mut b)).collect();
        assert_eq!(locked.r.to_vec(), want);
        // empty list = {zoom}; the draw is still consumed
        let z = md3_random_instance(&mut Msvc(3), &[], false, false, &Md3Locks::default());
        assert_eq!(z.pattern, Md3Pattern::Zoom);
        // auto hard cut restricts double-preset patterns to the first six
        for seed in 0..64 {
            let h =
                md3_random_instance(&mut Msvc(seed), &enabled, true, true, &Md3Locks::default());
            assert!(h.pattern.index() < 6, "{}", h.pattern.name());
        }
        // progress draws stay in the documented range of every pattern
        for p in Md3Pattern::ALL {
            let (a, b) = p.progress_range();
            for seed in 0..32 {
                let v = md3_draw_progress(p, &mut Msvc(seed));
                assert!(v >= b - 0.006 && v <= b + a + 0.006, "{} {v}", p.name());
            }
        }
    }

    #[test]
    fn stream_rng_is_a_rand15_source() {
        let mut r = StreamRng::new(1, "md3_test");
        let mut s = StreamRng::new(1, "md3_test");
        for _ in 0..100 {
            let v = r.rand15();
            assert!(v <= 0x7fff);
            assert_eq!(v, s.next32() >> 17);
        }
    }

    /// Every import resets the global `FFTAttack` / `FFTDecay` to 0.5 / 0.7 and then reads its own keys, so after a
    /// double preset is loaded the globals are PRESET2's (PRESET1 is imported first), also when PRESET2 has no keys.
    #[test]
    fn double_preset_leaves_preset2s_fft_params() {
        use crate::{Compat, Engine, EngineConfig, Transition};
        let device_queue = {
            let instance = wgpu::Instance::default();
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()
                .and_then(|a| {
                    pollster::block_on(a.request_device(&wgpu::DeviceDescriptor::default())).ok()
                })
        };
        let Some((device, queue)) = device_queue else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let half = |keys: &str| {
            format!(
                "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=0\nPSVERSION_COMP=0\n[preset00]\n{keys}fDecay=1\n"
            )
        };
        let file = |p1: &str, p2: &str| {
            format!(
                "Created with MILKDROP 3.37 on 05/01/2026 at 12:34:56\nwww.milkdrop3.com\n\nblending_pattern=zoom\nblending_progress=0.5\nblending_direction=1\nrandom_1=0.1\nrandom_2=0.2\nrandom_3=0.3\nrandom_4=0.4\nrandom_5=0.5\n[PRESET1_BEGIN]\nNAME=a\n{p1}[PRESET1_END]\n\n[PRESET2_BEGIN]\nNAME=b\n{p2}[PRESET2_END]\n"
            )
        };
        let mut e = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: 32,
                height: 24,
                mesh: [8, 8],
                compat: Compat::MilkDrop3,
                ..Default::default()
            },
        );
        let p1 = half("FFTAttack=0.9\nFFTDecay=0.2\n");
        let p2 = half("FFTAttack=0.3\nFFTDecay=0.4\n");
        e.load_preset(file(&p1, &p2).as_bytes(), "pair.milk2", Transition::Cut)
            .unwrap();
        assert_eq!(e.md3.fft_params(), (0.3, 0.4), "PRESET2's keys win");
        e.load_preset(
            file(&p1, &half("")).as_bytes(),
            "pair2.milk2",
            Transition::Cut,
        )
        .unwrap();
        assert_eq!(
            e.md3.fft_params(),
            (0.5, 0.7),
            "PRESET2 without keys resets them, PRESET1's are gone"
        );
    }

    /// Shader `time` counts from the start of each state's own preset: a hard load builds both halves at once (the
    /// same start); a soft transition starts PRESET1 with the request and PRESET2 when the first leg ends.
    #[test]
    fn halves_of_a_double_preset_have_their_own_shader_start_times() {
        use crate::{Compat, Engine, EngineConfig, FrameClock, Transition};
        let device_queue = {
            let instance = wgpu::Instance::default();
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()
                .and_then(|a| {
                    pollster::block_on(a.request_device(&wgpu::DeviceDescriptor::default())).ok()
                })
        };
        let Some((device, queue)) = device_queue else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 32,
                height: 24,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let half = "MILKDROP_PRESET_VERSION=201\nPSVERSION=2\nPSVERSION_WARP=0\nPSVERSION_COMP=0\n[preset00]\nfDecay=1\n";
        let file = format!(
            "Created with MILKDROP 3.37 on 05/01/2026 at 12:34:56\nwww.milkdrop3.com\n\nblending_pattern=zoom\nblending_progress=0.5\nblending_direction=1\nrandom_1=0.1\nrandom_2=0.2\nrandom_3=0.3\nrandom_4=0.4\nrandom_5=0.5\n[PRESET1_BEGIN]\nNAME=a\n{half}[PRESET1_END]\n\n[PRESET2_BEGIN]\nNAME=b\n{half}[PRESET2_END]\n"
        );
        let mut e = Engine::new(
            &device,
            &queue,
            EngineConfig {
                width: 32,
                height: 24,
                mesh: [8, 8],
                compat: Compat::MilkDrop3,
                blend_time_user: 1.0,
                ..Default::default()
            },
        );
        let mut frame = 0u32;
        let mut run = |e: &mut Engine, until: f64| {
            while f64::from(frame) / 60.0 <= until {
                let mut enc = device.create_command_encoder(&Default::default());
                e.render(
                    &mut enc,
                    &view,
                    FrameClock {
                        time: f64::from(frame) / 60.0,
                        frame,
                    },
                );
                queue.submit(Some(enc.finish()));
                frame += 1;
            }
        };
        e.load_preset(half.as_bytes(), "plain", Transition::Cut)
            .unwrap();
        run(&mut e, 2.0);
        // hard load at t = 2: both halves start there
        e.load_preset(file.as_bytes(), "pair.milk2", Transition::Cut)
            .unwrap();
        let (new, old) = (
            e.active.as_ref().unwrap().loaded.start_time,
            e.md2.as_ref().unwrap().old.start_time,
        );
        assert_eq!(new, old);
        assert!((new - 2.0).abs() < 0.05, "{new}");
        // soft transition at t = 4 from a plain preset (a soft request while a `.milk2` is on screen is deferred):
        // PRESET1 starts now, PRESET2 when the 1 s leg ends
        e.load_preset(half.as_bytes(), "plain", Transition::Cut)
            .unwrap();
        run(&mut e, 4.0);
        e.load_preset(
            file.as_bytes(),
            "pair2.milk2",
            Transition::Soft { seconds: 2.7 },
        )
        .unwrap();
        let p1_start = e.active.as_ref().unwrap().loaded.start_time;
        assert!((p1_start - 4.0).abs() < 0.05, "{p1_start}");
        run(&mut e, 5.5);
        let new = e.active.as_ref().unwrap().loaded.start_time;
        let old = e.md2.as_ref().unwrap().old.start_time;
        assert_eq!(old, p1_start, "PRESET1 keeps its own start");
        assert!((new - 5.0).abs() < 0.05, "PRESET2 starts with leg B: {new}");
    }
}
