// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Everything the engine hands to [`crate::Renderer::render_frame`]: per-frame values *after*
//! per-frame equations, per-vertex results, evaluated custom waves / shapes, and the main
//! waveform. Field docs name the MD2 variable they correspond to (state.cpp `var_pf_*`,
//! `mysound.*`, `CShape`, `CWave`).

use crate::wavegen::{NUM_FREQ_SAMPLES, NUM_WAVE_INPUT};

/// Per-frame values after per-frame equations.
#[derive(Clone, Debug)]
pub struct FrameInput {
    // ---- timing -------------------------------------------------------------------------
    /// `GetTime()` (== `time` var, `_c2.x`). Frame n is exactly `n / fps`.
    pub time: f64,
    /// `GetFps()` (`fps`, `_c2.y`).
    pub fps: f32,
    /// `GetFrame()` (`frame`, `_c2.z`).
    pub frame: u32,
    /// `progress` (`_c2.w`): `(time - preset_start) / (next_preset_time - preset_start)`.
    pub progress: f32,

    // ---- audio levels -------------------------------------------------------------------
    /// `mysound.imm_rel[0..3]` = `bass, mid, treb` (`_c3.xyz`).
    pub bass: f32,
    /// `mysound.imm_rel[1]` = `mid`.
    pub mid: f32,
    /// `mysound.imm_rel[2]` = `treb`.
    pub treb: f32,
    /// `mysound.avg_rel[0..3]` = `bass_att, mid_att, treb_att` (`_c4.xyz`).
    pub bass_att: f32,
    /// `mysound.avg_rel[1]` = `mid_att`.
    pub mid_att: f32,
    /// `mysound.avg_rel[2]` = `treb_att`.
    pub treb_att: f32,
    /// `mysound.imm[0..3]` (absolute, un-normalised). Only used by main-wave mode 3
    /// (`alpha *= pow(treble_rel, 2)` where MD2 reads `mysound.imm[2]`).
    pub imm_abs: [f32; 3],

    // ---- per-frame equation outputs -------------------------------------------------------
    /// `q1..q32` after per-frame code (`_qa.._qh`).
    pub q: [f32; 32],
    /// `m_rand_frame` (`rand_frame`): four `FRAND`s drawn once per frame.
    pub rand_frame: [f32; 4],
    /// `m_pState->m_rand_preset` (`rand_preset`): four `FRAND`s drawn once per preset.
    pub rand_preset: [f32; 4],

    /// Final UV of every warp-mesh vertex, `(gx+1)*(gy+1)` entries, row 0 first (clip y = -1); a different length
    /// is rejected by [`Renderer::render_frame`](crate::Renderer::render_frame). Build with [`crate::warp_mesh`].
    /// `None` = no motion (zoom 1, no warp). The UVs are also what the next frame's motion vectors read, and what
    /// the MilkDrop 2 compat paths draw.
    pub warp_uv: Option<Vec<[f32; 2]>>,
    /// Per-vertex inputs of projectM's warp *vertex shader*: with `Compat::ProjectM` the renderer lets the
    /// GPU evaluate the warp UV formula (`pow`/`sin`/`cos` are GPU approximations, not libm, so the
    /// result is only reproducible on the GPU) from these instead of using `warp_uv`. `None` = use `warp_uv`.
    pub warp_gpu: Option<WarpGpu>,

    /// Decay and composite-pass controls.
    pub post: PostParams,
    /// Blur-chain parameters.
    pub blur: BlurParams,
    /// Outer / inner borders.
    pub borders: Borders,
    /// Motion-vector field.
    pub mv: MotionVectors,
    /// Main waveform.
    pub main_wave: MainWave,
    /// Custom shapes in draw order (`m_shape[0..16]`, each instance already evaluated).
    pub shapes: Vec<ShapeInstance>,
    /// Custom waves in draw order (`m_wave[0..16]`, per-point equations already evaluated).
    pub waves: Vec<CustomWave>,
    /// `var_pf_darken_center`.
    pub darken_center: bool,
    /// Random state of the warp / composite shader pass (override of `rand_frame` / `rand_preset`
    /// above and of the renderer's rotation matrices); `None` = use the fields above.
    pub warp_random: Option<PassRandom>,
    /// Random state of the composite shader pass, see `warp_random`.
    pub comp_random: Option<PassRandom>,
    /// Phases of the four hue-shader corner colours (`m_fRandStart` / projectM `hueRandomOffsets`);
    /// `None` = the renderer's per-preset values.
    pub hue_offsets: Option<[f32; 4]>,
    /// MilkDrop 2's preset blend (`m_bBlending`) in progress (`CompatRules::md2_preset_blend`): the fields above are
    /// then the new state's blended values and this carries the rest, see [`Md2Blend`](crate::Md2Blend). `None` = no blend.
    pub md2_blend: Option<crate::blend::Md2Blend>,
    /// MilkDrop 3 shader inputs (`_c14`, `_qi.._qp`, shader time, FFT); neutral outside MilkDrop 3.
    pub ext: crate::ShaderExt,
}

/// Decay, composite-pass controls.
#[derive(Clone, Debug)]
pub struct PostParams {
    /// `var_pf_decay` (no-shader warp: multiplies the sampled image; quantised to 8 bit as MD2 does).
    pub decay: f32,
    /// `var_pf_gamma` (already clamped to 0..8 by the engine as MD2 does).
    pub gamma: f32,
    /// `var_pf_echo_zoom` (clamped to 0.001..1000).
    pub echo_zoom: f32,
    /// `var_pf_echo_alpha`.
    pub echo_alpha: f32,
    /// `(int)var_pf_echo_orient % 4`.
    pub echo_orient: i32,
    /// `var_pf_brighten` (non-zero == true).
    pub brighten: bool,
    /// `var_pf_darken`.
    pub darken: bool,
    /// `var_pf_solarize`.
    pub solarize: bool,
    /// `var_pf_invert`.
    pub invert: bool,
    /// `var_pf_wrap > 0.5` (`m_fSnapPoint`): address mode of the no-shader warp sampler.
    pub wrap: bool,
    /// Preset field `fShader` (`m_fShader.eval()`): hue-shader amount for the no-shader composite.
    pub fshader: f32,
}

/// Blur-chain parameters (`var_pf_blur*`).
#[derive(Clone, Debug)]
pub struct BlurParams {
    /// `var_pf_blur1min..blur3min`, `blur1max..blur3max` (raw; the renderer applies the "push apart" clamping).
    pub min: [f32; 3],
    /// `var_pf_blur1max..blur3max`.
    pub max: [f32; 3],
    /// `var_pf_blur1_edge_darken`.
    pub edge_darken: f32,
}

/// Outer/inner border: `var_pf_ob_*` / `var_pf_ib_*`.
#[derive(Clone, Debug)]
pub struct Borders {
    /// `var_pf_ob_size`.
    pub ob_size: f32,
    /// `var_pf_ob_r/g/b/a`.
    pub ob_rgba: [f32; 4],
    /// `var_pf_ib_size`.
    pub ib_size: f32,
    /// `var_pf_ib_r/g/b/a`.
    pub ib_rgba: [f32; 4],
    /// Skip both borders (`DrawSprites` border gate of MilkDrop 3: hidden during blends with `DisableBox`, never drawn
    /// for a double preset).
    pub hidden: bool,
}

/// Motion vector field: `var_pf_mv_x/y/dx/dy/l/r/g/b/a`.
#[derive(Clone, Debug)]
pub struct MotionVectors {
    /// `var_pf_mv_x`: number of columns.
    pub x: f32,
    /// `var_pf_mv_y`: number of rows.
    pub y: f32,
    /// `var_pf_mv_dx`: horizontal offset.
    pub dx: f32,
    /// `var_pf_mv_dy`: vertical offset.
    pub dy: f32,
    /// `var_pf_mv_l`: length scale.
    pub l: f32,
    /// `var_pf_mv_r/g/b/a`.
    pub rgba: [f32; 4],
}

/// Main waveform (`DrawWave` inputs).
#[derive(Clone, Debug)]
pub struct MainWave {
    /// `var_pf_wave_mode` as the renderer should draw it: the engine applies the compat mode's wrap (`% 18` MD2,
    /// `% 16` projectM, the C remainder `% 17` of MilkDrop 3, where a negative value draws nothing visible).
    pub mode: i32,
    /// `var_pf_wave_r/g/b` and `var_pf_wave_a`.
    pub rgba: [f32; 4],
    /// `var_pf_wave_x` (0..1).
    pub x: f32,
    /// `var_pf_wave_y` (0..1; y=1 is the top of the screen).
    pub y: f32,
    /// `var_pf_wave_mystery`.
    pub mystery: f32,
    /// `var_pf_wave_usedots`.
    pub usedots: bool,
    /// `var_pf_wave_thick`.
    pub thick: bool,
    /// `var_pf_wave_additive`.
    pub additive: bool,
    /// `var_pf_wave_brighten` (maximize colour).
    pub brighten: bool,
    /// `m_fWaveScale.eval()` (applied to the waveform at the top of `RenderFrame`: `scale = fWaveScale/128`).
    pub scale: f32,
    /// `m_fWaveSmoothing.eval()` (IIR smoothing with `mix2 = smoothing`).
    pub smoothing: f32,
    /// `m_bModWaveAlphaByVolume`.
    pub mod_alpha_by_volume: bool,
    /// `m_fModWaveAlphaStart`.
    pub mod_alpha_start: f32,
    /// `m_fModWaveAlphaEnd`.
    pub mod_alpha_end: f32,
    /// Raw `mysound.fWave[0]` / `[1]` (left, right): 576 samples, **MD2 units** (signed, full scale
    /// about ±128; the renderer multiplies by `scale/128` and smooths exactly like `RenderFrame`).
    /// Same layout as `lactea_audio::AudioFrame::wave` (only the first 480 are valid but the
    /// main-wave modes peek at up to index 511 like MD2).
    pub wave: [[f32; NUM_WAVE_INPUT]; 2],
    /// `mysound.fSpecLeft` / `fSpecRight` (512 bins), only read by wave mode 8
    /// (same layout as `AudioFrame::spectrum`).
    pub spectrum: [[f32; NUM_FREQ_SAMPLES]; 2],
}

/// One evaluated custom-shape instance (`CShape` after per-frame code for that instance).
#[derive(Clone, Debug)]
pub struct ShapeInstance {
    /// `var_pf_sides` (clamped to `3..=CompatRules::max_shape_sides` by the renderer).
    pub sides: i32,
    /// `var_pf_x`, projectM / MD2 compute the fan centre `x * 2.0 - 1.0` in double and round to `f32` once, so
    /// `x` / `y` stay in the equation VM's `f64` (0..1, y=1 top).
    pub x: f64,
    /// `var_pf_y`, see `x`.
    pub y: f64,
    /// `var_pf_rad`.
    pub rad: f32,
    /// `var_pf_ang`.
    pub ang: f32,
    /// Inner colour `var_pf_r/g/b/a`.
    pub rgba: [f32; 4],
    /// Outer colour `var_pf_r2/g2/b2/a2`.
    pub rgba2: [f32; 4],
    /// Border colour `var_pf_border_*`.
    pub border_rgba: [f32; 4],
    /// `var_pf_additive`.
    pub additive: bool,
    /// `var_pf_thick` (thick outline).
    pub thick: bool,
    /// `var_pf_textured`.
    pub textured: bool,
    /// `var_pf_tex_zoom`.
    pub tex_zoom: f32,
    /// `var_pf_tex_ang`.
    pub tex_ang: f32,
    /// projectM's `shapecode_N_image`: a textured shape samples this user texture (see
    /// [`Renderer::set_user_texture`](crate::Renderer::set_user_texture)) instead of the main image.
    /// `None` (the default, and every MilkDrop 2 shape) keeps the main-image behaviour.
    pub image: Option<ShapeImage>,
}

/// The texture of a shape's `image=` key, already split like projectM's `ExtractTextureSettings`
/// (`fw_` / `fc_` / `pw_` / `pc_` prefixes pick filter and wrapping; no prefix = linear + repeat).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShapeImage {
    /// Lower-case texture name without prefix and extension (the key of `set_user_texture`).
    pub name: String,
    /// Bilinear (`true`) or nearest (`false`) filtering.
    pub linear: bool,
    /// Repeat (`true`) or clamp-to-edge (`false`) addressing.
    pub wrap: bool,
}

impl Default for ShapeInstance {
    /// state.cpp defaults (`m_shape[i]`).
    fn default() -> Self {
        Self {
            sides: 4,
            x: 0.5,
            y: 0.5,
            rad: 0.1,
            ang: 0.0,
            rgba: [1.0, 0.0, 0.0, 1.0],
            rgba2: [0.0, 1.0, 0.0, 0.0],
            border_rgba: [1.0, 1.0, 1.0, 0.1],
            additive: false,
            thick: false,
            textured: false,
            tex_zoom: 1.0,
            tex_ang: 0.0,
            image: None,
        }
    }
}

/// One evaluated custom-wave point (`var_pp_x/y/r/g/b/a` after per-point code). `x` / `y` are the VM's `f64`:
/// projectM turns them into clip space as `static_cast<float>((x * 2.0 - 1.0) * invAspectX)`, i.e. the product is also
/// taken in double (MD2: `(float)(x * 2 - 1) * invAspectX`, see `CompatRules::wave_point_f64_aspect`).
#[derive(Clone, Copy, Debug)]
pub struct WavePoint {
    /// `var_pp_x` (0..1).
    pub x: f64,
    /// `var_pp_y` (0..1, y=1 top).
    pub y: f64,
    /// `var_pp_r/g/b/a`.
    pub rgba: [f32; 4],
}

/// One evaluated custom wave (`CWave`). `points.len()` is the `samples` actually produced
/// (<= 512). Spectrum / `sep` / smoothing / scaling are the engine's job (see
/// [`crate::wavegen::custom_wave_values`]).
#[derive(Clone, Debug, Default)]
pub struct CustomWave {
    /// The evaluated points.
    pub points: Vec<WavePoint>,
    /// `bUseDots`: draw points instead of a line.
    pub use_dots: bool,
    /// `bDrawThick`.
    pub thick: bool,
    /// `bAdditive`.
    pub additive: bool,
}

impl Default for BlurParams {
    fn default() -> Self {
        Self {
            min: [0.0; 3],
            max: [1.0; 3],
            edge_darken: 0.25,
        }
    }
}

impl Default for PostParams {
    fn default() -> Self {
        Self {
            decay: 0.98,
            gamma: 2.0,
            echo_zoom: 2.0,
            echo_alpha: 0.0,
            echo_orient: 0,
            brighten: false,
            darken: false,
            solarize: false,
            invert: false,
            wrap: true,
            fshader: 0.0,
        }
    }
}

impl Default for Borders {
    fn default() -> Self {
        // state.cpp: ob_size 0.01, ib_size 0.01, colours ob (0,0,0,0), ib (0.25,0.25,0.25,0)
        Self {
            ob_size: 0.01,
            ob_rgba: [0.0; 4],
            ib_size: 0.01,
            ib_rgba: [0.25, 0.25, 0.25, 0.0],
            hidden: false,
        }
    }
}

impl Default for MotionVectors {
    fn default() -> Self {
        // state.cpp defaults: mv_x 12, mv_y 9, mv_l 0.9, colour (1,1,1,1). Presets that want no
        // field set mv_a = 0 (.milk files always carry `mv_a`).
        Self {
            x: 12.0,
            y: 9.0,
            dx: 0.0,
            dy: 0.0,
            l: 0.9,
            rgba: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

impl Default for MainWave {
    fn default() -> Self {
        Self {
            mode: 0,
            rgba: [1.0, 1.0, 1.0, 0.8],
            x: 0.5,
            y: 0.5,
            mystery: 0.0,
            usedots: false,
            thick: false,
            additive: false,
            brighten: true,
            scale: 1.0,
            smoothing: 0.75,
            mod_alpha_by_volume: false,
            mod_alpha_start: 0.75,
            mod_alpha_end: 0.95,
            wave: [[0.0; NUM_WAVE_INPUT]; 2],
            spectrum: [[0.0; NUM_FREQ_SAMPLES]; 2],
        }
    }
}

/// See [`FrameInput::warp_gpu`].
#[derive(Clone, Debug)]
pub struct WarpGpu {
    /// `(gx+1)*(gy+1)` entries in the same order as `warp_uv`: the ten motion variables of each vertex
    /// after per-pixel code.
    pub vars: Vec<crate::warp_mesh::VertexVars>,
    /// Per-frame warp animation terms (`WarpAnim::projectm`).
    pub anim: crate::warp_mesh::WarpAnim,
}

/// Per-shader-pass random state. projectM gives the warp and the composite shader independent
/// `rand_preset` / `rand_frame` / `rot_*` streams (MD2 shares one set per preset); the engine
/// supplies them here. Any `None` falls back to the renderer's own state.
#[derive(Clone, Debug)]
pub struct PassRandom {
    /// `rand_frame`: four values drawn once per frame.
    pub rand_frame: [f32; 4],
    /// `rand_preset`: four values drawn once per preset.
    pub rand_preset: [f32; 4],
    /// The 24 `rot_*` matrices exactly as uploaded (ABI layout, see [`lactea_abi::MilkUniforms::rot`]).
    pub rot: Option<Box<[[[f32; 4]; 3]; lactea_abi::NUM_ROT]>>,
}

impl Default for FrameInput {
    /// A default MilkDrop state at frame 0 with silent audio (decay .98, gamma 2, white wave 0).
    fn default() -> Self {
        Self {
            time: 0.0,
            fps: 60.0,
            frame: 0,
            progress: 0.0,
            bass: 1.0,
            mid: 1.0,
            treb: 1.0,
            bass_att: 1.0,
            mid_att: 1.0,
            treb_att: 1.0,
            imm_abs: [0.0; 3],
            q: [0.0; 32],
            rand_frame: [0.0; 4],
            rand_preset: [0.0; 4],
            warp_uv: None,
            warp_gpu: None,
            post: PostParams::default(),
            blur: BlurParams::default(),
            borders: Borders::default(),
            mv: MotionVectors::default(),
            main_wave: MainWave::default(),
            shapes: Vec::new(),
            waves: Vec::new(),
            darken_center: false,
            warp_random: None,
            ext: crate::ShaderExt::default(),
            comp_random: None,
            hue_offsets: None,
            md2_blend: None,
        }
    }
}
