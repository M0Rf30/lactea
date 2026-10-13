// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! projectM's motion vector field (`MotionVectors.cpp` + `PresetMotionVectorsVertexShaderGlsl330.vert`).
//!
//! What projectM does:
//!
//! * the grid positions `pos = (posX, posY)` (0..1, y down) are computed like MD2, but the lines are drawn
//!   **vertically mirrored**: start at image position `(posX, 1 - posY)`, end at `(posX + dx, 1 - posY - dy)`
//!   (the vertex shader flips `y` and `orthogonalProjection` flips it once more, while the warp-UV lookup is not
//!   mirrored). MD2 draws `(posX, posY)` -> `(posX + dx, posY + dy)`;
//! * `dist = (oldUV - pos) * mv_l` where `oldUV` is a *texture lookup* of the previous frame's warp-UV map: an
//!   `RG16F` image of the render size written by the warp pass (per-pixel interpolation of the mesh UVs, rounded
//!   to half floats), sampled with `GL_LINEAR` at image position `(posX, posY)`. This emulates that lookup (mesh
//!   triangulation incl. diagonal flip, the rasteriser's snapped-vertex plane equation, f16 truncation, bilinear);
//! * `len < 1.25 px` (in UV units: `sqrt((1.25/W)^2 + (1.25/H)^2)`) is stretched to the minimum, zero length gives
//!   the diagonal `(min, min)`;
//! * the mesh is `GL_LINES` over a vertex buffer of `(countX + 1) * 2` entries that is **not cleared** between rows
//!   or frames and drawn completely after every row: slots a row does not fill (always at least the last pair, since
//!   `posX = 1` is skipped) keep their old content -- initially `(0, 0)`, i.e. an extra line from the image's
//!   bottom-left corner per row. The index buffer is created by the first draw and never resized, while the GL vertex
//!   buffer follows every frame's vertex count: indices past its end (a shrinking `mv_x`) fetch zeros.

use crate::frame::MotionVectors;
use crate::geom::WfVertex;
use crate::warp_mesh::MeshGeometry;

/// State of projectM's `m_motionVectorMesh` (vertex buffer content + index count), one per preset.
#[derive(Default, Clone, Debug)]
pub struct MotionVectorState {
    buf: Vec<[f32; 2]>,
    /// `m_indices.Size()` after the first draw (`None` until then).
    index_count: Option<usize>,
}

/// Convert to half float and back the way projectM's `RG16F` render target stores a fragment: **truncated toward
/// zero** (measured: nearest-even makes 30 % of the motion vector lines of a rotating warp one pixel off, truncation
/// 0.4 %); a finite value beyond the half range saturates to the largest finite half (65504, truncation never rounds
/// up to infinity: projectM stores 65504 there); +-inf and NaN stay.
pub fn f16_round(x: f32) -> f32 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let e = ((x.to_bits() >> 23) & 0xff) as i32 - 127;
    let q = if e < -14 {
        2f32.powi(-24) // subnormal half: fixed quantum
    } else {
        2f32.powi(e - 10)
    };
    let r = (x / q).trunc() * q;
    if r.abs() > 65504.0 {
        65504.0f32.copysign(x)
    } else {
        r
    }
}

/// The warp-UV map texel of image pixel `(i, j)` (row 0 = top): the mesh UVs interpolated at the pixel centre,
/// as stored in the `RG16F` attachment.
fn uv_texel(
    g: &MeshGeometry,
    uv: &[[f32; 2]],
    w: u32,
    h: u32,
    i: i64,
    j: i64,
    diag_flip: bool,
) -> [f32; 2] {
    let (gx, gy) = (g.gx as i64, g.gy as i64);
    // window position of the pixel centre, y up: mesh row 0 is the *bottom* of the image
    let (px, py) = (i as f64 + 0.5, (h as i64 - 1 - j) as f64 + 0.5);
    let (u, r) = (px / w as f64 * gx as f64, py / h as f64 * gy as f64);
    let cx = (u.floor() as i64).clamp(0, gx - 1);
    let cy = (r.floor() as i64).clamp(0, gy - 1);
    let (fu, fv) = (u - cx as f64, r - cy as f64);
    // The triangle containing the point (cell diagonal a-d when `diag_flip`, else b-c), its vertices in the order
    // the GPU draws them (as projectM's UV map shows for the a-d mesh: upper triangle c d a, lower
    // d a b; the b-c order is the same pattern, unmeasured: projectM always draws the a-d mesh). Only the three
    // vertices take part (a NaN UV of the fourth must not leak in, while a NaN in the triangle poisons every
    // pixel of it, like the GPU's plane-equation interpolation).
    let offs: [(i64, i64); 3] = if diag_flip {
        if fu < fv {
            [(0, 1), (1, 1), (0, 0)]
        } else {
            [(1, 1), (0, 0), (1, 0)]
        }
    } else if fu + fv <= 1.0 {
        [(1, 0), (0, 0), (0, 1)]
    } else {
        [(0, 1), (1, 1), (1, 0)]
    };
    // The GPU rasterises the triangle from its vertices snapped to the 1/256 pixel sub-position grid (ties to even)
    // and evaluates every attribute as a plane `t0 + a * (x - x0) + b * (y - y0)` in f32, the coefficients from the
    // snapped corners. (This matches projectM's own UV map to 0-2 texels in 115 200 per frame; the unsnapped mesh
    // fractions are not close enough: motion vector lines move by a pixel.)
    let sv = |v: f64| ((v * 256.0).round_ties_even() / 256.0) as f32;
    let p: [(f32, f32); 3] = offs.map(|(ox, oy)| {
        (
            sv((cx + ox) as f64 / gx as f64 * w as f64),
            sv((cy + oy) as f64 / gy as f64 * h as f64),
        )
    });
    let (dx1, dy1, dx2, dy2) = (
        p[1].0 - p[0].0,
        p[1].1 - p[0].1,
        p[2].0 - p[0].0,
        p[2].1 - p[0].1,
    );
    let det = dx1 * dy2 - dx2 * dy1;
    let (ex, ey) = (px as f32 - p[0].0, py as f32 - p[0].1);
    let t = offs.map(|(ox, oy)| uv[g.index((cx + ox) as u32, (cy + oy) as u32)]);
    let plane = |c: usize| {
        let (du1, du2) = (t[1][c] - t[0][c], t[2][c] - t[0][c]);
        let a = (du1 * dy2 - du2 * dy1) / det;
        let b = (du2 * dx1 - du1 * dx2) / det;
        f16_round(t[0][c] + a * ex + b * ey)
    };
    [plane(0), plane(1)]
}

/// The sampler's position along one axis for the normalised coordinate `c`: the first texel `x0` and the weight of
/// `x0 + 1`. Measured on projectM's GPU (Mesa iris, `RG16F`, `GL_LINEAR`, 57 600
/// random lookups per axis, 0 mismatches): the coordinate is first **truncated to 22 fractional bits**, scaled to
/// texels (`- 0.5`) without further rounding, and the fraction becomes the weight `k / 256` with `k` rounded half up.
/// (Plain f32 `c * size - 0.5` with a rounded weight picks the neighbouring weight for 0.7 % of the lookups; with a
/// length multiplier of 40 one weight step is 0.2 px of line end.)
fn sample_pos(c: f32, size: u32) -> (i64, f64) {
    let cq = (f64::from(c) * 4_194_304.0).floor() / 4_194_304.0;
    let t = cq * f64::from(size) - 0.5;
    let x0 = t.floor();
    let k = ((t - x0) * 256.0 + 0.5).floor();
    if k >= 256.0 {
        (x0 as i64 + 1, 0.0)
    } else {
        (x0 as i64, k / 256.0)
    }
}

/// `texture(warp_coordinates, (x, 1 - y))` with `GL_LINEAR` / clamp to edge, as the motion vector vertex shader calls
/// it. `(x, y)` is the preset-space position (y down). The map is stored upside down: texture row `m` holds image row
/// `h - 1 - m`. The four texels are blended exactly (the GPU's result equals the f64 weighted sum rounded to f32:
/// 0 mismatches in 115 200 lookups of a projectM UV map, f32 evaluation orders are off in 0.1 - 3 %).
fn lookup(
    g: &MeshGeometry,
    uv: &[[f32; 2]],
    w: u32,
    h: u32,
    diag_flip: bool,
    x: f32,
    y: f32,
) -> [f32; 2] {
    let (x0, fx) = sample_pos(x, w);
    let (m0, fy) = sample_pos(1.0 - y, h);
    let t = |i: i64, m: i64| {
        uv_texel(
            g,
            uv,
            w,
            h,
            i.clamp(0, i64::from(w) - 1),
            i64::from(h) - 1 - m.clamp(0, i64::from(h) - 1),
            diag_flip,
        )
    };
    let (a, b, c, d) = (t(x0, m0), t(x0 + 1, m0), t(x0, m0 + 1), t(x0 + 1, m0 + 1));
    let mut o = [0.0f32; 2];
    for k in 0..2 {
        let top = f64::from(a[k]) * (1.0 - fx) + f64::from(b[k]) * fx;
        let bot = f64::from(c[k]) * (1.0 - fx) + f64::from(d[k]) * fx;
        o[k] = (top * (1.0 - fy) + bot * fy) as f32;
    }
    o
}

impl MotionVectorState {
    /// New preset / resize: `MotionVectors` and its mesh are recreated.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Clip-space line segments (pairs of vertices) of one frame, in draw order (one `glDrawElements` of the whole
    /// buffer per valid grid row), written into `out` (cleared first). `prev_uv` = the previous frame's warp UV grid,
    /// `min_len_px` = 1.25.
    #[allow(clippy::too_many_arguments)]
    pub fn segments(
        &mut self,
        out: &mut Vec<WfVertex>,
        mv: &MotionVectors,
        g: &MeshGeometry,
        prev_uv: &[[f32; 2]],
        w: u32,
        h: u32,
        diag_flip: bool,
        min_len_px: f32,
        color: [f32; 4],
    ) {
        out.clear();
        let mut nx = mv.x as i32;
        let mut ny = mv.y as i32;
        if nx <= 0 || ny <= 0 {
            return;
        }
        let mut dx = mv.x - nx as f32;
        let mut dy = mv.y - ny as f32;
        if nx > 64 {
            nx = 64;
            dx = 0.0;
        }
        if ny > 48 {
            ny = 48;
            dy = 0.0;
        }
        let (dx2, dy2) = (mv.dx, mv.dy);
        dx = dx.clamp(0.0, 1.0);
        dy = dy.clamp(0.0, 1.0);
        let (wf, hf) = (w as f32, h as f32);
        let (iw, ih) = (min_len_px / wf, min_len_px / hf);
        let min_len = (iw * iw + ih * ih).sqrt();

        // SetVertexCount: resize keeps the old entries, new ones are zero
        self.buf.resize((nx as usize + 1) * 2, [0.0, 0.0]);

        for y in 0..ny {
            let pos_y = (y as f32 + 0.25) / (ny as f32 + dy + 0.25 - 1.0) - dy2;
            if !(pos_y > 0.0001 && pos_y < 0.9999) {
                continue;
            }
            let mut vertex = 0usize;
            for x in 0..nx {
                let pos_x = (x as f32 + 0.25) / (nx as f32 + dx + 0.25 - 1.0) + dx2;
                if pos_x > 0.0001 && pos_x < 0.9999 {
                    self.buf[vertex] = [pos_x, pos_y];
                    self.buf[vertex + 1] = [pos_x, pos_y];
                    vertex += 2;
                }
            }
            // Mesh::Draw: the index buffer is created once, with the vertex count of the first draw
            let n = *self.index_count.get_or_insert(self.buf.len());
            // The GL buffer is re-allocated at the vertex count of every frame while the index buffer keeps the first
            // draw's size: when the grid shrinks (animated `mv_x`) the indices past the end of the vertex buffer fetch
            // zeros, i.e. more lines from the image's bottom-left corner.
            for k in 0..n {
                let pos = self.buf.get(k).copied().unwrap_or([0.0, 0.0]);
                let p = if k % 2 == 1 {
                    // reverse propagation through the previous frame's warp UVs (vertex shader)
                    let old = lookup(g, prev_uv, w, h, diag_flip, pos[0], pos[1]);
                    let mut dist = [(old[0] - pos[0]) * mv.l, (old[1] - pos[1]) * mv.l];
                    let mut len = (dist[0] * dist[0] + dist[1] * dist[1]).sqrt();
                    if len > min_len {
                    } else if len > 0.000_000_01 {
                        len = min_len / len;
                        dist[0] *= len;
                        dist[1] *= len;
                    } else {
                        dist = [min_len, min_len];
                    }
                    [pos[0] + dist[0], pos[1] + dist[1]]
                } else {
                    pos
                };
                // pos * 2 - 1, then the y flip of the shader and the one of the ortho matrix cancel
                if p[0].is_nan() || p[1].is_nan() {
                    // a NaN vertex (NaN in the previous warp UV map) makes the GPU drop the whole line
                    if k % 2 == 1 {
                        out.pop();
                    }
                    continue;
                }
                out.push(WfVertex {
                    pos: [p[0] * 2.0 - 1.0, p[1] * 2.0 - 1.0],
                    color,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_rounding() {
        assert_eq!(f16_round(0.0), 0.0);
        assert_eq!(f16_round(1.0), 1.0);
        // 11 significant bits: 1 + 2^-11 is the first value below the half-ulp tie at 1 + 2^-10 / 2
        assert_eq!(f16_round(1.0 + 2f32.powi(-11)), 1.0);
        assert_eq!(f16_round(1.0 + 3.0 * 2f32.powi(-11)), 1.0 + 2f32.powi(-10)); // truncated, not rounded
        assert_eq!(f16_round(0.5 + 2f32.powi(-12)), 0.5); // quantum 2^-11 in [0.5, 1)
        assert_eq!(f16_round(0.999_999), 1.0 - 2f32.powi(-11));
        assert!((f16_round(0.3) - 0.299_804_7).abs() < 1e-7);
        // the RTZ conversion saturates at the largest finite half (65504); only a real infinity stays infinite
        assert_eq!(f16_round(70000.0), 65504.0);
        assert_eq!(f16_round(-1.0e30), -65504.0);
        assert_eq!(f16_round(f32::INFINITY), f32::INFINITY);
        assert_eq!(f16_round(-0.3), -f16_round(0.3));
    }

    #[test]
    fn sampler_position_truncates_the_coordinate_and_rounds_the_weight_half_up() {
        // texel position 4 + 1/512 exactly (a weight tie: 0.5 / 256): rounded half up to 1 / 256
        let c = (4.0 + 0.5 + 1.0 / 512.0) / 256.0;
        assert_eq!(sample_pos(c, 256), (4, 1.0 / 256.0));
        // one f32 step below the tie: the coordinate is truncated to 22 bits first (2^-22 * 256 = 2^-14 texel)
        let below = f32::from_bits(c.to_bits() - 1);
        assert_eq!(sample_pos(below, 256), (4, 0.0));
        // the weight rounds up into the next texel
        assert_eq!(sample_pos((4.0 + 0.5 + 255.6 / 256.0) / 256.0, 256).0, 5);
        assert_eq!(sample_pos(0.5, 320), (159, 0.5));
    }

    fn identity_uv(g: &MeshGeometry) -> Vec<[f32; 2]> {
        // uv = image position: u = x/2+.5, v = 1 - (y/2+.5) (mesh row 0 = bottom)
        (0..g.vertex_count())
            .map(|n| [g.x[n] * 0.5 + 0.5, 0.5 - g.y[n] * 0.5])
            .collect()
    }

    #[test]
    fn identity_warp_gives_minimum_length_lines_mirrored_in_y() {
        let (w, h) = (640u32, 360u32);
        let g = MeshGeometry::new(48, 36, w, h).unwrap();
        let uv = identity_uv(&g);
        let mut st = MotionVectorState::default();
        let mv = MotionVectors {
            x: 64.0,
            y: 48.0,
            l: 1.0,
            ..MotionVectors::default()
        };
        let mut segs = Vec::new();
        st.segments(&mut segs, &mv, &g, &uv, w, h, true, 1.25, [1.0; 4]);
        assert!(!segs.is_empty());
        // 48 rows (46 valid + ...) each drawing (64 + 1) pairs: the first pair of the first row
        let (a, b) = (segs[0].pos, segs[1].pos);
        // start: posX = 0.25/63.25, posY = 0.25/47.25 -> clip (2posX-1, 2posY-1): *bottom* part (clip y -0.99)
        assert!((a[0] - (2.0 * 0.25 / 63.25 - 1.0)).abs() < 1e-6);
        assert!((a[1] - (2.0 * 0.25 / 47.25 - 1.0)).abs() < 1e-6);
        // identity warp: oldUV == pos up to f16 rounding, so |dist| << min length -> stretched to the minimum
        let min = ((1.25f32 / 640.0).powi(2) + (1.25f32 / 360.0).powi(2)).sqrt();
        let dxy = [(b[0] - a[0]) / 2.0, (b[1] - a[1]) / 2.0];
        let len = (dxy[0] * dxy[0] + dxy[1] * dxy[1]).sqrt();
        assert!((len - min).abs() < 1e-4, "{len} vs {min}");
    }

    #[test]
    fn shrinking_grid_reads_zero_vertices_past_the_vertex_buffer() {
        let (w, h) = (640u32, 360u32);
        let g = MeshGeometry::new(48, 36, w, h).unwrap();
        let uv = identity_uv(&g);
        let mut st = MotionVectorState::default();
        let mut mv = MotionVectors {
            x: 64.0,
            y: 48.0,
            l: 0.0,
            ..MotionVectors::default()
        };
        let mut first = Vec::new();
        st.segments(&mut first, &mv, &g, &uv, w, h, true, 1.25, [1.0; 4]);
        let per_row = 65 * 2;
        assert_eq!(first.len() % per_row, 0);
        // 62 columns: 126 vertices in the buffer, but the index buffer still draws the first frame's 130
        mv.x = 62.0;
        let mut second = Vec::new();
        st.segments(&mut second, &mv, &g, &uv, w, h, true, 1.25, [1.0; 4]);
        assert_eq!(second.len() % per_row, 0);
        let row = &second[..per_row];
        // the last two pairs (vertices 126..130) are out of bounds: both ends of their lines start in the corner
        for pair in row[per_row - 4..].chunks(2) {
            assert!((pair[0].pos[0] + 1.0).abs() < 1e-6 && (pair[0].pos[1] + 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn unfilled_slots_draw_corner_lines_every_row() {
        let (w, h) = (640u32, 360u32);
        let g = MeshGeometry::new(48, 36, w, h).unwrap();
        let uv = identity_uv(&g);
        let mut st = MotionVectorState::default();
        let mv = MotionVectors {
            x: 64.0,
            y: 48.0,
            l: 0.0,
            ..MotionVectors::default()
        };
        let mut segs = Vec::new();
        st.segments(&mut segs, &mv, &g, &uv, w, h, true, 1.25, [1.0; 4]);
        // posX = 1 (x = 63) is skipped: slots 63 and 64 stay at (0, 0) -> the last two pairs of every row
        let per_row = 65 * 2;
        assert_eq!(segs.len() % per_row, 0);
        let rows = segs.len() / per_row;
        assert!(rows >= 46);
        let last = &segs[per_row - 4..per_row];
        for s in last.chunks(2) {
            assert!((s[0].pos[0] + 1.0).abs() < 1e-6 && (s[0].pos[1] + 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn uv_map_texels_of_a_linear_field_are_the_pixel_centres_in_half_floats() {
        let (w, h) = (640u32, 360u32);
        let g = MeshGeometry::new(48, 36, w, h).unwrap();
        let uv = identity_uv(&g);
        for flip in [true, false] {
            for &(i, j) in &[(0i64, 0i64), (20, 33), (639, 359), (321, 180)] {
                let t = uv_texel(&g, &uv, w, h, i, j, flip);
                let want = [
                    f16_round((i as f32 + 0.5) / 640.0),
                    f16_round((j as f32 + 0.5) / 360.0),
                ];
                assert!((t[0] - want[0]).abs() <= 2f32.powi(-10), "{t:?} {want:?}");
                assert!((t[1] - want[1]).abs() <= 2f32.powi(-10), "{t:?} {want:?}");
            }
        }
    }

    #[test]
    fn uv_map_follows_the_cell_diagonal() {
        let (w, h) = (640u32, 360u32);
        let g = MeshGeometry::new(48, 36, w, h).unwrap();
        // u = x * y (bilinear, not linear): the value at a pixel off the diagonal depends on the triangulation
        let uv: Vec<[f32; 2]> = (0..g.vertex_count())
            .map(|n| {
                let (x, y) = (g.x[n] * 0.5 + 0.5, 0.5 - g.y[n] * 0.5);
                [x * y, y]
            })
            .collect();
        // pixel in the lower-left corner of a cell (fu small, fv large): both triangulations disagree there
        let (i, j) = (6i64, 100i64);
        let a = uv_texel(&g, &uv, w, h, i, j, true);
        let b = uv_texel(&g, &uv, w, h, i, j, false);
        assert!((a[0] - b[0]).abs() > 1e-6, "{a:?} {b:?}");
    }

    #[test]
    fn uv_map_weights_come_from_the_snapped_triangle() {
        // 640 / 48 px per cell is not on the 1/256 sub-pixel grid: the rasteriser's triangle has vertices moved by
        // up to 1/512 px, which shifts the barycentric weight of a pixel next to a vertex by ~1e-4 (exact mesh
        // fraction 0.0125, snapped-triangle weight 0.0125952 -- value computed from the snapped corners by hand)
        let (w, h) = (640u32, 360u32);
        let g = MeshGeometry::new(48, 36, w, h).unwrap();
        let mut uv = vec![[0.0f32; 2]; g.vertex_count()];
        uv[g.index(5, 6)] = [1.0, 0.0];
        let t = uv_texel(&g, &uv, w, h, 53, 300, true);
        assert!((t[0] - 0.012_595_2).abs() < 1.5e-5, "{t:?}");
        assert!((t[0] - 0.0125).abs() > 5e-5, "{t:?}");
    }

    #[test]
    fn nan_uv_poisons_its_triangle_only() {
        let (w, h) = (64u32, 64u32);
        let g = MeshGeometry::new(2, 2, w, h).unwrap();
        let mut uv = identity_uv(&g);
        let nan = g.index(2, 1); // right edge, middle row: belongs to cells (1,0) and (1,1) only
        uv[nan] = [f32::NAN, f32::NAN];
        // a pixel in the left half never touches that vertex
        let t = uv_texel(&g, &uv, w, h, 8, 32, true);
        assert!(t[0].is_finite() && t[1].is_finite());
        // a pixel next to the NaN vertex is NaN
        let t = uv_texel(&g, &uv, w, h, 62, 33, true);
        assert!(t[0].is_nan() && t[1].is_nan());
    }
}
