//! insta snapshots of the translator output (WGSL after the frozen prelude + interface) for
//! hand-picked, representative shaders. Review changes with `cargo insta review` (or inspect
//! the `.snap.new` files).
mod common;

use common::*;
use lactea_abi::*;
use lactea_hlsl::*;

fn snap(name: &str, kind: PassKind, src: &str) {
    snap_with(name, kind, src, &Options::default());
}

fn snap_with(name: &str, kind: PassKind, src: &str, opts: &Options) {
    let t = translate(kind, src, opts).unwrap_or_else(|e| panic!("{name}: {e}"));
    naga_validate(&t.wgsl).unwrap_or_else(|e| panic!("{name}: naga: {e}\n{}", t.wgsl));
    let rest = t
        .wgsl
        .split_once(WGSL_COMMON)
        .map(|x| x.1)
        .unwrap_or(&t.wgsl);
    let out = format!(
        "{rest}\n// interface: {:#?}\n// texsize_params: {:?}\n",
        t.interface, t.texsize_params
    );
    insta::assert_snapshot!(name, out);
}

#[test]
fn default_warp() {
    snap(
        "default_warp",
        PassKind::Warp,
        &default_warp_shader(0.98, true),
    );
}

#[test]
fn default_comp_video_echo() {
    snap(
        "default_comp_video_echo",
        PassKind::Comp,
        &default_comp_shader(1.2, 0.5, 1.25, 1, 0.3, true, false, true, false, false),
    );
}

#[test]
fn noise_blur_texsize() {
    snap(
        "noise_blur_texsize",
        PassKind::Comp,
        "sampler sampler_pw_noise_lq;
shader_body {
    float2 n = tex2D(sampler_pw_noise_lq, uv * texsize.xy * texsize_noise_lq.zw + rand_frame.xy).xy;
    ret = GetBlur1(uv + n * 0.01) * 1.5 + GetBlur3(uv) * 0.25;
    ret = lerp(ret, GetPixel(uv_orig), saturate(bass_att - 0.5));
}",
    );
}

#[test]
fn user_functions_out_params() {
    snap(
        "user_functions_out_params",
        PassKind::Warp,
        "float2 rot2(float2 p, float a) { float s, c; sincos(a, s, c); return float2(p.x*c - p.y*s, p.x*s + p.y*c); }
void split(float x, out float ip, out float fp) { fp = modf(x, ip); }
shader_body {
    float i, f;
    split(uv.x * 8, i, f);
    float2 p = rot2(uv - 0.5, time * 0.3 + i);
    ret = tex2D(sampler_main, p + 0.5).xyz * (0.5 + 0.5 * f);
}",
    );
}

#[test]
fn matrices_and_rotations() {
    snap(
        "matrices_and_rotations",
        PassKind::Warp,
        "shader_body {
    float2x2 m = float2x2(q5, q6, -q6, q5) * 0.5;
    float2 v = mul(uv - 0.5, m) + mul(m, float2(0.1, 0.2));
    float3 w = mul(float4(v, 0.3, 1), rot_f1) + mul(rot_s2, float3(v, 1));
    ret = tex2D(sampler_main, frac(w.xy + 0.5)).xyz;
}",
    );
}

#[test]
fn control_flow() {
    snap(
        "control_flow",
        PassKind::Warp,
        "shader_body {
    float3 acc = 0;
    for (int i = 0; i < 6; i++) {
        if (i == 4) break;
        if (i % 2 == 0) continue;
        acc += tex2D(sampler_main, uv + i * 0.01).xyz;
    }
    int k = 0;
    while (k < 3) { acc *= 0.9; k++; }
    do { acc += 0.01; k--; } while (k > 0);
    ret = acc;
}",
    );
}

#[test]
fn swizzles_ternary_and_ints() {
    snap(
        "swizzles_ternary_and_ints",
        PassKind::Warp,
        "shader_body {
    float3 a = float3(uv, 0.5);
    a.zx = a.xz * 0.5;
    a.y += 0.25;
    int n = int(q3 * 4);
    float m = n / 3 + n % 2;
    bool b = uv.x > 0.5;
    ret = b ? a : a.zyx * m;
    ret += (a > 0.4) * 0.1;
    uv.x = a.x;
    ret *= q27;
    q27 = 0.5;
}",
    );
}

#[test]
fn user_textures_and_volume() {
    snap(
        "user_textures_and_volume",
        PassKind::Warp,
        "sampler sampler_fw_clouds;
sampler3D sampler_fw_noisevol_hq;
sampler2D sampler_rand02_smalltiled;
float4 texsize_clouds;
shader_body {
    float3 c = tex2D(sampler_fw_clouds, uv * texsize.xy * texsize_clouds.zw).xyz;
    float3 v = tex3D(sampler_fw_noisevol_hq, float3(uv * 2, time * 0.05)).xyz;
    float3 r = tex2D(sampler_rand02_smalltiled, uv_orig).xyz;
    ret = c * v + r * 0.1;
}",
    );
}

#[test]
fn preprocessor_macros() {
    snap(
        "preprocessor_macros",
        PassKind::Warp,
        "#define SAMP(uv) tex2D(sampler_main, uv).xyz
#define K 3
#if K > 2
#define W 0.25
#else
#define W 0.75
#endif
shader_body {
    ret = SAMP(uv) * W + SAMP(uv + float2(K, 0) * texsize.zw) * (1 - W);
}",
    );
}

#[test]
fn comp_hue_shader_and_globals() {
    snap(
        "comp_hue_shader_and_globals",
        PassKind::Comp,
        "static float3 tint = normalize(roam_cos.xyz + 0.5);
static const float2x2 ofs = float2x2(texsize.z, 0, 0, texsize.w) * 2;
float3 glow(float2 p) { return saturate(0.05 / length(p - 0.5)) * tint; }
shader_body {
    float2 p = mul(uv - 0.5, ofs) / texsize.zw;
    ret = tex2D(sampler_main, uv).xyz * hue_shader + glow(uv) * pow(abs(p.x), 1.5);
}",
    );
}

#[test]
fn md3_fft_mouse_and_q() {
    let mut opts = Options::default();
    opts.md3 = true;
    snap_with(
        "md3_fft_mouse_and_q",
        PassKind::Warp,
        "sampler2D sampler_pw_fft;
shader_body
{
    float2 p = mouse.xy - uv;
    q40 = q40 + mouse_z;
    float bar = get_fft(uv.x) * q33 + get_fft_hz(440 * (1 + q64));
    ret = tex2D(sampler_main, uv + p * mouse_w).xyz * (1 - bar)
        + tex2D(sampler_pw_fft, float2(uv.x * 3, 0)).r * q40;
}",
        &opts,
    );
}
