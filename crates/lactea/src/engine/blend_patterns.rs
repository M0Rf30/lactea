//! The pieces of the MilkDrop 2 and MilkDrop 3 soft-cut blend patterns that are the same arithmetic in both: the
//! plasma midpoint displacement, its min / max normalisation and the `(a, c)` entry of a banded wipe. The random
//! source is the only difference (MD2 draws from a stream, MD3 replays five stored values), so it is a closure.

/// The mesh of a plasma pattern: `gx` cells wide (row stride `gx + 1`) and the `(m_fAspectX, m_fAspectY)` that scale
/// the displacement of the edge midpoints.
#[derive(Clone, Copy)]
pub(super) struct PlasmaGrid {
    pub gx: usize,
    pub aspect: (f32, f32),
}

/// A rectangle of mesh vertices `x0..=x1` by `y0..=y1`.
#[derive(Clone, Copy)]
pub(super) struct Span {
    pub x0: usize,
    pub x1: usize,
    pub y0: usize,
    pub y1: usize,
}

/// `CPlugin::GenPlasma`: midpoint displacement over one `f32` of each cell (`value` picks it). `rand(k)` is the
/// `FRAND` of the `k`-th displacement of a call: 0 and 1 the left / right edge midpoints, 2 and 3 the top / bottom
/// ones, 4 the centre. The callee is asked in exactly that order, only for the midpoints it writes.
pub(super) fn gen_plasma<T>(
    cells: &mut [T],
    value: fn(&mut T) -> &mut f32,
    grid: PlasmaGrid,
    rand: &mut impl FnMut(usize) -> f32,
    span: Span,
    dt: f32,
) {
    let Span { x0, x1, y0, y1 } = span;
    let (ax, ay) = grid.aspect;
    let w = grid.gx + 1;
    let (mx, my) = ((x0 + x1) / 2, (y0 + y1) / 2);
    let at = |cells: &mut [T], x: usize, y: usize| *value(&mut cells[y * w + x]);
    let (t00, t01, t10, t11) = (
        at(cells, x0, y0),
        at(cells, x1, y0),
        at(cells, x0, y1),
        at(cells, x1, y1),
    );

    if y1 - y0 >= 2 {
        if x0 == 0 {
            *value(&mut cells[my * w + x0]) = 0.5 * (t00 + t10) + (rand(0) * 2.0 - 1.0) * dt * ay;
        }
        *value(&mut cells[my * w + x1]) = 0.5 * (t01 + t11) + (rand(1) * 2.0 - 1.0) * dt * ay;
    }
    if x1 - x0 >= 2 {
        if y0 == 0 {
            *value(&mut cells[y0 * w + mx]) = 0.5 * (t00 + t01) + (rand(2) * 2.0 - 1.0) * dt * ax;
        }
        *value(&mut cells[y1 * w + mx]) = 0.5 * (t10 + t11) + (rand(3) * 2.0 - 1.0) * dt * ax;
    }

    if y1 - y0 >= 2 && x1 - x0 >= 2 {
        // the centre, then the four quadrants
        let (l, r, t, b) = (
            at(cells, x0, my),
            at(cells, x1, my),
            at(cells, mx, y0),
            at(cells, mx, y1),
        );
        *value(&mut cells[my * w + mx]) = 0.25 * (t + b + l + r) + (rand(4) * 2.0 - 1.0) * dt;
        let half = dt * 0.5;
        for quadrant in [
            Span {
                x0,
                x1: mx,
                y0,
                y1: my,
            },
            Span {
                x0: mx,
                x1,
                y0,
                y1: my,
            },
            Span {
                x0,
                x1: mx,
                y0: my,
                y1,
            },
            Span {
                x0: mx,
                x1,
                y0: my,
                y1,
            },
        ] {
            gen_plasma(cells, value, grid, rand, quadrant, half);
        }
    }
}

/// `(min, max)` of `values` starting from `seed`, with plain `>` / `<` comparisons (a NaN never replaces either).
pub(super) fn extent(seed: f32, values: impl Iterator<Item = f32>) -> (f32, f32) {
    let (mut mn, mut mx) = (seed, seed);
    for v in values {
        if mn > v {
            mn = v;
        }
        if mx < v {
            mx = v;
        }
    }
    (mn, mx)
}

/// One vertex of a banded wipe: `a = inv_band * a_mul` (`a_mul` is `band + k`), `c = t / band - 1 / band`.
pub(super) fn band_entry(inv_band: f32, a_mul: f32, t: f32) -> [f32; 2] {
    [inv_band * a_mul, t * inv_band - inv_band]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c_of(v: &mut [f32; 2]) -> &mut f32 {
        &mut v[1]
    }

    #[test]
    fn gen_plasma_visits_every_vertex() {
        let (gx, gy) = (48usize, 36usize);
        let mut vi = vec![[1.0, f32::NAN]; (gx + 1) * (gy + 1)];
        vi[0][1] = 0.1;
        vi[gx][1] = 0.2;
        vi[gy * (gx + 1)][1] = 0.3;
        vi[gy * (gx + 1) + gx][1] = 0.4;
        let grid = PlasmaGrid {
            gx,
            aspect: (1.0, 0.5625),
        };
        let span = Span {
            x0: 0,
            x1: gx,
            y0: 0,
            y1: gy,
        };
        gen_plasma(&mut vi, c_of, grid, &mut |k| k as f32 / 4.0, span, 0.25);
        assert!(vi.iter().all(|v| v[1].is_finite()), "every c written");
    }

    #[test]
    fn extent_ignores_nan() {
        assert_eq!(extent(1.0, [3.0, f32::NAN, -2.0].into_iter()), (-2.0, 3.0));
    }
}
