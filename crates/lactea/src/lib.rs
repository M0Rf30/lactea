//! MilkDrop engine: preset + expressions + audio + shaders driving `lactea-render`.
//! Embeddable into any wgpu host: bytes in (presets, textures, PCM), a host-provided
//! `wgpu::Device` / `Queue` and target view out. No windows, no audio devices, no filesystem.
//!
//! This crate is the CPU side of MilkDrop 2's `RenderFrame` (`milkdropfs.cpp` + `state.cpp`),
//! behaving like projectM where projectM and MilkDrop 2 differ (projectM is the reference;
//! `docs/divergences.md`, `docs/engine.md`).
//!
//! ```no_run
//! use lactea::{Engine, EngineConfig, FrameClock, Transition};
//! # fn demo(device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::TextureView, milk: &[u8], pcm: &[f32]) {
//! let mut engine = Engine::new(device, queue, EngineConfig::default());
//! let report = engine.load_preset(milk, "my preset", Transition::Cut).unwrap();
//! for frame in 0..60u32 {
//!     engine.push_pcm_f32(pcm, 2);
//!     let mut enc = device.create_command_encoder(&Default::default());
//!     engine.render(&mut enc, target, FrameClock { time: frame as f64 / 60.0, frame });
//!     queue.submit(Some(enc.finish()));
//! }
//! # let _ = report;
//! # }
//! ```
//!
//! Preset changes: [`Transition::Cut`] replaces the preset at once, [`Transition::Soft`] blends the
//! old and the new preset over a number of seconds (both keep running: projectM's shader
//! transitions by default, MilkDrop 2's per-vertex blend with [`Compat::MilkDrop2`]); see
//! `docs/engine.md`.

#![warn(missing_docs)]

pub mod auto_cut;
mod cache;
mod callbacks;
mod control;
mod engine;
mod idle;
mod md3_inputs;
mod media;
mod mxd3q;
mod playlist;
mod rng;
mod runtime;
mod sprites;
mod teacher;
mod textures;
mod vars;

pub use callbacks::{Hook, SwitchFailure};
pub use control::{HARD_CUT_MIN_FRAMES, PresetControl, SwitchRequest};
pub use engine::{
    DebugImage, DoubleMode, DoublePresetInfo, Engine, Md3Locks, Md3Pattern, Md3PatternInstance,
    TouchType, default_enabled_patterns, md3_draw_progress, md3_fill_vertinfo, md3_random_instance,
};
pub use idle::{IDLE_PRESET, IDLE_PRESET_NAME};
pub use lactea_abi::PassKind;
pub use lactea_abi::dump::Stage;
pub use lactea_audio::AudioFrame;
pub use lactea_render::TransitionKind;
pub use lactea_render::{Compat, Renderer};
pub use md3_inputs::MouseState;
pub use media::{AnimFrame, AnimatedImage, Media, MediaFiles, TEXTURE_EXTENSIONS, decode_gif};
pub use playlist::{
    DEFAULT_RETRY_COUNT, FailedFn, Filter, INSERT_AT_END, LoadRequest, MAX_HISTORY, Playlist,
    SortOrder, SortPredicate, SwitchedFn,
};
pub use teacher::ForcedFrame;
pub use textures::{Image, TextureProvider};

use std::collections::HashMap;

/// User-supplied HLSL for a locked MilkDrop 3 preset (see [`EngineConfig::shader_overrides`]).
#[derive(Clone, Debug, Default)]
pub struct ShaderOverride {
    /// Replacement for the warp shader of the preset whose `MD31` handle matches.
    pub warp: Option<String>,
    /// Replacement for the composite shader of the preset whose `MD32` handle matches.
    pub comp: Option<String>,
}

/// The compatibility mode [`EngineConfig::auto_compat`] picks for a preset file: [`Compat::MilkDrop3`] when
/// [`lactea_preset::detect_compat`] finds evidence of a MilkDrop 3 feature, otherwise [`Compat::ProjectM`]
/// (MilkDrop 2 files are indistinguishable from projectM ones, so this never returns it; [`Engine::load_preset`]
/// reaches it only by retrying a preset projectM rejects with a compile error). Pure on the bytes; the
/// evidence itself is [`lactea_preset::detect_compat`].
pub fn compat_for_preset(bytes: &[u8]) -> Compat {
    match lactea_preset::detect_compat(bytes).family {
        lactea_preset::DetectedFamily::MilkDrop3 => Compat::MilkDrop3,
        _ => Compat::ProjectM,
    }
}
/// Engine construction parameters.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Render width in pixels (the target view passed to [`Engine::render`] has this size).
    pub width: u32,
    /// Render height in pixels.
    pub height: u32,
    /// Warp mesh cells `[x, y]` (both even).
    pub mesh: [u32; 2],
    /// Seeds every random stream (shader `rand_*` / `rot_*`, hue phases, `rand()` of the
    /// expressions, random textures, noise textures). Same seed, same inputs: same frames.
    pub seed: u64,
    /// projectM (default), MilkDrop 2 or MilkDrop 3 behaviour where they differ. With [`EngineConfig::auto_compat`]
    /// this is only the mode before the first preset is loaded; [`Engine::current_compat`] is the mode in use.
    pub compat: Compat,
    /// Choose the compatibility mode per preset: every [`Engine::load_preset`] looks at the preset's bytes
    /// ([`lactea_preset::detect_compat`]: MilkDrop 3 when the file has evidence of a MilkDrop 3 feature, otherwise
    /// projectM) and switches the engine to that mode when it differs from the current one; a preset projectM
    /// rejects with an expression compile error is retried once under MilkDrop 2. A switch is always a hard cut.
    /// Default `false`: [`EngineConfig::compat`] is fixed. See `docs/md3.md`,
    /// "Automatic compatibility".
    pub auto_compat: bool,
    /// Added to warp UVs in texels (0 on modern APIs; MilkDrop 2 on D3D9 used 0.5).
    pub texel_offset: [f32; 2],
    /// Format of the target view; `*Srgb` targets are compensated so bytes equal a linear render.
    pub output_format: wgpu::TextureFormat,
    /// Sample rate of the PCM pushed through [`Engine::push_pcm_f32`].
    pub sample_rate: u32,
    /// Frame rate reported to presets as `fps` (projectM reports the *target* rate).
    pub fps: u32,
    /// Seconds a preset is meant to run; `progress = elapsed / preset_duration`. The default
    /// (1e9 s) behaves like a locked preset: `progress` stays at ~0. Set the real duration if the
    /// host rotates presets.
    pub preset_duration: f64,
    /// projectM's `start_clean`: `true` starts every new preset from a black frame; `false`
    /// (projectM's default) warps the new preset's first frame from the previous active preset's
    /// finished image. The very first preset always starts black.
    pub start_clean: bool,
    /// MilkDrop 3's `DisableBox` setting (default on): under [`Compat::MilkDrop3`] the outer / inner borders
    /// (`ob_*` / `ib_*`) are not drawn while a preset blend is running. Ignored by the other compat modes.
    pub md3_disable_box: bool,
    /// MilkDrop 3 `AutoHardCutMode` (0 = off, 1..=7, see [`auto_cut`]). Only honoured with
    /// [`Compat::MilkDrop3`]; the other compat modes ignore it. [`Engine::set_auto_hard_cut_mode`] changes it
    /// at runtime.
    pub auto_hard_cut_mode: u8,
    /// MilkDrop 3 `VolumeSensitivity` (`settings.ini`, default 1): a linear gain on the 8-bit quantised input of the
    /// audio analysis. Only [`Compat::MilkDrop3`] uses it (the other modes ignore it). Change it live with
    /// [`Engine::set_volume_sensitivity`].
    pub volume_sensitivity: f32,
    /// MilkDrop 3 `BlendMilk2` (default on): transitions into and out of a `.milk2` double preset are soft. Off, every
    /// load of a `.milk2` and every change away from one is a hard cut. Only [`Compat::MilkDrop3`] reads it.
    pub blend_milk2: bool,
    /// MilkDrop 3 `BlendTimeUser` in seconds (default 1.7): the duration of the two legs of a soft transition into a
    /// `.milk2` (the request's own time only has to be positive) and of the soft-out ramp's time base.
    pub blend_time_user: f64,
    /// The patterns random soft cuts of [`Compat::MilkDrop3`] may pick (MD3's `[transitions]` settings; empty = only
    /// `zoom`). Default: the 13 patterns MD3 enables without a settings file.
    pub md3_transition_patterns: Vec<Md3Pattern>,
    /// Encrypted-shader overrides keyed by the preset's `MD31` / `MD32` handle (`docs/md3.md`, "Encrypted shaders").
    /// A matching entry replaces the preset's shader text (the placeholder stub). Override text is plain
    /// `\n`-separated HLSL, not the `\x01` form of preset text. Default: empty.
    pub shader_overrides: HashMap<u64, ShaderOverride>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            mesh: [48, 36],
            seed: 1,
            compat: Compat::ProjectM,
            auto_compat: false,
            texel_offset: [0.0, 0.0],
            output_format: wgpu::TextureFormat::Rgba8Unorm,
            sample_rate: 44_100,
            fps: 60,
            preset_duration: 1e9,
            start_clean: false,
            md3_disable_box: true,
            auto_hard_cut_mode: 0,
            volume_sensitivity: 1.0,
            blend_milk2: true,
            blend_time_user: 1.7,
            shader_overrides: HashMap::new(),
            md3_transition_patterns: engine::default_enabled_patterns(),
        }
    }
}

/// Time of the frame about to be rendered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameClock {
    /// Seconds since the engine's clock origin (`time` preset variable, `_c2.x`). The audio
    /// smoothing advances by the difference to the previous call (0 for the first frame).
    pub time: f64,
    /// Frame counter (`frame` preset variable).
    pub frame: u32,
}

/// How [`Engine::load_preset`] replaces the current preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transition {
    /// Immediately (projectM hard cut).
    Cut,
    /// Blend old and new preset over `seconds` (projectM's default soft cut is 3 s). A
    /// non-positive duration behaves like a cut one frame later.
    Soft {
        /// Blend duration in seconds.
        seconds: f64,
    },
}

/// The soft cut in progress ([`Engine::transition_info`]).
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct TransitionInfo {
    /// The transition shader (projectM mode).
    pub kind: TransitionKind,
    /// The four random integers fixed for the whole transition (`iRandStatic`).
    pub rand_static: [i32; 4],
    /// Clock time at which the soft cut was started (the previous frame's time).
    pub start_time: f64,
    /// Total duration of the soft cut in seconds.
    pub duration: f64,
}

/// Errors from [`Engine::load_preset`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The preset text could not be parsed.
    #[error("preset parse error: {0}")]
    Parse(#[from] lactea_preset::ParseError),
    /// Expression code that projectM refuses to load the preset for.
    #[error("{what}: {error}")]
    Compile {
        /// Which code section failed (e.g. "per-frame code").
        what: String,
        /// The expression compiler's error.
        error: lactea_eel::CompileError,
    },
    /// The GPU backend rejected the preset's resources.
    #[error("render error: {0}")]
    Render(#[from] lactea_render::RenderError),
    /// An operation on the loaded preset (e.g. [`Engine::import_custom_wave`]) was called with
    /// no preset loaded.
    #[error("no preset loaded")]
    NoPreset,
    /// The preset needs something the configured compatibility mode does not have (a `.milk2` double preset needs
    /// [`Compat::MilkDrop3`]).
    #[error("{0}")]
    Unsupported(String),
    /// A `.milk2` header value could not be read (a malformed number).
    #[error("double preset: {0}")]
    Milk2(#[from] lactea_preset::milk2::Milk2Error),
}

impl EngineError {
    pub(crate) fn compile(what: &str, error: lactea_eel::CompileError) -> Self {
        EngineError::Compile {
            what: what.to_string(),
            error,
        }
    }
}

/// How a warp / composite shader slot ended up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShaderOutcome {
    /// PSVERSION 0 (MilkDrop 1 preset): the fixed-function ("no shader") path.
    #[default]
    NotUsed,
    /// The preset's own shader, translated.
    User,
    /// PSVERSION > 0 without shader text: projectM's default (no-shader warp / pass-through
    /// composite); MilkDrop 2 compat: the synthesized default shader.
    Default,
    /// The preset's shader could not be translated / compiled and was replaced
    /// (see [`PresetReport::shader_fallbacks`]).
    Fallback,
}

/// A shader that was replaced by a default one.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ShaderFallback {
    /// Which shader pass was replaced.
    pub pass: PassKind,
    /// Why the preset's shader was rejected.
    pub reason: String,
    /// Description of the replacement.
    pub fallback: String,
}

/// What [`Engine::load_preset`] found.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct PresetReport {
    /// Name passed to [`Engine::load_preset`].
    pub name: String,
    /// `MILKDROP_PRESET_VERSION` of the parsed preset.
    pub preset_version: i32,
    /// The compatibility mode the preset was loaded under: [`EngineConfig::compat`], or with
    /// [`EngineConfig::auto_compat`] the one detected for this preset.
    pub compat: Compat,
    /// How the warp shader slot ended up.
    pub warp: ShaderOutcome,
    /// How the composite shader slot ended up.
    pub comp: ShaderOutcome,
    /// Non-fatal problems found while loading.
    pub warnings: Vec<String>,
    /// Shaders that were replaced by a default one.
    pub shader_fallbacks: Vec<ShaderFallback>,
    /// Texture names shaders reference that the provider could not supply.
    pub unresolved_textures: Vec<String>,
    /// `(name the shader uses, name loaded from the provider)`; differs for `randNN` samplers.
    pub resolved_textures: Vec<(String, String)>,
    /// `Compat::MilkDrop3`: the `SpriteName` of the preset's sprite when its image was loaded (so the sprite is drawn);
    /// a sprite that could not be loaded is a warning instead.
    pub sprite: Option<String>,
}
