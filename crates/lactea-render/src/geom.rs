// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! CPU-side geometry builders: warp / composite meshes, D3D9-style 1-pixel lines and points as
//! thin quads, shapes, borders, motion vectors. All outputs are clip space (x right, y up) with
//! row 0 of the render target at the top (`y = +1`).

use crate::config::CompatRules;
use crate::frame::{MotionVectors, ShapeInstance, WavePoint};
use crate::warp_mesh::MeshGeometry;
use bytemuck::{Pod, Zeroable};
use lactea_abi::MeshVertex;

/// Flat-colour vertex (`WFVERTEX`): clip position + RGBA.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct WfVertex {
    pub pos: [f32; 2],
    pub color: [f32; 4],
}

/// Comp mesh vertices per row (`FCGSX`) and column (`FCGSY`): projectM's 32x24 composite grid.
pub const FCGSX: usize = 32;
pub const FCGSY: usize = 24;

/// `D3DCOLOR_RGBA_01` component: `((int)(x*255)) & 0xFF`, as 0..1 float.
pub fn d3d_color_component(x: f32) -> f32 {
    (((x * 255.0) as i32) & 0xFF) as f32 / 255.0
}

/// projectM `Color::Modulo`: wrap into `[0, 256/255)` (emulates the `& 0xFF` of the D3D macro on float colours).
pub fn modulo_color_component(x: f32) -> f32 {
    if (0.0..=1.0).contains(&x) {
        return x;
    }
    let m = 256.0 / 255.0;
    ((x % m) + m) % m
}

/// Clip-space position of a custom-wave point (`CustomWaveform::Draw` / MD2 `DrawCustomWaves`): the point is `x * 2 - 1`
/// of the VM's double `x`, scaled by the inverse aspect. projectM rounds once, after the double product
/// (`CompatRules::wave_point_f64_aspect`); MD2 rounds the double `x * 2 - 1` to `f32` first. (y is returned for
/// "y up" clip space: projectM's `y * -2 + 1` goes through a y-flipping projection, the negation is exact.)
pub fn wave_point_clip(rules: &CompatRules, p: &WavePoint, inv_aspect: (f32, f32)) -> [f32; 2] {
    let (x, y) = (p.x * 2.0 - 1.0, p.y * 2.0 - 1.0);
    if rules.wave_point_f64_aspect {
        [
            (x * f64::from(inv_aspect.0)) as f32,
            (y * f64::from(inv_aspect.1)) as f32,
        ]
    } else {
        [x as f32 * inv_aspect.0, y as f32 * inv_aspect.1]
    }
}

/// Apply the compat colour policy to an RGBA colour.
pub fn conv_color(rules: &CompatRules, c: [f32; 4]) -> [f32; 4] {
    let f = if rules.quantize_vertex_colors {
        d3d_color_component
    } else {
        modulo_color_component
    };
    [f(c[0]), f(c[1]), f(c[2]), f(c[3])]
}

/// Colour as projectM passes it straight to `glVertexAttrib4f` (motion vectors, borders, main
/// wave, shape outlines): no wrap, the output stage clamps. MD2 quantizes like `D3DCOLOR_RGBA_01`.
pub fn conv_color_raw(rules: &CompatRules, c: [f32; 4]) -> [f32; 4] {
    if rules.quantize_vertex_colors {
        [
            d3d_color_component(c[0]),
            d3d_color_component(c[1]),
            d3d_color_component(c[2]),
            d3d_color_component(c[3]),
        ]
    } else {
        c
    }
}

/// `((int)(a * 255 * alpha_mult)) & 0xFF` as a 0..1 float: the alpha byte MD2's shape / wave drawing builds while two
/// presets blend (`DrawCustomShapes` / `DrawCustomWaves`: `var_pf_a * 255 * alpha_mult`).
fn d3d_alpha_component(a: f32, alpha_mult: f32) -> f32 {
    ((((a * 255.0) * alpha_mult) as i32) & 0xFF) as f32 / 255.0
}

/// [`conv_color`] with the alpha scaled by `alpha_mult` (1 = [`conv_color`] exactly).
pub fn conv_color_alpha_mult(rules: &CompatRules, c: [f32; 4], alpha_mult: f32) -> [f32; 4] {
    let mut o = conv_color(rules, c);
    if alpha_mult != 1.0 {
        o[3] = if rules.quantize_vertex_colors {
            d3d_alpha_component(c[3], alpha_mult)
        } else {
            modulo_color_component(c[3] * alpha_mult)
        };
    }
    o
}

/// [`conv_color_raw`] with the alpha scaled by `alpha_mult` (1 = [`conv_color_raw`] exactly).
pub fn conv_color_raw_alpha_mult(rules: &CompatRules, c: [f32; 4], alpha_mult: f32) -> [f32; 4] {
    let mut o = conv_color_raw(rules, c);
    if alpha_mult != 1.0 {
        o[3] = if rules.quantize_vertex_colors {
            d3d_alpha_component(c[3], alpha_mult)
        } else {
            c[3] * alpha_mult
        };
    }
    o
}

#[inline]
fn to_px(p: [f32; 2], w: f32, h: f32) -> [f32; 2] {
    [(p[0] * 0.5 + 0.5) * w, (0.5 - p[1] * 0.5) * h]
}
#[inline]
fn from_px(p: [f32; 2], w: f32, h: f32) -> [f32; 2] {
    [p[0] / w * 2.0 - 1.0, 1.0 - p[1] / h * 2.0]
}

fn push_quad(
    out: &mut Vec<WfVertex>,
    a0: [f32; 2],
    a1: [f32; 2],
    b0: [f32; 2],
    b1: [f32; 2],
    w: f32,
    h: f32,
    color: [f32; 4],
) {
    let v = |p: [f32; 2]| WfVertex {
        pos: from_px(p, w, h),
        color,
    };
    out.extend_from_slice(&[v(a0), v(a1), v(b0), v(b0), v(a1), v(b1)]);
}

/// One D3D9 1-pixel line segment as a thin quad. The quad is exactly one pixel tall (x-major
/// lines) or wide (y-major lines) and spans the segment's major-axis extent, so every pixel
/// centre inside the extent is covered exactly once — the same pixel set a Bresenham-style
/// (diamond-exit) rasteriser produces, up to tie-breaking at exact half-pixel positions.
/// Zero-length segments produce nothing, like D3D9.
pub fn push_line(
    out: &mut Vec<WfVertex>,
    p0: [f32; 2],
    p1: [f32; 2],
    w: f32,
    h: f32,
    color: [f32; 4],
) {
    let a = to_px(p0, w, h);
    let b = to_px(p1, w, h);
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    if dx.abs() >= dy.abs() {
        push_quad(
            out,
            [a[0], a[1] - 0.5],
            [a[0], a[1] + 0.5],
            [b[0], b[1] - 0.5],
            [b[0], b[1] + 0.5],
            w,
            h,
            color,
        );
    } else {
        push_quad(
            out,
            [a[0] - 0.5, a[1]],
            [a[0] + 0.5, a[1]],
            [b[0] - 0.5, b[1]],
            [b[0] + 0.5, b[1]],
            w,
            h,
            color,
        );
    }
}

/// A line strip as `LineList` vertex pairs into a reusable buffer, cleared first (`GL_LINE_STRIP` semantics: every
/// segment is rasterised on its own; vertex colours interpolate along each segment). `color_of(i)` is the colour of
/// point `i`.
pub fn strip_segments_into(
    out: &mut Vec<WfVertex>,
    pts: &[[f32; 2]],
    color_of: impl Fn(usize) -> [f32; 4],
) {
    out.clear();
    out.reserve(pts.len().saturating_sub(1) * 2);
    for i in 0..pts.len().saturating_sub(1) {
        out.push(WfVertex {
            pos: pts[i],
            color: color_of(i),
        });
        out.push(WfVertex {
            pos: pts[i + 1],
            color: color_of(i + 1),
        });
    }
}

/// `D3DPT_POINTLIST` with `D3DRS_POINTSIZE = size`: an axis-aligned `size`x`size` pixel square
/// centred on the vertex.
pub fn push_point(
    out: &mut Vec<WfVertex>,
    p: [f32; 2],
    size: f32,
    w: f32,
    h: f32,
    color: [f32; 4],
) {
    let c = to_px(p, w, h);
    let r = size * 0.5;
    push_quad(
        out,
        [c[0] - r, c[1] - r],
        [c[0] - r, c[1] + r],
        [c[0] + r, c[1] - r],
        [c[0] + r, c[1] + r],
        w,
        h,
        color,
    );
}

/// Triangle fan `v[0], v[1..]` -> triangle list. `colors` per vertex.
pub fn fan_to_list(pts: &[[f32; 2]], colors: &[[f32; 4]]) -> Vec<WfVertex> {
    let mut out = Vec::with_capacity(3 * pts.len().saturating_sub(2));
    push_fan_colors(&mut out, pts, colors);
    out
}

/// [`fan_to_list`] appended to `out`.
pub fn push_fan_colors(out: &mut Vec<WfVertex>, pts: &[[f32; 2]], colors: &[[f32; 4]]) {
    out.reserve(3 * pts.len().saturating_sub(2));
    for i in 1..pts.len() - 1 {
        for k in [0, i, i + 1] {
            out.push(WfVertex {
                pos: pts[k],
                color: colors[k],
            });
        }
    }
}

/// [`fan_to_list`] appended to `out` for a fan whose centre has colour `centre` and every other vertex `rim`.
pub fn push_fan(out: &mut Vec<WfVertex>, pts: &[[f32; 2]], centre: [f32; 4], rim: [f32; 4]) {
    out.reserve(3 * pts.len().saturating_sub(2));
    for i in 1..pts.len() - 1 {
        for k in [0, i, i + 1] {
            out.push(WfVertex {
                pos: pts[k],
                color: if k == 0 { centre } else { rim },
            });
        }
    }
}

// -----------------------------------------------------------------------------------------
// warp mesh
// -----------------------------------------------------------------------------------------

/// Triangle-list vertices of the warp mesh (`m_indices_list` order is irrelevant: no blending).
///
/// * `diffuse`: vertex colour (no-shader: `(decay,decay,decay,1)`).
/// * `uv`: per-vertex `(tu, tv)`; `texel_offset_uv` is added to `uv_orig` (MD2 `tu_orig`).
/// * `diag_flip`: cell triangulation of projectM (see below) instead of MD2's.
/// * `seam_fix`: MD2's hack for the angle-wrap seam: vertices of the left half of the middle row
///   get `ang = -pi` in triangles of the lower half and `+pi` in the upper half.
pub fn build_warp_vertices(
    g: &MeshGeometry,
    uv: &[[f32; 2]],
    diffuse: [f32; 4],
    texel_offset_uv: [f32; 2],
    seam_fix: bool,
    diag_flip: bool,
) -> Vec<MeshVertex> {
    warp_vertices_impl(
        g,
        uv,
        &|_| diffuse,
        &|_| true,
        texel_offset_uv,
        seam_fix,
        diag_flip,
    )
}

/// `WarpedBlit_*` while MD2 blends two presets: the same triangles as [`build_warp_vertices`] (MD2 cell
/// triangulation), every vertex carrying `(rgb, alpha[n] / 255)` (`Diffuse = rgb | (mix2*255) << 24`), and with
/// `cull = Some(flip_culling)` the triangles the pass skips left out (`bCullTiles` / `bFlipCulling`, see
/// [`crate::blend::tile_needed`]).
pub fn build_warp_vertices_blend(
    g: &MeshGeometry,
    uv: &[[f32; 2]],
    rgb: [f32; 3],
    alpha: &[u8],
    texel_offset_uv: [f32; 2],
    seam_fix: bool,
    cull: Option<bool>,
) -> Vec<MeshVertex> {
    warp_vertices_impl(
        g,
        uv,
        &|n| [rgb[0], rgb[1], rgb[2], f32::from(alpha[n]) / 255.0],
        &|t| match cull {
            Some(flip) => crate::blend::tile_needed([alpha[t[0]], alpha[t[1]], alpha[t[2]]], flip),
            None => true,
        },
        texel_offset_uv,
        seam_fix,
        false,
    )
}

fn warp_vertices_impl(
    g: &MeshGeometry,
    uv: &[[f32; 2]],
    diffuse_of: &dyn Fn(usize) -> [f32; 4],
    keep: &dyn Fn([usize; 3]) -> bool,
    texel_offset_uv: [f32; 2],
    seam_fix: bool,
    diag_flip: bool,
) -> Vec<MeshVertex> {
    let (gx, gy) = (g.gx, g.gy);
    let mut out = Vec::with_capacity((gx * gy * 6) as usize);
    let mk = |x: u32, y: u32, lower_half: bool| -> MeshVertex {
        let n = g.index(x, y);
        let mut ang = g.ang[n];
        if seam_fix && y == gy / 2 && x < gx / 2 {
            ang = if lower_half {
                -std::f32::consts::PI
            } else {
                std::f32::consts::PI
            };
        }
        MeshVertex {
            pos: [g.x[n], g.y[n]],
            diffuse: diffuse_of(n),
            uv: [
                uv[n][0],
                uv[n][1],
                g.x[n] * 0.5 + 0.5 + texel_offset_uv[0],
                -g.y[n] * 0.5 + 0.5 + texel_offset_uv[1],
            ],
            rad_ang: [g.rad[n], ang],
        }
    };
    for cy in 0..gy {
        let lower = cy < gy / 2;
        for cx in 0..gx {
            let a = mk(cx, cy, lower);
            let b = mk(cx + 1, cy, lower);
            let c = mk(cx, cy + 1, lower);
            let d = mk(cx + 1, cy + 1, lower);
            let (na, nb, nc, nd) = (
                g.index(cx, cy),
                g.index(cx + 1, cy),
                g.index(cx, cy + 1),
                g.index(cx + 1, cy + 1),
            );
            if diag_flip {
                // projectM's mesh has its row 0 at the top: the cell diagonal runs bottom-left to
                // top-right in image space, the other way round than MD2's (bottom-up rows).
                out.extend_from_slice(&[a, b, d, a, d, c]);
            } else {
                if keep([na, nb, nc]) {
                    out.extend_from_slice(&[a, b, c]);
                }
                if keep([nb, nc, nd]) {
                    out.extend_from_slice(&[b, c, d]);
                }
            }
        }
    }
    out
}

/// Vertex of projectM's warp mesh as its vertex shader consumes it (`PresetWarpVertexShaderGlsl330.vert`):
/// attribute 0 `vertex_position` (projectM's mesh `y`, row 0 = top), 3 `rad_ang`, 4 `transforms`
/// (`zoom, zoomExp, rot, warp`), 5 `warp_center`, 6 `warp_distance`, 7 `stretch`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct WarpVertex {
    pub pos: [f32; 2],
    pub rad_ang: [f32; 2],
    pub transforms: [f32; 4],
    pub center: [f32; 2],
    pub dist: [f32; 2],
    pub stretch: [f32; 2],
}

/// The vertex-shader inputs of mesh vertex `(x, y)` ([`build_warp_gpu_vertices`]).
pub fn warp_gpu_vertex(
    g: &MeshGeometry,
    vars: &[crate::warp_mesh::VertexVars],
    x: u32,
    y: u32,
) -> WarpVertex {
    let n = g.index(x, y);
    let (pos, rad, ang) = g.projectm_vertex(x, y);
    let v = &vars[n];
    WarpVertex {
        pos,
        rad_ang: [rad, ang],
        transforms: [v.zoom, v.zoomexp, v.rot, v.warp],
        center: [v.cx, v.cy],
        dist: [v.dx, v.dy],
        stretch: [v.sx, v.sy],
    }
}

/// The vertices of the warp mesh for the GPU vertex stage, one per mesh vertex in lactea mesh index order
/// ([`MeshGeometry::index`]; also the point order of the UV readback); `vars` is indexed the same way. Positions,
/// `rad` and `ang` are projectM's own floats ([`MeshGeometry::projectm_vertex`]). [`warp_gpu_indices`] gives the
/// triangles over them.
pub fn build_warp_gpu_vertices(
    g: &MeshGeometry,
    vars: &[crate::warp_mesh::VertexVars],
) -> Vec<WarpVertex> {
    let mut out = Vec::with_capacity(g.vertex_count());
    for y in 0..=g.gy {
        for x in 0..=g.gx {
            out.push(warp_gpu_vertex(g, vars, x, y));
        }
    }
    out
}

/// Triangle list of the warp mesh over [`build_warp_gpu_vertices`] (same cells / triangulation as
/// [`build_warp_vertices`]).
pub fn warp_gpu_indices(g: &MeshGeometry, diag_flip: bool) -> Vec<u32> {
    let (gx, gy) = (g.gx, g.gy);
    let mut out = Vec::with_capacity((gx * gy * 6) as usize);
    let ix = |x: u32, y: u32| g.index(x, y) as u32;
    for cy in 0..gy {
        for cx in 0..gx {
            let a = ix(cx, cy);
            let b = ix(cx + 1, cy);
            let c = ix(cx, cy + 1);
            let d = ix(cx + 1, cy + 1);
            if diag_flip {
                // projectM's index buffer: `(v, v+1, v+gx+1)`, `(v+1, v+gx+1, v+gx+2)` with `v` the top-left vertex
                // of its row-0-on-top mesh, i.e. `c d a` / `d a b` here. The order is the GPU's attribute-setup order.
                out.extend_from_slice(&[c, d, a, d, a, b]);
            } else {
                out.extend_from_slice(&[a, b, c, b, c, d]);
            }
        }
    }
    out
}

/// Triangles of the warp mesh with a NaN UV (projectM: `pow(negative zoom, ..)` is NaN on the GPU) sample the source
/// image at `(0, 0)` in every pixel (the sampler turns a NaN coordinate into 0): with `GL_REPEAT` that is the mean of
/// the four image corners, with clamping texel (0, 0). Measured against projectM: CAT Scan's flat blob at the zoom sign
/// change equals `mean(main corners) * decay` in every frame. Sets all three vertices' `(tu, tv)` to 0.
pub fn fix_nan_uv_triangles(verts: &mut [MeshVertex]) {
    for t in verts.as_chunks_mut::<3>().0 {
        if t.iter().any(|v| v.uv[0].is_nan() || v.uv[1].is_nan()) {
            for v in t.iter_mut() {
                v.uv[0] = 0.0;
                v.uv[1] = 0.0;
            }
        }
    }
}
// -----------------------------------------------------------------------------------------
// composite mesh
// -----------------------------------------------------------------------------------------

fn squish_to_center(x: f32, exp: f32) -> f32 {
    if x > 0.5 {
        return (x * 2.0 - 1.0).powf(exp) * 0.5 + 0.5;
    }
    (1.0 - (1.0 - x * 2.0).powf(exp)) * 0.5
}

/// `CPlugin::UvToMathSpace` (non screen-dependent mode).
fn uv_to_math_space(ax: f32, ay: f32, u: f32, v: f32) -> (f32, f32) {
    let px = (u * 2.0 - 1.0) * ax;
    let py = (v * 2.0 - 1.0) * ay;
    let rad = (px * px + py * py).sqrt() / (ax * ax + ay * ay).sqrt();
    let mut ang = py.atan2(px);
    if ang < 0.0 {
        ang += 6.2831853071796;
    }
    (rad, ang)
}

/// Hue-shader corner colours (`ShowToUser_Shaders`): returns `shade[corner][rgb]`.
/// `rand_start` = `m_fRandStart` (only indices 1..=3 are used).
pub fn hue_shade(time: f32, rand_start: [f32; 4]) -> [[f32; 3]; 4] {
    let mut shade = [[1.0f32; 3]; 4];
    for i in 0..4 {
        let fi = i as f32;
        shade[i][0] = 0.6 + 0.3 * (time * 30.0 * 0.0143 + 3.0 + fi * 21.0 + rand_start[3]).sin();
        shade[i][1] = 0.6 + 0.3 * (time * 30.0 * 0.0107 + 1.0 + fi * 13.0 + rand_start[1]).sin();
        shade[i][2] = 0.6 + 0.3 * (time * 30.0 * 0.0129 + 6.0 + fi * 9.0 + rand_start[2]).sin();
        let mut max = if shade[i][0] > shade[i][1] {
            shade[i][0]
        } else {
            shade[i][1]
        };
        if shade[i][2] > max {
            max = shade[i][2];
        }
        for k in 0..3 {
            shade[i][k] /= max;
            shade[i][k] = 0.5 + 0.5 * shade[i][k];
        }
    }
    shade
}

/// Triangle list of the 32x24 composite mesh (`m_comp_verts` / `m_comp_indices`).
/// Corner order of `shade`: 0 = (x=1,y=1 in 0..1 clip/2+.5 space) per `ShowToUser_Shaders`
/// interpolation weights `s0*x*y + s1*(1-x)*y + s2*x*(1-y) + s3*(1-x)*(1-y)`.
/// `uv_half_texel`: projectM adds `0.5/size` to the UVs.
pub fn comp_grid(
    w: u32,
    h: u32,
    aspect: (f32, f32),
    shade: &[[f32; 3]; 4],
    uv_half_texel: bool,
) -> Vec<MeshVertex> {
    let (ax, ay) = aspect;
    let half = if uv_half_texel {
        [0.5 / w as f32, 0.5 / h as f32]
    } else {
        [0.0, 0.0]
    };
    let div_x = 1.0 / (FCGSX - 2) as f32;
    let div_y = 1.0 / (FCGSY - 2) as f32;
    let pi = 3.1415926535898f32;
    let mut verts = vec![MeshVertex::default(); FCGSX * FCGSY];
    for j in 0..FCGSY {
        let j2 = j - j / (FCGSY / 2);
        let v = squish_to_center(j2 as f32 * div_y, 3.0);
        let sy = -(v * 2.0 - 1.0);
        for i in 0..FCGSX {
            let i2 = i - i / (FCGSX / 2);
            let u = squish_to_center(i2 as f32 * div_x, 3.0);
            let sx = u * 2.0 - 1.0;
            let (rad, mut ang) = uv_to_math_space(ax, ay, u, v);
            // fix-ups
            if i == FCGSX / 2 - 1 {
                ang = if j < FCGSY / 2 - 1 {
                    pi * 1.5
                } else if j == FCGSY / 2 - 1 {
                    pi * 1.25
                } else if j == FCGSY / 2 {
                    pi * 0.75
                } else {
                    pi * 0.5
                };
            } else if i == FCGSX / 2 {
                ang = if j < FCGSY / 2 - 1 {
                    pi * 1.5
                } else if j == FCGSY / 2 - 1 {
                    pi * 1.75
                } else if j == FCGSY / 2 {
                    pi * 0.25
                } else {
                    pi * 0.5
                };
            } else if j == FCGSY / 2 - 1 {
                ang = if i < FCGSX / 2 - 1 {
                    pi * 1.0
                } else {
                    pi * 2.0
                };
            } else if j == FCGSY / 2 {
                ang = if i < FCGSX / 2 - 1 {
                    pi * 1.0
                } else {
                    pi * 0.0
                };
            }
            // hue colour: interpolate the 4 corners
            let x01 = sx * 0.5 + 0.5;
            let y01 = sy * 0.5 + 0.5;
            let mut col = [1.0f32; 3];
            for c in 0..3 {
                col[c] = shade[0][c] * x01 * y01
                    + shade[1][c] * (1.0 - x01) * y01
                    + shade[2][c] * x01 * (1.0 - y01)
                    + shade[3][c] * (1.0 - x01) * (1.0 - y01);
            }
            verts[i + j * FCGSX] = MeshVertex {
                pos: [sx, sy],
                diffuse: [col[0], col[1], col[2], 1.0],
                uv: [u + half[0], v + half[1], 0.0, 0.0],
                rad_ang: [rad, ang],
            };
        }
    }
    verts
}

/// Vertex indices into the [`comp_grid`] of the composite triangle list (`m_comp_indices`), three per triangle.
pub fn comp_indices() -> Vec<usize> {
    let mut out = Vec::with_capacity((FCGSX - 2) * (FCGSY - 2) * 6);
    for y in 0..FCGSY - 1 {
        if y == FCGSY / 2 - 1 {
            continue;
        }
        for x in 0..FCGSX - 1 {
            if x == FCGSX / 2 - 1 {
                continue;
            }
            let left_half = x < FCGSX / 2;
            let top_half = y < FCGSY / 2;
            let center_4 =
                (x == FCGSX / 2 || x == FCGSX / 2 - 1) && (y == FCGSY / 2 || y == FCGSY / 2 - 1);
            let idx = |xx: usize, yy: usize| yy * FCGSX + xx;
            let tri: [usize; 6] = if (left_half as i32 + top_half as i32 + center_4 as i32) % 2 == 1
            {
                [
                    idx(x, y),
                    idx(x + 1, y),
                    idx(x + 1, y + 1),
                    idx(x + 1, y + 1),
                    idx(x, y + 1),
                    idx(x, y),
                ]
            } else {
                [
                    idx(x, y + 1),
                    idx(x, y),
                    idx(x + 1, y),
                    idx(x + 1, y),
                    idx(x + 1, y + 1),
                    idx(x, y + 1),
                ]
            };
            out.extend_from_slice(&tri);
        }
    }
    out
}

/// Triangle list of the 32x24 composite mesh ([`comp_grid`] drawn through [`comp_indices`]).
pub fn build_comp_vertices(
    w: u32,
    h: u32,
    aspect: (f32, f32),
    shade: &[[f32; 3]; 4],
    uv_half_texel: bool,
) -> Vec<MeshVertex> {
    let verts = comp_grid(w, h, aspect, shade, uv_half_texel);
    comp_indices().into_iter().map(|t| verts[t]).collect()
}

/// `VideoEcho::Draw`'s quad stretch (`aspectMultX/Y`), evaluated in `f32` exactly like projectM:
/// `aspect = W / (H * invAspectY)` with `invAspectY = 1 / aspectY` and `aspectY = H/W` for landscape targets, `1`
/// otherwise. For landscape targets the result is 1 up to rounding; for **portrait** targets (`W < H`) projectM keeps
/// `invAspectY = 1`, so `aspect = W/H < 1` and the quad is stretched by `H/W` in x while the UVs still span `0..1`:
/// the echo / gamma composite shows a horizontally magnified centre crop (a projectM quirk, kept by default).
pub fn echo_aspect_mult(w: u32, h: u32) -> [f32; 2] {
    let (wf, hf) = (w as f32, h as f32);
    let aspect_y = if w > h { hf / wf } else { 1.0 };
    let inv_aspect_y = 1.0 / aspect_y;
    let aspect = wf / (hf * inv_aspect_y);
    if aspect > 1.0 {
        [1.0, aspect]
    } else {
        [1.0 / aspect, 1.0]
    }
}

/// Full-screen quad(s) of `ShowToUser_NoShaders`: triangle list with per-corner colours.
/// Corner order: 0 = bottom-left, 1 = bottom-right, 2 = top-left, 3 = top-right (MD2 `v3[0..4]`).
/// `uv` per corner. The quad extends one pixel beyond the screen (`1 + 1/size`), scaled by `mult`
/// ([`echo_aspect_mult`] for projectM's echo / gamma quad, `[1, 1]` for MD2 and for the filter quads).
pub fn echo_quad(
    w: u32,
    h: u32,
    uv: [[f32; 2]; 4],
    colors: [[f32; 4]; 4],
    diag_flip: bool,
    mult: [f32; 2],
) -> Vec<MeshVertex> {
    let ex = (1.0 + 1.0 / w as f32) * mult[0];
    let ey = (1.0 + 1.0 / h as f32) * mult[1];
    let pos = [[-ex, -ey], [ex, -ey], [-ex, ey], [ex, ey]];
    let v = |i: usize| MeshVertex {
        pos: pos[i],
        diffuse: colors[i],
        uv: [uv[i][0], uv[i][1], 0.0, 0.0],
        rad_ang: [0.0; 2],
    };
    if diag_flip {
        // projectM draws the quad as the strip TL,TR,BL,BR: diagonal TR-BL (corners 3-0 here)
        vec![v(0), v(1), v(3), v(0), v(3), v(2)]
    } else {
        vec![v(0), v(1), v(2), v(2), v(1), v(3)]
    }
}

// -----------------------------------------------------------------------------------------
// motion vectors
// -----------------------------------------------------------------------------------------

/// Bilinear lookup of the previous frame's warp UV grid at fractional grid coords `(gxf, gyf)`
/// (`ReversePropagatePoint`). Returns `None` outside the grid.
fn bilerp_uv(g: &MeshGeometry, uv: &[[f32; 2]], gxf: f32, gyf: f32) -> Option<[f32; 2]> {
    let x0 = gxf as i32;
    let y0 = gyf as i32;
    let dx = gxf - x0 as f32;
    let dy = gyf - y0 as f32;
    let (x1, y1) = (x0 + 1, y0 + 1);
    if x0 < 0 || y0 < 0 || x1 > g.gx as i32 || y1 > g.gy as i32 {
        return None;
    }
    let at = |x: i32, y: i32| uv[g.index(x as u32, y as u32)];
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    let mut r = [0.0; 2];
    for k in 0..2 {
        r[k] = a[k] * (1.0 - dx) * (1.0 - dy)
            + b[k] * dx * (1.0 - dy)
            + c[k] * (1.0 - dx) * dy
            + d[k] * dx * dy;
    }
    Some(r)
}

/// Motion-vector line list (pairs of clip-space vertices) of `DrawMotionVectors`, written into `out` (cleared first).
/// `prev_uv` is the warp UV grid of the *previous* frame (what MD2's `m_verts` still holds at
/// that point of `RenderFrame`).
#[allow(clippy::too_many_arguments)]
pub fn motion_vector_lines(
    out: &mut Vec<WfVertex>,
    mv: &MotionVectors,
    g: &MeshGeometry,
    prev_uv: &[[f32; 2]],
    w: u32,
    h: u32,
    rules: &CompatRules,
    color: [f32; 4],
) {
    out.clear();
    let mut nx = mv.x as i32;
    let mut ny = mv.y as i32;
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
    if nx <= 0 || ny <= 0 {
        return;
    }
    let dx2 = mv.dx;
    let dy2 = mv.dy;
    let len_mult = mv.l;
    dx = dx.clamp(0.0, 1.0);
    dy = dy.clamp(0.0, 1.0);
    let (wf, hf) = (w as f32, h as f32);
    let min_len = if rules.mv_mirrored_lookup {
        1.0 / wf
    } else {
        rules.mv_min_len_px * ((1.0 / wf).powi(2) + (1.0 / hf).powi(2)).sqrt()
    };
    for y in 0..ny {
        let mut fy = (y as f32 + 0.25) / (ny as f32 + dy + 0.25 - 1.0);
        fy -= dy2;
        if !(fy > 0.0001 && fy < 0.9999) {
            continue;
        }
        for x in 0..nx {
            let mut fx = (x as f32 + 0.25) / (nx as f32 + dx + 0.25 - 1.0);
            fx += dx2;
            if !(fx > 0.0001 && fx < 0.9999) {
                continue;
            }
            // reverse propagation
            let (gxf, gyf) = if rules.mv_mirrored_lookup {
                (fx * g.gx as f32, fy * g.gy as f32)
            } else {
                (fx * g.gx as f32, (1.0 - fy) * g.gy as f32)
            };
            let Some(uv) = bilerp_uv(g, prev_uv, gxf, gyf) else {
                continue;
            };
            let fx2 = uv[0];
            let fy2 = if rules.mv_mirrored_lookup {
                1.0 - uv[1]
            } else {
                uv[1]
            };
            let mut ddx = (fx2 - fx) * len_mult;
            let mut ddy = (fy2 - fy) * len_mult;
            let mut len = (ddx * ddx + ddy * ddy).sqrt();
            if len > min_len {
            } else if len > 0.00000001 {
                len = min_len / len;
                ddx *= len;
                ddy *= len;
            } else {
                ddx = min_len;
                ddy = min_len;
            }
            let (ex, ey) = (fx + ddx, fy + ddy);
            out.push(WfVertex {
                pos: [fx * 2.0 - 1.0, 1.0 - fy * 2.0],
                color,
            });
            out.push(WfVertex {
                pos: [ex * 2.0 - 1.0, 1.0 - ey * 2.0],
                color,
            });
        }
    }
}

// -----------------------------------------------------------------------------------------
// shapes / borders / darken center
// -----------------------------------------------------------------------------------------

/// Geometry of one custom-shape instance (`DrawCustomShapes`).
#[derive(Default)]
pub struct ShapeGeom {
    /// Fan: centre + `sides` corners + closing corner (`sides + 2` points), clip space.
    pub pts: Vec<[f32; 2]>,
    /// Texture coordinates for the textured variant (same indexing); empty unless asked for.
    pub uv: Vec<[f32; 2]>,
    #[allow(dead_code)]
    pub sides: usize,
}

/// [`shape_geometry_tex_max`] with the projectM / MD2 limit of 100 sides, same aspect for the texture coordinates
/// (the renderer passes `CompatRules::max_shape_sides` itself).
#[cfg(test)]
pub fn shape_geometry(s: &ShapeInstance, aspect_y: f32) -> ShapeGeom {
    shape_geometry_tex(s, aspect_y, aspect_y)
}

/// [`shape_geometry`] with a separate aspect for the texture coordinates: projectM samples a shape's
/// `image=` texture with `textureAspectY = 1` while the outline keeps `renderContext.aspectY`.
#[cfg(test)]
pub fn shape_geometry_tex(s: &ShapeInstance, aspect_y: f32, tex_aspect_y: f32) -> ShapeGeom {
    shape_geometry_tex_max(s, aspect_y, tex_aspect_y, 100)
}

/// [`shape_geometry_tex`] with the compat mode's side limit (`CompatRules::max_shape_sides`: 100, 500 under
/// MilkDrop 3, whose vertex buffers hold the `sides + 2` fan vertices).
#[cfg(test)]
pub fn shape_geometry_tex_max(
    s: &ShapeInstance,
    aspect_y: f32,
    tex_aspect_y: f32,
    max_sides: i32,
) -> ShapeGeom {
    let mut g = ShapeGeom::default();
    shape_geometry_into(&mut g, s, aspect_y, tex_aspect_y, max_sides, true);
    g
}

/// The geometry of a shape instance into `g`, reusing its buffers. The texture coordinates (`g.uv`) are only
/// computed with `with_uv`: an untextured shape never reads them.
pub fn shape_geometry_into(
    g: &mut ShapeGeom,
    s: &ShapeInstance,
    aspect_y: f32,
    tex_aspect_y: f32,
    max_sides: i32,
    with_uv: bool,
) {
    let sides = s.sides.clamp(3, max_sides.max(3)) as usize;
    let pi = 3.1415927f32;
    // clip (fixed-function y is the negation): `static_cast<float>(x * 2.0 - 1.0)` with the VM's double `x`
    let c0 = [(s.x * 2.0 - 1.0) as f32, (s.y * 2.0 - 1.0) as f32];
    g.pts.clear();
    g.uv.clear();
    g.pts.reserve(sides + 2);
    g.pts.push(c0);
    if with_uv {
        g.uv.reserve(sides + 2);
        g.uv.push([0.5f32, 0.5f32]);
    }
    for j in 1..sides + 1 {
        let t = (j - 1) as f32 / sides as f32;
        let a = t * pi * 2.0 + s.ang + pi * 0.25;
        g.pts
            .push([c0[0] + s.rad * a.cos() * aspect_y, c0[1] - s.rad * a.sin()]);
        if with_uv {
            let ta = t * pi * 2.0 + s.tex_ang + pi * 0.25;
            // projectM: `1.0f - (0.5f - 0.5f * sinf(angle) / zoom)` (vertical flip); the same real number as
            // `0.5 + 0.5 * sin / zoom` but the f32 rounding of the two differs by an ulp, which flips
            // nearest-filtered ties
            g.uv.push([
                0.5 + 0.5 * ta.cos() / s.tex_zoom * tex_aspect_y,
                1.0 - (0.5 - 0.5 * ta.sin() / s.tex_zoom),
            ]);
        }
    }
    g.pts.push(g.pts[1]);
    if with_uv {
        g.uv.push(g.uv[1]);
    }
    g.sides = sides;
}

/// Darken-center triangle fan (clip space), colours: centre alpha 3/32, rim 0, written into `out` (cleared first).
pub fn darken_center_triangles(out: &mut Vec<WfVertex>, aspect_y: f32, rules: &CompatRules) {
    let hs = 0.05f32;
    let pts = [
        [0.0, 0.0],
        [-hs * aspect_y, 0.0],
        [0.0, -hs],
        [hs * aspect_y, 0.0],
        [0.0, hs],
        [-hs * aspect_y, 0.0],
    ];
    let c0 = conv_color(rules, [0.0, 0.0, 0.0, 3.0 / 32.0]);
    let c1 = conv_color(rules, [0.0, 0.0, 0.0, 0.0]);
    let colors = [c0, c1, c1, c1, c1, c1];
    out.clear();
    push_fan_colors(out, &pts, &colors);
}

/// Outer (`it == 0`) / inner (`it == 1`) border as projectM draws it (`Border::Draw`): 8 vertices and the same 8
/// triangles in the same index order, so degenerate sizes (negative, `ob + ib > 1` = negative inner radius) give the
/// same self-overlapping triangles (each blended separately) as projectM. projectM's vertices go through the
/// y-flipping `orthogonalProjection`, so the mesh is mirrored vertically against `y = +1 = top` clip space; that only
/// shows when the mesh is not symmetric, i.e. for those degenerate sizes.
pub fn border_triangles(
    out: &mut Vec<WfVertex>,
    it: usize,
    ob_size: f32,
    ib_size: f32,
    rgba: [f32; 4],
) {
    let inner = if it == 0 {
        1.0 - ob_size
    } else {
        1.0 - ob_size - ib_size
    };
    let outer = if it == 0 { 1.0 } else { 1.0 - ob_size };
    let v = [
        [outer, outer],
        [outer, -outer],
        [-outer, outer],
        [-outer, -outer],
        [inner, inner],
        [inner, -inner],
        [-inner, inner],
        [-inner, -inner],
    ];
    const INDICES: [usize; 24] = [
        0, 1, 4, 1, 4, 5, 2, 3, 6, 3, 7, 6, 2, 0, 6, 0, 4, 6, 3, 7, 5, 1, 3, 5,
    ];
    out.clear();
    out.extend(INDICES.iter().map(|&i| WfVertex {
        pos: [v[i][0], -v[i][1]],
        color: rgba,
    }));
}

/// Outer / inner border as MD2 draws it: four quads, the first one rotated by 90 degrees three times.
pub fn border_triangles_md2(
    out: &mut Vec<WfVertex>,
    it: usize,
    ob_size: f32,
    ib_size: f32,
    rgba: [f32; 4],
) {
    let inner = if it == 0 {
        1.0 - ob_size
    } else {
        1.0 - ob_size - ib_size
    };
    let outer = if it == 0 { 1.0 } else { 1.0 - ob_size };
    let mut v = [
        [inner, inner],
        [outer, outer],
        [outer, -outer],
        [inner, -inner],
    ];
    out.clear();
    for _ in 0..4 {
        push_fan_colors(out, &v, &[rgba; 4]);
        let t = 1.570796327f32;
        for p in v.iter_mut() {
            let (x, y) = (p[0], p[1]);
            p[0] = x * t.cos() - y * t.sin();
            p[1] = x * t.sin() + y * t.cos();
        }
    }
}

/// Sub-pixel precision of the GPU's vertex snapping (8 bits), see `stroke.rs`.
const SUB_PIXEL: f32 = 256.0;

/// Emulate projectM's (GL window space, y up) rasterisation of pixel centres that lie exactly on a horizontal
/// triangle edge (`CompatRules::gl_fill_ties`).
///
/// wgpu / D3D9 (y down, top-left rule) give such a pixel to the polygon *below* the edge; projectM gives it to the
/// polygon *above* (bottom edge inclusive, top edge exclusive, in image space). Both are the same as shifting the
/// edge down by a hair, so every vertex of a horizontal *boundary* edge whose snapped row is `k + 1/2` moves down by
/// one sub-pixel step (`1/256` px, the smallest move that survives the GPU's own snapping). All vertices at the same
/// snapped position move together, so polygons built from several triangles stay watertight. Vertical edges and
/// every other position are untouched (the left-edge rule is the same in both).
///
/// An edge that two triangles of the list share (the spokes of a fan) is inside the polygon: its pixels are drawn
/// once whichever side owns them, so it never triggers a move. The colour of a moved vertex is moved along the
/// triangle's own colour plane, so the interpolated colour at every pixel centre stays what it was: only the
/// coverage changes (projectM's fills are exact there, a plain move would flip colour roundings by one level).
pub fn gl_fill_ties(tris: &mut [WfVertex], w: f32, h: f32) {
    gl_fill_ties_by(tris, w, h, |v| &mut v.pos, 4, |v, i| &mut v.color[i]);
}

/// [`gl_fill_ties`] for any vertex type: `pos` projects a vertex onto its clip-space position and `attr(v, i)` for
/// `i < n_attr` onto the vertex attributes that are interpolated linearly in screen space.
pub fn gl_fill_ties_by<T>(
    tris: &mut [T],
    w: f32,
    h: f32,
    mut pos: impl FnMut(&mut T) -> &mut [f32; 2],
    n_attr: usize,
    mut attr: impl FnMut(&mut T, usize) -> &mut f32,
) {
    let snap = |v: f32| (v * SUB_PIXEL).round_ties_even() as i64;
    let key = |pos: &mut dyn FnMut(&mut T) -> &mut [f32; 2], v: &mut T| {
        let q = to_px(*pos(v), w, h);
        (snap(q[0]), snap(q[1]))
    };
    let half = SUB_PIXEL as i64 / 2;
    let mut edges: Vec<[(i64, i64); 2]> = Vec::new();
    for t in tris.as_chunks_mut::<3>().0 {
        let k = [
            key(&mut pos, &mut t[0]),
            key(&mut pos, &mut t[1]),
            key(&mut pos, &mut t[2]),
        ];
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            let (ka, kb) = (k[a], k[b]);
            if ka.1 == kb.1 && ka.0 != kb.0 && ka.1.rem_euclid(SUB_PIXEL as i64) == half {
                edges.push(if ka < kb { [ka, kb] } else { [kb, ka] });
            }
        }
    }
    edges.sort_unstable();
    let mut moved: Vec<(i64, i64)> = Vec::new();
    let mut i = 0;
    while i < edges.len() {
        let n = edges[i..].iter().take_while(|e| **e == edges[i]).count();
        if n == 1 {
            moved.extend(edges[i]);
        }
        i += n;
    }
    if moved.is_empty() {
        return;
    }
    moved.sort_unstable();
    moved.dedup();
    for t in tris.as_chunks_mut::<3>().0 {
        let k = [
            key(&mut pos, &mut t[0]),
            key(&mut pos, &mut t[1]),
            key(&mut pos, &mut t[2]),
        ];
        let hit = k.map(|k| moved.binary_search(&k).is_ok());
        if !hit.contains(&true) {
            continue;
        }
        let p = [
            to_px(*pos(&mut t[0]), w, h),
            to_px(*pos(&mut t[1]), w, h),
            to_px(*pos(&mut t[2]), w, h),
        ];
        let (e1, e2) = (
            [p[1][0] - p[0][0], p[1][1] - p[0][1]],
            [p[2][0] - p[0][0], p[2][1] - p[0][1]],
        );
        let area = e1[0] * e2[1] - e2[0] * e1[1];
        for j in 0..n_attr {
            let a = [
                *attr(&mut t[0], j),
                *attr(&mut t[1], j),
                *attr(&mut t[2], j),
            ];
            // d attr / d y of the triangle's plane
            let gy = (e1[0] * (a[2] - a[0]) - e2[0] * (a[1] - a[0])) / area;
            if gy.is_finite() {
                for i in 0..3 {
                    if hit[i] {
                        *attr(&mut t[i], j) = a[i] + gy / SUB_PIXEL;
                    }
                }
            }
        }
        for i in 0..3 {
            if hit[i] {
                pos(&mut t[i])[1] = from_px([0.0, (k[i].1 + 1) as f32 / SUB_PIXEL], w, h)[1];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Compat;

    fn px_extent(v: &[WfVertex], w: f32, h: f32) -> ([f32; 2], [f32; 2]) {
        let mut mn = [f32::MAX; 2];
        let mut mx = [f32::MIN; 2];
        for p in v {
            let q = to_px(p.pos, w, h);
            for k in 0..2 {
                mn[k] = mn[k].min(q[k]);
                mx[k] = mx[k].max(q[k]);
            }
        }
        (mn, mx)
    }

    #[test]
    fn horizontal_line_is_one_pixel_tall() {
        let (w, h) = (64.0, 32.0);
        let a = from_px([10.5, 5.5], w, h);
        let b = from_px([20.5, 5.5], w, h);
        let mut v = Vec::new();
        push_line(&mut v, a, b, w, h, [1.0; 4]);
        assert_eq!(v.len(), 6);
        let (mn, mx) = px_extent(&v, w, h);
        assert!((mn[0] - 10.5).abs() < 1e-4 && (mx[0] - 20.5).abs() < 1e-4);
        assert!(
            (mn[1] - 5.0).abs() < 1e-4 && (mx[1] - 6.0).abs() < 1e-4,
            "covers exactly pixel row 5"
        );
    }

    #[test]
    fn steep_line_is_one_pixel_wide_and_zero_length_is_empty() {
        let (w, h) = (64.0, 32.0);
        let mut v = Vec::new();
        push_line(
            &mut v,
            from_px([3.5, 2.5], w, h),
            from_px([5.5, 20.5], w, h),
            w,
            h,
            [1.0; 4],
        );
        let (mn, mx) = px_extent(&v, w, h);
        assert!((mn[1] - 2.5).abs() < 1e-4 && (mx[1] - 20.5).abs() < 1e-4);
        assert!((mx[0] - mn[0] - 3.0).abs() < 1e-3); // 2 px run + 1 px width
        let mut e = Vec::new();
        push_line(&mut e, [0.1, 0.1], [0.1, 0.1], w, h, [1.0; 4]);
        assert!(e.is_empty());
    }

    #[test]
    fn points_are_squares() {
        let mut v = Vec::new();
        push_point(
            &mut v,
            from_px([10.5, 10.5], 64.0, 64.0),
            2.0,
            64.0,
            64.0,
            [1.0; 4],
        );
        let (mn, mx) = px_extent(&v, 64.0, 64.0);
        assert!((mn[0] - 9.5).abs() < 1e-4 && (mx[0] - 11.5).abs() < 1e-4);
    }

    /// The GPU's attribute setup depends on the order of a triangle's vertices (matters for huge per-vertex UVs).
    /// projectM draws `(v, v+1, v+gx+1)`, `(v+1, v+gx+1, v+gx+2)` with `v` the top-left vertex of its row-0-on-top mesh.
    #[test]
    fn warp_gpu_triangles_use_projectms_vertex_order() {
        let g = MeshGeometry::new(4, 2, 64, 48).unwrap();
        let vars = vec![crate::warp_mesh::VertexVars::default(); g.vertex_count()];
        let verts = build_warp_gpu_vertices(&g, &vars);
        assert_eq!(verts.len(), g.vertex_count());
        let idx = warp_gpu_indices(&g, true);
        assert_eq!(idx.len(), 4 * 2 * 6);
        let v: Vec<WarpVertex> = idx.iter().map(|&i| verts[i as usize]).collect();
        // first cell (lactea row 0 = image bottom): top-left c = (0, 1), top-right d = (1, 1), bottom-left a = (0, 0), b = (1, 0)
        let at = |x: u32, y: u32| warp_gpu_vertex(&g, &vars, x, y);
        let want = [at(0, 1), at(1, 1), at(0, 0), at(1, 1), at(0, 0), at(1, 0)];
        assert_eq!(&v[..6], &want[..]);
        // MD2 order: a b c / b c d
        let md2: Vec<WarpVertex> = warp_gpu_indices(&g, false)
            .iter()
            .map(|&i| verts[i as usize])
            .collect();
        let want = [at(0, 0), at(1, 0), at(0, 1), at(1, 0), at(0, 1), at(1, 1)];
        assert_eq!(&md2[..6], &want[..]);
    }

    #[test]
    fn warp_mesh_counts_and_seam() {
        let g = MeshGeometry::new(8, 6, 64, 48).unwrap();
        let uv: Vec<[f32; 2]> = (0..g.vertex_count()).map(|n| [g.x[n], g.y[n]]).collect();
        let v = build_warp_vertices(&g, &uv, [1.0; 4], [0.0; 2], true, false);
        assert_eq!(v.len(), 8 * 6 * 6);
        let neg = v
            .iter()
            .filter(|m| m.rad_ang[1] == -std::f32::consts::PI)
            .count();
        assert!(neg > 0);
        let v2 = build_warp_vertices(&g, &uv, [1.0; 4], [0.0; 2], false, false);
        assert_eq!(
            v2.iter()
                .filter(|m| m.rad_ang[1] == -std::f32::consts::PI)
                .count(),
            0
        );
        // uv_orig: row 0 (y=-1) -> v_orig = 1
        assert!((v[0].uv[3] - 1.0).abs() < 1e-6 && (v[0].uv[2] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn comp_mesh_has_expected_triangle_count_and_uv_range() {
        let shade = hue_shade(0.0, [0.0; 4]);
        let v = build_comp_vertices(640, 360, (1.0, 0.5625), &shade, false);
        assert_eq!(v.len(), (FCGSX - 2) * (FCGSY - 2) * 6);
        for m in &v {
            assert!(
                m.uv[0] >= -1e-6
                    && m.uv[0] <= 1.0 + 1e-6
                    && m.uv[1] >= -1e-6
                    && m.uv[1] <= 1.0 + 1e-6
            );
            assert!(m.pos[0].abs() <= 1.0 + 1e-6);
        }
        let v = build_comp_vertices(640, 360, (1.0, 0.5625), &shade, true);
        assert!(v.iter().any(|m| m.uv[0] > 1.0));
    }

    #[test]
    fn echo_quad_diagonal_follows_projectm_strip() {
        // projectM draws TL,TR,BL,BR as a strip: shared edge TR-BL (our corners 3 and 0).
        let c = [[0.0; 4]; 4];
        let uv = [[0.0; 2]; 4];
        let pm = echo_quad(640, 360, uv, c, true, [1.0, 1.0]);
        let md2 = echo_quad(640, 360, uv, c, false, [1.0, 1.0]);
        assert_eq!(pm.len(), 6);
        // corners 0 (bottom-left) and 3 (top-right) are on both triangles in projectM's split
        let corner = |i: usize| echo_quad(640, 360, uv, c, false, [1.0, 1.0])[[0, 1, 2, 5][i]].pos;
        assert_eq!(pm.iter().filter(|m| m.pos == corner(0)).count(), 2);
        assert_eq!(pm.iter().filter(|m| m.pos == corner(3)).count(), 2);
        assert_eq!(pm.iter().filter(|m| m.pos == corner(1)).count(), 1);
        // MD2's split shares corners 1 and 2 instead
        assert_eq!(md2.iter().filter(|m| m.pos == corner(1)).count(), 2);
        assert_eq!(md2.iter().filter(|m| m.pos == corner(2)).count(), 2);
    }

    #[test]
    fn echo_aspect_mult_is_identity_for_landscape_and_stretches_portrait() {
        // landscape / square: `aspect = W / (H * (1 / (H/W)))` rounds to 1 in f32 for the usual sizes
        for (w, h) in [
            (320, 180),
            (640, 360),
            (1280, 720),
            (1920, 1080),
            (800, 600),
            (400, 400),
        ] {
            assert_eq!(echo_aspect_mult(w, h), [1.0, 1.0], "{w}x{h}");
        }
        // portrait: projectM keeps invAspectY = 1, so aspect = W/H < 1 and the quad is stretched by H/W in x
        let m = echo_aspect_mult(360, 640);
        assert_eq!(m[1], 1.0);
        assert!((m[0] - 640.0 / 360.0).abs() < 1e-6);
        // the quad itself: x extent scaled, y untouched
        let q = echo_quad(360, 640, [[0.0; 2]; 4], [[0.0; 4]; 4], true, m);
        let ex = (1.0 + 1.0 / 360.0) * m[0];
        assert!(
            q.iter()
                .all(|v| v.pos[0].abs() == ex && v.pos[1].abs() == 1.0 + 1.0 / 640.0)
        );
    }

    #[test]
    fn hue_shade_in_range() {
        let s = hue_shade(12.3, [1.0, 2.0, 3.0, 4.0]);
        for c in s {
            for k in c {
                assert!((0.5..=1.0).contains(&k));
            }
            assert!(
                c.iter().any(|&k| (k - 1.0).abs() < 1e-6),
                "max component normalised to 1"
            );
        }
    }

    #[test]
    fn colors_quantize_and_wrap() {
        assert!((d3d_color_component(0.98) - 249.0 / 255.0).abs() < 1e-6);
        assert!((d3d_color_component(1.0) - 1.0).abs() < 1e-6);
        assert!((d3d_color_component(2.0) - 254.0 / 255.0).abs() < 1e-6);
        assert!((d3d_color_component(-1.0) - 1.0 / 255.0).abs() < 1e-6);
        assert_eq!(modulo_color_component(0.3), 0.3);
        assert!((modulo_color_component(2.0) - (2.0 - 256.0 / 255.0)).abs() < 1e-6);
        let _ = Compat::ProjectM;
    }

    #[test]
    fn motion_vectors_identity_warp_gives_min_length_vectors() {
        let g = MeshGeometry::new(48, 36, 640, 360).unwrap();
        let uv: Vec<[f32; 2]> = (0..g.vertex_count())
            .map(|n| [g.x[n] * 0.5 + 0.5, -g.y[n] * 0.5 + 0.5])
            .collect();
        let mv = MotionVectors {
            x: 12.0,
            y: 9.0,
            dx: 0.0,
            dy: 0.0,
            l: 0.9,
            rgba: [1.0; 4],
        };
        for compat in [Compat::MilkDrop2, Compat::MilkDrop3, Compat::ProjectM] {
            let rules = compat.rules();
            let mut l = Vec::new();
            motion_vector_lines(&mut l, &mv, &g, &uv, 640, 360, &rules, [1.0; 4]);
            assert_eq!(l.len() % 2, 0);
            assert!(l.len() >= 2 * 12 * 9 - 2 * 20, "{compat:?}: {}", l.len());
            let (a, b) = (l[0].pos, l[1].pos);
            let len = (((b[0] - a[0]) / 2.0).powi(2) + ((b[1] - a[1]) / 2.0).powi(2)).sqrt();
            assert!(len > 0.0 && len < 0.01, "min-length vector {len}");
        }
    }

    #[test]
    fn motion_vector_zoom_points_inward_in_both_lookups() {
        // zoom 2: every pixel samples from the centre side -> vectors point towards the centre
        let g = MeshGeometry::new(48, 36, 640, 360).unwrap();
        let anim = crate::warp_mesh::WarpAnim::new(0.0, 1.0, 1.0);
        let uv = crate::warp_mesh::warp_mesh_uniform(
            &g,
            &crate::warp_mesh::VertexVars {
                zoom: 2.0,
                warp: 0.0,
                ..Default::default()
            },
            &anim,
            [0.0; 2],
        );
        let mv = MotionVectors {
            x: 4.0,
            y: 4.0,
            dx: 0.0,
            dy: 0.0,
            l: 1.0,
            rgba: [1.0; 4],
        };
        for compat in [Compat::MilkDrop2, Compat::MilkDrop3, Compat::ProjectM] {
            let mut l = Vec::new();
            motion_vector_lines(&mut l, &mv, &g, &uv, 640, 360, &compat.rules(), [1.0; 4]);
            for p in l.chunks(2) {
                let (p0, p1) = (p[0].pos, p[1].pos);
                let d0 = (p0[0].powi(2) + p0[1].powi(2)).sqrt();
                let d1 = (p1[0].powi(2) + p1[1].powi(2)).sqrt();
                assert!(d1 < d0, "{compat:?}: end not closer to centre {p:?}");
            }
        }
    }

    #[test]
    fn shape_geometry_matches_formula() {
        let s = ShapeInstance {
            sides: 4,
            x: 0.5,
            y: 0.5,
            rad: 0.1,
            ang: 0.0,
            ..Default::default()
        };
        let g = shape_geometry(&s, 0.5625);
        assert_eq!(g.pts.len(), 6);
        assert_eq!(g.pts[5], g.pts[1]);
        // corner 1: angle pi/4: x = 0.1*cos(pi/4)*0.5625, y = -0.1*sin(pi/4) (clip, y up)
        assert!((g.pts[1][0] - 0.1 * 0.70710678 * 0.5625).abs() < 1e-5);
        assert!((g.pts[1][1] + 0.1 * 0.70710678).abs() < 1e-5);
        assert_eq!(g.uv[0], [0.5, 0.5]);
        let s2 = ShapeInstance {
            sides: 1000,
            ..Default::default()
        };
        assert_eq!(shape_geometry(&s2, 1.0).sides, 100);
    }

    /// An untextured shape skips the texture coordinates; the outline is the same, and a reused buffer carries nothing
    /// over from the previous instance.
    #[test]
    fn untextured_shape_geometry_skips_uv_and_reuses_buffers() {
        let big = ShapeInstance {
            sides: 40,
            rad: 0.3,
            tex_zoom: 1.5,
            tex_ang: 0.7,
            ..Default::default()
        };
        let small = ShapeInstance {
            sides: 5,
            ..big.clone()
        };
        let mut g = ShapeGeom::default();
        shape_geometry_into(&mut g, &big, 0.75, 0.75, 100, true);
        shape_geometry_into(&mut g, &small, 0.75, 0.75, 100, false);
        let want = shape_geometry_tex(&small, 0.75, 0.75);
        assert_eq!(g.pts, want.pts);
        assert_eq!(g.sides, 5);
        assert!(g.uv.is_empty());
        shape_geometry_into(&mut g, &small, 0.75, 0.75, 100, true);
        assert_eq!(g.uv, want.uv);
    }

    /// MilkDrop 3 (`max_shape_sides` 500): `sides` is `clamp(int(sides), 3, 500)`, the fan has `sides + 2` vertices.
    #[test]
    fn shape_sides_limit_follows_the_rule() {
        let sides = |n: i32, max: i32| {
            let s = ShapeInstance {
                sides: n,
                ..Default::default()
            };
            shape_geometry_tex_max(&s, 1.0, 1.0, max)
        };
        let md3 = 500;
        for (n, want) in [
            (2, 3),
            (3, 3),
            (100, 100),
            (250, 250),
            (500, 500),
            (600, 500),
            (-3, 3),
        ] {
            assert_eq!(sides(n, md3).sides, want, "sides {n}");
        }
        // 500 sides: centre + 500 ring vertices + the closing vertex
        let g = sides(500, md3);
        assert_eq!((g.pts.len(), g.uv.len()), (502, 502));
        assert_eq!(g.pts[501], g.pts[1]);
        // the same shape under the MD2 limit stops at 100
        assert_eq!(sides(500, 100).pts.len(), 102);
    }

    /// `shapecode_N_image` (projectM `CustomShape.cpp:155-167`): the texture coordinates use `textureAspectY = 1`
    /// while the outline keeps the viewport aspect.
    #[test]
    fn shape_image_uses_unit_texture_aspect() {
        let s = ShapeInstance {
            sides: 4,
            rad: 0.1,
            tex_zoom: 2.0,
            ..Default::default()
        };
        let main = shape_geometry(&s, 0.5625);
        let img = shape_geometry_tex(&s, 0.5625, 1.0);
        assert_eq!(main.pts, img.pts, "the outline is the same");
        // corner 1 (angle pi/4): u = 0.5 + 0.5 * cos / tex_zoom * aspect
        let c = (std::f32::consts::FRAC_PI_4).cos();
        assert!((main.uv[1][0] - (0.5 + 0.5 * c / 2.0 * 0.5625)).abs() < 1e-6);
        assert!((img.uv[1][0] - (0.5 + 0.5 * c / 2.0)).abs() < 1e-6);
        assert_eq!(main.uv[1][1], img.uv[1][1]);
    }

    /// `CustomShape::Draw`: the fan centre is `static_cast<float>(x * 2.0 - 1.0)` of the VM's *double* `x`, and the
    /// vertical texture coordinate `1.0f - (0.5f - 0.5f * sinf(a) / zoom)`: the same real number as
    /// `0.5 + 0.5 * sin / zoom` but not the same `f32`, which flips nearest-filtered texels and 1/256-px snap ties
    /// (seen with nearest-filtered shape images).
    #[test]
    fn shape_geometry_rounds_like_projectm() {
        let mut uv_differs = 0;
        for zoom in [0.274246f32, 0.369642, 0.45, 1.3, 3.0, 4.0] {
            for tex_ang in [0.0f32, 0.3, 1.1, 2.5] {
                let s = ShapeInstance {
                    sides: 6,
                    tex_zoom: zoom,
                    tex_ang,
                    ..Default::default()
                };
                let g = shape_geometry(&s, 0.5625);
                let pi = 3.1415927f32;
                for j in 1..=6usize {
                    let t = (j - 1) as f32 / 6.0;
                    let a = t * pi * 2.0 + tex_ang + pi * 0.25;
                    assert_eq!(
                        g.uv[j][1].to_bits(),
                        (1.0 - (0.5 - 0.5 * a.sin() / zoom)).to_bits()
                    );
                    uv_differs += (g.uv[j][1] != 0.5 + 0.5 * a.sin() / zoom) as u32;
                }
            }
        }
        assert!(
            uv_differs > 0,
            "the probe values must expose the rounding difference"
        );
        let s = ShapeInstance {
            x: 0.72,
            y: 0.28,
            ..Default::default()
        };
        let c = shape_geometry(&s, 1.0).pts[0];
        assert_eq!(
            c,
            [(0.72f64 * 2.0 - 1.0) as f32, (0.28f64 * 2.0 - 1.0) as f32]
        );
        assert_ne!(
            c[0],
            0.72f32 * 2.0 - 1.0,
            "rounding x to f32 first moves the centre by ulps"
        );
    }

    /// `CustomWaveform::Draw` rounds `(x * 2.0 - 1.0) * invAspectX` once (double product); MD2 rounds `x * 2 - 1` to
    /// `f32` before the multiplication.
    #[test]
    fn wave_point_rounding_follows_the_compat_rule() {
        let (pm, md2) = (Compat::ProjectM.rules(), Compat::MilkDrop2.rules());
        let inv = (1.0f32 / 1.7777778, 1.0f32);
        let (mut pm_vs_md2, mut pm_vs_f32) = (0, 0);
        for i in 0..2000 {
            let x = 0.5 + 0.5 * (f64::from(i) * 0.7311).sin();
            let p = WavePoint {
                x,
                y: x,
                rgba: [1.0; 4],
            };
            let a = wave_point_clip(&pm, &p, inv);
            let b = wave_point_clip(&md2, &p, inv);
            assert_eq!(a[0], ((x * 2.0 - 1.0) * f64::from(inv.0)) as f32);
            assert_eq!(b[0], (x * 2.0 - 1.0) as f32 * inv.0);
            assert_eq!(
                a[1],
                (x * 2.0 - 1.0) as f32,
                "unit aspect: only the double -> f32 rounding"
            );
            pm_vs_md2 += (a[0] != b[0]) as u32;
            pm_vs_f32 += (a[0] != (x as f32 * 2.0 - 1.0) * inv.0) as u32;
        }
        assert!(pm_vs_md2 > 0 && pm_vs_f32 > 0, "{pm_vs_md2} {pm_vs_f32}");
    }

    #[test]
    fn border_and_darken_center() {
        let mut t = Vec::new();
        border_triangles(&mut t, 0, 0.1, 0.1, [1.0; 4]);
        assert_eq!(t.len(), 4 * 6);
        let mut d = Vec::new();
        darken_center_triangles(&mut d, 0.5625, &Compat::ProjectM.rules());
        assert_eq!(d.len(), 4 * 3);
        assert!((d[0].color[3] - 3.0 / 32.0).abs() < 1e-6);
    }

    /// `Border.cpp`: 8 vertices `(o,o) (o,-o) (-o,o) (-o,-o) (i,i) (i,-i) (-i,i) (-i,-i)`, 8 triangles in index order
    /// `0 1 4 | 1 4 5 | 2 3 6 | 3 7 6 | 2 0 6 | 0 4 6 | 3 7 5 | 1 3 5`, mirrored vertically (y-flipping projection).
    #[test]
    fn border_mesh_is_projectms() {
        let mut t = Vec::new();
        border_triangles(&mut t, 1, 0.1, 0.05, [0.5; 4]);
        let (o, i) = (1.0f32 - 0.1, 1.0f32 - 0.1 - 0.05);
        let p: Vec<[f32; 2]> = t.iter().map(|v| v.pos).collect();
        assert_eq!(p.len(), 24);
        assert_eq!(&p[0..3], &[[o, -o], [o, o], [i, -i]]); // 0 1 4
        assert_eq!(&p[21..24], &[[o, o], [-o, o], [i, i]]); // 1 3 5
        assert!(t.iter().all(|v| v.color == [0.5; 4]));
        // degenerate: ob + ib > 1 -> negative inner radius, the triangles overlap instead of forming a frame
        let mut d = Vec::new();
        border_triangles(&mut d, 0, 1.2, 0.0, [1.0; 4]);
        let i = 1.0f32 - 1.2; // negative inner radius
        assert_eq!(d[2].pos, [i, -i]); // vertex 4 = (i, i), mirrored in y
        // MD2: four rotated quads (the old geometry), same coverage for sane sizes
        border_triangles_md2(&mut d, 0, 0.1, 0.0, [1.0; 4]);
        assert_eq!(d.len(), 24);
    }

    /// Edges on pixel centres (`size * H / 2` half integer) move down one sub-pixel step, nothing else moves.
    #[test]
    fn gl_fill_ties_nudge_only_horizontal_centre_edges() {
        let (w, h) = (40.0f32, 30.0f32);
        let mut t = Vec::new();
        border_triangles(&mut t, 0, 0.1, 0.0, [1.0; 4]); // inner edges at rows 1.5 and 28.5
        let before = t.clone();
        gl_fill_ties(&mut t, w, h);
        for (a, b) in before.iter().zip(&t) {
            let (pa, pb) = (to_px(a.pos, w, h), to_px(b.pos, w, h));
            assert_eq!(pa[0], pb[0]);
            if a.pos[1].abs() == 1.0 {
                assert_eq!(a.pos, b.pos); // outer corners (rows 0 and 30) are not ties
            } else {
                assert!(
                    (pb[1] - pa[1] - 1.0 / 256.0).abs() < 1e-4,
                    "{pa:?} -> {pb:?}"
                );
            }
        }
        // 0.05 on 30 rows: edges at 0.75 / 29.25; 0.075 on 40 columns: a tie in x, which is not touched
        for (size, ww) in [(0.05f32, 40.0f32), (0.075, 40.0)] {
            let mut u = Vec::new();
            border_triangles(&mut u, 0, size, 0.0, [1.0; 4]);
            let b = u.clone();
            gl_fill_ties(&mut u, ww, h);
            assert_eq!(u, b, "size {size}");
        }
    }

    /// A fan `centre + closed rim` in pixel coordinates, centre colour 1, rim colour 0.2.
    fn px_fan(centre: [f32; 2], rim: &[[f32; 2]], (w, h): (f32, f32)) -> Vec<WfVertex> {
        let mut pts = vec![from_px(centre, w, h)];
        pts.extend(rim.iter().map(|p| from_px(*p, w, h)));
        let colors: Vec<[f32; 4]> = (0..pts.len())
            .map(|k| if k == 0 { [1.0; 4] } else { [0.2; 4] })
            .collect();
        fan_to_list(&pts, &colors)
    }

    /// Linear interpolation of the vertex colours of a triangle at pixel position `p`.
    fn plane_colour(t: &[WfVertex], p: [f32; 2], (w, h): (f32, f32)) -> f32 {
        let q: Vec<[f32; 2]> = t.iter().map(|v| to_px(v.pos, w, h)).collect();
        let d =
            (q[1][0] - q[0][0]) * (q[2][1] - q[0][1]) - (q[2][0] - q[0][0]) * (q[1][1] - q[0][1]);
        let l1 =
            ((p[0] - q[0][0]) * (q[2][1] - q[0][1]) - (q[2][0] - q[0][0]) * (p[1] - q[0][1])) / d;
        let l2 =
            ((q[1][0] - q[0][0]) * (p[1] - q[0][1]) - (p[0] - q[0][0]) * (q[1][1] - q[0][1])) / d;
        t[0].color[0] * (1.0 - l1 - l2) + t[1].color[0] * l1 + t[2].color[0] * l2
    }

    /// A fan whose top and bottom edges lie on pixel centres: those edges move down a step, the centre does not, and
    /// the colour plane of every triangle (so the colour at any pixel) is the one it had before.
    #[test]
    fn gl_fill_ties_keep_the_colour_plane_of_moved_vertices() {
        let wh = (64.0f32, 64.0f32);
        let rim = [
            [20.0, 10.5],
            [44.0, 10.5],
            [44.0, 30.5],
            [20.0, 30.5],
            [20.0, 10.5],
        ];
        let before = px_fan([32.0, 20.5], &rim, wh);
        let mut t = before.clone();
        gl_fill_ties(&mut t, wh.0, wh.1);
        let dy = |i: usize| to_px(t[i].pos, wh.0, wh.1)[1] - to_px(before[i].pos, wh.0, wh.1)[1];
        // triangles (c, r0, r1) top edge, (c, r1, r2), (c, r2, r3) bottom edge, (c, r3, r0)
        assert!(
            (dy(1) - 1.0 / 256.0).abs() < 1e-4 && (dy(2) - 1.0 / 256.0).abs() < 1e-4,
            "top edge"
        );
        assert!(
            (dy(7) - 1.0 / 256.0).abs() < 1e-4 && (dy(8) - 1.0 / 256.0).abs() < 1e-4,
            "bottom edge"
        );
        assert_eq!(
            t[0].pos, before[0].pos,
            "the centre is not on a boundary edge"
        );
        for tri in 0..4 {
            let (a, b) = (&before[tri * 3..tri * 3 + 3], &t[tri * 3..tri * 3 + 3]);
            for p in [[30.0, 20.0], [26.5, 14.25], [38.0, 25.75]] {
                let (x, y) = (plane_colour(a, p, wh), plane_colour(b, p, wh));
                assert!((x - y).abs() < 1e-5, "triangle {tri} at {p:?}: {x} vs {y}");
            }
        }
    }

    /// An edge that two triangles of a fan share (a spoke) is inside the polygon, so a spoke on a pixel centre moves
    /// nothing.
    #[test]
    fn gl_fill_ties_ignore_spokes_inside_the_polygon() {
        let wh = (64.0f32, 64.0f32);
        let rim = [
            [44.0, 20.5],
            [38.0, 10.2],
            [26.0, 10.2],
            [20.0, 20.5],
            [26.0, 30.8],
            [38.0, 30.8],
            [44.0, 20.5],
        ];
        let before = px_fan([32.0, 20.5], &rim, wh);
        let mut t = before.clone();
        gl_fill_ties(&mut t, wh.0, wh.1);
        assert_eq!(t, before);
    }

    #[test]
    fn nan_uv_triangles_sample_uv_zero() {
        let g = MeshGeometry::new(2, 2, 64, 64).unwrap();
        let mut uv = vec![[0.25f32, 0.75]; g.vertex_count()];
        uv[g.index(1, 1)] = [f32::NAN, f32::NAN];
        let mut v = build_warp_vertices(&g, &uv, [1.0; 4], [0.0; 2], false, true);
        fix_nan_uv_triangles(&mut v);
        for t in v.chunks_exact(3) {
            let uvs: Vec<[f32; 2]> = t.iter().map(|m| [m.uv[0], m.uv[1]]).collect();
            // the centre vertex is shared by 6 of the 8 triangles (diagonals a-d), those are flattened, the others keep their UV
            let flat = uvs.iter().all(|u| *u == [0.0, 0.0]);
            let keep = uvs.iter().all(|u| *u == [0.25, 0.75]);
            assert!(flat ^ keep, "{uvs:?}");
        }
        assert!(v.iter().any(|m| m.uv[0] == 0.0) && v.iter().any(|m| m.uv[0] == 0.25));
        assert!(v.iter().all(|m| !m.uv[0].is_nan() && !m.uv[1].is_nan()));
    }
}
