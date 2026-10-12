// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-FileCopyrightText: 2005-2013 Nullsoft, Inc.
// SPDX-License-Identifier: (MIT OR Apache-2.0) AND BSD-3-Clause

//! Renderer configuration and the MD2-vs-projectM compatibility switch.

use lactea_abi::{RunConfig, ShaderInterface};

/// Which reference behaviour the renderer follows where MilkDrop 2 (`third_party/md2`) and
/// projectM differ. Default: projectM.
///
/// Every divergence handled by this switch (see [`Compat::rules`]):
///
/// | item | `ProjectM` | `MilkDrop2` |
/// |---|---|---|
/// | `_c3.w`/`_c4.w` (`vol`, `vol_att`) | `(bass+mid+treb)*0.333` | `0.3333*treb` (C comma-operator bug) |
/// | `_c12.y` (`mip_y`) | from height | from width (typo) |
/// | `_c1` (aspect "sd") | zeros | `aspect` for the sd variant |
/// | no-shader warp decay | `min(decay,1)` as float, no 8-bit rounding | `(int)(decay*255)&0xFF` (truncated, wraps) |
/// | vertex colours (waves, shapes, borders, mv, echo) | float, modulo-wrapped like `& 0xFF` when out of 0..1 | 8-bit truncated |
/// | no-shader comp hue tint | always applied | only if `fShader > 0.001`, blended by `fShader` |
/// | thick / dots main-wave passes | whenever `thick \|\| dots` | additionally needs `texsize >= 512` |
/// | custom-shape thick outline offset | `1/W` (half a pixel) | `2/W` (one pixel) |
/// | motion vectors: first frame | skipped | drawn |
/// | motion vectors: min length / lookup | 1.25 px, geometrically correct row | 1 px, mirrored-row lookup (`ReversePropagatePoint`) |
/// | comp-shader `sampler_main` | current frame (after waves/borders) | previous frame (`m_lpVS[0]` in `ApplyShaderParams`) |
/// | warp mesh `ang` seam | no fix (interpolates across the +-pi seam row) | `ang` patched to -pi / +pi per half |
/// | comp mesh UV | `uv + 0.5/size` (half-texel) | exact (the D3D9 half-pixel compensation nets to 0) |
/// | warp-shader `VsOut.diffuse` | `(decay, decay, decay, 1)` | `(1, 1, 1, 1)` |
/// | no-shader composite hue-tint corners | `shade[0..1]` at the *top* corners (projectM draws the quad in its flipped frame) | `shade[0..1]` at the bottom corners |
/// | blur pass 0 vertical `+1 texel` bias | `-1` (projectM samples the upside-down stored source with `uv.y` flipped, so the bias goes up) | `+1` (down) |
/// | warp mesh cell diagonal | bottom-left to top-right in image space (projectM's mesh row 0 is the top) | top-left to bottom-right |
/// | no-shader composite / filter quad diagonal | `TL,TR,BL,BR` strip: split along top-right to bottom-left (hue-tint colours interpolate differently) | triangles `(0,1,2),(2,1,3)` of `geom::echo_quad` (bottom-right to top-left) |
/// | no-shader echo / gamma quad size | scaled by `VideoEcho`'s `aspectMultX/Y` (x stretched by `H/W` on portrait targets, UVs unchanged) | `1 + 1/size` |
/// | no-shader gamma pass count | `(int)(gamma - 0.0001) + 1` | `(int)(gamma - 0.001) + 1` |
/// | lines / points | GL rasterisation rules emulated exactly ([`StrokeRaster`]) | D3D9 thin quads |
/// | custom-wave dots | the 2n-1 vertices of `SmoothWave`, always 1 px (`gl_PointSize` ignored) | the n source points, `1 + thick` px |
/// | custom-wave thick offset | `1/W` ndc in x *and* y (half a pixel in x, `H/2W` px in y) | `2/W`, `2/H` (one pixel) |
/// | `rot_*` uniforms | translation row dropped (`glm::mat3x4` of a `mat4`: only the 3 rotation columns are uploaded) | `float4x3` incl. the translation row |
/// | first frame of a preset whose shaders sample blur (the frame that allocates the blur textures) | shapes, waves, main wave, darken centre, borders lost (`BlurTexture::Update` restores FBO 0) | drawn |
/// | custom wave / shape slots that run | 4 each (`CustomWaveformCount`, `CustomShapeCount`; slots 4..15 are ignored) | 16 each |
/// | warp shader `sampler_main` (first `main` sampler in name order) | filter linear, address from `bTexWrap` (`WarpedBlit` rebinds unit 0) | whatever the shader's sampler prefix says |
/// | preset change (`Transition::Soft`) | image-space cross-fade of two independently rendered presets by one of six built-in transition shaders (`md2_preset_blend = false`) | MD2's own blend inside one frame pipeline: both states' equations run, per-vertex UVs blended by a random pattern, old/new shaders drawn as alpha-tiled passes (`md2_preset_blend = true`) |
/// | preset code lines (`md2_code_join`) | joined with `\n` | joined with nothing and `//` / `\\` comments stripped (`CState::StripLinefeedCharsAndComments`) |
/// | gamma / echo / filter flags / main-wave mode and flags (`frame_vars_drive_post`) | the preset's initial values | the per-frame variable values |
/// | shader random streams (`shared_shader_rand`) | separate warp / comp streams, constructor draws after `rand_preset` | one shared stream per preset |
/// | warp animation terms (`warp_anim_projectm`) | projectM's `y` sign and GPU `pow` | MD2's |
/// | `shapecode_N_image` key (`custom_shape_image`) | read | not a key (stays in `extra`) |
/// | no-shader composite sampler address (`noshader_comp_wrap`) | clamp | wrap |
/// | expression language (`md3_ns_eel2`) | projectm-eval | projectm-eval / projectm-eval / ns-eel2 (MilkDrop 3 only) |
/// | preset code that fails to compile (`md3_drop_failed_code`) | the preset is rejected | the preset is rejected (MilkDrop 2 itself drops the part; not ported) / MilkDrop 3 drops the part and loads |
/// | `.milk2` double presets (`md3_double_preset`) | not loaded (error) | not loaded (error) |
/// | soft-cut blend patterns (`md3_blend_patterns`) | (projectM transition shaders) | BeatDrop's 19 (MD3: its 30) |
///
/// `MilkDrop3` is `MilkDrop2` plus the MilkDrop 3 deltas (see `docs/md3.md`); every MD3 delta is a field of
/// [`CompatRules`], and [`Compat::md2_family`] is true for both variants.
///
/// Not switchable (identical in both): blur chain, blur source (previous frame image), blur
/// min/max "push apart" typo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Compat {
    /// projectM's behaviour (the default).
    #[default]
    ProjectM,
    /// MilkDrop 2's behaviour (Direct3D 9 semantics).
    MilkDrop2,
    /// MilkDrop 3 (3.37) behaviour: MilkDrop 2 / BeatDrop rules plus the MilkDrop 3 deltas. MD3's source is unpublished;
    /// its behaviour is implemented from clean-room specs, see `docs/md3.md`.
    MilkDrop3,
}

/// The individual behaviours selected by a [`Compat`] value (see the table there for the
/// projectM / MilkDrop 2 values of each).
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct CompatRules {
    /// `vol` / `vol_att` follow MD2's comma-operator bug (`0.3333*treb`).
    pub vol_is_comma_bug: bool,
    /// `_c12.y` (`mip_y`) is derived from the width instead of the height (MD2 typo).
    pub mip_y_from_width: bool,
    /// `_c1` (aspect "sd") is all zeros.
    pub c1_is_zero: bool,
    /// The no-shader warp decay is truncated to 8 bits and wraps, `(int)(decay*255)&0xFF`.
    pub decay_8bit: bool,
    /// Vertex colours are 8-bit truncated; otherwise float, modulo-wrapped like `& 0xFF` when outside 0..1.
    pub quantize_vertex_colors: bool,
    /// The no-shader composite hue tint is always applied; otherwise only if `fShader > 0.001`, blended by `fShader`.
    pub hue_tint_always: bool,
    /// Thick / dots main-wave passes additionally need a texture size of at least 512.
    pub thick_needs_512: bool,
    /// Main waveform geometry/alpha follow projectM `Waveforms/*.cpp` (`wavegen::main_wave_geometry_projectm`)
    /// instead of the MD2 `DrawWave` port (`wavegen::main_wave_geometry`).
    pub pm_waveforms: bool,
    /// The custom-shape thick outline offset is one pixel (`2/W`) instead of half a pixel (`1/W`).
    pub shape_thick_full_pixel: bool,
    /// Motion vectors are skipped on the first frame.
    pub mv_skip_first_frame: bool,
    /// Minimum motion-vector length in pixels.
    pub mv_min_len_px: f32,
    /// Motion-vector lookup uses MD2's mirrored row (`ReversePropagatePoint`).
    pub mv_mirrored_lookup: bool,
    /// The composite shader's `sampler_main` reads the previous frame instead of the current one.
    pub comp_reads_previous: bool,
    /// The warp mesh `ang` seam is patched to -pi / +pi per half instead of interpolating across the seam row.
    pub ang_seam_fix: bool,
    /// The composite mesh UV gets a half-texel offset (`uv + 0.5/size`).
    pub comp_uv_half_texel: bool,
    /// A warp shader's `VsOut.diffuse` is `(decay, decay, decay, 1)` instead of `(1, 1, 1, 1)`.
    pub shader_warp_diffuse_is_decay: bool,
    /// The translation row of the `rot_*` uniforms is dropped (`glm::mat3x4` of a `mat4`).
    pub rot_translation_dropped: bool,
    /// The no-shader composite hue-tint colours `shade[0..1]` sit at the top corners instead of the bottom ones.
    pub shade_top_first: bool,
    /// Blur pass 0 samples an upside-down source, so its vertical `+1 texel` bias is `-1`.
    pub blur_src_flipped: bool,
    /// Warp mesh cells are split bottom-left to top-right in image space instead of top-left to bottom-right.
    pub warp_diag_flipped: bool,
    /// The no-shader composite / filter quad is split top-right to bottom-left instead of bottom-right to top-left.
    pub echo_diag_flipped: bool,
    /// projectM stretches the echo / gamma quad by `aspectMultX/Y` of `VideoEcho::Draw` (`geom::echo_aspect_mult`):
    /// identity for landscape targets, a horizontal `H/W` magnification of the centre for portrait ones. MD2 draws the
    /// plain `1 + 1/size` quad.
    pub echo_quad_aspect: bool,
    /// Epsilon of the gamma pass count of the no-echo composite, `(int)(gamma - eps) + 1` passes: projectM
    /// (`VideoEcho::DrawGammaAdjustment`) `0.0001`, MD2 (`ShowToUser_NoShaders`) `0.001`.
    pub gamma_pass_eps: f32,
    /// Lines and points follow OpenGL rasterisation rules (see [`StrokeRaster`]) instead of D3D9 thin quads.
    pub gl_strokes: bool,
    /// Custom-wave dots use the `2n-1` vertices of `SmoothWave`; otherwise the `n` source points.
    pub wave_dots_smoothed: bool,
    /// Point sizes (`1 + thick` px) are honoured; otherwise points are always 1 px (`gl_PointSize` ignored).
    pub point_size_honored: bool,
    /// The custom-wave thick offset is `1/W` ndc in x *and* y; otherwise `2/W`, `2/H` (one pixel).
    pub wave_thick_ndc_inc: bool,
    /// Custom-wave point positions: projectM `static_cast<float>((x * 2.0 - 1.0) * invAspectX)` (the product is taken in
    /// double, one rounding to `f32`); MD2 `(float)(x * 2 - 1) * m_fInvAspectX` (rounded to `f32` before the product).
    pub wave_point_f64_aspect: bool,
    /// projectM textures have a single level (no `glGenerateMipmap`, samplers use the plain `GL_LINEAR` /
    /// `GL_NEAREST` min filter), so `tex2D` never blends mip levels and `tex2Dlod` / `tex2Dbias` read level 0.
    /// MD2 uses D3D9 textures with a mip chain (trilinear).
    pub no_mipmaps: bool,
    /// projectM evaluates the warp UV formula (`pow`, `sin`, `cos`, multiply-adds) in its *vertex shader*, so the
    /// result carries the GPU's approximations and fusion; MD2 does it on the CPU (`D3DX`/libm floats).
    /// With this rule the renderer runs the warp vertex shader on the per-vertex inputs of `FrameInput::warp_gpu`.
    pub warp_uv_on_gpu: bool,
    /// projectM: the internal images are RGBA; `main` starts at alpha 0, the no-shader warp keeps `tex.a`,
    /// shaders write 1, overlays blend alpha with the colour factors and textured fills multiply by `tex.a`.
    /// MD2 (X8R8G8B8) has no alpha: sampled alpha is 1 and nothing is written.
    pub main_alpha: bool,
    /// projectM binds the `GL_NEAREST` flip texture (set by `TextureAttachment`) for textured shapes that sample the
    /// main image; MD2 samples it bilinearly.
    pub shape_main_nearest: bool,
    /// projectM: on the frame that (re)allocates the blur textures (first frame after preset load / resize) of a preset
    /// whose shaders sample blur, `BlurTexture::Update` restores the wrong framebuffer, so shapes, waves, main wave,
    /// darken-centre and borders of that frame are drawn to the default framebuffer and lost. MD2 draws them.
    pub blur_alloc_drops_overlays: bool,
    /// projectM draws in GL window space (y up), lactea's targets are y down (wgpu / D3D9): both use the top-left
    /// rule, so a pixel centre lying exactly on a *horizontal* triangle edge goes to the polygon below the edge in
    /// wgpu and to the polygon above it in projectM. With this rule such triangle fills (borders whose
    /// `size * H / 2` is a half integer) are nudged by one sub-pixel step (`geom::gl_fill_ties`). MD2 (D3D9) has
    /// the wgpu rule.
    pub gl_fill_ties: bool,
    /// projectM's GL driver (Mesa) moves `interpolated input ± uniform/constant` (and a few other linear operations)
    /// of the user warp / composite fragment shader into the vertex stage and interpolates the result, which differs
    /// from interpolating the input and adding in the fragment stage by about 1 ulp. With this rule the HLSL
    /// translator emits those per-vertex values (`CompiledShader::folds`) and the renderer delivers them as extra
    /// varyings. MD2 (D3D9) and wgpu keep the fragment-stage arithmetic.
    pub gl_varying_fold: bool,
    /// Borders are one 8-vertex / 8-triangle mesh in projectM (`Border.cpp`, mirrored vertically by its y-flipping
    /// projection), four rotated quads in MD2. Identical for sane sizes, different when `ob + ib > 1` or a size is
    /// negative (self-overlapping triangles).
    pub pm_border_mesh: bool,
    /// Number of custom wave / custom shape slots that run: projectM `CustomWaveformCount` /
    /// `CustomShapeCount` = 4 (the keys `wavecode_4..15` / `shapecode_4..15` are never read), MD2 / `MAX_CUSTOM_*` = 16.
    pub custom_slots: usize,
    /// projectM's `PerPixelMesh::WarpedBlit` binds its own sampler (`GL_LINEAR`, `GL_REPEAT` / `GL_CLAMP_TO_EDGE` from the
    /// per-frame `wrap` variable, i.e. `bTexWrap`) on texture unit 0 *after* the warp shader loaded its samplers, which
    /// replaces the sampler of the first `main` descriptor (`std::set` order of the sampler names, `main` always
    /// present): a warp shader's `sampler_main` follows `wrap`, not the `fw_`/`fc_`/`pw_`/`pc_` prefix. MD2 uses the
    /// sampler declared in the shader.
    pub warp_main_sampler_from_wrap: bool,
    /// Soft cuts follow MilkDrop 2's `m_bBlending` (`state.cpp` `StartBlendFrom`, `milkdropfs.cpp` `RenderFrame`): the old
    /// preset's state stays alive next to the new one inside a single frame pipeline (one renderer, old shaders in
    /// `m_OldShaders`). projectM instead renders both presets to their own images and blends the finished images with
    /// one of six transition shaders ([`TransitionKind`](crate::TransitionKind)).
    pub md2_preset_blend: bool,
    /// Preset code lines are joined like `CState::StripLinefeedCharsAndComments`: the `\x01` line separators are
    /// deleted without a replacement and `//` / `\\` comments are dropped up to the next separator, so a statement
    /// split across two lines glues its halves together (`k1 = is_` + `beat*...` is `k1 = is_beat*...`). Otherwise the
    /// lines are joined with `\n` (projectM). Applies to every EEL code part, never to shader text.
    pub md2_code_join: bool,
    /// `gamma`, `echo_*`, `brighten` / `darken` / `solarize` / `invert` and the main wave's `wave_mode` /
    /// `wave_thick` / `wave_usedots` / `wave_additive` take the per-frame variable values instead of the preset's
    /// initial values (projectM reads the file values).
    pub frame_vars_drive_post: bool,
    /// The warp and composite shaders share one random stream per preset (drawn once per frame); projectM keeps one
    /// stream per pass and consumes the constructor draws after `rand_preset`.
    pub shared_shader_rand: bool,
    /// The warp animation terms use projectM's `y` sign and GPU-style `pow` (`WarpAnim::projectm`) instead of MD2's.
    pub warp_anim_projectm: bool,
    /// `shapecode_N_image` (projectM addition) names a user texture a textured shape samples.
    pub custom_shape_image: bool,
    /// The no-shader composite pass samples the feedback image with the `Wrap` address mode; otherwise `Clamp`.
    pub noshader_comp_wrap: bool,
    /// The main waveform follows MilkDrop 3 (`wavegen_md3::main_wave_geometry_md3_raw`): 17 modes `0..=16` selected by
    /// the C remainder `(int)wave_mode % 17` (a negative result draws 480 zero vertices), one channel per formula.
    pub md3_wave_modes: bool,
    /// Largest `sides` a custom shape is drawn with (smallest is 3): projectM and MilkDrop 2 100, MilkDrop 3 500.
    pub max_shape_sides: i32,
    /// MilkDrop 3 adds the four warp `sin` / `cos` terms to the warp UV only when `|warp| > 0.0001`.
    pub md3_warp_skip_tiny: bool,
    /// MilkDrop 3 hides the outer / inner borders while a preset blend is running if the host's `DisableBox`
    /// setting is on (`EngineConfig::md3_disable_box`).
    pub md3_border_gate: bool,
    /// MD3 `AutoHardCutMode` 1..7 (`lactea::auto_cut`) is active: the engine watches the previous frame's `bass` /
    /// `treb` and asks for instant preset changes or injects composite-shader effects. Off under projectM and MD2.
    pub md3_auto_hard_cut: bool,
    /// MD3 display time of a preset: `BlendTimeAuto + TimeBetweenPresets + (rand() % 1000) * TimeBetweenPresetsRand *
    /// 0.001` (hosts feed it to `PresetControl::set_md3_time_rand`) instead of projectM's gaussian easter egg.
    pub md3_preset_time_rand: bool,
    /// MilkDrop 3 audio analysis (`AnalysisMode::Md3`): left channel only, 576-sample Hann FFT, 8-bit quantised input
    /// scaled by `VolumeSensitivity`, silence -> 1.0 (`docs/md3.md`, shader inputs).
    pub md3_audio_analysis: bool,
    /// The MilkDrop 3 shader FFT texture (`sampler_fft`): a max-normalised, gated, smeared attack / decay recursion of
    /// the left spectrum, run once per rendered frame and kept across presets; `FFTAttack` / `FFTDecay` are sticky
    /// globals set by every preset load.
    pub md3_shader_fft: bool,
    /// MilkDrop 3's extended expression and shader variables: `q1..q64`, `t1..t16`, `monitor2` / `monitor3` and the
    /// `_c14` / `_qi.._qp` shader constants (the mouse is a shader input only: `mouse.x` does not compile in preset code).
    pub md3_extended_vars: bool,
    /// The MilkDrop 3 shader prelude is part of every translated shader (`get_fft`, `get_fft_hz`, `mouse*`,
    /// `q33..q64`, `sampler_fft`).
    pub md3_shader_prelude: bool,
    /// The shader constant `time` (`_c2.x`) is the time since *this state's* preset started, modulo 10000; otherwise
    /// the absolute frame time.
    pub md3_shader_time: bool,
    /// The generated composite shader of presets without `comp_N` text uses the original MilkDrop 2 formulas:
    /// brighten `sqrt(ret)`, solarize `ret * (1 - ret) * 4` (BeatDrop: `1 - (1 - ret)^2`, `* 2`).
    pub md3_md2_comp_effects: bool,
    /// Preset expressions run on ns-eel2 (`lactea_eel::Dialect::NsEel2`, MilkDrop 3's engine: unguarded `/`,
    /// assignment filtering, integer `%`, `$PI` constants, 8M-entry `megabuf`, wrapping `gmegabuf`
    /// and registers that outlive presets, ...) instead of projectm-eval. `docs/md3.md`, "Expression engine".
    pub md3_ns_eel2: bool,
    /// A code part that fails to compile is dropped (with a warning) and the preset still loads: a failed init part
    /// zeroes the post-init `q` values, a failed per-frame / per-vertex / wave / shape part simply does not run.
    /// Otherwise the first failing part rejects the preset (`EngineError::Compile`).
    pub md3_drop_failed_code: bool,
    /// MilkDrop 3 `.milk2` double presets: two presets in one file, played as a frozen MD2 soft cut with its own
    /// progress modes (`docs/md3.md`). Loading a `.milk2` without this rule is an error.
    pub md3_double_preset: bool,
    /// Ordinary soft cuts draw their blend pattern from MilkDrop 3's 30 patterns (the enabled list) instead of
    /// BeatDrop's 19. Needs `md2_preset_blend`.
    pub md3_blend_patterns: bool,
    /// MilkDrop 3: a preset's sprite block (`SpriteName` .. `SpriteRepeatY`, docs/md3.md) loads an image and draws
    /// it every frame at the layer the preset names, with the 11 sprite blend modes. projectM and MilkDrop 2 never
    /// draw preset sprites.
    pub md3_preset_sprites: bool,
    /// MilkDrop 3: shader textures (`sampler_<name>`) are also searched in the `sprites` and `gifs` folders, accept
    /// `.gif` / `.jfif` files, and an animated GIF shows the frame picked by `fmod(time, total)` of its delays.
    pub md3_gif_textures: bool,
    /// MilkDrop 3: a burning sprite in layers 1..4 is drawn **twice** into the feedback image (it burns into the
    /// image it is already drawn on), so additive and alpha modes are twice as strong with `SpriteBurn=1` as with 0.
    /// Measured against MD3 under Wine (docs/md3.md, sprites).
    pub md3_sprite_burn_twice: bool,
}

impl Compat {
    /// The behaviours of this compatibility mode.
    pub fn rules(self) -> CompatRules {
        match self {
            Compat::ProjectM => CompatRules {
                vol_is_comma_bug: false,
                mip_y_from_width: false,
                c1_is_zero: true,
                decay_8bit: false,
                quantize_vertex_colors: false,
                hue_tint_always: true,
                thick_needs_512: false,
                pm_waveforms: true,
                shape_thick_full_pixel: false,
                mv_skip_first_frame: true,
                mv_min_len_px: 1.25,
                mv_mirrored_lookup: false,
                comp_reads_previous: false,
                ang_seam_fix: false,
                comp_uv_half_texel: true,
                shader_warp_diffuse_is_decay: true,
                rot_translation_dropped: true,
                shade_top_first: true,
                blur_src_flipped: true,
                warp_diag_flipped: true,
                echo_diag_flipped: true,
                echo_quad_aspect: true,
                gamma_pass_eps: 0.0001,
                gl_strokes: true,
                wave_dots_smoothed: true,
                point_size_honored: false,
                wave_thick_ndc_inc: true,
                wave_point_f64_aspect: true,
                no_mipmaps: true,
                warp_uv_on_gpu: true,
                main_alpha: true,
                shape_main_nearest: true,
                blur_alloc_drops_overlays: true,
                gl_fill_ties: true,
                gl_varying_fold: true,
                pm_border_mesh: true,
                custom_slots: 4,
                warp_main_sampler_from_wrap: true,
                md2_preset_blend: false,
                md2_code_join: false,
                frame_vars_drive_post: false,
                shared_shader_rand: false,
                warp_anim_projectm: true,
                custom_shape_image: true,
                noshader_comp_wrap: false,
                md3_wave_modes: false,
                max_shape_sides: 100,
                md3_warp_skip_tiny: false,
                md3_border_gate: false,
                md3_auto_hard_cut: false,
                md3_preset_time_rand: false,
                md3_audio_analysis: false,
                md3_shader_fft: false,
                md3_extended_vars: false,
                md3_shader_prelude: false,
                md3_shader_time: false,
                md3_md2_comp_effects: false,
                md3_ns_eel2: false,
                md3_drop_failed_code: false,
                md3_double_preset: false,
                md3_blend_patterns: false,
                md3_preset_sprites: false,
                md3_gif_textures: false,
                md3_sprite_burn_twice: false,
            },
            Compat::MilkDrop2 => CompatRules {
                vol_is_comma_bug: true,
                mip_y_from_width: true,
                c1_is_zero: false,
                decay_8bit: true,
                quantize_vertex_colors: true,
                hue_tint_always: false,
                thick_needs_512: true,
                pm_waveforms: false,
                shape_thick_full_pixel: true,
                mv_skip_first_frame: false,
                mv_min_len_px: 1.0,
                mv_mirrored_lookup: true,
                comp_reads_previous: true,
                ang_seam_fix: true,
                comp_uv_half_texel: false,
                shader_warp_diffuse_is_decay: false,
                rot_translation_dropped: false,
                shade_top_first: false,
                blur_src_flipped: false,
                warp_diag_flipped: false,
                echo_diag_flipped: false,
                echo_quad_aspect: false,
                gamma_pass_eps: 0.001,
                gl_strokes: false,
                wave_dots_smoothed: false,
                point_size_honored: true,
                wave_thick_ndc_inc: false,
                wave_point_f64_aspect: false,
                no_mipmaps: false,
                warp_uv_on_gpu: false,
                main_alpha: false,
                shape_main_nearest: false,
                blur_alloc_drops_overlays: false,
                gl_fill_ties: false,
                gl_varying_fold: false,
                pm_border_mesh: false,
                custom_slots: 16,
                warp_main_sampler_from_wrap: false,
                md2_preset_blend: true,
                md2_code_join: true,
                frame_vars_drive_post: true,
                shared_shader_rand: true,
                warp_anim_projectm: false,
                custom_shape_image: false,
                noshader_comp_wrap: true,
                md3_wave_modes: false,
                max_shape_sides: 100,
                md3_warp_skip_tiny: false,
                md3_border_gate: false,
                md3_auto_hard_cut: false,
                md3_preset_time_rand: false,
                md3_audio_analysis: false,
                md3_shader_fft: false,
                md3_extended_vars: false,
                md3_shader_prelude: false,
                md3_shader_time: false,
                md3_md2_comp_effects: false,
                md3_ns_eel2: false,
                md3_drop_failed_code: false,
                md3_double_preset: false,
                md3_blend_patterns: false,
                md3_preset_sprites: false,
                md3_gif_textures: false,
                md3_sprite_burn_twice: false,
            },
            // The MilkDrop 3 deltas go here, one `CompatRules` field each.
            Compat::MilkDrop3 => CompatRules {
                md3_wave_modes: true,
                max_shape_sides: 500,
                md3_warp_skip_tiny: true,
                md3_border_gate: true,
                md3_auto_hard_cut: true,
                md3_preset_time_rand: true,
                md3_audio_analysis: true,
                md3_shader_fft: true,
                md3_extended_vars: true,
                md3_shader_prelude: true,
                md3_shader_time: true,
                md3_md2_comp_effects: true,
                md3_ns_eel2: true,
                md3_drop_failed_code: true,
                md3_double_preset: true,
                md3_blend_patterns: true,
                md3_preset_sprites: true,
                md3_gif_textures: true,
                md3_sprite_burn_twice: true,
                ..Compat::MilkDrop2.rules()
            },
        }
    }

    /// MilkDrop 2 or MilkDrop 3: the modes that run the whole MilkDrop 2 code path (preset importer, shader
    /// translation, soft-cut machinery). Individual behaviours are [`CompatRules`] fields, not this.
    pub fn md2_family(self) -> bool {
        matches!(self, Compat::MilkDrop2 | Compat::MilkDrop3)
    }
}

/// How 1-pixel lines and points (custom/main waves, shape outlines, motion vectors) are rasterised.
///
/// projectM draws `GL_LINES`/`GL_LINE_STRIP`/`GL_LINE_LOOP`/`GL_POINTS`, non-antialiased (`GL_LINE_SMOOTH` has no
/// effect on Mesa). The GPU's own line rule (diamond exit) decides every stroke pixel, so the choice below only
/// matters for pixel-exactness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum StrokeRaster {
    /// `wgpu` `LineList` / `PointList` primitives (1 px): fastest, but the rule is the driver's and can differ
    /// from projectM's GL rules for many stroke pixels.
    Native,
    /// Exact CPU emulation of projectM's GL rules (strip / diamond exit / point floor), every lit pixel
    /// drawn as a 1-px point at its centre: identical on every GPU.
    #[default]
    Emulated,
    /// Thin quads (one pixel wide band covering the major axis extent): the D3D9 look MilkDrop 2 had.
    /// `Compat::MilkDrop2` always draws strokes this way, whatever the configured mode.
    Quads,
}

/// Renderer construction parameters.
#[derive(Clone, Debug)]
pub struct RenderConfig {
    /// Internal image width (`m_nTexSizeX`; MD2 "auto exact" = window size), also the output width.
    pub width: u32,
    /// Internal image height, also the output height.
    pub height: u32,
    /// Warp mesh cells `[gx, gy]` (both even, e.g. `[48, 36]`).
    pub mesh: [u32; 2],
    /// In texels; added to warp UVs (`MD2: 0.5`, projectM/modern APIs: `0`).
    pub texel_offset: [f32; 2],
    /// Format of the host target view passed to `render_frame`. `*Srgb` formats are compensated so
    /// stored bytes equal a non-sRGB render.
    pub output_format: wgpu::TextureFormat,
    /// Seeds noise textures, rotation matrices, hue offsets.
    pub seed: u64,
    /// MD2 vs projectM behaviour where they differ (default projectM).
    pub compat: Compat,
    /// Rasterisation of 1-pixel lines and points, see [`StrokeRaster`].
    pub stroke: StrokeRaster,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self::from_run(&RunConfig::default(), wgpu::TextureFormat::Rgba8Unorm)
    }
}

impl RenderConfig {
    /// Build a config from the ABI's [`RunConfig`] (size, mesh, texel offset, seed) and the host
    /// target format; compatibility and stroke mode take their defaults.
    pub fn from_run(run: &RunConfig, output_format: wgpu::TextureFormat) -> Self {
        Self {
            width: run.width,
            height: run.height,
            mesh: run.mesh,
            texel_offset: run.texel_offset,
            output_format,
            seed: run.seed,
            compat: Compat::default(),
            stroke: StrokeRaster::default(),
        }
    }
}

/// A warp or composite shader: WGSL module source (starting with `lactea_abi::WGSL_COMMON`,
/// entry `fs_main`) plus the resource interface the renderer binds in group 1.
#[derive(Clone, Debug)]
pub struct CompiledShader {
    /// WGSL module source.
    pub wgsl: String,
    /// Resource interface bound in group 1.
    pub interface: ShaderInterface,
    /// Per-vertex values interpolated as extra varyings (`lactea_hlsl::Translated::folds`). Entry `k` is a complete
    /// WGSL function `fn fold_k(vs_uv: vec4<f32>, vs_rad_ang: vec2<f32>, vs_diffuse: vec4<f32>) -> f32` that may read
    /// the `WGSL_COMMON` uniforms and call earlier `fold_j`. The fragment shader's `VsOut` carries them as
    /// `@location(3 + k / 4) fold{k / 4}: vec4<f32>`, component `k % 4`. Empty: no extra varyings.
    pub folds: Vec<String>,
}

/// Errors reported by the renderer.
#[derive(Debug)]
#[non_exhaustive]
pub enum RenderError {
    /// Shader module / pipeline rejected by wgpu validation.
    Shader(String),
    /// Bad argument (sizes, data length, ...).
    Invalid(String),
    /// GPU read-back failed.
    Readback(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Shader(s) => write!(f, "shader error: {s}"),
            RenderError::Invalid(s) => write!(f, "invalid argument: {s}"),
            RenderError::Readback(s) => write!(f, "readback error: {s}"),
        }
    }
}
impl std::error::Error for RenderError {}
