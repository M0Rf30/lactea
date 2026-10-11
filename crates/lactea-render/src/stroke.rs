// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Exact CPU emulation of the 1-pixel `GL_LINES` / `GL_LINE_STRIP` / `GL_LINE_LOOP` / `GL_POINTS`
//! rasterisation of projectM (on Mesa iris, Intel Xe).
//!
//! projectM's line rules were measured, they are *not* the thin quad that D3D9 / Vulkan's default rectangular lines give:
//!
//! * **points** (custom wave dots): pixel `(floor x, floor y)` of the snapped window position, always 1 px
//!   (`gl_PointSize` is ignored: `GL_PROGRAM_POINT_SIZE` is off);
//! * **strips** (`GL_LINE_STRIP`, `GL_LINE_SMOOTH` off: custom and main waves): for an x-major segment
//!   (`|dx| >= |dy|`) one pixel per column whose centre `c` lies in `[x_lo, x_hi)`, row `floor(y(c))`; for a y-major
//!   segment one pixel per row with `c` in `(y_lo, y_hi]`, column `floor(x(c))`. Exact ties on the minor axis
//!   (`y(c)` or `x(c)` an integer, e.g. a line on a screen edge) go to the upper row for an x-major segment rising to
//!   the right and to the left column for every y-major segment;
//! * **smooth lines** (`GL_LINE_SMOOTH` on: shape outlines `GL_LINE_LOOP`, motion vectors `GL_LINES`; Mesa draws them
//!   aliased): the *diamond-exit* rule. A pixel is lit iff the segment intersects the pixel's diamond `D` and the
//!   segment's end point is not in `D`, where `D = {|x-cx| + |y-cy| < 1/2}` plus the boundary points with
//!   `y < cy`, or `y == cy && x > cx` (the diamond is closed on its upper edges, open on its lower edges,
//!   closed at the right tip).
//!
//! Vertex positions are first snapped to the 1/256 pixel grid like the GPU does. All decisions are then made in
//! exact integer arithmetic, so the result is independent of the GPU/driver. Each lit pixel is emitted as a
//! [`WfVertex`] at its pixel centre, to be drawn as a 1-px `PointList` (exact on every GPU: the point sits on
//! the centre of the pixel it hits).
//!
//! Window space is the render-target pixel grid, `x` right, `y` down, row 0 = top (clip `y = +1`).

use crate::geom::WfVertex;
use std::cmp::Ordering;

/// Sub-pixel precision of the GPU's vertex snapping (8 bits).
const SUB: i64 = 256;
/// Half a pixel in sub-pixel units.
const HALF: i64 = SUB / 2;
/// Half-extent of the GPU's clip guard band in window pixels **around the viewport centre** (Mesa iris; measured:
/// `x` in `W/2 +- 16384`, `y` in `H/2 +- 16384`, e.g. -16224 ..= 16544 and -16294 ..=
/// 16474 at 320x180, same rule at 640x360, 200x300, 1000x200). Points outside it are culled, segments are clipped to it
/// (so a motion vector of a preset with a huge previous-frame UV still draws its visible part) before the vertices
/// are snapped.
const GUARD_PX: f32 = 16384.0;

/// The viewport transform of the GPU's clip unit for one axis: `fma(c, size / 2, size / 2)`, **truncated toward zero** to
/// f32 (the fixed-function unit does not round to nearest: measured on Mesa iris,
/// ~15 k - 160 k f32 values straddling every pixel boundary per axis at 320x180, 321x181, 640x360, 1000x200, 200x300
/// and 1920x1080, for points and smooth / non-smooth lines: 0 mismatches; round-to-nearest of the same expression, of
/// `c * h + h` and of `(c * 0.5 + 0.5) * size` disagree in 0.5 - 4 % of the boundary cases). The product and the sum
/// are one operation here (`c * half` is exact in f64, the sum's rounding error is recovered with TwoSum), so the
/// truncation is decided on the exact value.
#[inline]
fn viewport_axis(c: f32, size: f32) -> f32 {
    let half = f64::from(size) * 0.5;
    let hi = f64::from(c) * half;
    let s = hi + half;
    let bb = s - hi;
    let err = (hi - (s - bb)) + (half - bb);
    let t = s as f32;
    if !t.is_finite() {
        return t;
    }
    let d = (f64::from(t) - s) - err; // t - exact
    if (t > 0.0 && d > 0.0) || (t < 0.0 && d < 0.0) {
        f32::from_bits(t.to_bits() - 1)
    } else {
        t
    }
}

/// `v` (f32, window px) in 1/256 px, rounded to nearest even (the hardware's fixed-point conversion).
#[inline]
fn snap(v: f32) -> i64 {
    (v * SUB as f32).round_ties_even() as i64
}

/// Clip position -> window position in 1/256 px, `x` right, `y` down (the GPU works with `y` up: its snapped value is
/// mirrored exactly, `SUB * h - s`). No guard-band check.
#[inline]
fn snap_pos(p: [f32; 2], w: f32, h: f32) -> (i64, i64) {
    (
        snap(viewport_axis(p[0], w)),
        SUB * h as i64 - snap(viewport_axis(p[1], h)),
    )
}

/// Clip position -> window position in 1/256 px (`None` when not finite / outside the guard band).
#[inline]
fn window(p: [f32; 2], w: f32, h: f32) -> Option<(i64, i64)> {
    let x = viewport_axis(p[0], w);
    let y = viewport_axis(p[1], h);
    if !(x.is_finite() && y.is_finite())
        || (x - w * 0.5).abs() > GUARD_PX
        || (y - h * 0.5).abs() > GUARD_PX
    {
        return None;
    }
    Some(snap_pos(p, w, h))
}

/// A segment's window end points in 1/256 px plus the colour fractions of its (possibly clipped) ends and whether
/// the guard band clipped it: `None` when it is not finite or lies wholly outside the guard band; clipped to the
/// guard band otherwise.
fn window_segment(a: [f32; 2], b: [f32; 2], w: f32, h: f32) -> Option<([i64; 4], [f32; 2], bool)> {
    let win = |p: [f32; 2]| {
        (
            (f64::from(p[0]) * 0.5 + 0.5) * f64::from(w),
            (0.5 - f64::from(p[1]) * 0.5) * f64::from(h),
        )
    };
    let ((x0, y0), (x1, y1)) = (win(a), win(b));
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
        return None;
    }
    let g = f64::from(GUARD_PX);
    let (cx, cy) = (f64::from(w) * 0.5, f64::from(h) * 0.5);
    if [x0 - cx, y0 - cy, x1 - cx, y1 - cy]
        .iter()
        .all(|v| v.abs() <= g)
    {
        // the common case: both ends inside
        let (p, q) = (snap_pos(a, w, h), snap_pos(b, w, h));
        return Some(([p.0, p.1, q.0, q.1], [0.0, 1.0], false));
    }
    // Liang-Barsky against the box [c - g, c + g]^2
    let (dx, dy) = (x1 - x0, y1 - y0);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, x0 - cx + g),
        (dx, g - (x0 - cx)),
        (-dy, y0 - cy + g),
        (dy, g - (y0 - cy)),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    let s = |v: f64| (v * SUB as f64).round_ties_even() as i64;
    Some((
        [
            s(x0 + t0 * dx),
            s(y0 + t0 * dy),
            s(x0 + t1 * dx),
            s(y0 + t1 * dy),
        ],
        [t0 as f32, t1 as f32],
        true,
    ))
}

#[inline]
fn div_floor(a: i64, b: i64) -> i64 {
    let q = a / b;
    if a % b != 0 && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

#[inline]
fn div_ceil(a: i64, b: i64) -> i64 {
    -div_floor(-a, b)
}

/// Pixel-centre vertex for pixel `(i, j)` of a `w x h` target, `None` when outside.
#[inline]
fn pixel_vertex(i: i64, j: i64, w: u32, h: u32, color: [f32; 4]) -> Option<WfVertex> {
    if i < 0 || j < 0 || i >= i64::from(w) || j >= i64::from(h) {
        return None;
    }
    Some(WfVertex {
        pos: [
            (i as f32 + 0.5) / w as f32 * 2.0 - 1.0,
            1.0 - (j as f32 + 0.5) / h as f32 * 2.0,
        ],
        color,
    })
}

/// Colour of the fragment at window position `(px, py)` (1/256 px) of segment `(x0,y0)-(x1,y1)`: linear along the
/// segment (GL: `t = (p - p0).d / |d|^2`), clamped to the displayable range.
#[inline]
fn lerp_color(
    c0: [f32; 4],
    c1: [f32; 4],
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
    px: i64,
    py: i64,
) -> [f32; 4] {
    if c0 == c1 {
        return c0;
    }
    let (dx, dy) = ((x1 - x0) as f64, (y1 - y0) as f64);
    let t = (((px - x0) as f64) * dx + ((py - y0) as f64) * dy) / (dx * dx + dy * dy);
    let t = t as f32;
    let mut c = [0.0; 4];
    for k in 0..4 {
        c[k] = (c0[k] + (c1[k] - c0[k]) * t).clamp(0.0, 1.0);
    }
    c
}

/// Pixels of one non-smooth line segment (see the module docs), in window pixel coordinates.
/// `emit(i, j, px, py)` gets the pixel and its centre in 1/256 px.
///
/// Exact ties on the minor axis (the segment crosses a pixel boundary exactly at a pixel centre of the major axis) are
/// decided like the hardware does (860 measured segments, 100 % identical): an x-major
/// segment that rises to the right (`dy` and `dx` of opposite sign) gives the tie to the upper row (`floor(y) - 1`),
/// every other x-major segment to the lower one (`floor(y)`); a y-major segment always gives it to the left column
/// (`floor(x) - 1`). (Without ties the pixel is `floor` of the exact rational, whatever the direction.)
fn strip_segment(
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
    w: i64,
    h: i64,
    mut emit: impl FnMut(i64, i64),
) {
    let (dx, dy) = (x1 - x0, y1 - y0);
    if dx == 0 && dy == 0 {
        return;
    }
    if dx.abs() >= dy.abs() {
        // x-major: centres c = i*256 + 128 in [lo, hi)
        let (lo, hi) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
        let i0 = div_ceil(lo - HALF, SUB).max(0);
        let i1 = (div_ceil(hi - HALF, SUB) - 1).min(w - 1);
        let rising = dy != 0 && (dx > 0) != (dy > 0);
        for i in i0..=i1 {
            let c = i * SUB + HALF;
            // y(c) = y0 + (c - x0) * dy / dx, row = floor(y / 256) (`ceil - 1` on a tie of a rising segment)
            let num = y0 * dx + (c - x0) * dy;
            let den = dx * SUB;
            let j = if rising {
                div_ceil(num, den) - 1
            } else {
                div_floor(num, den)
            };
            if (0..h).contains(&j) {
                emit(i, j);
            }
        }
    } else {
        // y-major: centres c = j*256 + 128 in (lo, hi]
        let (lo, hi) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
        let j0 = (div_floor(lo - HALF, SUB) + 1).max(0);
        let j1 = div_floor(hi - HALF, SUB).min(h - 1);
        for j in j0..=j1 {
            let c = j * SUB + HALF;
            // x(c) = x0 + (c - y0) * dx / dy, column = `ceil - 1` (= floor except on a tie, which goes left)
            let num = x0 * dy + (c - y0) * dx;
            // x(c) exactly on a pixel boundary (a vertical line on an integer x, ...): the lower column
            let i = div_ceil(num, dy * SUB) - 1;
            if (0..w).contains(&i) {
                emit(i, j);
            }
        }
    }
}

/// Exact rational `n / d` with `d > 0`.
#[derive(Clone, Copy)]
struct Frac {
    n: i128,
    d: i128,
}

impl Frac {
    fn new(n: i64, d: i64) -> Frac {
        debug_assert!(d != 0);
        if d > 0 {
            Frac {
                n: n as i128,
                d: d as i128,
            }
        } else {
            Frac {
                n: -(n as i128),
                d: -(d as i128),
            }
        }
    }
    fn cmp(&self, o: &Frac) -> Ordering {
        (self.n * o.d).cmp(&(o.n * self.d))
    }
    fn lt(&self, o: &Frac) -> bool {
        self.cmp(o) == Ordering::Less
    }
    fn gt(&self, o: &Frac) -> bool {
        self.cmp(o) == Ordering::Greater
    }
}

/// `D`: the pixel's diamond including its closed boundary half (upper edges and the right tip).
#[inline]
fn in_diamond(ox: i64, oy: i64) -> bool {
    let d = ox.abs() + oy.abs();
    d < HALF || (d == HALF && (oy < 0 || (oy == 0 && ox > 0)))
}

/// Does segment `(x0,y0)-(x1,y1)` light pixel `(i, j)` under the diamond-exit rule?
fn diamond_lit(x0: i64, y0: i64, x1: i64, y1: i64, i: i64, j: i64) -> bool {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let cx = i * SUB + HALF;
    let cy = j * SUB + HALF;
    // closed-diamond constraints  a*t <= b  (four half planes s1*(x-cx) + s2*(y-cy) <= 128)
    let mut lo: Option<Frac> = None;
    let mut hi: Option<Frac> = None;
    let mut edge: Option<(i64, i64)> = None; // segment lies on this edge's line
    for (s1, s2) in [(1i64, 1i64), (1, -1), (-1, 1), (-1, -1)] {
        let a = s1 * dx + s2 * dy;
        let b = HALF - (s1 * (x0 - cx) + s2 * (y0 - cy));
        if a == 0 {
            if b < 0 {
                return false;
            }
            if b == 0 {
                edge = Some((s1, s2));
            }
        } else {
            let t = Frac::new(b, a);
            if a > 0 {
                if hi.is_none_or(|h| t.lt(&h)) {
                    hi = Some(t);
                }
            } else if lo.is_none_or(|l| t.gt(&l)) {
                lo = Some(t);
            }
        }
    }
    let (Some(lo), Some(hi)) = (lo, hi) else {
        return false; // a segment of non-zero length always has both
    };
    if lo.gt(&hi) {
        return false;
    }
    let zero = Frac { n: 0, d: 1 };
    let one = Frac { n: 1, d: 1 };
    // closed intersection with [0, 1]
    let tlo = if lo.gt(&zero) { lo } else { zero };
    let thi = if hi.lt(&one) { hi } else { one };
    if tlo.gt(&thi) {
        return false;
    }
    let intersects = if let Some((s1, s2)) = edge {
        // the segment runs along the boundary: its points are in D only on the (closed) upper edges
        (s1, s2) == (-1, -1) || (s1, s2) == (1, -1)
    } else if lo.lt(&hi) && hi.gt(&zero) && lo.lt(&one) {
        true // reaches the open interior
    } else {
        // tangent: touches the boundary in one point, which belongs to D only on the closed half
        let ex = (x0 - cx) as i128 * tlo.d + tlo.n * dx as i128; // scaled by tlo.d
        let ey = (y0 - cy) as i128 * tlo.d + tlo.n * dy as i128;
        let on_boundary = ex.abs() + ey.abs() == HALF as i128 * tlo.d;
        on_boundary && (ey < 0 || (ey == 0 && ex > 0))
    };
    intersects && !in_diamond(x1 - cx, y1 - cy)
}

/// Pixels of one smooth (diamond-exit) segment.
fn diamond_segment(
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
    w: i64,
    h: i64,
    mut emit: impl FnMut(i64, i64),
) {
    let (dx, dy) = (x1 - x0, y1 - y0);
    if dx == 0 && dy == 0 {
        return;
    }
    let (xlo, xhi) = (x0.min(x1), x0.max(x1));
    let (ylo, yhi) = (y0.min(y1), y0.max(y1));
    if dx.abs() >= dy.abs() {
        // walk the columns; rows within one pixel of the line's y range over the column
        let i0 = (div_floor(xlo, SUB) - 1).max(-1);
        let i1 = (div_floor(xhi, SUB) + 1).min(w);
        for i in i0..=i1 {
            let xa = (i * SUB).clamp(xlo, xhi);
            let xb = ((i + 1) * SUB).clamp(xlo, xhi);
            let ya = div_floor(y0 * dx + (xa - x0) * dy, dx);
            let yb = div_floor(y0 * dx + (xb - x0) * dy, dx);
            let (ja, jb) = (ya.min(yb), ya.max(yb));
            for j in (div_floor(ja, SUB) - 1).max(-1)..=(div_floor(jb, SUB) + 1).min(h) {
                if (0..w).contains(&i) && (0..h).contains(&j) && diamond_lit(x0, y0, x1, y1, i, j) {
                    emit(i, j);
                }
            }
        }
    } else {
        let j0 = (div_floor(ylo, SUB) - 1).max(-1);
        let j1 = (div_floor(yhi, SUB) + 1).min(h);
        for j in j0..=j1 {
            let ya = (j * SUB).clamp(ylo, yhi);
            let yb = ((j + 1) * SUB).clamp(ylo, yhi);
            let xa = div_floor(x0 * dy + (ya - y0) * dx, dy);
            let xb = div_floor(x0 * dy + (yb - y0) * dx, dy);
            let (ia, ib) = (xa.min(xb), xa.max(xb));
            for i in (div_floor(ia, SUB) - 1).max(-1)..=(div_floor(ib, SUB) + 1).min(w) {
                if (0..w).contains(&i) && (0..h).contains(&j) && diamond_lit(x0, y0, x1, y1, i, j) {
                    emit(i, j);
                }
            }
        }
    }
}

/// Which GL rule a batch of segments follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineRule {
    /// `GL_LINE_SMOOTH` disabled (waves): half-open centre-in-extent, see the module docs.
    Strip,
    /// `GL_LINE_SMOOTH` enabled (shape outlines, motion vectors): diamond exit.
    Diamond,
}

/// Rasterise `segs` (pairs of clip-space vertices with colours) of a `w x h` target; every lit pixel is appended to
/// `out` as a vertex at the pixel centre with the colour interpolated along its segment.
pub fn raster_segments(segs: &[WfVertex], w: u32, h: u32, rule: LineRule, out: &mut Vec<WfVertex>) {
    let (wf, hf) = (w as f32, h as f32);
    for s in segs.as_chunks::<2>().0 {
        let Some(([mut x0, mut y0, mut x1, mut y1], [t0, t1], clipped)) =
            window_segment(s[0].pos, s[1].pos, wf, hf)
        else {
            continue;
        };
        let lerp = |t: f32| {
            std::array::from_fn::<f32, 4, _>(|k| {
                s[0].color[k] + (s[1].color[k] - s[0].color[k]) * t
            })
        };
        let (mut c0, mut c1) = if t0 == 0.0 && t1 == 1.0 {
            (s[0].color, s[1].color)
        } else {
            (lerp(t0), lerp(t1))
        };
        if clipped && matches!(rule, LineRule::Diamond) {
            // The GPU's clipper hands a guard-band-clipped line to the rasteriser reversed: the diamond-exit rule then
            // treats the original end as the start (lit if inside its diamond) and the original start as the end (never
            // lit). Measured: a start pixel stops being lit exactly when the far
            // vertex crosses the guard band, an end pixel starts being lit; GL_LINE_SMOOTH off is unaffected.
            std::mem::swap(&mut x0, &mut x1);
            std::mem::swap(&mut y0, &mut y1);
            std::mem::swap(&mut c0, &mut c1);
        }
        let mut emit = |i: i64, j: i64| {
            let col = lerp_color(c0, c1, x0, y0, x1, y1, i * SUB + HALF, j * SUB + HALF);
            if let Some(v) = pixel_vertex(i, j, w, h, col) {
                out.push(v);
            }
        };
        match rule {
            LineRule::Strip => strip_segment(x0, y0, x1, y1, i64::from(w), i64::from(h), &mut emit),
            LineRule::Diamond => {
                diamond_segment(x0, y0, x1, y1, i64::from(w), i64::from(h), &mut emit)
            }
        }
    }
}

/// 1-px points: pixel `(floor x, floor y)` of the snapped window position.
pub fn raster_points(pts: &[WfVertex], w: u32, h: u32, out: &mut Vec<WfVertex>) {
    let (wf, hf) = (w as f32, h as f32);
    for p in pts {
        let Some((x, y)) = window(p.pos, wf, hf) else {
            continue;
        };
        if let Some(v) = pixel_vertex(div_floor(x, SUB), div_floor(y, SUB), w, h, p.color) {
            out.push(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 64;
    const H: u32 = 64;

    /// window px -> clip
    fn clip(x: f32, y: f32) -> [f32; 2] {
        [x / W as f32 * 2.0 - 1.0, 1.0 - y / H as f32 * 2.0]
    }

    fn seg(a: (f32, f32), b: (f32, f32)) -> Vec<WfVertex> {
        let c = [1.0; 4];
        vec![
            WfVertex {
                pos: clip(a.0, a.1),
                color: c,
            },
            WfVertex {
                pos: clip(b.0, b.1),
                color: c,
            },
        ]
    }

    fn pixels(v: &[WfVertex]) -> Vec<(i64, i64)> {
        let mut p: Vec<(i64, i64)> = v
            .iter()
            .map(|v| {
                (
                    ((v.pos[0] * 0.5 + 0.5) * W as f32).floor() as i64,
                    ((0.5 - v.pos[1] * 0.5) * H as f32).floor() as i64,
                )
            })
            .collect();
        p.sort_unstable();
        p
    }

    fn run(rule: LineRule, a: (f32, f32), b: (f32, f32)) -> Vec<(i64, i64)> {
        let mut out = Vec::new();
        raster_segments(&seg(a, b), W, H, rule, &mut out);
        pixels(&out)
    }

    #[test]
    fn points_are_floor_of_snapped_position() {
        let mut out = Vec::new();
        let pts: Vec<WfVertex> = [(10.0, 10.0), (10.99, 3.5), (0.0, 0.0), (63.99, 63.5)]
            .iter()
            .map(|&(x, y)| WfVertex {
                pos: clip(x, y),
                color: [1.0; 4],
            })
            .collect();
        raster_points(&pts, W, H, &mut out);
        assert_eq!(pixels(&out), vec![(0, 0), (10, 3), (10, 10), (63, 63)]);
    }

    #[test]
    fn strip_minor_axis_tie_goes_to_the_higher_row() {
        // horizontal line exactly between rows 64 and 65 of a taller target -> row 65 (projectM's rule)
        let mut out = Vec::new();
        let c = [1.0; 4];
        let clip2 = |x: f32, y: f32| [x / 64.0 * 2.0 - 1.0, 1.0 - y / 128.0 * 2.0];
        let s = vec![
            WfVertex {
                pos: clip2(10.0, 65.0),
                color: c,
            },
            WfVertex {
                pos: clip2(20.0, 65.0),
                color: c,
            },
        ];
        raster_segments(&s, 64, 128, LineRule::Strip, &mut out);
        let rows: Vec<i64> = out
            .iter()
            .map(|v| ((0.5 - v.pos[1] * 0.5) * 128.0).floor() as i64)
            .collect();
        assert!(rows.iter().all(|&r| r == 65), "{rows:?}");
        assert_eq!(rows.len(), 10); // columns 10..=19: [x_lo, x_hi) of centres
    }

    /// Strip pixels of segment `a -> b` (window px) on the 320x180 corpus target as `(col, row)`.
    fn strip320(a: (f32, f32), b: (f32, f32)) -> Vec<(i64, i64)> {
        let c = [1.0; 4];
        let clip = |p: (f32, f32)| [p.0 / 320.0 * 2.0 - 1.0, 1.0 - p.1 / 180.0 * 2.0];
        let mut out = Vec::new();
        raster_segments(
            &[
                WfVertex {
                    pos: clip(a),
                    color: c,
                },
                WfVertex {
                    pos: clip(b),
                    color: c,
                },
            ],
            320,
            180,
            LineRule::Strip,
            &mut out,
        );
        let mut p: Vec<(i64, i64)> = out
            .iter()
            .map(|v| {
                (
                    ((v.pos[0] * 0.5 + 0.5) * 320.0).floor() as i64,
                    ((0.5 - v.pos[1] * 0.5) * 180.0).floor() as i64,
                )
            })
            .collect();
        p.sort_unstable();
        p
    }

    /// projectM ties (Rovastar - Braindance 1 frame 0): x-major segment through the centre of
    /// column 220 at exactly y = 92.0. Rising to the right it lights row 91, falling (or flat) row 92, whatever the
    /// drawing direction.
    #[test]
    fn strip_x_major_tie_depends_on_the_slope_sign() {
        let (p0, p1) = ((219.3984375, 92.00390625), (221.6015625, 91.99609375)); // snapped 56166,23553 -> 56730,23551
        let rising = vec![(219, 92), (220, 91), (221, 91)];
        assert_eq!(strip320(p0, p1), rising);
        assert_eq!(strip320(p1, p0), rising);
        let (q0, q1) = ((219.3984375, 91.99609375), (221.6015625, 92.00390625));
        let falling = vec![(219, 91), (220, 92), (221, 92)];
        assert_eq!(strip320(q0, q1), falling);
        assert_eq!(strip320(q1, q0), falling);
    }

    /// projectM ties (Idiot - 9-7-02 main wave on the screen edges): a y-major segment exactly on a pixel boundary lights
    /// the column to its left, so a line on x = 0 is off-screen and one on x = W is column W - 1.
    #[test]
    fn strip_y_major_tie_goes_to_the_left_column() {
        assert_eq!(strip320((0.0, 10.25), (0.0, 13.75)), vec![]);
        assert_eq!(
            strip320((320.0, 10.25), (320.0, 12.75)),
            vec![(319, 10), (319, 11), (319, 12)]
        );
        assert_eq!(
            strip320((160.0, 10.25), (160.0, 12.75)),
            vec![(159, 10), (159, 11), (159, 12)]
        );
        // a slanted y-major segment crossing x = 160.0 exactly at the centre of row 11
        assert_eq!(
            strip320((159.5, 10.5), (160.5, 12.5)),
            vec![(159, 11), (160, 12)]
        );
    }

    #[test]
    fn strip_extents_are_half_open_per_axis() {
        // x-major: centres in [lo, hi): start exactly on a centre is included, end exactly on a centre excluded
        assert_eq!(
            run(LineRule::Strip, (10.5, 5.5), (13.5, 5.5)),
            vec![(10, 5), (11, 5), (12, 5)]
        );
        // y-major: centres in (lo, hi]
        assert_eq!(
            run(LineRule::Strip, (30.5, 5.5), (30.5, 8.5)),
            vec![(30, 6), (30, 7), (30, 8)]
        );
        // a start that lies past the first centre skips that pixel (no diamond start inclusion for strips)
        assert_eq!(
            run(LineRule::Strip, (10.75, 5.5), (13.5, 5.5)),
            vec![(11, 5), (12, 5)]
        );
    }

    #[test]
    fn diamond_includes_the_start_pixel_behind_the_centre() {
        // projectM motion vector (isolated GL_LINES, smooth): the start pixel whose centre lies slightly *behind*
        // the start is lit because the start is inside its diamond (a strip would skip it)
        let (a, b) = ((2.53125, 7.62109375), (5.078125, 6.18359375));
        assert_eq!(run(LineRule::Diamond, a, b), vec![(2, 7), (3, 7), (4, 6)]);
        assert_eq!(run(LineRule::Strip, a, b), vec![(3, 7), (4, 6)]);
    }

    #[test]
    fn diamond_end_on_boundary_is_lit_only_on_the_lower_edges() {
        // end exactly on the lower right edge of the diamond of pixel (10, 10) (centre 10.5, 10.5): exits -> lit
        assert_eq!(
            run(LineRule::Diamond, (8.0, 10.75), (10.75, 10.75)),
            vec![(8, 10), (9, 10), (10, 10)]
        );
        // same on the upper right edge: the end is still in the (closed) diamond -> not lit
        assert_eq!(
            run(LineRule::Diamond, (8.0, 10.25), (10.75, 10.25)),
            vec![(8, 10), (9, 10)]
        );
    }

    #[test]
    fn diamond_start_on_upper_edge_leaving_is_lit() {
        // starts exactly on the closed upper-left edge of pixel (10, 10) and leaves at once
        let r = run(LineRule::Diamond, (10.25, 10.25), (6.0, 6.0));
        assert!(r.contains(&(10, 10)), "{r:?}");
        // ... but not on the open lower-left edge
        let r = run(LineRule::Diamond, (10.25, 10.75), (6.0, 14.0));
        assert!(!r.contains(&(10, 10)), "{r:?}");
    }

    #[test]
    fn diamond_45_degree_line_along_a_diamond_edge() {
        // y = x - 0.5 coincides with the upper right edge of pixel (10, 10)'s diamond (closed): lit
        let r = run(LineRule::Diamond, (9.0, 8.5), (14.0, 13.5));
        assert!(r.contains(&(10, 10)), "{r:?}");
        // y = x + 0.5 coincides with its lower left edge (open): not lit
        let r = run(LineRule::Diamond, (9.0, 9.5), (14.0, 14.5));
        assert!(!r.contains(&(10, 10)), "{r:?}");
    }

    #[test]
    fn colours_interpolate_along_the_segment() {
        let mut out = Vec::new();
        let v = vec![
            WfVertex {
                pos: clip(0.5, 5.5),
                color: [0.0, 0.0, 0.0, 1.0],
            },
            WfVertex {
                pos: clip(10.5, 5.5),
                color: [1.0, 0.5, 0.0, 1.0],
            },
        ];
        raster_segments(&v, W, H, LineRule::Strip, &mut out);
        assert_eq!(out.len(), 10);
        assert!(out[0].color[0] < 0.06 && out[9].color[0] > 0.89);
        assert!((out[5].color[0] - 0.5).abs() < 0.06 && (out[5].color[1] - 0.25).abs() < 0.03);
    }

    #[test]
    fn long_and_far_away_segments_do_not_stall() {
        let mut out = Vec::new();
        let v = vec![
            WfVertex {
                pos: [-1.0e4, 0.3],
                color: [1.0; 4],
            },
            WfVertex {
                pos: [1.0e4, -0.2],
                color: [1.0; 4],
            },
        ];
        raster_segments(&v, W, H, LineRule::Strip, &mut out);
        raster_segments(&v, W, H, LineRule::Diamond, &mut out);
        assert!(!out.is_empty());
        let v = vec![
            WfVertex {
                pos: [f32::NAN, 0.0],
                color: [1.0; 4],
            },
            WfVertex {
                pos: [1.0, 1.0e30],
                color: [1.0; 4],
            },
        ];
        let n = out.len();
        raster_segments(&v, W, H, LineRule::Diamond, &mut out);
        assert_eq!(out.len(), n);
    }

    #[test]
    fn segments_beyond_the_guard_band_are_clipped_not_dropped() {
        // starts in the frame, ends ~1e7 px away (a motion vector whose previous warp UV was huge)
        let v = vec![
            WfVertex {
                pos: clip(32.5, 10.5),
                color: [1.0; 4],
            },
            WfVertex {
                pos: clip(32.5, 1.0e7),
                color: [1.0; 4],
            },
        ];
        for rule in [LineRule::Strip, LineRule::Diamond] {
            let mut out = Vec::new();
            raster_segments(&v, W, H, rule, &mut out);
            let mut p = pixels(&out);
            p.retain(|&(x, _)| x == 32);
            // strip: centres in (10.5, ..); diamond: the clipper reverses the line, so the start pixel (the new end) is
            // not lit
            let (n, first) = (53, 11);
            assert_eq!(p.len(), n, "{rule:?} {p:?}");
            assert_eq!(p.first(), Some(&(32, first)));
        }
        // entirely beyond it: culled
        let far = vec![
            WfVertex {
                pos: clip(32.5, 2.0e4),
                color: [1.0; 4],
            },
            WfVertex {
                pos: clip(40.5, 1.0e7),
                color: [1.0; 4],
            },
        ];
        let mut out = Vec::new();
        raster_segments(&far, W, H, LineRule::Diamond, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn guard_band_is_centred_on_the_viewport() {
        // 64x64: the band is x, y in 32 +- 16384 (measured at 320x180 / 640x360 / 200x300 / 1000x200)
        let inside = vec![
            WfVertex {
                pos: clip(32.5, 10.5),
                color: [1.0; 4],
            },
            WfVertex {
                pos: clip(32.5 + 16300.0, 10.5),
                color: [1.0; 4],
            },
        ];
        let mut out = Vec::new();
        raster_segments(&inside, W, H, LineRule::Diamond, &mut out);
        // not clipped (16332 < 32 + 16384): the start pixel is lit
        assert!(pixels(&out).contains(&(32, 10)));
        // beyond 32 + 16384 on the right: clipped, reversed
        let beyond = vec![
            WfVertex {
                pos: clip(32.5, 10.5),
                color: [1.0; 4],
            },
            WfVertex {
                pos: clip(32.5 + 16400.0, 10.5),
                color: [1.0; 4],
            },
        ];
        let mut out = Vec::new();
        raster_segments(&beyond, W, H, LineRule::Diamond, &mut out);
        assert!(!pixels(&out).contains(&(32, 10)));
        assert!(pixels(&out).contains(&(33, 10)));
    }

    #[test]
    fn clipped_diamond_line_ending_inside_lights_its_end_pixel() {
        // far start, end inside: the clipper reverses the line, the end becomes the start and is lit
        let v = vec![
            WfVertex {
                pos: clip(40.5 - 1.0e5, 20.5),
                color: [1.0; 4],
            },
            WfVertex {
                pos: clip(40.5, 20.5),
                color: [1.0; 4],
            },
        ];
        let mut out = Vec::new();
        raster_segments(&v, W, H, LineRule::Diamond, &mut out);
        assert!(pixels(&out).contains(&(40, 20)));
        // an unclipped line does not light its end pixel
        let u = vec![
            WfVertex {
                pos: clip(10.5, 20.5),
                color: [1.0; 4],
            },
            WfVertex {
                pos: clip(40.5, 20.5),
                color: [1.0; 4],
            },
        ];
        let mut out = Vec::new();
        raster_segments(&u, W, H, LineRule::Diamond, &mut out);
        assert!(!pixels(&out).contains(&(40, 20)));
    }

    /// Snap measurements on Mesa iris (smooth vertical / horizontal lines, 320x180): the f32 clip value straddling the
    /// first pixel boundary in each pair, `(bits, lit GL pixel index)`. A boundary lies at 1/256 px above the pixel edge
    /// (ties go to the lower pixel); the viewport transform truncates, so these pairs sit 1 - 3 f32 ulps away from where
    /// round-to-nearest of `fma(c, size/2, size/2)` would put them.
    #[test]
    fn viewport_snap_matches_the_gpu_at_pixel_boundaries() {
        let x: [(u32, i64); 14] = [
            (0xbf74cc00, 6),
            (0xbf74cbff, 7),
            (0xbf6198cd, 18),
            (0xbf6198cc, 19),
            (0xbf44cc00, 36),
            (0xbf44cbff, 37),
            (0xbf14cc00, 66),
            (0xbf14cbff, 67),
            (0xbebffe65, 99),
            (0xbebffe64, 100),
            (0xbd7ff31a, 149),
            (0xbd7ff319, 150),
            (0x3d8cd33f, 170),
            (0x3d8cd340, 171),
        ];
        for (bits, col) in x {
            let (sx, _) = snap_pos([f32::from_bits(bits), 0.0], 320.0, 180.0);
            assert_eq!(div_ceil(sx, SUB) - 1, col, "x {bits:08x}");
        }
        let y: [(u32, i64); 14] = [
            (0xbf6c1556, 6),
            (0xbf6c1555, 7),
            (0xbf49f333, 18),
            (0xbf49f332, 19),
            (0xbf16c000, 36),
            (0xbf16bfff, 37),
            (0xbe82d553, 66),
            (0xbe82d552, 67),
            (0x3de399a4, 99),
            (0x3de399a5, 100),
            (0x3f2aac19, 149),
            (0x3f2aac1a, 150),
            (0x3f6667d5, 170),
            (0x3f6667d6, 171),
        ];
        for (bits, row_gl) in y {
            // the GPU's y is up: image row (down) = 179 - row
            let (_, sy) = snap_pos([0.0, f32::from_bits(bits)], 320.0, 180.0);
            assert_eq!(div_floor(sy, SUB), 179 - row_gl, "y {bits:08x}");
        }
    }

    /// Singularity In My Oscilloscope frame 7 (corpus-all, 320x180): a motion vector whose line passes through the
    /// shared tip of two pixel diamonds at the left edge lit `(0, 40)` with round-to-nearest vertex snapping and `(0, 41)`
    /// on the GPU (`warpsrc`, 2 pixels).
    #[test]
    fn motion_vector_tip_tie_uses_the_gpu_snap() {
        let v = vec![
            WfVertex {
                pos: [f32::from_bits(0xbf0d0cd8), f32::from_bits(0x3f30d9b6)],
                color: [1.0; 4],
            },
            WfVertex {
                pos: [f32::from_bits(0xbf916eaf), f32::from_bits(0x3eff5830)],
                color: [1.0; 4],
            },
        ];
        let mut out = Vec::new();
        raster_segments(&v, 320, 180, LineRule::Diamond, &mut out);
        let p: Vec<(i64, i64)> = out
            .iter()
            .map(|v| {
                (
                    ((v.pos[0] * 0.5 + 0.5) * 320.0).floor() as i64,
                    ((0.5 - v.pos[1] * 0.5) * 180.0).floor() as i64,
                )
            })
            .collect();
        assert!(p.contains(&(0, 41)) && !p.contains(&(0, 40)), "{p:?}");
    }
}
