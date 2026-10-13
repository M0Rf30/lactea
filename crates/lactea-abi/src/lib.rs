//! Frozen contract shared by every lactea crate.
//!
//! Everything here mirrors MilkDrop 2 (vis_milk2, `third_party/md2/`) one-to-one so the
//! translator (`lactea-hlsl`), the GPU backend (`lactea-render`), the engine and the
//! fidelity tooling (`oracle/`, `lactea-compare`) agree without talking to each other.
//!
//! Changing anything in this crate is a cross-crate change: update every consumer in the
//! same commit.

#![warn(missing_docs)]

use bytemuck::{Pod, Zeroable};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Number of `q` variables shared between per-frame equations and shaders.
pub const NUM_Q: usize = 32;
/// Number of `t1`..`t8` per-object variables of custom waves and shapes.
pub const NUM_T: usize = 8;
/// `q` variables of MilkDrop 3 (`q1..q64`); the first [`NUM_Q`] live in [`MilkUniforms::q`], the rest in
/// [`MilkExtUniforms::q_hi`].
pub const NUM_Q_MD3: usize = 64;
/// `t1..t16` per-object variables of MilkDrop 3 custom waves and shapes.
pub const NUM_T_MD3: usize = 16;
/// Bins of the MilkDrop 3 shader FFT texture ([`TexSource::Fft`]): a 512 x 1 `R32Float` texture.
pub const FFT_BINS: usize = 512;
/// Number of rotation matrices in [`MilkUniforms::rot`].
pub const NUM_ROT: usize = 24;
/// MilkDrop 2 targets ps_3_0: at most 16 samplers per pass. Also wgpu's default limit.
pub const MAX_PASS_TEXTURES: usize = 16;
/// MilkDrop 2 internal textures are 8 bits per channel (`m_nTexBitsPerCh = 8`).
/// Feedback quantization is part of the look: never use float targets for the main image.
pub const INTERNAL_FORMAT_NAME: &str = "Rgba8Unorm";

/// Shader constants, laid out exactly like `include.fx` + `CPlugin::ApplyShaderParams`
/// (milkdropfs.cpp). One instance per pass draw; lives at `@group(0) @binding(0)`.
///
/// Register meaning (MD2 names):
/// - `c[0]`  `aspect`: (aspect_x, aspect_y, 1/aspect_x, 1/aspect_y)
/// - `c[1]`  aspect for the "sd" variant (see ApplyShaderParams)
/// - `c[2]`  (time, fps, frame, progress)
/// - `c[3]`  (bass, mid, treb, vol)              — imm_rel
/// - `c[4]`  (bass_att, mid_att, treb_att, vol_att) — avg_rel
/// - `c[5]`  (blur1 scale, blur1 bias, blur2 scale, blur2 bias)
/// - `c[6]`  (blur3 scale, blur3 bias, blur1_min, blur1_max)
/// - `c[7]`  `texsize`: (w, h, 1/w, 1/h)
/// - `c[8]`  `roam_cos`, `c[9]` `roam_sin`, `c[10]` `slow_roam_cos`, `c[11]` `slow_roam_sin`
/// - `c[12]` mip info (mip_x, mip_y, mip_avg, unused)
/// - `c[13]` (blur2_min, blur2_max, blur3_min, blur3_max)
///
/// Exact values (including MD2 quirks such as the `vol` comma-operator bug in
/// ApplyShaderParams) are the GPU backend's responsibility; this struct only fixes layout.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct MilkUniforms {
    /// Per-frame random values (`rand_frame`).
    pub rand_frame: [f32; 4],
    /// Per-preset random values (`rand_preset`).
    pub rand_preset: [f32; 4],
    /// `_c0` .. `_c13`.
    pub c: [[f32; 4]; 14],
    /// `_qa` .. `_qh`; `q1 == q[0][0]`, `q32 == q[7][3]`.
    pub q: [[f32; 4]; 8],
    /// `rot_s1..4, rot_d1..4, rot_f1..4, rot_vf1..4, rot_uf1..4, rot_rand1..4` in that order
    /// (index = MD2 `rot_mat[i]`). Each HLSL `float4x3` is stored as WGSL `mat3x4<f32>`:
    /// `rot[i][col][row] = D3DXMATRIX.m[row][col]` for `col < 3`, `row < 4`, so that HLSL
    /// `mul(float4 v, rot_x)` == WGSL `v * rot_x`.
    pub rot: [[[f32; 4]; 3]; NUM_ROT],
    /// `texsize_noise_lq, _lq_lite, _mq, _hq, texsize_noisevol_lq, _hq` as (w, h, 1/w, 1/h).
    pub texsize_noise: [[f32; 4]; 6],
}

impl Default for MilkUniforms {
    fn default() -> Self {
        Zeroable::zeroed()
    }
}

/// MilkDrop 3 shader inputs that do not fit the frozen [`MilkUniforms`] block, at `@group(0) @binding(1)`
/// (`mx` in [`WGSL_COMMON`]). Always bound by the renderer; all-default (`mouse = (-1, -1, 0, 0)`, zero `q_hi`)
/// outside MilkDrop 3.
///
/// - `mouse` = `_c14`: `(x, y, held, released)`; `x = px/W`, `y = py/H` (top origin) and `-1, -1` when the cursor is
///   outside the window; `held` is 1 while the left button is down, `released` 1 for the one frame after it.
/// - `q_hi` = `_qi .. _qp`: `q33 == q_hi[0][0]`, `q64 == q_hi[7][3]`.
/// - `mxd3q` = `_mxd3qxa .. _mxd3qsc`: the audio-envelope / beat-latch float4s of the MilkDrop 3 block.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct MilkExtUniforms {
    /// `_c14` (mouse).
    pub mouse: [f32; 4],
    /// `_qi` .. `_qp`.
    pub q_hi: [[f32; 4]; 8],
    /// `_mxd3qxa` .. `_mxd3qsc`.
    pub mxd3q: [[f32; 4]; 7],
}

impl Default for MilkExtUniforms {
    fn default() -> Self {
        Self {
            mouse: [-1.0, -1.0, 0.0, 0.0],
            q_hi: [[0.0; 4]; 8],
            mxd3q: [[0.0; 4]; 7],
        }
    }
}

/// Per-pass uniforms at `@group(1) @binding(0)`: `texsize_<name>` values for the textures a
/// translated shader references, indexed by [`ShaderInterface::texsize_slots`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct PassUniforms {
    /// `texsize` (w, h, 1/w, 1/h) per referenced texture slot.
    pub texsize: [[f32; 4]; MAX_PASS_TEXTURES],
}

impl Default for PassUniforms {
    fn default() -> Self {
        Zeroable::zeroed()
    }
}

/// WGSL declarations every warp/comp module (translated or built-in) starts with.
/// The translator emits `WGSL_COMMON` + its own group(1) texture bindings + body +
/// `@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32>`.
pub const WGSL_COMMON: &str = r#"
struct MilkUniforms {
    rand_frame: vec4<f32>,
    rand_preset: vec4<f32>,
    c: array<vec4<f32>, 14>,
    q: array<vec4<f32>, 8>,
    rot: array<mat3x4<f32>, 24>,
    texsize_noise: array<vec4<f32>, 6>,
};
@group(0) @binding(0) var<uniform> mu: MilkUniforms;

struct MilkExt {
    mouse: vec4<f32>,
    q_hi: array<vec4<f32>, 8>,
    mxd3q: array<vec4<f32>, 7>,
};
@group(0) @binding(1) var<uniform> mx: MilkExt;

struct PassUniforms {
    texsize: array<vec4<f32>, 16>,
};
@group(1) @binding(0) var<uniform> pu: PassUniforms;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    // MD2 vertex Diffuse (warp: per-vertex color/alpha; comp: hue_shader corner colors)
    @location(0) diffuse: vec4<f32>,
    // xy = uv (warped UV in the warp pass), zw = uv_orig
    @location(1) uv: vec4<f32>,
    // x = rad, y = ang
    @location(2) rad_ang: vec2<f32>,
};
"#;

/// Bind group of the per-frame [`MilkUniforms`].
pub const GROUP_FRAME: u32 = 0;
/// Bind group of the per-pass [`PassUniforms`] and the shader's textures.
pub const GROUP_PASS: u32 = 1;
/// Binding of [`MilkUniforms`] in [`GROUP_FRAME`].
pub const BINDING_MILK_UNIFORMS: u32 = 0;
/// Binding of [`PassUniforms`] in [`GROUP_PASS`].
pub const BINDING_PASS_UNIFORMS: u32 = 0;
/// Binding of [`MilkExtUniforms`] in [`GROUP_FRAME`].
pub const BINDING_MILK_EXT: u32 = 1;
/// Binding of texture `i` of [`ShaderInterface::textures`] in [`GROUP_PASS`].
pub const fn texture_binding(i: u32) -> u32 {
    1 + 2 * i
}
/// Binding of the sampler paired with texture `i` in [`GROUP_PASS`].
pub const fn sampler_binding(i: u32) -> u32 {
    2 + 2 * i
}
/// Entry point name of the generated fragment shader.
pub const FRAGMENT_ENTRY: &str = "fs_main";

/// Vertex format of the warp mesh and composite mesh (MD2 `MYVERTEX` + rad/ang texcoord).
/// `pos` is clip space, x right, y up. Varyings map 1:1 onto [`WGSL_COMMON`]'s `VsOut`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct MeshVertex {
    /// Clip-space position.
    pub pos: [f32; 2],
    /// Vertex colour (MD2 `Diffuse`).
    pub diffuse: [f32; 4],
    /// Texture coordinates.
    pub uv: [f32; 4],
    /// Polar radius and angle of the vertex.
    pub rad_ang: [f32; 2],
}

/// Which pass a translated shader runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum PassKind {
    /// Warp pass.
    Warp,
    /// Composite pass.
    Comp,
}

/// Built-in noise textures (`noise_*` / `noisevol_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum NoiseTex {
    /// `noise_lq`.
    Lq,
    /// `noise_lq_lite`.
    LqLite,
    /// `noise_mq`.
    Mq,
    /// `noise_hq`.
    Hq,
    /// `noisevol_lq` (3D).
    VolLq,
    /// `noisevol_hq` (3D).
    VolHq,
}

impl NoiseTex {
    /// Index into [`MilkUniforms::texsize_noise`].
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// What a shader sampler reads from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TexSource {
    /// Previous frame image (`sampler_main`, `sampler_fw_main`, ...).
    Main,
    /// `sampler_blur1..3` (value 1..=3).
    Blur(u8),
    /// Built-in noise texture.
    Noise(NoiseTex),
    /// User texture referenced by name, lowercased, without prefix/extension
    /// (`sampler_fw_clouds2` -> `"clouds2"`, `sampler_rand03_smalltiled` -> `"rand03_smalltiled"`).
    /// Resolution to an actual image (and MD2 `randXX` selection) is the engine's job.
    User(String),
    /// MilkDrop 3 shader FFT (`sampler_fft`): a [`FFT_BINS`] x 1 `R32Float` texture holding the smoothed spectrum.
    /// The translator lowers every lookup of such a sampler to a manual clamp + bilinear (or point) fetch with
    /// `textureLoad`, because `R32Float` is not filterable without `FLOAT32_FILTERABLE`. Only produced for
    /// translations made with the MD3 prelude; otherwise `sampler_fft` is the user texture `"fft"`.
    Fft,
}

/// Sampler filter mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Filter {
    /// Bilinear.
    Linear,
    /// Nearest neighbour.
    Point,
}

/// Sampler addressing mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Address {
    /// Repeat.
    Wrap,
    /// Clamp to edge.
    Clamp,
}

/// Texture dimensionality of a sampler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TexDim {
    /// 2D texture.
    D2,
    /// 3D (volume) texture.
    D3,
}

/// One sampler of a translated shader and how it is filtered and addressed.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TextureBinding {
    /// Sampler identifier as written in the HLSL source (e.g. `sampler_fw_main`).
    pub hlsl_name: String,
    /// What the sampler reads from.
    pub source: TexSource,
    /// Minification/magnification filter.
    pub filter: Filter,
    /// Addressing mode for both axes.
    pub address: Address,
    /// Texture dimensionality.
    pub dim: TexDim,
}

/// Resource interface of a translated warp/comp shader. The GPU backend builds the
/// group(1) bind group from this, in order.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ShaderInterface {
    /// Texture bindings in group(1) order.
    pub textures: Vec<TextureBinding>,
    /// `PassUniforms.texsize[i]` holds the size of `texsize_slots[i]`.
    pub texsize_slots: Vec<TexSource>,
    /// Highest blur level (0..=3) referenced; MD2 only renders blur passes that are used.
    pub max_blur_level: u8,
}

/// Deterministic run settings shared by the projectM reference renderer, lactea and the comparator.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct RunConfig {
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Warp mesh grid (cells), e.g. [48, 36]. MD2: gridY = gridX * 3 / 4.
    pub mesh: [u32; 2],
    /// Frame rate; frame `n` is rendered at `n / fps` seconds.
    pub fps: u32,
    /// Number of frames to render.
    pub frames: u32,
    /// Seed for the deterministic random sources.
    pub seed: u64,
    /// In texels. D3D9 MD2 uses 0.5 to compensate D3D9 pixel centers; on modern APIs the
    /// equivalent net behaviour is 0.0 (projectM default). Kept configurable for experiments.
    pub texel_offset: [f32; 2],
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            mesh: [48, 36],
            fps: 60,
            frames: 300,
            seed: 1,
            texel_offset: [0.0, 0.0],
        }
    }
}

/// Audio feeding rule shared by every producer: before rendering frame `n`, push the PCM
/// sample frames `[start, end)` returned here (per channel; interleave as the API requires).
/// Frame time is exactly `n / fps` seconds.
///
/// `fps` must be non-zero. The products are computed in `u128`, so large frame numbers
/// cannot overflow intermediate math.
pub fn pcm_range(frame: u64, sample_rate: u64, fps: u64) -> (u64, u64) {
    let at = |f: u64| (u128::from(f) * u128::from(sample_rate) / u128::from(fps)) as u64;
    (at(frame), at(frame + 1))
}

/// On-disk dump layout produced by the projectM reference renderer and by lactea,
/// consumed by `lactea-compare`.
///
/// ```text
/// <dir>/meta.json                 DumpMeta
/// <dir>/<stage>/<frame:05>.png    RGBA8, row 0 = top of the screen as finally displayed
/// <dir>/vars/<frame:05>.json      {"name": f64, ...} per-frame vars after per-frame eqs
///                                 (lowercase; q1..q32; rand_frame.x..w; rand_preset.x..w)
/// <dir>/textures/<name>.png       optional static textures (noise_lq, ...); 3D as .bin
/// ```
pub mod dump {
    use super::RunConfig;
    #[cfg(feature = "serde")]
    use serde::{Deserialize, Serialize};

    /// Dump metadata file name inside a dump directory.
    pub const META_FILE: &str = "meta.json";
    /// Per-frame variable dump sub-directory.
    pub const VARS_DIR: &str = "vars";
    /// Optional static texture sub-directory.
    pub const TEXTURES_DIR: &str = "textures";

    /// Which intermediate or final image a dump frame holds.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
    #[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
    pub enum Stage {
        /// Composite output (what the user sees).
        Final,
        /// Internal image after warp + shapes + waves + borders; next frame's `sampler_main`.
        Main,
        /// Internal image right after the warp pass, before anything is drawn on top.
        Warped,
        /// Blur pyramid level 1.
        Blur1,
        /// Blur pyramid level 2.
        Blur2,
        /// Blur pyramid level 3.
        Blur3,
    }

    impl Stage {
        /// Every stage, in dump order.
        pub const ALL: [Stage; 6] = [
            Stage::Final,
            Stage::Main,
            Stage::Warped,
            Stage::Blur1,
            Stage::Blur2,
            Stage::Blur3,
        ];
        /// Sub-directory name of this stage inside a dump directory.
        pub const fn dir(self) -> &'static str {
            match self {
                Stage::Final => "final",
                Stage::Main => "main",
                Stage::Warped => "warped",
                Stage::Blur1 => "blur1",
                Stage::Blur2 => "blur2",
                Stage::Blur3 => "blur3",
            }
        }
    }

    /// File name of frame `frame` with extension `ext`, e.g. `00042.png`.
    pub fn frame_file(frame: u32, ext: &str) -> String {
        format!("{frame:05}.{ext}")
    }

    /// `meta.json` contents: who produced the dump and with which settings.
    #[derive(Clone, Debug, PartialEq)]
    #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
    pub struct DumpMeta {
        /// e.g. `projectm@<git sha>` or `lactea@0.1.0`.
        pub producer: String,
        /// Preset file the dump was rendered from.
        pub preset: String,
        /// Audio input the dump was rendered with.
        pub audio: String,
        /// Deterministic run settings used for the dump.
        pub config: RunConfig,
        /// Stages written under `<dir>/<stage>/`.
        pub stages: Vec<Stage>,
        /// Whether `vars/` holds per-frame variable dumps.
        pub has_vars: bool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wgsl_common_validates() {
        let src = format!(
            "{WGSL_COMMON}\n@group(1) @binding(1) var t0: texture_2d<f32>;\n@group(1) @binding(2) var s0: sampler;\n\
             @fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> {{\n\
               let m = mu.rot[3]; let r = vec4<f32>(in.uv.xy, 0.0, 1.0) * m;\n\
               return vec4<f32>(textureSample(t0, s0, in.uv.xy).xyz * mu.c[2].x + r + pu.texsize[0].xyz + mx.q_hi[7].xyz * mx.mouse.x, in.diffuse.w);\n}}\n"
        );
        let module = naga::front::wgsl::parse_str(&src).expect("parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .expect("validate");
    }

    #[test]
    fn uniform_layout_is_vec4_aligned() {
        assert_eq!(std::mem::size_of::<MilkUniforms>() % 16, 0);
        assert_eq!(
            std::mem::size_of::<MilkUniforms>(),
            16 * 2 + 16 * 14 + 16 * 8 + 48 * 24 + 16 * 6
        );
        assert_eq!(std::mem::size_of::<PassUniforms>(), 16 * 16);
        assert_eq!(std::mem::size_of::<MeshVertex>(), 4 * 12);
        assert_eq!(std::mem::size_of::<MilkExtUniforms>(), 16 + 16 * 8 + 16 * 7);
        assert_eq!(MilkExtUniforms::default().mouse, [-1.0, -1.0, 0.0, 0.0]);
    }

    #[test]
    fn pcm_ranges_tile_without_gaps() {
        let mut prev = 0;
        for f in 0..1000 {
            let (s, e) = pcm_range(f, 44100, 60);
            assert_eq!(s, prev);
            assert!(e - s == 735);
            prev = e;
        }
    }
}
