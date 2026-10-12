//! The engine: preset loading and the per-frame CPU pipeline (projectM `MilkdropPreset`).

use crate::md3_inputs::{Md3Frame, Md3Inputs, MouseState, new_analyzer, shader_ext};
use crate::media::Media;
use crate::rng::{ShaderStream, StreamRng, hue_offsets};
use crate::runtime::{
    Env, MAX_Q, NUM_Q, PerFrame, PerPixel, ShapeRt, WaveRt, compile_part, eel_dialect,
};
use crate::sprites::{SpriteInputs, SpriteRt};
use crate::textures::{TextureProvider, pick_random, rand_slot};
use crate::{
    EngineConfig, EngineError, ForcedFrame, FrameClock, PresetReport, ShaderFallback,
    ShaderOutcome, Transition, TransitionInfo,
};
use lactea_abi::{PassKind, ShaderInterface, TexSource};
use lactea_audio::{Analyzer, AudioFrame};
use lactea_eel::GlobalMemory;
use lactea_hlsl::{Options as HlslOptions, default_comp_shader, default_warp_shader, translate};
use lactea_preset::{
    BaseValues, Flavor, MAX_CUSTOM_SHAPES, MAX_CUSTOM_WAVES, Preset, code_to_source,
};
use lactea_render::warp_mesh::{self, VertexVars, WarpAnim};
use lactea_render::wavegen::{CustomWaveSampling, NUM_WAVEFORM_SAMPLES};
use lactea_render::{
    Compat, CompiledShader, CustomWave as RenderWave, FrameInput, MeshGeometry, PassRandom,
    RenderConfig, Renderer, ShapeInstance, TransitionBlender, TransitionKind, TransitionParams,
    WarpGpu, WavePoint,
};
use std::collections::HashMap;
use std::sync::Arc;
use wgpu::util::DeviceExt;

mod custom_io;
mod md2_blend;
use md2_blend::{BlendBase, BlendState, build_blend_frame};
mod comp_effects;
mod md3_blend;
pub use md3_blend::{
    DoubleMode, DoublePresetInfo, Md3Locks, Md3Pattern, Md3PatternInstance,
    default_enabled_patterns, md3_draw_progress, md3_fill_vertinfo, md3_random_instance,
};

/// projectM's default composite shader (`FinalComposite.cpp`): plain copy of the main image.
const PASSTHROUGH_COMP: &str = "shader_body\n{\nret = tex2D(sampler_main, uv).xyz;\n}";

const ROT_NAMES: [&str; 24] = [
    "rot_s1",
    "rot_s2",
    "rot_s3",
    "rot_s4",
    "rot_d1",
    "rot_d2",
    "rot_d3",
    "rot_d4",
    "rot_f1",
    "rot_f2",
    "rot_f3",
    "rot_f4",
    "rot_vf1",
    "rot_vf2",
    "rot_vf3",
    "rot_vf4",
    "rot_uf1",
    "rot_uf2",
    "rot_uf3",
    "rot_uf4",
    "rot_rand1",
    "rot_rand2",
    "rot_rand3",
    "rot_rand4",
];

/// A loaded preset: parsed model + all expression contexts + per-preset random state.
struct Loaded {
    name: String,
    preset: Preset,
    pf: PerFrame,
    pp: PerPixel,
    waves: Vec<WaveRt>,
    shapes: Vec<ShapeRt>,
    /// `PresetState::frameQVariables`.
    frame_q: [f64; MAX_Q],
    /// Engine time at which this state's preset was loaded (`m_fPresetStartTime`): MilkDrop 3's shader `time` counts
    /// from it.
    start_time: f64,
    warp_stream: Option<ShaderStream>,
    comp_stream: Option<ShaderStream>,
    hue_offsets: [f32; 4],
    has_warp: bool,
    has_comp: bool,
    /// Frames rendered since the load.
    frames: u64,
    /// MilkDrop 2's `CBlendableFloat` blend state of this preset's base values (MD2-family soft cuts).
    floats: BlendBase,
    /// MD3 preset sprite (`CompatRules::md3_preset_sprites`); gone when its `done` fires or the preset is replaced.
    sprite: Option<SpriteRt>,
}

/// What the last rendered frame left behind for [`Engine::frame_vars`].
#[derive(Default)]
struct LastFrame {
    regs: Vec<f64>,
    audio: Option<AudioFrame>,
    time: f32,
    frame: u32,
    progress: f32,
    warp: Option<PassRandom>,
    comp: Option<PassRandom>,
}

/// A running preset together with the renderer (images, shaders, textures) it draws with: what
/// projectM calls a `MilkdropPreset` (one framebuffer pair per preset).
struct Slot {
    renderer: Renderer,
    loaded: Loaded,
    last: LastFrame,
}

/// What [`Engine::enter_compat`] replaced, to put back when the load that triggered the switch fails.
struct CompatUndo {
    compat: Compat,
    analyzer: Analyzer,
    rng_owner: GlobalMemory,
}

/// The part of projectM's `TimeKeeper` presets can observe: when the active preset started, the
/// smoothing (soft cut) window and the `progress` value derived from them.
#[derive(Clone, Copy, Debug)]
struct PresetClock {
    /// Time of the frame being / last rendered (`m_currentTime`).
    now: f64,
    preset_time_a: f64,
    preset_time_b: f64,
    smoothing: bool,
    soft_cut: f64,
    preset_duration: f64,
}

impl PresetClock {
    fn start_preset(&mut self) {
        self.smoothing = false;
        self.preset_time_a = self.now;
    }

    fn start_smoothing(&mut self, soft_cut: f64) {
        self.smoothing = true;
        self.preset_time_b = self.now;
        self.soft_cut = soft_cut;
    }

    fn end_smoothing(&mut self) {
        self.smoothing = false;
        self.preset_time_a = self.preset_time_b;
    }

    fn smooth_ratio(&self) -> f64 {
        (self.now - self.preset_time_b) / self.soft_cut
    }

    /// `PresetProgressA`: 1 while smoothing, else the elapsed fraction of the preset duration.
    fn progress_a(&self) -> f64 {
        if self.smoothing {
            1.0
        } else {
            ((self.now - self.preset_time_a) / self.preset_duration).min(1.0)
        }
    }
}

/// The soft cut in progress (`PresetTransition`).
#[derive(Clone, Copy, Debug)]
struct ActiveTransition {
    kind: TransitionKind,
    rand_static: [i32; 4],
    start: f64,
    duration: f64,
    last_draw: f64,
}

impl ActiveTransition {
    /// `PresetTransition::IsDone`.
    fn is_done(&self, now: f64) -> bool {
        self.duration <= 0.0 || now - self.start >= self.duration
    }

    /// `PresetTransition::Progress`.
    fn progress(&self, now: f64) -> f64 {
        ((now - self.start) / self.duration).clamp(0.0, 1.0)
    }
}

/// The MilkDrop engine. See the crate docs.
pub struct Engine {
    cfg: EngineConfig,
    device: wgpu::Device,
    queue: wgpu::Queue,
    analyzer: Analyzer,
    provider: Option<Box<dyn TextureProvider>>,
    /// The preset shown (the outgoing one during a soft cut). `None` until the first load.
    active: Option<Slot>,
    /// The preset fading in during a soft cut.
    incoming: Option<Slot>,
    /// An idle renderer (the one every load builds into; also the base renderer before the first
    /// load). Invariant: `active` or `spare` is `Some`.
    spare: Option<Renderer>,
    blender: Option<TransitionBlender>,
    transition: Option<ActiveTransition>,
    clock: PresetClock,
    /// Owner of the expressions' `rand()` stream, shared by every preset (projectm-eval's static).
    rng_owner: GlobalMemory,
    transition_select: StreamRng,
    transition_static: StreamRng,
    forced_kind: Option<TransitionKind>,
    forced_static: Option<[i32; 4]>,
    last_time: f64,
    last_frame_no: u32,
    forced: Option<ForcedFrame>,
    forced_incoming: Option<ForcedFrame>,
    forced_blend: Option<(Vec<u8>, Vec<u8>)>,
    /// MilkDrop 2 preset blend (`CompatRules::md2_preset_blend`): `m_pOldState` + the blend bookkeeping while a soft
    /// cut runs. `active` is then `m_pState` (the new preset) and its renderer keeps the old shaders.
    md2: Option<BlendState>,
    /// `m_vertinfo` of `RandomizeBlendPattern` (`(a, c)` per mesh vertex), its seeded stream, the animated patterns'
    /// clocks and `m_nTransitionBlendPattern` (`None` = -1 = random).
    md2_vertinfo: Vec<[f32; 2]>,
    md2_pattern_rng: StreamRng,
    md2_clocks: md2_blend::PatternClocks,
    md2_forced_pattern: Option<u32>,
    /// MilkDrop 3 (`CompatRules::md3_double_preset`): the `.milk2` on screen, the header values the previous load or
    /// ordinary soft cut left (a `.milk2` without those keys falls back on them) and the stream the MD3 soft-cut
    /// patterns are drawn from.
    double: Option<md3_blend::DoubleState>,
    md3_carry: lactea_preset::milk2::Milk2Header,
    md3_pattern_rng: StreamRng,
    on_switch_failed: crate::callbacks::Hook<crate::callbacks::SwitchFailure>,
    /// MD3 `AutoHardCutMode` state and the cuts it asked for since the host last called `take_auto_cuts`.
    auto_cut: crate::auto_cut::AutoCut,
    pending_auto_cuts: u32,
    /// MilkDrop 3 host inputs (mouse, shader FFT, sticky `FFTAttack` / `FFTDecay`).
    md3: Md3Inputs,
}

fn eel_seed(run_seed: u64) -> u64 {
    // projectm-eval's global Mersenne Twister: 0x4141f00d, shifted by the run seed so runs are reproducible.
    let s =
        0x4141_f00du32.wrapping_add((run_seed.wrapping_sub(1)).wrapping_mul(0x9E37_79B9) as u32);
    u64::from(s)
}

impl Engine {
    /// Creates an engine rendering into textures on `device`/`queue`, sized and seeded by `cfg`.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, cfg: EngineConfig) -> Self {
        // the renderer rejects zero sizes and meshes that are not even grids of at least 2x2 cells; a config
        // that asks for one is rounded to the nearest usable value (a no-op for every valid config)
        let rc = RenderConfig {
            width: cfg.width.max(1),
            height: cfg.height.max(1),
            mesh: cfg.mesh.map(|m| m.max(2).next_multiple_of(2)),
            texel_offset: cfg.texel_offset,
            output_format: cfg.output_format,
            seed: cfg.seed,
            compat: cfg.compat,
            stroke: Default::default(),
        };
        let renderer =
            Renderer::new(device, queue, rc).expect("the render config was sanitised above");
        let rng_owner = GlobalMemory::with_dialect(eel_dialect(&cfg.compat.rules()));
        rng_owner.set_rng_seed(eel_seed(cfg.seed));
        Self {
            analyzer: new_analyzer(&cfg),
            device: device.clone(),
            queue: queue.clone(),
            provider: None,
            active: None,
            incoming: None,
            spare: Some(renderer),
            blender: None,
            transition: None,
            clock: PresetClock {
                now: 0.0,
                preset_time_a: 0.0,
                preset_time_b: 0.0,
                smoothing: false,
                soft_cut: 3.0,
                preset_duration: cfg.preset_duration,
            },
            rng_owner,
            transition_select: StreamRng::new(cfg.seed, "transition_select"),
            transition_static: StreamRng::new(cfg.seed, "transition_static"),
            forced_kind: None,
            forced_static: None,
            last_time: 0.0,
            last_frame_no: 0,
            forced: None,
            forced_incoming: None,
            forced_blend: None,
            md2: None,
            md2_vertinfo: Vec::new(),
            md2_pattern_rng: StreamRng::new(cfg.seed, "md2_blend_pattern"),
            md2_clocks: Default::default(),
            md2_forced_pattern: None,
            double: None,
            md3_carry: Default::default(),
            md3_pattern_rng: StreamRng::new(cfg.seed, "md3_blend_pattern"),
            on_switch_failed: Default::default(),
            auto_cut: crate::auto_cut::AutoCut::new(cfg.seed, cfg.auto_hard_cut_mode),
            pending_auto_cuts: 0,
            md3: Md3Inputs::new(),
            cfg,
        }
    }

    /// The configuration the engine was created with.
    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    /// The compatibility mode of the presets now running: [`EngineConfig::compat`] for a fixed engine, and with
    /// [`EngineConfig::auto_compat`] the mode the last loaded preset resolved to (the configured one before the
    /// first load).
    pub fn current_compat(&self) -> Compat {
        self.cfg.compat
    }

    /// The renderer of the active preset (the outgoing one during a soft cut); before the first
    /// load, the idle base renderer. Settings made here (stroke rasteriser, minimum blur levels,
    /// injected noise) are inherited by the renderers later created for other presets.
    pub fn renderer(&self) -> &Renderer {
        self.active
            .as_ref()
            .map(|s| &s.renderer)
            .or(self.spare.as_ref())
            .expect("engine always owns a renderer")
    }

    /// Mutable access to [`Engine::renderer`].
    pub fn renderer_mut(&mut self) -> &mut Renderer {
        match self.active.as_mut() {
            Some(s) => &mut s.renderer,
            None => self.spare.as_mut().expect("engine always owns a renderer"),
        }
    }

    /// The renderer of the incoming preset while a soft cut is in progress.
    pub fn incoming_renderer(&self) -> Option<&Renderer> {
        self.incoming.as_ref().map(|s| &s.renderer)
    }

    /// The audio analysis of the last rendered frame.
    pub fn audio_frame(&self) -> &AudioFrame {
        self.analyzer.frame()
    }

    /// Name given to the last successfully loaded preset (the incoming one during a soft cut).
    pub fn preset_name(&self) -> Option<&str> {
        self.incoming
            .as_ref()
            .or(self.active.as_ref())
            .map(|s| s.loaded.name.as_str())
    }

    /// Name of the preset that is being faded out (or shown, without a soft cut).
    pub fn active_preset_name(&self) -> Option<&str> {
        self.active.as_ref().map(|s| s.loaded.name.as_str())
    }

    /// Linear progress `0..=1` of the soft cut in progress, `None` when there is none. In the MD2 family
    /// ([`crate::Compat::md2_family`]) this is MD2's `m_fBlendProgress` (`(GetTime() - start) / duration`, as of the last
    /// rendered frame; 0 before the first blend frame).
    pub fn transition_progress(&self) -> Option<f64> {
        if let Some(m) = &self.md2 {
            return Some(f64::from(m.progress).clamp(0.0, 1.0));
        }
        match (&self.transition, &self.incoming) {
            (Some(t), Some(_)) => Some(t.progress(self.clock.now)),
            _ => None,
        }
    }

    /// `m_nTransitionBlendPattern` (MilkDrop 2 soft cuts): `Some(k)` forces blend pattern `k` (0 = uniform fade,
    /// 1 = directional wipe, 2 = plasma ... 18 = Julia fractal) for the following soft cuts, `None` (the default,
    /// MD2's -1) draws one per soft cut from the seeded stream `md2_blend_pattern`.
    pub fn set_md2_blend_pattern(&mut self, pattern: Option<u32>) {
        let limit = if self.cfg.compat.rules().md3_blend_patterns {
            Md3Pattern::ALL.len() as u32 - 1
        } else {
            md2_blend::TRANSITION_LIMIT
        };
        self.md2_forced_pattern = pattern.map(|k| k.min(limit));
    }

    /// The pattern kind of the MilkDrop 2 blend in progress.
    pub fn md2_blend_pattern(&self) -> Option<u32> {
        self.md2.as_ref().map(|m| m.pattern_kind)
    }

    /// The soft cut in progress (shader, random integers, start time, duration).
    pub fn transition_info(&self) -> Option<TransitionInfo> {
        match (&self.transition, &self.incoming) {
            (Some(t), Some(_)) => Some(TransitionInfo {
                kind: t.kind,
                rand_static: t.rand_static,
                start_time: t.start,
                duration: t.duration,
            }),
            _ => None,
        }
    }

    /// Force the shader and/or the four static random integers of the **next** soft cut (the
    /// seeded streams still advance). For tests and the oracle comparison.
    pub fn force_next_transition(
        &mut self,
        kind: Option<TransitionKind>,
        rand_static: Option<[i32; 4]>,
    ) {
        self.forced_kind = kind;
        self.forced_static = rand_static;
    }

    /// Sets the source of user textures (`sampler_<name>` in preset shaders).
    pub fn set_texture_provider(&mut self, provider: Box<dyn TextureProvider>) {
        self.provider = Some(provider);
    }

    /// Interleaved float PCM (`-1..1`), any channel count (only the first two are used).
    pub fn push_pcm_f32(&mut self, interleaved: &[f32], channels: usize) {
        self.analyzer.push_pcm_f32(interleaved, channels);
    }

    /// MilkDrop 3: the mouse for the following frames (see [`MouseState`]). The engine samples it once per rendered
    /// frame; ignored (but harmless) under the other compat modes.
    pub fn set_mouse(&mut self, mouse: MouseState) {
        self.md3.set_mouse(mouse);
    }

    /// MilkDrop 3 `VolumeSensitivity` (linear gain on the quantised input, default 1); only with
    /// [`Compat::MilkDrop3`](crate::Compat::MilkDrop3).
    pub fn set_volume_sensitivity(&mut self, sensitivity: f32) {
        self.cfg.volume_sensitivity = sensitivity;
        self.analyzer.set_volume_sensitivity(sensitivity);
    }

    /// Every renderer the engine owns (active, incoming, idle spare).
    pub(crate) fn all_renderers_mut(&mut self) -> Vec<&mut Renderer> {
        let mut v: Vec<&mut Renderer> = Vec::new();
        if let Some(s) = self.active.as_mut() {
            v.push(&mut s.renderer);
        }
        if let Some(s) = self.incoming.as_mut() {
            v.push(&mut s.renderer);
        }
        if let Some(r) = self.spare.as_mut() {
            v.push(r);
        }
        v
    }

    /// Resize the render size (clears the feedback image). Presets keep running. A zero size is raised to 1.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        self.cfg.width = width;
        self.cfg.height = height;
        for r in self.all_renderers_mut() {
            // the size is non-zero and the mesh was validated when it was set, so this cannot fail
            let _ = r.resize(width, height);
        }
        self.md3_refresh_pattern();
    }

    /// Teacher forcing for the **next** [`Engine::render`] call only.
    pub fn force_next_frame(&mut self, forced: Option<ForcedFrame>) {
        self.forced = forced;
    }

    /// Teacher forcing of the transition pass for the **next** [`Engine::render`] call: blend
    /// these `old` / `new` images (RGBA8, `width*height*4`, row 0 = top: the oracle's `old_out` /
    /// `new_out`) instead of the presets' own. No effect when no soft cut is drawn that frame.
    pub fn force_next_blend_inputs(&mut self, inputs: Option<(Vec<u8>, Vec<u8>)>) {
        self.forced_blend = inputs;
    }

    /// Teacher forcing of the **incoming** preset for the next [`Engine::render`] call (the
    /// oracle's `new_vars` / `new_main`); only used while a soft cut is drawn.
    pub fn force_next_incoming_frame(&mut self, forced: Option<ForcedFrame>) {
        self.forced_incoming = forced;
    }

    // -----------------------------------------------------------------------------------------
    // preset loading
    // -----------------------------------------------------------------------------------------

    /// Load a preset. `transition` says how it replaces the current one: [`Transition::Cut`]
    /// immediately, [`Transition::Soft`] by blending the two presets (both keep running) over
    /// the given number of seconds, like projectM's smooth preset change. With nothing loaded
    /// yet a soft cut is a cut. On error the previously loaded preset(s) stay untouched.
    ///
    /// Unless [`EngineConfig::start_clean`] is set the new preset's first frame is warped from
    /// the old active preset's finished image instead of black (projectM's default).
    pub fn load_preset(
        &mut self,
        bytes: &[u8],
        name: &str,
        transition: Transition,
    ) -> Result<PresetReport, EngineError> {
        let result = self.load_preset_inner(bytes, name, transition);
        if let Err(e) = &result {
            self.on_switch_failed
                .fire(&crate::callbacks::SwitchFailure {
                    name: name.to_string(),
                    message: e.to_string(),
                });
        }
        result
    }

    fn load_preset_inner(
        &mut self,
        bytes: &[u8],
        name: &str,
        transition: Transition,
    ) -> Result<PresetReport, EngineError> {
        let want = self
            .cfg
            .auto_compat
            .then(|| crate::compat_for_preset(bytes));
        let result = self.load_preset_resolved(bytes, name, transition, want);
        // Auto mode: a preset that resolved to projectM and was rejected because its code does not compile may still
        // be a MilkDrop 2 preset (the MD2 code-line glueing joins what projectM keeps apart, e.g. an identifier split
        // over two lines). Retry once under MilkDrop 2; if that fails too, the projectM error is the one reported.
        match result {
            Err(rejected @ EngineError::Compile { .. }) if want == Some(Compat::ProjectM) => {
                match self.load_preset_resolved(bytes, name, transition, Some(Compat::MilkDrop2)) {
                    Ok(mut report) => {
                        report
                            .warnings
                            .push(format!("fallback: projectM rejected: {rejected}"));
                        Ok(report)
                    }
                    Err(_) => Err(rejected),
                }
            }
            result => result,
        }
    }

    /// [`Engine::load_preset`] once the compatibility mode is known (`None`: keep the current one). A different mode
    /// replaces everything the engine derives from the mode and loads the preset with a hard cut: the running
    /// preset's contexts, renderer rules and audio analysis belong to the old mode and cannot be blended into the
    /// new one (`docs/md3.md`, "Automatic compatibility"). A failed load restores the old mode.
    fn load_preset_resolved(
        &mut self,
        bytes: &[u8],
        name: &str,
        transition: Transition,
        want: Option<Compat>,
    ) -> Result<PresetReport, EngineError> {
        let Some(want) = want.filter(|w| *w != self.cfg.compat) else {
            return self.load_preset_in_mode(bytes, name, transition);
        };
        let undo = self.enter_compat(want);
        let result = self.load_preset_in_mode(bytes, name, Transition::Cut);
        if result.is_err() {
            self.leave_compat(undo);
        }
        result
    }

    /// Switch the engine's mode-dependent state to `compat`: the analysis mode of the audio, the EEL dialect (a fresh
    /// `reg00..reg99` / `gmegabuf`, one `rand()` stream throughout) and the mode every renderer built from now on
    /// follows ([`Engine::acquire_renderer`]). Returns what [`Engine::leave_compat`] needs to undo it.
    fn enter_compat(&mut self, compat: Compat) -> CompatUndo {
        let previous = self.cfg.compat;
        self.cfg.compat = compat;
        let mut analyzer = new_analyzer(&self.cfg);
        analyzer.take_pcm_from(&self.analyzer);
        let owner =
            GlobalMemory::sharing_rng_with_dialect(&self.rng_owner, eel_dialect(&compat.rules()));
        CompatUndo {
            compat: previous,
            analyzer: std::mem::replace(&mut self.analyzer, analyzer),
            rng_owner: std::mem::replace(&mut self.rng_owner, owner),
        }
    }

    /// Undo [`Engine::enter_compat`] after a failed load: the running preset keeps its mode.
    fn leave_compat(&mut self, undo: CompatUndo) {
        self.cfg.compat = undo.compat;
        self.analyzer = undo.analyzer;
        self.rng_owner = undo.rng_owner;
        // a failed load hands its renderer back to the spare slot, built for the mode that failed
        if let Some(r) = self.spare.as_mut().filter(|r| r.compat() != undo.compat) {
            *r = r.sibling_with_compat(undo.compat);
        }
    }

    /// The idle renderer every load builds into, following the engine's current mode: the spare one, or a sibling
    /// of the active renderer (whose pipelines and noise it shares). A renderer of another mode is rebuilt.
    fn acquire_renderer(&mut self) -> Renderer {
        let compat = self.cfg.compat;
        match self.spare.take() {
            Some(r) if r.compat() == compat => r,
            Some(r) => r.sibling_with_compat(compat),
            None => self
                .active
                .as_ref()
                .expect("engine always owns a renderer")
                .renderer
                .sibling_with_compat(compat),
        }
    }

    fn load_preset_in_mode(
        &mut self,
        bytes: &[u8],
        name: &str,
        transition: Transition,
    ) -> Result<PresetReport, EngineError> {
        // MilkDrop 3: `.milk2` double presets, and the next preset asked for while one is on screen
        let transition = match self.load_preset_md3(bytes, name, transition) {
            md3_blend::Md3Dispatch::Done(r) => return r,
            md3_blend::Md3Dispatch::Plain(t) => t,
        };
        // MilkDrop 2 blends inside one pipeline; a zero duration is its hard-cut path (`fBlendTime == 0`)
        let transition = if self.cfg.compat.rules().md2_preset_blend {
            match transition {
                Transition::Soft { seconds } if seconds > 0.0 && self.active.is_some() => {
                    return self.load_preset_md2_soft(bytes, name, seconds);
                }
                Transition::Soft { .. } => Transition::Cut,
                t => t,
            }
        } else {
            transition
        };
        // every load builds into an idle renderer: the previous preset keeps drawing untouched
        let mut renderer = self.acquire_renderer();
        renderer.reset_images();
        match self.build_loaded(bytes, name, &mut renderer, true) {
            Ok((loaded, report)) => {
                self.commit(
                    Slot {
                        renderer,
                        loaded,
                        last: LastFrame::default(),
                    },
                    transition,
                );
                Ok(report)
            }
            Err(e) => {
                self.spare = Some(renderer);
                Err(e)
            }
        }
    }

    /// `ProjectM::StartPresetTransition`.
    fn commit(&mut self, mut slot: Slot, transition: Transition) {
        let soft = match transition {
            Transition::Soft { seconds } if self.active.is_some() => Some(seconds),
            _ => None,
        };
        // already in a transition: complete it immediately
        if self.incoming.is_some() {
            self.promote_incoming();
        }
        if let Some(act) = self.active.as_ref().filter(|_| !self.cfg.start_clean) {
            let mut enc = self.device.create_command_encoder(&Default::default());
            // equal sizes by construction (every renderer follows `Engine::resize`); otherwise the new preset
            // simply starts black
            let _ = slot.renderer.draw_initial_image(&mut enc, &act.renderer);
            self.queue.submit(Some(enc.finish()));
        }
        match soft {
            None => {
                if let Some(mut old) = self.active.replace(slot) {
                    // MilkDrop 2: a hard cut ends a running blend (`Default()` clears `m_bBlending`) and releases
                    // `m_OldShaders`
                    self.md2 = None;
                    self.double = None;
                    old.renderer.set_old_shader_state(None);
                    self.spare = Some(old.renderer);
                }
                self.clock.start_preset();
            }
            Some(seconds) => {
                // RandomTransition(): shader index from the select stream, then PresetTransition's
                // four static integers
                let idx = self.transition_select.next32();
                let mut rand_static = [0i32; 4];
                for r in rand_static.iter_mut() {
                    *r = self.transition_static.next32() as i32;
                }
                self.incoming = Some(slot);
                self.clock.start_smoothing(seconds);
                self.transition = Some(ActiveTransition {
                    kind: self
                        .forced_kind
                        .take()
                        .unwrap_or_else(|| TransitionKind::from_index(idx)),
                    rand_static: self.forced_static.take().unwrap_or(rand_static),
                    start: self.clock.now,
                    duration: seconds,
                    last_draw: 0.0,
                });
            }
        }
    }

    /// The incoming preset becomes the active one (`m_activePreset = move(m_transitioningPreset)`).
    fn promote_incoming(&mut self) {
        if let Some(inc) = self.incoming.take()
            && let Some(old) = self.active.replace(inc)
        {
            self.spare = Some(old.renderer);
        }
        self.transition = None;
    }

    /// Everything of a load that can fail or has to run in projectM's order: parse, expression
    /// contexts, init code, shaders, textures. `sprite` says whether the text's sprite block is honoured: a `.milk2`
    /// half reads it only for PRESET2 with the header's `sprite=1`.
    fn build_loaded(
        &mut self,
        bytes: &[u8],
        name: &str,
        renderer: &mut Renderer,
        sprite: bool,
    ) -> Result<(Loaded, PresetReport), EngineError> {
        let flavor = if self.cfg.compat.md2_family() {
            Flavor::Md2
        } else {
            Flavor::ProjectM
        };
        let mut preset = Preset::parse_with(bytes, flavor)?;
        if !sprite {
            preset.sprite = None;
        }
        let mut report = PresetReport {
            name: name.to_string(),
            preset_version: preset.milkdrop_preset_version,
            compat: self.cfg.compat,
            ..Default::default()
        };
        let gm = Arc::new(GlobalMemory::sharing_rng_with(&self.rng_owner));
        let rules = self.cfg.compat.rules();

        let env = self.init_env();
        let mut pf = PerFrame::new(&gm);
        let mut pp = PerPixel::new(&gm);
        if rules.md3_extended_vars {
            pf.extend_md3();
            pp.extend_md3();
        }
        let base = preset.base.clone();

        // ---- per-frame init + per-frame code, per-vertex code ------------------------------
        pf.load_state(&base, &env);
        // A failed init part (MD3) runs nothing, so every q stays 0, which is what the failure rule asks for.
        if let Some(prog) = compile_part(
            &mut pf.ctx,
            &preset.per_frame_init,
            &rules,
            "per-frame init code",
            &mut report.warnings,
        )? {
            pf.ctx.run(&prog);
        }
        for i in 0..pf.v.q.len() {
            pf.q_after_init[i] = pf.q(i);
        }
        let mut frame_q = pf.q_after_init;
        pf.prog = compile_part(
            &mut pf.ctx,
            &preset.per_frame,
            &rules,
            "per-frame code",
            &mut report.warnings,
        )?;
        pp.prog = compile_part(
            &mut pp.ctx,
            &preset.per_pixel,
            &rules,
            "per-pixel code",
            &mut report.warnings,
        )?;

        // ---- custom waves -----------------------------------------------------------------
        let mut waves = Vec::with_capacity(MAX_CUSTOM_WAVES);
        let slots = rules.custom_slots;
        for (i, cfg) in preset.waves.iter().take(slots).enumerate() {
            let mut w = WaveRt::new(&gm, cfg.clone());
            if rules.md3_extended_vars {
                w.extend_md3();
            }
            let enabled = w.enabled();
            let step = |what: &str, r: Result<(), EngineError>, report: &mut PresetReport| match r {
                Ok(()) => Ok(()),
                Err(e) if !enabled => {
                    report
                        .warnings
                        .push(format!("custom wave {i} (disabled) {what}: {e}"));
                    Ok(())
                }
                Err(e) => Err(e),
            };
            w.load_state(&pf, &env);
            let init = compile_part(
                &mut w.pf,
                &cfg.init,
                &rules,
                &format!("custom wave {i} init code"),
                &mut report.warnings,
            );
            let r = init.map(|p| {
                if let Some(p) = p {
                    w.pf.run(&p);
                }
            });
            step("init", r, &mut report)?;
            for t in 0..w.pfv.t.len() {
                w.t_after_init[t] = w.pf.get(w.pfv.t[t]);
            }
            let r = compile_part(
                &mut w.pf,
                &cfg.per_frame,
                &rules,
                &format!("custom wave {i} per-frame code"),
                &mut report.warnings,
            )
            .map(|p| w.pf_prog = p);
            step("per-frame", r, &mut report)?;
            let r = compile_part(
                &mut w.pp,
                &cfg.per_point,
                &rules,
                &format!("custom wave {i} per-point code"),
                &mut report.warnings,
            )
            .map(|p| w.pp_prog = p);
            step("per-point", r, &mut report)?;
            waves.push(w);
        }

        // ---- custom shapes ----------------------------------------------------------------
        let mut shapes = Vec::with_capacity(MAX_CUSTOM_SHAPES);
        for (i, cfg) in preset.shapes.iter().take(slots).enumerate() {
            let mut s = ShapeRt::new(&gm, cfg.clone());
            if rules.md3_extended_vars {
                s.extend_md3();
            }
            let enabled = s.enabled();
            let step = |what: &str, r: Result<(), EngineError>, report: &mut PresetReport| match r {
                Ok(()) => Ok(()),
                Err(e) if !enabled => {
                    report
                        .warnings
                        .push(format!("custom shape {i} (disabled) {what}: {e}"));
                    Ok(())
                }
                Err(e) => Err(e),
            };
            s.load_state(&frame_q, &env, 0);
            let init = compile_part(
                &mut s.ctx,
                &cfg.init,
                &rules,
                &format!("custom shape {i} init code"),
                &mut report.warnings,
            );
            let r = init.map(|p| {
                if let Some(p) = p {
                    s.ctx.run(&p);
                }
            });
            step("init", r, &mut report)?;
            for t in 0..s.v.t.len() {
                s.t_after_init[t] = s.ctx.get(s.v.t[t]);
            }
            let r = compile_part(
                &mut s.ctx,
                &cfg.per_frame,
                &rules,
                &format!("custom shape {i} per-frame code"),
                &mut report.warnings,
            )
            .map(|p| s.prog = p);
            step("per-frame", r, &mut report)?;
            shapes.push(s);
        }
        // projectM: `frameQVariables` holds the q values after the per-frame init code.
        frame_q = pf.q_after_init;

        // ---- shaders ----------------------------------------------------------------------
        let (warp, comp) = self.build_shaders(&preset, &mut report);
        let warp_iface = warp.as_ref().map(|s| s.interface.clone());
        let comp_iface = comp.as_ref().map(|s| s.interface.clone());
        let (has_warp, has_comp) = Self::install_shaders(renderer, warp, comp, &mut report);
        let ifaces: Vec<ShaderInterface> = warp_iface
            .filter(|_| has_warp)
            .into_iter()
            .chain(comp_iface.filter(|_| has_comp))
            .collect();
        self.resolve_textures(renderer, &ifaces, &mut report);
        self.resolve_shape_images(renderer, &preset, &mut report);

        let (warp_stream, comp_stream) = if self.cfg.compat.rules().shared_shader_rand {
            // MD2 shares one random state between the passes.
            let shared = ShaderStream::new(self.cfg.seed, "shader_warp");
            (
                has_warp.then(|| shared.clone()),
                has_comp.then(|| shared.clone()),
            )
        } else {
            (
                has_warp.then(|| ShaderStream::new(self.cfg.seed, "shader_warp")),
                has_comp.then(|| ShaderStream::new(self.cfg.seed, "shader_comp")),
            )
        };

        // ---- MD3 preset sprite: killed with the previous preset, loaded with this one ------------------
        let sprite = match preset.sprite.as_ref().filter(|_| rules.md3_preset_sprites) {
            Some(cfg) => crate::sprites::load(
                self.provider.as_mut(),
                cfg,
                renderer,
                &gm,
                &rules,
                (self.last_time, self.last_frame_no),
                &mut report,
            ),
            None => {
                renderer.clear_sprite_images();
                None
            }
        };

        let loaded = Loaded {
            name: name.to_string(),
            preset,
            pf,
            pp,
            waves,
            shapes,
            frame_q,
            warp_stream,
            comp_stream,
            hue_offsets: hue_offsets(self.cfg.seed),
            has_warp,
            has_comp,
            frames: 0,
            floats: Default::default(),
            start_time: self.last_time,
            sprite,
        };
        self.md3.on_preset_loaded(&rules, &loaded.preset.base);
        Ok((loaded, report))
    }

    fn init_env(&self) -> Env {
        let (w, h) = (self.cfg.width, self.cfg.height);
        let (ax, ay) = warp_mesh::aspect_xy(w, h);
        Env {
            time: self.last_time as f32,
            fps: self.cfg.fps as f32,
            frame: self.last_frame_no,
            progress: self.clock.progress_a() as f32,
            mesh: self.cfg.mesh,
            pixels: [w, h],
            aspect: [ax, ay],
            inv_aspect: [1.0 / ax, 1.0 / ay],
            ..Default::default()
        }
    }

    /// Translate the preset's shaders following the PSVERSION rules of projectM (MilkDrop 2 with
    /// [`crate::Compat::md2_family`]).
    fn build_shaders(
        &self,
        p: &Preset,
        report: &mut PresetReport,
    ) -> (Option<CompiledShader>, Option<CompiledShader>) {
        let md2 = self.cfg.compat.md2_family();
        // projectM lowers float `%` / `fmod` to GLSL `mod` (floor semantics); MD2/HLSL truncates. Its hlslparser also
        // parses, emits asin/acos and prints float literals differently from the D3DX compiler (`projectm_flavor`).
        let mut opts = HlslOptions::default();
        opts.floor_mod = !md2;
        opts.projectm_flavor = !md2;
        opts.md3 = self.cfg.compat.rules().md3_shader_prelude;
        let to_compiled = |t: lactea_hlsl::Translated, report: &mut PresetReport| {
            report.warnings.extend(t.warnings.iter().cloned());
            CompiledShader {
                wgsl: t.wgsl,
                interface: t.interface,
            }
        };

        // ---- warp ---------------------------------------------------------------------
        let mut warp = None;
        if p.warp_ps_version > 0 {
            let text = p.warp_shader.as_deref().map(code_to_source);
            match text.filter(|t| !t.trim().is_empty()) {
                Some(src) => match translate(PassKind::Warp, &src, &opts) {
                    Ok(t) => {
                        report.warp = ShaderOutcome::User;
                        warp = Some(to_compiled(t, report));
                    }
                    Err(e) => {
                        report.warp = ShaderOutcome::Fallback;
                        report.shader_fallbacks.push(ShaderFallback {
                            pass: PassKind::Warp,
                            reason: e.to_string(),
                            fallback: "default (no-shader) warp".into(),
                        });
                    }
                },
                None if md2 => {
                    let src = default_warp_shader(p.base.decay, p.base.tex_wrap);
                    match translate(PassKind::Warp, &src, &opts) {
                        Ok(t) => {
                            report.warp = ShaderOutcome::Default;
                            warp = Some(to_compiled(t, report));
                        }
                        Err(e) => report.warnings.push(format!("default warp shader: {e}")),
                    }
                }
                None => report.warp = ShaderOutcome::Default,
            }
        }

        // ---- composite ----------------------------------------------------------------
        let mut comp = None;
        if p.comp_ps_version > 0 {
            let text = p.comp_shader.as_deref().map(code_to_source);
            let default_src = if md2 {
                default_comp_shader(
                    p.base.gamma_adj,
                    p.base.video_echo_alpha,
                    p.base.video_echo_zoom,
                    p.base.video_echo_orientation,
                    p.base.shader,
                    p.base.brighten,
                    p.base.darken,
                    p.base.solarize,
                    p.base.invert,
                    self.cfg.compat.rules().md3_md2_comp_effects,
                )
            } else {
                PASSTHROUGH_COMP.to_string()
            };
            let user = text.filter(|t| !t.trim().is_empty());
            let mut used_default = user.is_none();
            if let Some(src) = &user {
                match translate(PassKind::Comp, src, &opts) {
                    Ok(t) => {
                        report.comp = ShaderOutcome::User;
                        comp = Some(to_compiled(t, report));
                    }
                    Err(e) => {
                        report.comp = ShaderOutcome::Fallback;
                        report.shader_fallbacks.push(ShaderFallback {
                            pass: PassKind::Comp,
                            reason: e.to_string(),
                            fallback: "default composite shader".into(),
                        });
                        used_default = true;
                    }
                }
            }
            if used_default {
                if report.comp != ShaderOutcome::Fallback {
                    report.comp = ShaderOutcome::Default;
                }
                match translate(PassKind::Comp, &default_src, &opts) {
                    Ok(t) => comp = Some(to_compiled(t, report)),
                    Err(e) => report
                        .warnings
                        .push(format!("default composite shader: {e}")),
                }
            }
        }
        (warp, comp)
    }

    /// Hand the shaders to the renderer; wgpu rejecting a translated module is handled like a
    /// translation failure. Returns whether a warp / comp shader ended up installed.
    fn install_shaders(
        renderer: &mut Renderer,
        warp: Option<CompiledShader>,
        comp: Option<CompiledShader>,
        report: &mut PresetReport,
    ) -> (bool, bool) {
        let mut warp_ok = false;
        let mut comp_ok = false;
        let had_warp = warp.is_some();
        if had_warp {
            match renderer.set_warp_shader(warp) {
                Ok(()) => warp_ok = true,
                Err(e) => {
                    report.warp = ShaderOutcome::Fallback;
                    report.shader_fallbacks.push(ShaderFallback {
                        pass: PassKind::Warp,
                        reason: e.to_string(),
                        fallback: "default (no-shader) warp".into(),
                    });
                    let _ = renderer.set_warp_shader(None);
                }
            }
        } else {
            let _ = renderer.set_warp_shader(None);
        }
        if let Some(c) = comp {
            match renderer.set_comp_shader(Some(c)) {
                Ok(()) => comp_ok = true,
                Err(e) => {
                    report.comp = ShaderOutcome::Fallback;
                    report.shader_fallbacks.push(ShaderFallback {
                        pass: PassKind::Comp,
                        reason: e.to_string(),
                        fallback: "default composite shader".into(),
                    });
                    let passthrough =
                        translate(PassKind::Comp, PASSTHROUGH_COMP, &HlslOptions::default())
                            .ok()
                            .map(|t| CompiledShader {
                                wgsl: t.wgsl,
                                interface: t.interface,
                            });
                    if let Some(p) = passthrough {
                        comp_ok = renderer.set_comp_shader(Some(p)).is_ok();
                    }
                }
            }
        } else {
            let _ = renderer.set_comp_shader(None);
        }
        (warp_ok, comp_ok)
    }

    /// Give every user texture the shaders reference its pixels (`randNN` samplers pick from
    /// the provider's sorted pool, one pick per slot shared between the passes).
    fn resolve_textures(
        &mut self,
        renderer: &mut Renderer,
        ifaces: &[ShaderInterface],
        report: &mut PresetReport,
    ) {
        let mut wanted: Vec<(String, String)> = Vec::new(); // (render name, projectM sampler name)
        for iface in ifaces {
            for t in &iface.textures {
                if let TexSource::User(name) = &t.source {
                    if wanted.iter().any(|(n, _)| n == name) {
                        continue;
                    }
                    let pm = t
                        .hlsl_name
                        .strip_prefix("sampler_")
                        .unwrap_or(&t.hlsl_name)
                        .to_string();
                    wanted.push((name.clone(), pm));
                }
            }
        }
        if wanted.is_empty() {
            return;
        }
        let seed = self.cfg.seed;
        let Some(provider) = self.provider.as_mut() else {
            for (name, _) in wanted {
                report.unresolved_textures.push(name);
            }
            return;
        };
        let pool = provider.random_pool();
        let mut slot_pick: HashMap<usize, Option<String>> = HashMap::new();
        let mut cache: HashMap<String, Option<Media>> = HashMap::new();
        let md3_gif = self.cfg.compat.rules().md3_gif_textures;
        for (name, pm_name) in wanted {
            let provider_name = match rand_slot(&name) {
                Some(slot) => slot_pick
                    .entry(slot)
                    .or_insert_with(|| pick_random(seed, &pm_name, &pool))
                    .clone(),
                None => Some(name.clone()),
            };
            let Some(pn) = provider_name else {
                report.unresolved_textures.push(name);
                continue;
            };
            let media = cache.entry(pn.clone()).or_insert_with(|| {
                if md3_gif {
                    provider.load_md3_texture(&pn)
                } else {
                    provider.load(&pn).map(Media::Still)
                }
            });
            let set = match media {
                Some(Media::Still(img))
                    if img.width > 0
                        && img.height > 0
                        && img.rgba8.len() == (img.width * img.height * 4) as usize =>
                {
                    Some(renderer.set_user_texture(&name, img.width, img.height, &img.rgba8))
                }
                Some(Media::Animated(a)) => {
                    let frames: Vec<&[u8]> = a.frames.iter().map(|f| f.rgba8.as_slice()).collect();
                    Some(renderer.set_user_animation(
                        &name,
                        a.width,
                        a.height,
                        &frames,
                        &a.delays(),
                    ))
                }
                _ => None,
            };
            match set {
                Some(Ok(())) => report.resolved_textures.push((name, pn)),
                Some(Err(e)) => {
                    report.warnings.push(format!("texture {name}: {e}"));
                    report.unresolved_textures.push(name);
                }
                None => report.unresolved_textures.push(name),
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // rendering
    // -----------------------------------------------------------------------------------------

    /// Render one frame into `target` (a view of a `width x height` texture of the configured
    /// `output_format`). `clock.time` is the frame time in seconds; the audio analysis advances by
    /// `time - previous time` (0 for the first frame, like projectM).
    ///
    /// While a soft cut is in progress both presets are rendered (the incoming one first, like
    /// projectM's `ProjectM::RenderFrame`) and a transition shader blends their images into
    /// `target`.
    pub fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clock: FrameClock,
    ) {
        self.auto_cut_step(clock.time);
        let dt = (clock.time - self.last_time).max(0.0);
        self.last_time = clock.time;
        self.last_frame_no = clock.frame;
        self.analyzer.update_dt(dt);
        let mut audio = self.analyzer.frame().clone();
        let forced = self.forced.take();
        let forced_incoming = self.forced_incoming.take();
        let forced_blend = self.forced_blend.take();
        if let Some(f) = &forced {
            f.override_audio(&mut audio);
        }
        let md3 = self.md3.begin_frame(&self.cfg.compat.rules(), &audio);

        // ProjectM::RenderFrame: timers, end of smoothing, end of the transition.
        self.clock.now = clock.time;
        if self.clock.smoothing && self.incoming.is_some() && self.clock.smooth_ratio() >= 1.0 {
            self.clock.end_smoothing();
        }
        let mut progress = self.clock.progress_a() as f32;
        // MilkDrop 2 `RenderFrame`: update `m_fBlendProgress`; the blend ends once it is above 1 (`m_OldShaders` are
        // released); `progress` is then `(time - m_fPresetStartTime) / (m_fNextPresetTime - m_fPresetStartTime)`
        if let Some(m) = self.md2.as_mut() {
            if m.update(clock.time as f32) {
                progress = m.preset_progress(clock.time, self.cfg.preset_duration);
            } else {
                self.md2 = None;
                if let Some(a) = self.active.as_mut() {
                    a.renderer.set_old_shader_state(None);
                }
            }
        }
        // MilkDrop 3 `progressMode` events (the first soft-in leg ends, the soft-out ramp ends): a new blend state may
        // have replaced the old one
        if self.double_tick()
            && let Some(m) = self.md2.as_mut()
        {
            progress = m.preset_progress(clock.time, self.cfg.preset_duration);
        }
        if self.incoming.is_some()
            && self
                .transition
                .as_ref()
                .is_some_and(|t| t.is_done(clock.time))
        {
            self.promote_incoming();
        }
        let blending = self.incoming.is_some() && self.transition.is_some();

        // The incoming preset renders first (its image only), then the active one.
        if let Some(inc) = self.incoming.as_mut().filter(|_| blending) {
            let fi = prepare_slot(
                inc,
                &self.cfg,
                clock,
                progress,
                &audio,
                forced_incoming.as_ref(),
                None,
                md3.as_ref(),
            );
            let _ = inc.renderer.render_frame_to_output(encoder, &fi);
        }
        let cfg = &self.cfg;
        match self.active.as_mut() {
            Some(act) => {
                let fi = prepare_slot(
                    act,
                    cfg,
                    clock,
                    progress,
                    &audio,
                    forced.as_ref(),
                    self.md2.as_mut(),
                    md3.as_ref(),
                );
                // `prepare_slot` builds `warp_uv` from the renderer's own mesh and `md2_blend` only with
                // `Compat::md2_family`, the two inputs the renderer can reject
                let _ = if blending {
                    act.renderer.render_frame_to_output(encoder, &fi)
                } else {
                    act.renderer.render_frame(encoder, &fi, target)
                };
            }
            None => {
                let fi = FrameInput {
                    time: f64::from(clock.time as f32),
                    frame: clock.frame,
                    fps: cfg.fps as f32,
                    ..Default::default()
                };
                if let Some(r) = self.spare.as_mut() {
                    // a default frame always matches the mesh
                    let _ = r.render_frame(encoder, &fi, target);
                }
            }
        }

        if blending {
            self.draw_transition(encoder, target, clock.time, forced_blend);
        }
    }

    /// `PresetTransition::Draw`: blend the two finished images into `target`.
    fn draw_transition(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        now: f64,
        forced: Option<(Vec<u8>, Vec<u8>)>,
    ) {
        let (Some(act), Some(inc), Some(tr)) = (
            self.active.as_ref(),
            self.incoming.as_ref(),
            self.transition.as_mut(),
        ) else {
            return;
        };
        let blender = self
            .blender
            .get_or_insert_with(|| TransitionBlender::new(&self.device, self.cfg.output_format));
        let params = TransitionParams::new(
            tr.kind,
            tr.rand_static,
            tr.duration,
            now - tr.start,
            now - tr.last_draw,
        );
        tr.last_draw = now;
        let size = (self.cfg.width, self.cfg.height);
        match forced {
            Some((old, new)) => {
                let mk = |data: &[u8]| {
                    let t = self.device.create_texture_with_data(
                        &self.queue,
                        &wgpu::TextureDescriptor {
                            label: Some("forced transition input"),
                            size: wgpu::Extent3d {
                                width: size.0,
                                height: size.1,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING,
                            view_formats: &[],
                        },
                        wgpu::util::TextureDataOrder::LayerMajor,
                        data,
                    );
                    t.create_view(&Default::default())
                };
                let (o, n) = (mk(&old), mk(&new));
                blender.draw(encoder, &o, &n, target, size, &params);
            }
            None => blender.draw(
                encoder,
                act.renderer.output_view(),
                inc.renderer.output_view(),
                target,
                size,
                &params,
            ),
        }
    }

    /// Variables of the last frame in the `vars/NNNNN.json` dump format: every per-frame variable
    /// (builtins + user variables, lowercase), `reg00..reg99` as seen right after the per-frame
    /// code, `audio.*`, `render.*`, `hue_offset.0..3` and, when shaders are active, the per-pass
    /// `warp.*` / `comp.*` random state (and the unprefixed `rand_frame.*` / `rand_preset.*` of
    /// the composite — warp when there is no composite shader — pass).
    ///
    /// During a soft cut these are the **active** (outgoing) preset's; the incoming preset's are
    /// [`Engine::incoming_frame_vars`].
    pub fn frame_vars(&self) -> Vec<(String, f64)> {
        self.active
            .as_ref()
            .map(slot_frame_vars)
            .unwrap_or_default()
    }

    /// [`Engine::frame_vars`] of the incoming preset while a soft cut is in progress.
    pub fn incoming_frame_vars(&self) -> Vec<(String, f64)> {
        self.incoming
            .as_ref()
            .map(slot_frame_vars)
            .unwrap_or_default()
    }
}

/// What a slot's last frame left behind, in the `vars/NNNNN.json` dump naming.
fn slot_frame_vars(slot: &Slot) -> Vec<(String, f64)> {
    let l = &slot.loaded;
    let last = &slot.last;
    let mut out: Vec<(String, f64)> = l.pf.ctx.vars().map(|(n, v)| (n.to_string(), v)).collect();
    for (i, v) in last.regs.iter().enumerate() {
        out.push((format!("reg{i:02}"), *v));
    }
    if let Some(a) = &last.audio {
        for (n, v) in [
            ("bass", a.bass),
            ("mid", a.mid),
            ("treb", a.treb),
            ("vol", a.vol),
            ("bass_att", a.bass_att),
            ("mid_att", a.mid_att),
            ("treb_att", a.treb_att),
            ("vol_att", a.vol_att),
        ] {
            out.push((format!("audio.{n}"), f64::from(v)));
        }
    }
    out.push(("render.time".into(), f64::from(last.time)));
    out.push(("render.frame".into(), f64::from(last.frame)));
    for (i, h) in l.hue_offsets.iter().enumerate() {
        out.push((format!("hue_offset.{i}"), f64::from(*h)));
    }
    let xyzw = ['x', 'y', 'z', 'w'];
    let mut push_pass = |prefix: &str, p: &PassRandom| {
        for (k, axis) in xyzw.iter().enumerate() {
            out.push((
                format!("{prefix}rand_frame.{axis}"),
                f64::from(p.rand_frame[k]),
            ));
            out.push((
                format!("{prefix}rand_preset.{axis}"),
                f64::from(p.rand_preset[k]),
            ));
        }
        if !prefix.is_empty() {
            let ft = last.time;
            let wrapped = ft - ((f64::from(ft) / 10000.0) as i32 * 10000) as f32;
            out.push((format!("{prefix}time"), f64::from(wrapped)));
            out.push((format!("{prefix}progress"), f64::from(last.progress)));
            if let Some(rot) = &p.rot {
                for (m, name) in ROT_NAMES.iter().enumerate() {
                    for col in 0..3 {
                        for row in 0..4 {
                            out.push((
                                format!("{prefix}{name}.{}", col * 4 + row),
                                f64::from(rot[m][col][row]),
                            ));
                        }
                    }
                }
            }
        }
    };
    if let Some(w) = &last.warp {
        push_pass("warp.", w);
    }
    if let Some(c) = &last.comp {
        push_pass("comp.", c);
    }
    if let Some(p) = last.comp.as_ref().or(last.warp.as_ref()) {
        push_pass("", p);
    }
    out
}

/// Evaluate a slot's preset for this frame and hand the teacher-forced images to its renderer.
#[allow(clippy::too_many_arguments)]
fn prepare_slot(
    slot: &mut Slot,
    cfg: &EngineConfig,
    clock: FrameClock,
    progress: f32,
    audio: &AudioFrame,
    forced: Option<&ForcedFrame>,
    md2: Option<&mut BlendState>,
    md3: Option<&Md3Frame>,
) -> FrameInput {
    let g = slot.renderer.mesh_geometry();
    // sprite code sees `m_pState->m_fBlendProgress`: the blend's progress (`P` in a steady `.milk2`), 1 once none runs
    let blend_progress = md2.as_ref().map_or(1.0, |m| m.progress);
    let mut fi = match md2 {
        Some(m) => {
            let cx = FrameCtx {
                cfg,
                g,
                clock,
                progress,
                audio,
                md3,
            };
            build_blend_frame(&mut slot.loaded, &mut slot.last, m, &cx, forced)
        }
        None => build_frame(
            &mut slot.loaded,
            cfg,
            g,
            clock,
            progress,
            audio,
            forced,
            &mut slot.last,
            md3,
        ),
    };
    if cfg.compat.rules().md3_preset_sprites
        && let Some(sp) = slot.loaded.sprite.as_mut()
    {
        let inp = SpriteInputs {
            time: clock.time,
            frame: clock.frame,
            fps: cfg.fps as f32,
            progress: blend_progress,
            bass: audio.bass,
            mid: audio.mid,
            treb: audio.treb,
            bass_att: audio.bass_att,
            mid_att: audio.mid_att,
            treb_att: audio.treb_att,
        };
        fi.sprites.extend(sp.frame(&inp, (cfg.width, cfg.height)));
    }
    slot.last.audio = Some(audio.clone());
    slot.last.time = clock.time as f32;
    slot.last.frame = clock.frame;
    #[cfg(feature = "diagnostics")]
    if let Some(f) = forced {
        if let Some(m) = &f.prev_main
            && let Err(e) = slot.renderer.inject_main(m)
        {
            eprintln!("lactea: teacher forcing: {e}");
        }
        if let Some(m) = &f.comp_main
            && let Err(e) = slot.renderer.inject_comp_source(m)
        {
            eprintln!("lactea: teacher forcing (comp source): {e}");
        }
    }
    fi
}

// ---------------------------------------------------------------------------------------------
// per-frame pipeline
// ---------------------------------------------------------------------------------------------

/// Host-side inputs of one frame, shared by the stages of [`build_frame`] (and by the MilkDrop 2 blend frame).
#[derive(Clone, Copy)]
struct FrameCtx<'a> {
    cfg: &'a EngineConfig,
    g: &'a MeshGeometry,
    clock: FrameClock,
    progress: f32,
    audio: &'a AudioFrame,
    md3: Option<&'a Md3Frame>,
}

#[allow(clippy::too_many_arguments)]
fn build_frame(
    l: &mut Loaded,
    cfg: &EngineConfig,
    g: &MeshGeometry,
    clock: FrameClock,
    progress: f32,
    audio: &AudioFrame,
    forced: Option<&ForcedFrame>,
    last: &mut LastFrame,
    md3: Option<&Md3Frame>,
) -> FrameInput {
    let cx = FrameCtx {
        cfg,
        g,
        clock,
        progress,
        audio,
        md3,
    };
    let base = l.preset.base.clone();
    let env = frame_env(&cx);
    eval_per_frame(l, &cx, &env, &base, forced, last);
    let anim = warp_anim(&cx, &base);
    let toff = warp_mesh::texel_offset_uv(cfg.texel_offset, cfg.width, cfg.height);
    let (vertex_vars, warp_uv) = eval_vertices(l, g, &anim, toff);
    let mut fi = frame_head(l, &cx, &base, vertex_vars, warp_uv, anim, true, None);
    push_shapes(l, &env, &mut fi.shapes);
    push_waves(l, &cx, &env, &mut fi.waves);
    finish_frame(l, &cx, forced, last, &mut fi);
    fi
}

/// What the expression contexts see of the host this frame.
fn frame_env(cx: &FrameCtx) -> Env {
    let (cfg, g) = (cx.cfg, cx.g);
    let (w, h) = (cfg.width, cfg.height);
    let (ax, ay) = (g.aspect_x(), g.aspect_y());
    Env {
        time: cx.clock.time as f32,
        fps: cfg.fps as f32,
        frame: cx.clock.frame,
        progress: cx.progress,
        bass: cx.audio.bass,
        mid: cx.audio.mid,
        treb: cx.audio.treb,
        bass_att: cx.audio.bass_att,
        mid_att: cx.audio.mid_att,
        treb_att: cx.audio.treb_att,
        mesh: cfg.mesh,
        pixels: [w, h],
        aspect: [ax, ay],
        inv_aspect: [1.0 / ax, 1.0 / ay],
    }
}

/// `MilkdropPreset::PerFrameUpdate` / MD2's `LoadPerFrameEvallibVars` + the per-frame code of one state; `base` is
/// the state's base values as the equations start from them this frame.
fn eval_per_frame(
    l: &mut Loaded,
    cx: &FrameCtx,
    env: &Env,
    base: &BaseValues,
    forced: Option<&ForcedFrame>,
    last: &mut LastFrame,
) {
    l.pf.load_state(base, env);
    l.pp.load_readonly(&l.pf, env);
    l.pf.run();
    if let Some(f) = forced {
        f.apply_vars(&mut l.pf.ctx);
    }
    l.pp.load_q(&l.pf);
    for i in 0..l.pf.v.q.len() {
        l.frame_q[i] = l.pf.q(i);
    }
    l.pf.clamp();
    last.regs.clear();
    last.regs.extend((0..100).map(|i| l.pf.ctx.global().reg(i)));
    last.progress = cx.progress;
}

/// The warp animation terms of the frame (`fWarpTime`, `fWarpScaleInv`, the four `f[]`).
fn warp_anim(cx: &FrameCtx, base: &BaseValues) -> WarpAnim {
    let time = cx.clock.time as f32;
    let rules = cx.cfg.compat.rules();
    let mut anim = if rules.warp_anim_projectm {
        WarpAnim::projectm(time, base.warp_anim_speed, base.warp_scale)
    } else {
        WarpAnim::new(time, base.warp_anim_speed, base.warp_scale)
    };
    anim.skip_tiny_warp = rules.md3_warp_skip_tiny;
    anim
}

/// The ten motion variables per mesh vertex (per-vertex code when the preset has any, else the per-frame values) and
/// the warp UV of every vertex they give.
fn eval_vertices(
    l: &mut Loaded,
    g: &MeshGeometry,
    anim: &WarpAnim,
    toff: [f32; 2],
) -> (Vec<VertexVars>, Vec<[f32; 2]>) {
    let vertex_vars: Vec<VertexVars> = if l.pp.prog.is_some() {
        per_pixel_vars(l, g)
    } else {
        let pv = |v| l.pf.ctx.get(v);
        let vars = &l.pf.v;
        let vv = VertexVars {
            zoom: pv(vars.zoom) as f32,
            zoomexp: pv(vars.zoomexp) as f32,
            rot: pv(vars.rot) as f32,
            warp: pv(vars.warp) as f32,
            cx: pv(vars.cx) as f32,
            cy: pv(vars.cy) as f32,
            dx: pv(vars.dx) as f32,
            dy: pv(vars.dy) as f32,
            sx: pv(vars.sx) as f32,
            sy: pv(vars.sy) as f32,
        };
        vec![vv; g.vertex_count()]
    };
    let warp_uv: Vec<[f32; 2]> = vertex_vars
        .iter()
        .enumerate()
        .map(|(n, vv)| warp_mesh::warp_vertex_uv(g, n, vv, anim, toff))
        .collect();
    (vertex_vars, warp_uv)
}

/// Everything of a frame except shapes, custom waves and the shader pass random state: timing, audio, `q`, warp
/// mesh, post / blur / border / motion-vector parameters and the main waveform, from the state's per-frame
/// variables (after the per-frame code; for a MilkDrop 2 blend frame the already blended ones).
///
/// * `base`: the base values the frame's non-variable parameters (`fShader`, wave scale / smoothing ...) come from.
/// * `gpu`: hand the per-vertex inputs of projectM's GPU warp vertex stage over as well.
/// * `wrap_snap`: MD2 blend: the sampler address is `wrap > m_fSnapPoint` instead of `wrap > 0.0001`.
#[allow(clippy::too_many_arguments)]
fn frame_head(
    l: &mut Loaded,
    cx: &FrameCtx,
    base: &BaseValues,
    vertex_vars: Vec<VertexVars>,
    warp_uv: Vec<[f32; 2]>,
    anim: WarpAnim,
    gpu: bool,
    wrap_snap: Option<f32>,
) -> FrameInput {
    let (cfg, audio, clock, progress) = (cx.cfg, cx.audio, cx.clock, cx.progress);
    let frame_vars = cfg.compat.rules().frame_vars_drive_post;
    let time = clock.time as f32;
    let mut fi = FrameInput {
        time: f64::from(time),
        fps: cfg.fps as f32,
        frame: clock.frame,
        progress,
        bass: audio.bass,
        mid: audio.mid,
        treb: audio.treb,
        bass_att: audio.bass_att,
        mid_att: audio.mid_att,
        treb_att: audio.treb_att,
        imm_abs: [audio.bass, audio.mid, audio.treb],
        ..Default::default()
    };
    let pf = &l.pf;
    for i in 0..NUM_Q {
        fi.q[i] = pf.q(i) as f32;
    }
    if let Some(m) = cx.md3 {
        fi.ext = shader_ext(&cfg.compat.rules(), m, pf, l.start_time, clock.time, true);
    }
    fi.warp_uv = Some(warp_uv);
    // projectM evaluates the warp UV formula on the GPU (the renderer ignores this for MilkDrop 2 compat)
    if gpu {
        fi.warp_gpu = Some(WarpGpu {
            vars: vertex_vars,
            anim,
        });
    }

    // ---- post / composite parameters ----------------------------------------------------
    let vars = &pf.v;
    let f32v = |v| pf.ctx.get(v) as f32;
    fi.post.decay = f32v(vars.decay);
    fi.post.wrap = match wrap_snap {
        Some(snap) => pf.ctx.get(vars.wrap) > f64::from(snap),
        None => pf.ctx.get(vars.wrap) > 0.0001,
    };
    fi.post.fshader = base.shader;
    if frame_vars {
        fi.post.gamma = f32v(vars.gamma);
        fi.post.echo_zoom = f32v(vars.echo_zoom);
        fi.post.echo_alpha = f32v(vars.echo_alpha);
        fi.post.echo_orient = pf.ctx.get(vars.echo_orient) as i32;
        fi.post.brighten = pf.ctx.get(vars.brighten) != 0.0;
        fi.post.darken = pf.ctx.get(vars.darken) != 0.0;
        fi.post.solarize = pf.ctx.get(vars.solarize) != 0.0;
        fi.post.invert = pf.ctx.get(vars.invert) != 0.0;
    } else {
        // projectM's VideoEcho / Filters read the preset's initial values, not the per-frame variables.
        fi.post.gamma = base.gamma_adj;
        fi.post.echo_zoom = base.video_echo_zoom;
        fi.post.echo_alpha = base.video_echo_alpha;
        fi.post.echo_orient = base.video_echo_orientation;
        fi.post.brighten = base.brighten;
        fi.post.darken = base.darken;
        fi.post.solarize = base.solarize;
        fi.post.invert = base.invert;
    }
    fi.blur.min = [
        f32v(vars.blur1_min),
        f32v(vars.blur2_min),
        f32v(vars.blur3_min),
    ];
    fi.blur.max = [
        f32v(vars.blur1_max),
        f32v(vars.blur2_max),
        f32v(vars.blur3_max),
    ];
    fi.blur.edge_darken = f32v(vars.blur1_edge_darken);
    fi.borders.ob_size = f32v(vars.ob_size);
    fi.borders.ob_rgba = [
        f32v(vars.ob_r),
        f32v(vars.ob_g),
        f32v(vars.ob_b),
        f32v(vars.ob_a),
    ];
    fi.borders.ib_size = f32v(vars.ib_size);
    fi.borders.ib_rgba = [
        f32v(vars.ib_r),
        f32v(vars.ib_g),
        f32v(vars.ib_b),
        f32v(vars.ib_a),
    ];
    fi.mv.x = f32v(vars.mv_x);
    fi.mv.y = f32v(vars.mv_y);
    fi.mv.dx = f32v(vars.mv_dx);
    fi.mv.dy = f32v(vars.mv_dy);
    fi.mv.l = f32v(vars.mv_l);
    fi.mv.rgba = [
        f32v(vars.mv_r),
        f32v(vars.mv_g),
        f32v(vars.mv_b),
        f32v(vars.mv_a),
    ];
    fi.darken_center = pf.ctx.get(vars.darken_center) > 0.0;

    // ---- main waveform -----------------------------------------------------------------
    let mw = &mut fi.main_wave;
    let (mode_mod, thick, dots, additive) = if frame_vars {
        (
            18,
            pf.ctx.get(vars.wave_thick) != 0.0,
            pf.ctx.get(vars.wave_usedots) != 0.0,
            pf.ctx.get(vars.wave_additive) != 0.0,
        )
    } else {
        // projectM's Waveform::Draw reads the preset's initial mode / flags.
        // `waveMode % WaveformMode::Count` (16 modes incl. the Milkdrop2077 additions).
        (16, base.wave_thick, base.wave_dots, base.additive_waves)
    };
    let mode_src = if frame_vars {
        pf.ctx.get(vars.wave_mode) as i32
    } else {
        base.wave_mode
    };
    mw.mode = if cfg.compat.rules().md3_wave_modes {
        // the C remainder of the raw double, not `rem_euclid`: a negative value is not a mode
        lactea_render::wavegen_md3::md3_wave_mode(pf.ctx.get(vars.wave_mode))
    } else {
        mode_src.rem_euclid(mode_mod)
    };
    mw.rgba = [
        f32v(vars.wave_r),
        f32v(vars.wave_g),
        f32v(vars.wave_b),
        f32v(vars.wave_a),
    ];
    mw.x = f32v(vars.wave_x);
    mw.y = f32v(vars.wave_y);
    mw.mystery = f32v(vars.wave_mystery);
    mw.usedots = dots;
    mw.thick = thick;
    mw.additive = additive;
    mw.brighten = pf.ctx.get(vars.wave_brighten) > 0.0;
    mw.scale = base.wave_scale;
    mw.smoothing = base.wave_smoothing;
    mw.mod_alpha_by_volume = base.mod_wave_alpha_by_volume;
    mw.mod_alpha_start = base.mod_wave_alpha_start;
    mw.mod_alpha_end = base.mod_wave_alpha_end;
    mw.wave = audio.wave;
    mw.spectrum = audio.spectrum;
    fi
}

/// Custom shapes (all instances), appended to `out` in draw order (before the custom waves, like projectM).
fn push_shapes(l: &mut Loaded, env: &Env, out: &mut Vec<ShapeInstance>) {
    let frame_q = l.frame_q;
    for s in l.shapes.iter_mut() {
        if !s.enabled() {
            continue;
        }
        let n = s.cfg.instances.clamp(0, 1024);
        for inst in 0..n {
            s.load_state(&frame_q, env, inst);
            if let Some(p) = &s.prog {
                s.ctx.run(p);
            }
            let v = &s.v;
            let c = |x| s.ctx.get(x) as f32;
            out.push(ShapeInstance {
                sides: s.ctx.get(v.sides) as i32,
                x: s.ctx.get(v.x),
                y: s.ctx.get(v.y),
                rad: c(v.rad),
                ang: c(v.ang),
                rgba: [c(v.r), c(v.g), c(v.b), c(v.a)],
                rgba2: [c(v.r2), c(v.g2), c(v.b2), c(v.a2)],
                border_rgba: [c(v.border_r), c(v.border_g), c(v.border_b), c(v.border_a)],
                additive: s.ctx.get(v.additive) as i32 != 0,
                thick: s.cfg.thick_outline != 0,
                textured: s.ctx.get(v.textured) as i32 != 0,
                tex_zoom: c(v.tex_zoom),
                tex_ang: c(v.tex_ang),
                // projectM `shapecode_N_image` (empty = the main image); MD2-flavour presets never set it
                image: crate::textures::shape_image(&s.cfg.image),
            });
        }
    }
}

/// Custom waves (per-frame code, `samples`, smoothing, per-point code), appended to `out` in draw order.
fn push_waves(l: &mut Loaded, cx: &FrameCtx, env: &Env, out: &mut Vec<RenderWave>) {
    let audio = cx.audio;
    // `m_fWaveScale.eval(-1)`: the unblended value
    let wave_scale = l.preset.base.wave_scale;
    for wv in l.waves.iter_mut() {
        if !wv.enabled() {
            continue;
        }
        wv.load_state(&l.pf, env);
        wv.load_point_readonly(&l.pf);
        if let Some(p) = &wv.pf_prog {
            wv.pf.run(p);
        }
        wv.init_point_qt();
        let samples = (wv.pf.get(wv.pfv.samples) as i32).clamp(0, 512);
        let use_dots = wv.cfg.b_use_dots != 0;
        if (use_dots && samples < 1) || samples < 2 {
            continue;
        }
        let spectrum = wv.cfg.b_spectrum != 0;
        let sampling = CustomWaveSampling {
            spectrum,
            samples,
            sep: wv.cfg.sep,
            scaling: wv.cfg.scaling,
            smoothing: wv.cfg.smoothing,
            wave_scale,
        };
        let (d1, d2): (&[f32], &[f32]) = if spectrum {
            (&audio.spectrum[0], &audio.spectrum[1])
        } else {
            (
                &audio.wave[0][..NUM_WAVEFORM_SAMPLES],
                &audio.wave[1][..NUM_WAVEFORM_SAMPLES],
            )
        };
        let values = lactea_render::wavegen::custom_wave_values(&sampling, d1, d2);
        let n = values.len();
        let mult = if n > 1 { 1.0 / (n - 1) as f32 } else { 0.0 };
        let (r, g, b, a) = (
            wv.pf.get(wv.pfv.r),
            wv.pf.get(wv.pfv.g),
            wv.pf.get(wv.pfv.b),
            wv.pf.get(wv.pfv.a),
        );
        let mut points = Vec::with_capacity(n);
        for (i, [v1, v2]) in values.iter().enumerate() {
            let pv = &wv.ppv;
            wv.pp.set(pv.sample, f64::from(i as f32 * mult));
            wv.pp.set(pv.value1, f64::from(*v1));
            wv.pp.set(pv.value2, f64::from(*v2));
            wv.pp.set(pv.x, f64::from(0.5 + *v1));
            wv.pp.set(pv.y, f64::from(0.5 + *v2));
            wv.pp.set(pv.r, r);
            wv.pp.set(pv.g, g);
            wv.pp.set(pv.b, b);
            wv.pp.set(pv.a, a);
            if let Some(p) = &wv.pp_prog {
                wv.pp.run(p);
            }
            let c = |x| wv.pp.get(x) as f32;
            points.push(WavePoint {
                x: wv.pp.get(pv.x),
                y: wv.pp.get(pv.y),
                rgba: [c(pv.r), c(pv.g), c(pv.b), c(pv.a)],
            });
        }
        out.push(RenderWave {
            points,
            use_dots,
            thick: wv.cfg.b_draw_thick != 0,
            additive: wv.cfg.b_additive != 0,
        });
    }
}

/// Shader pass random state (`rand_frame`, the per-pass rotation matrices, hue phases) of one state's frame; counts
/// the frame in the preset.
fn finish_frame(
    l: &mut Loaded,
    cx: &FrameCtx,
    forced: Option<&ForcedFrame>,
    last: &mut LastFrame,
    fi: &mut FrameInput,
) {
    let shared = cx.cfg.compat.rules().shared_shader_rand;
    let time = cx.clock.time as f32;
    let drop = !shared;
    let (mut warp_r, mut comp_r) = (None, None);
    if shared {
        // one shared stream: draw once per frame
        if let Some(s) = l.warp_stream.as_mut() {
            let p = s.frame(time, drop);
            if l.has_warp {
                warp_r = Some(p.clone());
            }
            if l.has_comp {
                comp_r = Some(p);
            }
        }
    } else {
        warp_r = l.warp_stream.as_mut().map(|s| s.frame(time, drop));
        comp_r = l.comp_stream.as_mut().map(|s| s.frame(time, drop));
    }
    let mut hue = l.hue_offsets;
    if let Some(f) = forced {
        if let Some(p) = f.pass_random("warp.")
            && warp_r.is_some()
        {
            warp_r = Some(p);
        }
        if let Some(p) = f.pass_random("comp.")
            && comp_r.is_some()
        {
            comp_r = Some(p);
        }
        if let Some(h) = f.hue_offsets() {
            hue = h;
        }
    }
    if let Some(p) = comp_r.as_ref().or(warp_r.as_ref()) {
        fi.rand_frame = p.rand_frame;
        fi.rand_preset = p.rand_preset;
    }
    // projectM's first-frame overlay loss exists only for presets whose shaders sample blur and is decided by the
    // renderer (`CompatRules::blur_alloc_drops_overlays`): the per-frame/per-shape/per-wave expressions always run and
    // their results are handed over unmodified.
    l.frames += 1;
    fi.warp_random = warp_r.clone();
    fi.comp_random = comp_r.clone();
    fi.hue_offsets = Some(hue);
    last.warp = warp_r;
    last.comp = comp_r;
    l.hue_offsets = hue;
}

/// Per-pixel inputs `(x, y, rad, ang)` exactly as projectM computes them (`PerPixelMesh`):
/// its own mesh `y_pm = row_pm/gy*2-1` (row 0 on top; `-y` of ours is *not* bit-identical in f32:
/// e.g. 2/36*2-1 and -(34/36*2-1) differ by 1 ulp), `rad = hypotf(x*ax, y_pm*ay)`,
/// `ang = -atan2f(y_pm*ay, x*ax)` (`atan2f(+0, -x) = +pi`, so the centre row left of the centre
/// is `-pi`). `rad` is compared against constants by presets (`below(rad,0.5)` ...), so a 1-ulp
/// difference flips whole cells (Memories Of The Castle: vertex rows 2 / 34, warped 48 -> 96+ dB).
fn per_pixel_inputs(g: &MeshGeometry, x: u32, row_pm: u32) -> [f32; 4] {
    let vx = x as f32 / g.gx() as f32 * 2.0 - 1.0;
    let vy = row_pm as f32 / g.gy() as f32 * 2.0 - 1.0;
    let (ax, ay) = (g.aspect_x(), g.aspect_y());
    let rad = (vx * ax).hypot(vy * ay);
    let ang = if row_pm == g.gy() / 2 && x == g.gx() / 2 {
        0.0
    } else {
        (vy * ay).atan2(vx * ax)
    };
    [vx * 0.5 * ax + 0.5, vy * 0.5 * ay + 0.5, rad, -ang]
}

/// Per-vertex code over the mesh in projectM's iteration order (its mesh row 0 is the top of the
/// image; ours is the bottom); the ten motion variables per vertex, stored at MD2 mesh indices.
fn per_pixel_vars(l: &mut Loaded, g: &MeshGeometry) -> Vec<VertexVars> {
    let mut out = vec![VertexVars::default(); g.vertex_count()];
    let pfv = &l.pf.v;
    let get = |v| l.pf.ctx.get(v);
    let frame_vals = [
        get(pfv.zoom),
        get(pfv.zoomexp),
        get(pfv.rot),
        get(pfv.warp),
        get(pfv.cx),
        get(pfv.cy),
        get(pfv.dx),
        get(pfv.dy),
        get(pfv.sx),
        get(pfv.sy),
    ];
    let pp = &mut l.pp;
    let prog = pp.prog.take();
    let v = &pp.v;
    for row_pm in 0..=g.gy() {
        let row = g.gy() - row_pm;
        for x in 0..=g.gx() {
            let n = g.index(x, row);
            let c = &mut pp.ctx;
            let [px, py, prad, pang] = per_pixel_inputs(g, x, row_pm);
            c.set(v.x, f64::from(px));
            c.set(v.y, f64::from(py));
            c.set(v.rad, f64::from(prad));
            c.set(v.ang, f64::from(pang));
            c.set(v.zoom, frame_vals[0]);
            c.set(v.zoomexp, frame_vals[1]);
            c.set(v.rot, frame_vals[2]);
            c.set(v.warp, frame_vals[3]);
            c.set(v.cx, frame_vals[4]);
            c.set(v.cy, frame_vals[5]);
            c.set(v.dx, frame_vals[6]);
            c.set(v.dy, frame_vals[7]);
            c.set(v.sx, frame_vals[8]);
            c.set(v.sy, frame_vals[9]);
            if let Some(p) = &prog {
                c.run(p);
            }
            let vv = VertexVars {
                zoom: c.get(v.zoom) as f32,
                zoomexp: c.get(v.zoomexp) as f32,
                rot: c.get(v.rot) as f32,
                warp: c.get(v.warp) as f32,
                cx: c.get(v.cx) as f32,
                cy: c.get(v.cy) as f32,
                dx: c.get(v.dx) as f32,
                dy: c.get(v.dy) as f32,
                sx: c.get(v.sx) as f32,
                sy: c.get(v.sy) as f32,
            };
            out[n] = vv;
        }
    }
    pp.prog = prog;
    out
}

#[cfg(test)]
mod per_pixel_input_tests {
    use super::*;

    /// Regression (Memories Of The Castle): `rad` must be projectM's own `hypotf(x*ax, y_pm*ay)`
    /// with `y_pm = row/gy*2-1` (row 0 on top). Row 2 gives exactly 0.5, the mirrored row 34 gives
    /// 0.49999997, so presets testing `below(rad, 0.5)` flip a whole mesh column.
    #[test]
    fn per_pixel_inputs_follow_projectm_float_rounding() {
        let g = MeshGeometry::new(48, 36, 640, 360).unwrap();
        let top = per_pixel_inputs(&g, 24, 2);
        let bottom = per_pixel_inputs(&g, 24, 34);
        assert_eq!(top[2], 0.5);
        assert!(bottom[2] < 0.5);
        // `y` (0.5 = middle) and `ang = -atan2(y_pm*ay, x*ax)`
        assert!(top[1] < 0.5 && bottom[1] > 0.5);
        assert!(top[3] > 0.0 && bottom[3] < 0.0);
    }

    /// Centre row left of the centre: `atan2f(+0, -x) = +pi` is negated by projectM -> `-pi`.
    #[test]
    fn per_pixel_ang_centre_row_is_minus_pi() {
        let g = MeshGeometry::new(48, 36, 640, 360).unwrap();
        let left = per_pixel_inputs(&g, 3, 18);
        assert_eq!(left[3], -std::f32::consts::PI);
        let right = per_pixel_inputs(&g, 40, 18);
        assert_eq!(right[3].abs(), 0.0);
        assert_eq!(per_pixel_inputs(&g, 24, 18)[3], 0.0);
    }
}

// Runtime parameters (`projectm_set_*` equivalents); a child module so it sees the engine's state.
#[path = "extras.rs"]
mod extras;
#[path = "params.rs"]
mod params;
#[path = "shape_images.rs"]
mod shape_images;
pub use extras::{DebugImage, TouchType};
