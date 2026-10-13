// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Warp-mesh geometry and MD2's per-vertex UV formula (milkdropfs.cpp
//! `CPlugin::ComputeGridAlphaValues`, vertex setup in plugin.cpp `AllocateMilkDropDX9`).
//!
//! The engine evaluates per-pixel code (if any) for every grid vertex and hands the resulting
//! variables to [`warp_vertex_uv`]; presets without per-pixel code use [`warp_mesh_uniform`].
//! The result is the `warp_uv` grid in [`crate::FrameInput`].
//!
//! Conventions (identical to MD2):
//! * grid is `(gx+1) x (gy+1)` vertices, row-major, **row 0 is stored at clip y = -1**
//!   (bottom of the screen). UV `v = 0` is the top of the texture, so row 0 samples `v ~ 1`.
//! * `x`/`y` are clip-space coordinates (`x*0.5+0.5` / `y*-0.5+0.5` are the preset-space `x`/`y`
//!   seen by per-pixel code, before the aspect correction).

use crate::RenderError;

/// Largest warp mesh (cells per axis) accepted.
pub const MAX_MESH_CELLS: u32 = 300;

/// Static mesh data: vertex positions and MD2's `rad`/`ang` per-vertex attributes.
///
/// The grid has an even number of cells (>= 2) per axis; [`MeshGeometry::new`] enforces that, which is why the
/// fields are private.
#[derive(Clone, Debug)]
pub struct MeshGeometry {
    pub(crate) gx: u32,
    pub(crate) gy: u32,
    pub(crate) aspect_x: f32,
    pub(crate) aspect_y: f32,
    pub(crate) x: Vec<f32>,
    pub(crate) y: Vec<f32>,
    pub(crate) rad: Vec<f32>,
    pub(crate) ang: Vec<f32>,
    /// [`MeshGeometry::projectm_vertex`] of every vertex, indexed like `x`: constant for the mesh, so computed once.
    pm: Vec<([f32; 2], f32, f32)>,
}

/// `(m_fAspectX, m_fAspectY)` from texture size (plugin.cpp).
pub fn aspect_xy(tex_w: u32, tex_h: u32) -> (f32, f32) {
    let ax = if tex_h > tex_w {
        tex_w as f32 / tex_h as f32
    } else {
        1.0
    };
    let ay = if tex_w > tex_h {
        tex_h as f32 / tex_w as f32
    } else {
        1.0
    };
    (ax, ay)
}

impl MeshGeometry {
    /// Build the mesh of `gx x gy` cells for a `tex_w x tex_h` image.
    ///
    /// Fails with [`RenderError::Invalid`] unless both cell counts are even and at least 2 (MD2 builds the grid
    /// from four quadrants).
    pub fn new(gx: u32, gy: u32, tex_w: u32, tex_h: u32) -> Result<Self, RenderError> {
        if gx > MAX_MESH_CELLS || gy > MAX_MESH_CELLS {
            return Err(RenderError::Invalid(format!(
                "mesh dimensions must be at most {MAX_MESH_CELLS}, got {gx}x{gy}"
            )));
        }
        if gx < 2 || gy < 2 || !gx.is_multiple_of(2) || !gy.is_multiple_of(2) {
            return Err(RenderError::Invalid(format!(
                "mesh dimensions must be even and >= 2, got {gx}x{gy}"
            )));
        }
        let (ax, ay) = aspect_xy(tex_w, tex_h);
        let n = ((gx + 1) * (gy + 1)) as usize;
        let mut g = MeshGeometry {
            gx,
            gy,
            aspect_x: ax,
            aspect_y: ay,
            x: Vec::with_capacity(n),
            y: Vec::with_capacity(n),
            rad: Vec::with_capacity(n),
            ang: Vec::with_capacity(n),
            pm: Vec::with_capacity(n),
        };
        for y in 0..=gy {
            for x in 0..=gx {
                let vx = x as f32 / gx as f32 * 2.0 - 1.0;
                let vy = y as f32 / gy as f32 * 2.0 - 1.0;
                let rad = (vx * ax).hypot(vy * ay);
                let ang = if y == gy / 2 && x == gx / 2 {
                    0.0
                } else {
                    (vy * ay).atan2(vx * ax)
                };
                g.x.push(vx);
                g.y.push(vy);
                g.rad.push(rad);
                g.ang.push(ang);
                g.pm.push(projectm_vertex(gx, gy, ax, ay, x, gy - y));
            }
        }
        Ok(g)
    }

    /// Number of cells (not vertices) in x.
    pub fn gx(&self) -> u32 {
        self.gx
    }

    /// Number of cells (not vertices) in y.
    pub fn gy(&self) -> u32 {
        self.gy
    }

    /// `m_fAspectX` (computed from the texture size).
    pub fn aspect_x(&self) -> f32 {
        self.aspect_x
    }

    /// `m_fAspectY` (computed from the texture size).
    pub fn aspect_y(&self) -> f32 {
        self.aspect_y
    }

    /// Clip-space x of each vertex (`m_verts[n].x`).
    pub fn x(&self) -> &[f32] {
        &self.x
    }

    /// Clip-space y of each vertex (`m_verts[n].y`).
    pub fn y(&self) -> &[f32] {
        &self.y
    }

    /// `m_vertinfo[n].rad` of each vertex.
    pub fn rad(&self) -> &[f32] {
        &self.rad
    }

    /// `m_vertinfo[n].ang` of each vertex (-pi..pi as `atan2f`, centre vertex 0).
    pub fn ang(&self) -> &[f32] {
        &self.ang
    }

    /// Number of vertices, `(gx + 1) * (gy + 1)`.
    pub fn vertex_count(&self) -> usize {
        ((self.gx + 1) * (self.gy + 1)) as usize
    }

    /// Index of the vertex at column `x`, row `y` (row 0 = bottom of the screen).
    #[inline]
    pub fn index(&self, x: u32, y: u32) -> usize {
        (y * (self.gx + 1) + x) as usize
    }

    /// Preset-space `x` (0 = left .. 1 = right) of vertex `n`, as per-pixel code sees it:
    /// `m_verts[n].x*0.5*aspectX + 0.5`.
    pub fn pv_x(&self, n: usize) -> f32 {
        self.x[n] * 0.5 * self.aspect_x + 0.5
    }
    /// Preset-space `y` (0 = top .. 1 = bottom): `m_verts[n].y*-0.5*aspectY + 0.5`.
    pub fn pv_y(&self, n: usize) -> f32 {
        self.y[n] * -0.5 * self.aspect_y + 0.5
    }

    /// projectM's own vertex data (`PerPixelMesh::InitializeMesh`) for the vertex at column `x`, lactea
    /// row `y` (row 0 = bottom). projectM's mesh row 0 is the *top* of the image, so its `y` is
    /// `row_pm / gy * 2 - 1` with `row_pm = gy - y` (float rounding differs from `-self.y[n]`);
    /// `rad = hypotf(x*ax, y*ay)`, `ang = atan2f(y*ay, x*ax)` (0 at the centre vertex).
    /// Returns `(pos, rad, ang)`.
    #[inline]
    pub fn projectm_vertex(&self, x: u32, y: u32) -> ([f32; 2], f32, f32) {
        if x <= self.gx && y <= self.gy {
            self.pm[self.index(x, y)]
        } else {
            projectm_vertex(
                self.gx,
                self.gy,
                self.aspect_x,
                self.aspect_y,
                x,
                self.gy.wrapping_sub(y),
            )
        }
    }
}

/// The uncached formula behind [`MeshGeometry::projectm_vertex`]; `row_pm` is projectM's mesh row (0 = top).
fn projectm_vertex(
    gx: u32,
    gy: u32,
    ax: f32,
    ay: f32,
    x: u32,
    row_pm: u32,
) -> ([f32; 2], f32, f32) {
    let vx = x as f32 / gx as f32 * 2.0 - 1.0;
    let vy = row_pm as f32 / gy as f32 * 2.0 - 1.0;
    let rad = (vx * ax).hypot(vy * ay);
    let ang = if row_pm == gy / 2 && x == gx / 2 {
        0.0
    } else {
        (vy * ay).atan2(vx * ax)
    };
    ([vx, vy], rad, ang)
}

/// The ten per-vertex motion variables after (optional) per-pixel code
/// (`pv_zoom, pv_zoomexp, pv_rot, pv_warp, pv_cx, pv_cy, pv_dx, pv_dy, pv_sx, pv_sy`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VertexVars {
    /// `pv_zoom`
    pub zoom: f32,
    /// `pv_zoomexp`
    pub zoomexp: f32,
    /// `pv_rot`
    pub rot: f32,
    /// `pv_warp`
    pub warp: f32,
    /// `pv_cx`
    pub cx: f32,
    /// `pv_cy`
    pub cy: f32,
    /// `pv_dx`
    pub dx: f32,
    /// `pv_dy`
    pub dy: f32,
    /// `pv_sx`
    pub sx: f32,
    /// `pv_sy`
    pub sy: f32,
}

impl Default for VertexVars {
    /// MD2 defaults (state.cpp `Default`): identity motion.
    fn default() -> Self {
        Self {
            zoom: 1.0,
            zoomexp: 1.0,
            rot: 0.0,
            warp: 1.0,
            cx: 0.5,
            cy: 0.5,
            dx: 0.0,
            dy: 0.0,
            sx: 1.0,
            sy: 1.0,
        }
    }
}

/// Warp-animation terms computed once per frame in `ComputeGridAlphaValues`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarpAnim {
    /// projectM's warp vertex shader evaluates the four warp terms with a mesh `y` of the
    /// opposite sign (its mesh row 0 is the top of the image, MD2's the bottom). `true` flips
    /// `y` in those terms ([`WarpAnim::projectm`]).
    pub y_flip: bool,
    /// projectM computes `pow(zoom, ..)` in a GPU vertex shader, where a negative base gives NaN (`exp2(y*log2(x))`);
    /// MD2 does it on the CPU (`powf`, finite). `true` in [`WarpAnim::projectm`].
    pub gpu_pow: bool,
    /// MilkDrop 3 adds the four warp `sin` / `cos` terms only when `|warp| > 0.0001`; MD2 always adds them
    /// (`CompatRules::md3_warp_skip_tiny`).
    pub skip_tiny_warp: bool,
    /// `fWarpTime = GetTime() * m_fWarpAnimSpeed`
    pub warp_time: f32,
    /// `fWarpScaleInv = 1 / m_fWarpScale`
    pub scale_inv: f32,
    /// `f[0..4]`
    pub f: [f32; 4],
}

impl WarpAnim {
    /// `time` = `GetTime()`, `warp_anim_speed` = preset `fWarpAnimSpeed`, `warp_scale` = `fWarpScale`.
    pub fn new(time: f32, warp_anim_speed: f32, warp_scale: f32) -> Self {
        let warp_time = time * warp_anim_speed;
        let f = [
            11.68 + 4.0 * (warp_time * 1.413 + 10.0).cos(),
            8.77 + 3.0 * (warp_time * 1.113 + 7.0).cos(),
            10.54 + 3.0 * (warp_time * 1.233 + 3.0).cos(),
            11.49 + 4.0 * (warp_time * 0.933 + 5.0).cos(),
        ];
        Self {
            y_flip: false,
            gpu_pow: false,
            skip_tiny_warp: false,
            warp_time,
            scale_inv: 1.0 / warp_scale,
            f,
        }
    }

    /// Like [`WarpAnim::new`] with projectM's `y` convention in the warp terms.
    pub fn projectm(time: f32, warp_anim_speed: f32, warp_scale: f32) -> Self {
        Self {
            y_flip: true,
            gpu_pow: true,
            ..Self::new(time, warp_anim_speed, warp_scale)
        }
    }
}

/// MD2's per-vertex UV (the non-blending branch of `ComputeGridAlphaValues`).
///
/// `texel_offset_uv` = `texel_offset / texsize` (MD2: `0.5 / m_nTexSizeX`); added last.
/// Returns `(tu, tv)`.
pub fn warp_vertex_uv(
    g: &MeshGeometry,
    n: usize,
    v: &VertexVars,
    anim: &WarpAnim,
    texel_offset_uv: [f32; 2],
) -> [f32; 2] {
    let vx = g.x[n];
    let vy = g.y[n];
    // `y` as the warp terms see it (see `WarpAnim::y_flip`)
    let wy = if anim.y_flip { -vy } else { vy };
    let rad = g.rad[n];
    let f = &anim.f;

    let zoom2 = if anim.gpu_pow && v.zoom < 0.0 {
        f32::NAN
    } else {
        v.zoom.powf(v.zoomexp.powf(rad * 2.0 - 1.0))
    };
    let zoom2_inv = 1.0 / zoom2;

    let mut u = vx * g.aspect_x * 0.5 * zoom2_inv + 0.5;
    let mut vv = -vy * g.aspect_y * 0.5 * zoom2_inv + 0.5;

    // stretch on X, Y
    u = (u - v.cx) / v.sx + v.cx;
    vv = (vv - v.cy) / v.sy + v.cy;

    // warping
    let wt = anim.warp_time;
    let si = anim.scale_inv;
    if !anim.skip_tiny_warp || v.warp.abs() > 0.0001 {
        u += v.warp * 0.0035 * (wt * 0.333 + si * (vx * f[0] - wy * f[3])).sin();
        vv += v.warp * 0.0035 * (wt * 0.375 - si * (vx * f[2] + wy * f[1])).cos();
        u += v.warp * 0.0035 * (wt * 0.753 - si * (vx * f[1] - wy * f[2])).cos();
        vv += v.warp * 0.0035 * (wt * 0.825 + si * (vx * f[0] + wy * f[3])).sin();
    }

    // rotation
    let u2 = u - v.cx;
    let v2 = vv - v.cy;
    let cos_rot = v.rot.cos();
    let sin_rot = v.rot.sin();
    u = u2 * cos_rot - v2 * sin_rot + v.cx;
    vv = u2 * sin_rot + v2 * cos_rot + v.cy;

    // translation
    u -= v.dx;
    vv -= v.dy;

    // undo aspect ratio fix
    u = (u - 0.5) * (1.0 / g.aspect_x) + 0.5;
    vv = (vv - 0.5) * (1.0 / g.aspect_y) + 0.5;

    // final texel-offset translation
    u += texel_offset_uv[0];
    vv += texel_offset_uv[1];
    [u, vv]
}

/// The default per-vertex path (no per-pixel code): every vertex uses the same frame variables.
pub fn warp_mesh_uniform(
    g: &MeshGeometry,
    v: &VertexVars,
    anim: &WarpAnim,
    texel_offset_uv: [f32; 2],
) -> Vec<[f32; 2]> {
    (0..g.vertex_count())
        .map(|n| warp_vertex_uv(g, n, v, anim, texel_offset_uv))
        .collect()
}

/// `texel_offset` (texels) -> UV offset for a texture size, as `RenderConfig` defines it.
pub fn texel_offset_uv(texel_offset: [f32; 2], tex_w: u32, tex_h: u32) -> [f32; 2] {
    [
        texel_offset[0] / tex_w as f32,
        texel_offset[1] / tex_h as f32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn geometry_matches_md2_setup() {
        let g = MeshGeometry::new(48, 36, 640, 360).unwrap();
        assert_eq!(g.vertex_count(), 49 * 37);
        assert!(close(g.aspect_x, 1.0));
        assert!(close(g.aspect_y, 360.0 / 640.0));
        // row 0 = clip y -1, x runs -1..1
        assert!(close(g.x[0], -1.0) && close(g.y[0], -1.0));
        assert!(close(g.x[48], 1.0) && close(g.y[48], -1.0));
        let last = g.vertex_count() - 1;
        assert!(close(g.x[last], 1.0) && close(g.y[last], 1.0));
        // center vertex: rad 0, ang forced to 0
        let c = g.index(24, 18);
        assert!(close(g.rad[c], 0.0) && g.ang[c] == 0.0);
        // corner rad = sqrt(ax^2 + ay^2)
        let corner = (1.0f32 + (360.0f32 / 640.0).powi(2)).sqrt();
        assert!(close(g.rad[0], corner));
        // preset-space coords: row 0 is the *bottom* (pv_y = 1)
        assert!(close(g.pv_y(0), 0.5 + 0.5 * g.aspect_y));
        assert!(close(g.pv_x(0), 0.0));
    }

    #[test]
    fn identity_motion_maps_to_screen_uv() {
        // zoom=1, rot=0, warp=0, no translation: uv must equal the screen position (v=0 at top)
        let g = MeshGeometry::new(48, 36, 640, 360).unwrap();
        let anim = WarpAnim::new(3.0, 1.0, 1.0);
        let v = VertexVars {
            warp: 0.0,
            ..Default::default()
        };
        let uv = warp_mesh_uniform(&g, &v, &anim, [0.0, 0.0]);
        for n in 0..g.vertex_count() {
            assert!(close(uv[n][0], g.x[n] * 0.5 + 0.5), "u {n}");
            assert!(close(uv[n][1], -g.y[n] * 0.5 + 0.5), "v {n}");
        }
        // row 0 (bottom of screen) samples v = 1
        assert!(close(uv[0][1], 1.0));
    }

    #[test]
    fn zoom_rot_translate_hand_computed() {
        let g = MeshGeometry::new(2, 2, 100, 100).unwrap(); // aspect 1,1; vertices at -1,0,1
        let anim = WarpAnim::new(0.0, 1.0, 1.0);
        // zoom 2 (zoomexp 1 -> constant), pure zoom: u = x*0.5/2 + 0.5
        let v = VertexVars {
            zoom: 2.0,
            warp: 0.0,
            ..Default::default()
        };
        let uv = warp_vertex_uv(&g, g.index(2, 0), &v, &anim, [0.0, 0.0]); // x=1,y=-1
        assert!(close(uv[0], 0.75) && close(uv[1], 0.75));
        // rotation by pi/2 about (0.5,0.5): vertex x=1,y=1 (screen u=1,v=0): (u2,v2)=(0.5,-0.5)
        // -> (u2*0 - v2*1, u2*1 + v2*0) = (0.5, 0.5) -> (1.0, 1.0)
        let v = VertexVars {
            rot: std::f32::consts::FRAC_PI_2,
            warp: 0.0,
            ..Default::default()
        };
        let uv = warp_vertex_uv(&g, g.index(2, 2), &v, &anim, [0.0, 0.0]);
        assert!(close(uv[0], 1.0) && close(uv[1], 1.0));
        // translation + texel offset
        let v = VertexVars {
            dx: 0.1,
            dy: -0.2,
            warp: 0.0,
            ..Default::default()
        };
        let uv = warp_vertex_uv(&g, g.index(1, 1), &v, &anim, [0.001, 0.002]);
        assert!(close(uv[0], 0.5 - 0.1 + 0.001) && close(uv[1], 0.5 + 0.2 + 0.002));
    }

    #[test]
    fn warp_terms_are_applied() {
        let g = MeshGeometry::new(4, 4, 64, 64).unwrap();
        let anim = WarpAnim::new(1.0, 1.0, 1.0);
        assert!(close(anim.f[0], 11.68 + 4.0 * (1.413f32 + 10.0).cos()));
        let a = warp_vertex_uv(
            &g,
            7,
            &VertexVars {
                warp: 0.0,
                ..Default::default()
            },
            &anim,
            [0.0; 2],
        );
        let b = warp_vertex_uv(
            &g,
            7,
            &VertexVars {
                warp: 1.0,
                ..Default::default()
            },
            &anim,
            [0.0; 2],
        );
        let (du, dv) = ((b[0] - a[0]).abs(), (b[1] - a[1]).abs());
        assert!(
            du > 0.0 && du < 0.0141 && dv < 0.0141,
            "warp displacement bounded by 2*0.0035: {du} {dv}"
        );
    }

    /// MilkDrop 3: the four warp terms are added only when `|warp| > 0.0001` (T4: 0.0001 skips, 0.0002 adds
    /// them with the factor `0.0002 * 0.0035`); MD2 adds them for any warp.
    #[test]
    fn tiny_warp_skips_the_terms_under_md3() {
        let g = MeshGeometry::new(4, 4, 64, 64).unwrap();
        let md2 = WarpAnim::new(1.0, 1.0, 1.0);
        let md3 = WarpAnim {
            skip_tiny_warp: true,
            ..md2
        };
        let uv = |warp: f32, anim: &WarpAnim| {
            warp_vertex_uv(
                &g,
                7,
                &VertexVars {
                    warp,
                    ..Default::default()
                },
                anim,
                [0.0; 2],
            )
        };
        let base = uv(0.0, &md3);
        assert_eq!(uv(0.0001, &md3), base, "|warp| <= 1e-4: skipped");
        assert_eq!(uv(-0.0001, &md3), base);
        assert_ne!(uv(0.0001, &md2), base, "MD2 adds the terms");
        let big = uv(0.0002, &md3);
        assert_eq!(big, uv(0.0002, &md2), "above the threshold both add them");
        assert_ne!(big, base);
        assert_eq!(uv(1.0, &md3), uv(1.0, &md2));
    }

    /// `projectm_vertex` is projectM's `InitializeMesh` row for the mirrored mesh row: row 0 (bottom) is
    /// projectM's last row (`y = +1`), the centre vertex has `ang = 0`, and the float rounding is projectM's
    /// (`y = row_pm/gy*2-1`, e.g. row 34 of 36 is 0.8888889 where `-(2/36*2-1)` is not bit-identical).
    #[test]
    fn projectm_vertex_follows_initialize_mesh() {
        let g = MeshGeometry::new(48, 36, 640, 360).unwrap();
        let (p0, rad0, _) = g.projectm_vertex(0, 0);
        assert_eq!(p0, [-1.0, 1.0]);
        assert!(close(rad0, (1.0f32 + (360.0f32 / 640.0).powi(2)).sqrt()));
        let (pc, radc, angc) = g.projectm_vertex(24, 18);
        assert_eq!((pc, radc, angc), ([0.0, 0.0], 0.0, 0.0));
        // mesh row 2 of projectM (top) is lactea row 34; its y is exactly 2/36*2-1
        let (p, rad, ang) = g.projectm_vertex(24, 34);
        assert_eq!(p[1], 2.0f32 / 36.0 * 2.0 - 1.0);
        assert_eq!(rad, (p[1] * g.aspect_y).abs());
        assert!(ang < 0.0, "top half has negative atan2(y, x)");
        // centre row, left half: atan2f(+0, -x) = +pi
        assert_eq!(g.projectm_vertex(3, 18).2, std::f32::consts::PI);
    }

    #[test]
    fn negative_zoom_is_nan_on_the_gpu_but_finite_in_md2() {
        let g = MeshGeometry::new(4, 4, 64, 64).unwrap();
        let v = VertexVars {
            zoom: -0.5,
            ..Default::default()
        };
        let uv = warp_vertex_uv(&g, 7, &v, &WarpAnim::projectm(1.0, 1.0, 1.0), [0.0; 2]);
        assert!(uv[0].is_nan() && uv[1].is_nan());
        let uv = warp_vertex_uv(&g, 7, &v, &WarpAnim::new(1.0, 1.0, 1.0), [0.0; 2]);
        assert!(uv[0].is_finite() && uv[1].is_finite());
        // positive zoom is unaffected
        let v = VertexVars {
            zoom: 1.5,
            ..Default::default()
        };
        let uv = warp_vertex_uv(&g, 7, &v, &WarpAnim::projectm(1.0, 1.0, 1.0), [0.0; 2]);
        assert!(uv[0].is_finite() && uv[1].is_finite());
    }
}
