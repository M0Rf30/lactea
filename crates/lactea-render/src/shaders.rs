// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Built-in WGSL: fixed-function replacements, blur passes, final blit, and the vertex shader
//! shared with user warp / composite shaders.

/// Vertex shader for every `MeshVertex` pass (warp, composite, textured fills). Output layout
/// matches `lactea_abi::WGSL_COMMON`'s `VsOut` (locations 0..2 + position).
pub const MESH_VS: &str = r#"
struct VOut {
    @builtin(position) position: vec4<f32>,
    @location(0) diffuse: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) rad_ang: vec2<f32>,
};
@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) diffuse: vec4<f32>,
           @location(2) uv: vec4<f32>, @location(3) rad_ang: vec2<f32>) -> VOut {
    var o: VOut;
    o.position = vec4<f32>(pos, 0.0, 1.0);
    o.diffuse = diffuse;
    o.uv = uv;
    o.rad_ang = rad_ang;
    return o;
}
"#;

/// projectM's warp vertex shader (`PresetWarpVertexShaderGlsl330.vert`), statement for statement, so the GPU's own
/// `pow` / `sin` / `cos` and its multiply-add fusion produce the same UVs as projectM's GL driver.
/// `group` is the bind group of [`WarpVsUniforms`]. Output layout = [`MESH_VS`] (`VsOut`).
pub fn warp_vs(group: u32) -> String {
    warp_vs_src(group, false, 0)
}

/// The same vertex stage, but every vertex is drawn as a point into texel `(i % (gx+1), i / (gx+1))` of a
/// `(gx+1) x (gy+1)` target (`wu.diffuse.xy` = `(gx+1, gy+1)`), with the fragment stage `fs_probe` writing
/// `uv.xy`: reads the GPU's own per-vertex UVs back (what projectM's `RG16F` UV map interpolates).
pub fn warp_uv_readback(group: u32) -> String {
    warp_vs_src(group, true, 0)
}

/// `lactea_abi::WGSL_COMMON` followed by every fold function: the declarations a fold vertex stage shares with the
/// user fragment shader (`mu` / `mx` / `pu`, `fold_k`).
fn fold_prelude(folds: &[String]) -> String {
    let mut s = String::from(lactea_abi::WGSL_COMMON);
    for f in folds {
        s.push('\n');
        s.push_str(f);
    }
    s.push('\n');
    s
}

/// `@location(3 + j) fold{j}: vec4<f32>,` members of a vertex output struct, `ceil(n / 4)` of them.
fn fold_members(n: usize) -> String {
    (0..n.div_ceil(4))
        .map(|j| format!("    @location({}) fold{j}: vec4<f32>,\n", 3 + j))
        .collect()
}

/// Statements filling `o.fold{j}` from `o.uv`, `o.rad_ang`, `o.diffuse`: component `k % 4` of `fold{k / 4}` is
/// `fold_k(..)`, the unused components of the last vec4 are 0.
fn fold_stmts(n: usize) -> String {
    let mut s = String::new();
    for j in 0..n.div_ceil(4) {
        let comps: Vec<String> = (0..4)
            .map(|c| {
                let k = 4 * j + c;
                if k < n {
                    format!("fold_{k}(o.uv, o.rad_ang, o.diffuse)")
                } else {
                    "0.0".into()
                }
            })
            .collect();
        s.push_str(&format!(
            "    o.fold{j} = vec4<f32>({});\n",
            comps.join(", ")
        ));
    }
    s
}

/// [`MESH_VS`] plus the fold varyings: module text = `WGSL_COMMON`, the fold functions and a vertex stage with the
/// same attributes, entry name and varyings 0..2 whose output also carries `fold{j}` (locations `3 + j`).
pub fn mesh_vs_folds(folds: &[String]) -> String {
    format!(
        r#"{prelude}
struct VOut {{
    @builtin(position) position: vec4<f32>,
    @location(0) diffuse: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) rad_ang: vec2<f32>,
{members}}};
@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) diffuse: vec4<f32>,
           @location(2) uv: vec4<f32>, @location(3) rad_ang: vec2<f32>) -> VOut {{
    var o: VOut;
    o.position = vec4<f32>(pos, 0.0, 1.0);
    o.diffuse = diffuse;
    o.uv = uv;
    o.rad_ang = rad_ang;
{stmts}    return o;
}}
"#,
        prelude = fold_prelude(folds),
        members = fold_members(folds.len()),
        stmts = fold_stmts(folds.len()),
    )
}

/// [`warp_vs`] plus the fold varyings (see [`mesh_vs_folds`]); the warp math is the same text. `group` must not be
/// 0 or 1, which `WGSL_COMMON` uses.
pub fn warp_vs_folds(group: u32, folds: &[String]) -> String {
    format!(
        "{}{}",
        fold_prelude(folds),
        warp_vs_src(group, false, folds.len())
    )
}

/// `n_folds` extra `fold{j}` outputs (see [`warp_vs_folds`]); the module must then declare the fold functions.
fn warp_vs_src(group: u32, probe: bool, n_folds: usize) -> String {
    let (vid_param, position) = if probe {
        (
            ", @builtin(vertex_index) vid: u32",
            "let gw = u32(wu.diffuse.x);\n    \
             o.position = vec4<f32>((f32(vid % gw) + 0.5) / wu.diffuse.x * 2.0 - 1.0, \
             1.0 - (f32(vid / gw) + 0.5) / wu.diffuse.y * 2.0, 0.0, 1.0);",
        )
    } else {
        (
            "",
            "// glm::ortho(-1, 1, 1, -1): y is flipped\n    o.position = vec4<f32>(pos.x, -pos.y, 0.0, 1.0);",
        )
    };
    let fs = if probe {
        r#"
@fragment
fn fs_probe(in: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.uv.xy, 0.0, 0.0);
}
"#
    } else {
        ""
    };
    format!(
        r#"
struct WarpVsUniforms {{
    aspect: vec4<f32>,     // aspectX, aspectY, invAspectX, invAspectY
    timing: vec4<f32>,     // warpTime, warpScaleInverse, texelOffset.x, texelOffset.y
    factors: vec4<f32>,    // warpFactors
    diffuse: vec4<f32>,    // frag_COLOR (decay or 1)
}};
@group({group}) @binding(0) var<uniform> wu: WarpVsUniforms;

struct VOut {{
    @builtin(position) position: vec4<f32>,
    @location(0) diffuse: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) rad_ang: vec2<f32>,
{members}}};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) rad_ang: vec2<f32>, @location(2) transforms: vec4<f32>,
           @location(3) warp_center: vec2<f32>, @location(4) warp_distance: vec2<f32>,
           @location(5) stretch: vec2<f32>{vid_param}) -> VOut {{
    let radius = rad_ang.x;
    let zoom = transforms.x;
    let zoom_exp = transforms.y;
    let rot = transforms.z;
    let warp = transforms.w;
    let aspect_x = wu.aspect.x;
    let aspect_y = wu.aspect.y;
    let inv_aspect_x = wu.aspect.z;
    let inv_aspect_y = wu.aspect.w;
    let warp_time = wu.timing.x;
    let warp_scale_inverse = wu.timing.y;
    let texel_offset = wu.timing.zw;
    let wf = wu.factors;

    var o: VOut;
    {position}

    let zoom2 = pow(zoom, pow(zoom_exp, radius * 2.0 - 1.0));
    let zoom2_inverse = 1.0 / zoom2;

    var u = pos.x * aspect_x * 0.5 * zoom2_inverse + 0.5;
    var v = pos.y * aspect_y * 0.5 * zoom2_inverse + 0.5;

    let uv_original = vec2<f32>(pos.x * 0.5 + 0.5 + texel_offset.x,
                                pos.y * 0.5 + 0.5 + texel_offset.y);

    u = (u - warp_center.x) / stretch.x + warp_center.x;
    v = (v - warp_center.y) / stretch.y + warp_center.y;

    u += warp * 0.0035 * sin(warp_time * 0.333 + warp_scale_inverse * (pos.x * wf.x - pos.y * wf.w));
    v += warp * 0.0035 * cos(warp_time * 0.375 - warp_scale_inverse * (pos.x * wf.z + pos.y * wf.y));
    u += warp * 0.0035 * cos(warp_time * 0.753 - warp_scale_inverse * (pos.x * wf.y - pos.y * wf.z));
    v += warp * 0.0035 * sin(warp_time * 0.825 + warp_scale_inverse * (pos.x * wf.x + pos.y * wf.w));

    let u2 = u - warp_center.x;
    let v2 = v - warp_center.y;

    let cos_rotation = cos(rot);
    let sin_rotation = sin(rot);
    u = u2 * cos_rotation - v2 * sin_rotation + warp_center.x;
    v = u2 * sin_rotation + v2 * cos_rotation + warp_center.y;

    u -= warp_distance.x;
    v -= warp_distance.y;

    u = (u - 0.5) * inv_aspect_x + 0.5;
    v = (v - 0.5) * inv_aspect_y + 0.5;

    u += texel_offset.x;
    v += texel_offset.y;

    o.diffuse = wu.diffuse;
    o.uv = vec4<f32>(u, v, uv_original);
    o.rad_ang = rad_ang;
{stmts}    return o;
}}
{fs}"#,
        members = fold_members(n_folds),
        stmts = fold_stmts(n_folds),
    )
}

/// Fixed-function "texture modulate diffuse" (D3DTOP_MODULATE colour, alpha = diffuse alpha).
pub const TEX_MOD_FS: &str = r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
struct FIn {
    @builtin(position) position: vec4<f32>,
    @location(0) diffuse: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) rad_ang: vec2<f32>,
};
@fragment
fn fs_main(in: FIn) -> @location(0) vec4<f32> {
    let t = textureSample(tex, samp, in.uv.xy);
    return vec4<f32>(t.rgb * in.diffuse.rgb, in.diffuse.a);
}
// projectM (`TexturedDrawFragmentShader`): colour = diffuse * texel, alpha included.
@fragment
fn fs_main_ta(in: FIn) -> @location(0) vec4<f32> {
    return in.diffuse * textureSample(tex, samp, in.uv.xy);
}
"#;

/// Flat coloured geometry (`WFVERTEX`).
pub const FLAT_SRC: &str = r#"
struct VOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};
@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> VOut {
    var o: VOut;
    o.position = vec4<f32>(pos, 0.0, 1.0);
    o.color = color;
    return o;
}
@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

/// Blur passes (milkdropfs.cpp `BlurPasses`, blur1_ps.fx / blur2_ps.fx). One uniform block:
/// `c0` = source texsize + inverse, `c1` = w1..w4, `c2` = d1..d4, `c3` = (scale, bias, w_div, 0),
/// `c5` = (w1, w2, d1, d2), `c6` = (w_div, edge_darken_c1, c2, c3).
pub const BLUR_SRC: &str = r#"
struct BlurParams {
    c0: vec4<f32>,
    c1: vec4<f32>,
    c2: vec4<f32>,
    c3: vec4<f32>,
    c5: vec4<f32>,
    c6: vec4<f32>,
};
@group(0) @binding(0) var<uniform> bp: BlurParams;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
    // full-screen triangle; uv.y = 0 at the top of the target
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var o: VOut;
    let xy = p[vi];
    o.position = vec4<f32>(xy, 0.0, 1.0);
    o.uv = vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - xy.y * 0.5);
    return o;
}

@fragment
fn fs_blur_h(in: VOut) -> @location(0) vec4<f32> {
    // LONG HORIZ. PASS 1 (blur1_ps.fx). "+ moves blur UP, LEFT by 1-pixel increments".
    let uv2 = in.uv + bp.c0.zw * vec2<f32>(1.0, bp.c3.w);
    let dx = bp.c0.z;
    var blur =
        (textureSampleLevel(tex, samp, uv2 + vec2<f32>( bp.c2.x * dx, 0.0), 0.0).xyz +
         textureSampleLevel(tex, samp, uv2 + vec2<f32>(-bp.c2.x * dx, 0.0), 0.0).xyz) * bp.c1.x +
        (textureSampleLevel(tex, samp, uv2 + vec2<f32>( bp.c2.y * dx, 0.0), 0.0).xyz +
         textureSampleLevel(tex, samp, uv2 + vec2<f32>(-bp.c2.y * dx, 0.0), 0.0).xyz) * bp.c1.y +
        (textureSampleLevel(tex, samp, uv2 + vec2<f32>( bp.c2.z * dx, 0.0), 0.0).xyz +
         textureSampleLevel(tex, samp, uv2 + vec2<f32>(-bp.c2.z * dx, 0.0), 0.0).xyz) * bp.c1.z +
        (textureSampleLevel(tex, samp, uv2 + vec2<f32>( bp.c2.w * dx, 0.0), 0.0).xyz +
         textureSampleLevel(tex, samp, uv2 + vec2<f32>(-bp.c2.w * dx, 0.0), 0.0).xyz) * bp.c1.w;
    blur = blur * bp.c3.z;
    blur = blur * bp.c3.x + bp.c3.y;
    return vec4<f32>(blur, 1.0);
}

@fragment
fn fs_blur_v(in: VOut) -> @location(0) vec4<f32> {
    // SHORT VERTICAL PASS 2 (blur2_ps.fx)
    let uv2 = in.uv;
    let dy = bp.c0.w;
    var blur =
        (textureSampleLevel(tex, samp, uv2 + vec2<f32>(0.0,  bp.c5.z * dy), 0.0).xyz +
         textureSampleLevel(tex, samp, uv2 + vec2<f32>(0.0, -bp.c5.z * dy), 0.0).xyz) * bp.c5.x +
        (textureSampleLevel(tex, samp, uv2 + vec2<f32>(0.0,  bp.c5.w * dy), 0.0).xyz +
         textureSampleLevel(tex, samp, uv2 + vec2<f32>(0.0, -bp.c5.w * dy), 0.0).xyz) * bp.c5.y;
    blur = blur * bp.c6.x;
    // tone it down at the edges (only on the 1st vertical pass)
    var t = min(min(in.uv.x, in.uv.y), 1.0 - max(in.uv.x, in.uv.y));
    t = sqrt(t);
    t = bp.c6.y + bp.c6.z * clamp(t * bp.c6.w, 0.0, 1.0);
    blur = blur * t;
    return vec4<f32>(blur, 1.0);
}
"#;

/// Final blit of the internal Rgba8Unorm image to the host target. Nearest `textureLoad`, so
/// equal sizes copy bit-exactly; for `*Srgb` targets the value is sRGB-decoded first so the
/// hardware's linear->sRGB encode stores the original bytes.
pub const BLIT_SRC: &str = r#"
struct BlitParams {
    // x,y = src size, z,w = dst size
    sizes: vec4<f32>,
    // x = 1 when the target is an sRGB format
    flags: vec4<f32>,
};
@group(0) @binding(0) var<uniform> bp: BlitParams;
@group(0) @binding(1) var tex: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(p[vi], 0.0, 1.0);
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let src = vec2<i32>(floor(pos.xy * bp.sizes.xy / bp.sizes.zw));
    let c = textureLoad(tex, clamp(src, vec2<i32>(0), vec2<i32>(bp.sizes.xy) - vec2<i32>(1)), 0);
    var rgb = c.rgb;
    if (bp.flags.x > 0.5) {
        rgb = srgb_to_linear(rgb);
    }
    return vec4<f32>(rgb, 1.0);
}
"#;
