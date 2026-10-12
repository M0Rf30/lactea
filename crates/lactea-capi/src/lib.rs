//! C ABI for the lactea MilkDrop engine.
//!
//! Self-contained mode: `lactea_create` makes its own wgpu instance/device (Vulkan, Metal, DX12
//! or GL, whichever wgpu picks) and an internal `Rgba8Unorm` render target. The host feeds PCM and
//! presets, calls `lactea_render` (GPU only) or `lactea_render_to_buffer` (render + read-back
//! into caller memory, tightly packed RGBA8, row 0 = top).
//!
//! Threading: an engine handle must be used from one thread at a time. Error strings
//! (`lactea_last_error`) are thread-local. Nothing here unwinds across the C boundary: panics
//! become [`LacteaStatus::Panic`].
//!
//! Preset changes: `lactea_load_preset` cuts at once, `lactea_load_preset_ex` blends over a
//! number of seconds (soft cut). The host learns that a preset should change through
//! `lactea_poll_switch_request` or a callback (`lactea_set_switch_requested_callback`); failed
//! loads reach `lactea_set_switch_failed_callback`.
//!
//! Vulkan interop: `lactea_create_vulkan` runs the engine on the host's `VkDevice` and
//! `lactea_render_to_vk_image` renders straight into a host `VkImage` (see `src/vulkan.rs`).

#![warn(missing_docs)]

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use lactea::{
    Compat, Engine, EngineConfig, FrameClock, Image, PresetControl, SwitchRequest, TextureProvider,
};

mod events;
mod vulkan;

use events::{CCallbacks, LacteaSwitchFailedFn, LacteaSwitchRequestedFn, SwitchEvents};

/// Result of every fallible call. `0` is success; failures are negative and the message is in
/// `lactea_last_error`.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LacteaStatus {
    /// Success.
    Ok = 0,
    /// NULL pointer, bad size, non-UTF-8 string, buffer too small.
    InvalidArgument = -1,
    /// No GPU adapter / device, or read-back failed.
    Gpu = -2,
    /// The preset did not parse or compile; the previous preset keeps running.
    Preset = -3,
    /// Internal panic caught at the boundary.
    Panic = -4,
}

/// Which implementation to follow where projectM, MilkDrop 2 and MilkDrop 3 differ (`docs/divergences.md`,
/// `docs/md3.md`).
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LacteaCompat {
    /// Follow projectM.
    Projectm = 0,
    /// Follow MilkDrop 2.
    Milkdrop2 = 1,
    /// Follow MilkDrop 3.
    Milkdrop3 = 2,
}

/// Construction parameters. Fill with `lactea_config_default` and override fields.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LacteaConfig {
    /// Render width in pixels, `1..=LACTEA_MAX_DIMENSION`.
    pub width: u32,
    /// Render height in pixels, `1..=LACTEA_MAX_DIMENSION`.
    pub height: u32,
    /// Warp mesh cells in x (even).
    pub mesh_x: u32,
    /// Warp mesh cells in y (even).
    pub mesh_y: u32,
    /// Seeds every random stream: same seed + same inputs = same frames.
    pub seed: u64,
    /// Which implementation to follow where projectM and MilkDrop 2 differ.
    pub compat: LacteaCompat,
    /// Sample rate of the PCM given to `lactea_push_pcm_f32`.
    pub sample_rate: u32,
    /// Frame rate presets see as `fps`.
    pub fps: u32,
    /// Seconds a preset is meant to run (`progress = elapsed / duration`).
    pub preset_duration: f64,
    /// MilkDrop 3 `AutoHardCutMode`: 0 = off, 1..=6 beat-driven instant preset changes (reported as
    /// hard-cut requests, see `lactea_poll_switch_request`), 7 = a random effect injected into the
    /// composite shader without a preset change; larger values count as 7. Acts only with
    /// `LACTEA_COMPAT_MILKDROP3` (stored and ignored otherwise); `lactea_set_auto_hard_cut_mode`
    /// changes it at runtime.
    pub auto_hard_cut_mode: u8,
}

/// Largest accepted render width or height, in pixels. Larger requests would exceed what GPUs
/// can allocate and are rejected with `LACTEA_STATUS_INVALID_ARGUMENT`.
pub const LACTEA_MAX_DIMENSION: u32 = 16384;
const MAX_DIMENSION: u32 = LACTEA_MAX_DIMENSION;

/// MilkDrop 3's default `TimeBetweenPresetsRand` in seconds (`LacteaSwitchSettings::preset_duration_rand`).
const MD3_TIME_BETWEEN_PRESETS_RAND: f64 = 10.0;

/// The engine's own render target (self-contained mode; absent for engines made by
/// `lactea_create_vulkan`, which render into host images).
struct InternalTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// Opaque engine handle.
pub struct LacteaEngine {
    engine: Engine,
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: Option<InternalTarget>,
    width: u32,
    height: u32,
    /// projectM's preset-switching settings and timer (`lactea::PresetControl`).
    control: PresetControl,
    /// A switch request raised by a rendered frame, until `lactea_poll_switch_request` takes it.
    pending_switch: Option<SwitchRequest>,
    /// Host notifications (`lactea_set_switch_requested_callback` and friends).
    events: CCallbacks,
    /// `control`'s soft-cut duration while a `lactea_load_preset_ex` soft cut overrides it; put back
    /// when that soft cut ends.
    restore_soft_cut: Option<f64>,
    /// Set for engines created by `lactea_create_vulkan`.
    vulkan: Option<vulkan::VulkanState>,
    /// Read-back staging buffer for `lactea_render_to_buffer`, sized for the current target.
    readback: Option<wgpu::Buffer>,
    /// MilkDrop 3 `TimeBetweenPresetsRand` as the host last set it (`lactea_set_switch_settings`);
    /// kept here because `control` only holds it under `LACTEA_COMPAT_MILKDROP3`.
    md3_time_rand: f64,
}

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(msg: impl std::fmt::Display) {
    let s = msg.to_string().replace('\0', " ");
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::new(s).unwrap_or_default());
}

fn fail(status: LacteaStatus, msg: impl std::fmt::Display) -> LacteaStatus {
    set_error(msg);
    status
}

/// Run `f`, turning panics into [`LacteaStatus::Panic`].
fn guard(f: impl FnOnce() -> LacteaStatus) -> LacteaStatus {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|p| {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        fail(LacteaStatus::Panic, format!("internal panic: {msg}"))
    })
}

fn create_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lactea target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

fn init_gpu() -> Result<(wgpu::Device, wgpu::Queue), String> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|e| format!("no wgpu adapter: {e}"))?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .map_err(|e| format!("request_device: {e}"))
}

// ---------------------------------------------------------------------------------------------
// textures
// ---------------------------------------------------------------------------------------------

const TEXTURE_EXTENSIONS: [&str; 7] = ["jpg", "jpeg", "dds", "png", "tga", "bmp", "dib"];

/// Directory-backed [`TextureProvider`]: recursive scan, keyed by lowercase base name, random pool
/// sorted by (name, path) (same rules as the CLI and projectM's `FileScanner`).
struct DirTextures {
    files: Vec<(String, PathBuf)>,
}

impl DirTextures {
    fn scan(root: &Path) -> Self {
        let mut files = Vec::new();
        scan_dir(root, &mut files, 0);
        files.sort();
        Self { files }
    }
}

fn scan_dir(dir: &Path, out: &mut Vec<(String, PathBuf)>, depth: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 8 {
                scan_dir(&p, out, depth + 1);
            }
            continue;
        }
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase);
        if let (Some(ext), Some(stem)) = (ext, p.file_stem().and_then(|s| s.to_str()))
            && TEXTURE_EXTENSIONS.contains(&ext.as_str())
        {
            out.push((stem.to_ascii_lowercase(), p));
        }
    }
}

fn decode_file(path: &Path) -> Option<Image> {
    let bytes = std::fs::read(path).ok()?;
    if lactea_jpeg::is_jpeg(&bytes)
        && let Ok(img) = lactea_jpeg::decode_rgba(&bytes)
    {
        return Some(Image {
            width: img.width,
            height: img.height,
            rgba8: img.rgba,
        });
    }
    let rgba = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(Image {
        width,
        height,
        rgba8: rgba.into_raw(),
    })
}

impl TextureProvider for DirTextures {
    fn load(&mut self, name: &str) -> Option<Image> {
        let lower = name.to_ascii_lowercase();
        self.files
            .iter()
            .filter(|(n, _)| *n == lower)
            .find_map(|(_, path)| decode_file(path))
    }

    fn random_pool(&self) -> Vec<String> {
        self.files.iter().map(|(n, _)| n.clone()).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// C API
// ---------------------------------------------------------------------------------------------

/// Message of the last failed call on this thread (empty if none). The pointer stays valid until
/// the next lactea call on the same thread; never NULL, never free it.
#[unsafe(no_mangle)]
pub extern "C" fn lactea_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

/// Library version, e.g. `"0.1.0"`. Static string.
#[unsafe(no_mangle)]
pub extern "C" fn lactea_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// Fill `out` with defaults (640x360, 48x36 mesh, seed 1, projectM, 44100 Hz, 60 fps).
///
/// # Safety
/// `out` must be NULL or point to writable memory for one `LacteaConfig`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_config_default(out: *mut LacteaConfig) {
    if out.is_null() {
        return;
    }
    let d = EngineConfig::default();
    unsafe {
        out.write(LacteaConfig {
            width: d.width,
            height: d.height,
            mesh_x: d.mesh[0],
            mesh_y: d.mesh[1],
            seed: d.seed,
            compat: LacteaCompat::Projectm,
            sample_rate: d.sample_rate,
            fps: d.fps,
            preset_duration: d.preset_duration,
            auto_hard_cut_mode: 0,
        });
    }
}

/// Create an engine with its own GPU device. Returns NULL on failure (see `lactea_last_error`).
/// `config` may be NULL for defaults.
///
/// # Safety
/// `config` must be NULL or point to a valid `LacteaConfig`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_create(config: *const LacteaConfig) -> *mut LacteaEngine {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(mut cfg) = (unsafe { engine_config(config) }) else {
            return std::ptr::null_mut();
        };
        cfg.output_format = wgpu::TextureFormat::Rgba8Unorm;
        let (device, queue) = match init_gpu() {
            Ok(x) => x,
            Err(e) => {
                set_error(e);
                return std::ptr::null_mut();
            }
        };
        let (texture, view) = create_target(&device, cfg.width, cfg.height);
        build_engine(
            cfg,
            device,
            queue,
            Some(InternalTarget { texture, view }),
            None,
        )
    }));
    result.unwrap_or_else(|_| {
        set_error("internal panic while creating the engine");
        std::ptr::null_mut()
    })
}

/// `config` (NULL = defaults) as an [`EngineConfig`]; sets the error message when it is invalid.
///
/// # Safety
/// `config` NULL or valid.
unsafe fn engine_config(config: *const LacteaConfig) -> Option<EngineConfig> {
    let mut cfg = EngineConfig::default();
    if let Some(c) = unsafe { config.as_ref() } {
        if c.width == 0 || c.height == 0 || c.mesh_x == 0 || c.mesh_y == 0 || c.fps == 0 {
            set_error("width, height, mesh and fps must be non-zero");
            return None;
        }
        if c.width > MAX_DIMENSION || c.height > MAX_DIMENSION {
            set_error(format!("width and height must be at most {MAX_DIMENSION}"));
            return None;
        }
        cfg.width = c.width;
        cfg.height = c.height;
        cfg.mesh = [c.mesh_x, c.mesh_y];
        cfg.seed = c.seed;
        cfg.compat = match c.compat {
            LacteaCompat::Milkdrop2 => Compat::MilkDrop2,
            LacteaCompat::Milkdrop3 => Compat::MilkDrop3,
            LacteaCompat::Projectm => Compat::ProjectM,
        };
        cfg.sample_rate = c.sample_rate;
        cfg.fps = c.fps;
        cfg.preset_duration = c.preset_duration;
        cfg.auto_hard_cut_mode = c.auto_hard_cut_mode;
    }
    Some(cfg)
}

/// The engine handle for `cfg` on `device`.
fn build_engine(
    cfg: EngineConfig,
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: Option<InternalTarget>,
    vulkan: Option<vulkan::VulkanState>,
) -> *mut LacteaEngine {
    let engine = Engine::new(&device, &queue, cfg.clone());
    let mut control = PresetControl::new(cfg.seed);
    control.set_preset_duration(cfg.preset_duration);
    // MD3 display time: BlendTimeAuto + TimeBetweenPresets + a random part up to TimeBetweenPresetsRand.
    if cfg.compat.rules().md3_preset_time_rand {
        control.set_md3_time_rand(Some(MD3_TIME_BETWEEN_PRESETS_RAND));
    }
    Box::into_raw(Box::new(LacteaEngine {
        engine,
        device,
        queue,
        target,
        width: cfg.width,
        height: cfg.height,
        control,
        pending_switch: None,
        events: CCallbacks::default(),
        restore_soft_cut: None,
        vulkan,
        readback: None,
        md3_time_rand: MD3_TIME_BETWEEN_PRESETS_RAND,
    }))
}

/// Destroy an engine. NULL is a no-op.
///
/// # Safety
/// `engine` must come from `lactea_create` and not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_destroy(engine: *mut LacteaEngine) {
    if !engine.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(engine) })));
    }
}

/// Load a `.milk` preset from `len` bytes (hard cut). `name` is a NUL-terminated label (may be
/// NULL). On error the previous preset stays active and the failure callback
/// (`lactea_set_switch_failed_callback`) runs.
///
/// # Safety
/// `engine` valid; `bytes` points to `len` readable bytes; `name` NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_load_preset(
    engine: *mut LacteaEngine,
    bytes: *const u8,
    len: usize,
    name: *const c_char,
) -> LacteaStatus {
    unsafe { lactea_load_preset_ex(engine, bytes, len, name, 0.0) }
}

/// `lactea_load_preset` with a transition: `transition_seconds == 0` replaces the preset at once
/// (hard cut), `> 0` is a soft cut: the old and the new preset both keep running and blend over that
/// many seconds (projectM's default is 3; the current `soft_cut_duration` setting is in
/// `LacteaSwitchSettings`). Negative or non-finite values are `InvalidArgument`. The next preset's
/// display timer starts when the blend ends. Loading another preset during a soft cut finishes the
/// first blend immediately.
///
/// # Safety
/// `engine` valid; `bytes` points to `len` readable bytes; `name` NULL or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_load_preset_ex(
    engine: *mut LacteaEngine,
    bytes: *const u8,
    len: usize,
    name: *const c_char,
    transition_seconds: f64,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        if bytes.is_null() && len != 0 {
            return fail(LacteaStatus::InvalidArgument, "bytes is NULL");
        }
        if !transition_seconds.is_finite() || transition_seconds < 0.0 {
            return fail(
                LacteaStatus::InvalidArgument,
                "transition_seconds must be finite and >= 0",
            );
        }
        let data = if len == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(bytes, len) }
        };
        let name = if name.is_null() {
            "preset".to_string()
        } else {
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        };
        let soft = (transition_seconds > 0.0).then_some(transition_seconds);
        let transition = soft.map_or(lactea::Transition::Cut, |seconds| {
            lactea::Transition::Soft { seconds }
        });
        match e.engine.load_preset(data, &name, transition) {
            Ok(_) => {
                preset_started(e, soft);
                LacteaStatus::Ok
            }
            Err(err) => load_failed(e, &name, err),
        }
    })
}

/// A preset was loaded (`soft`: blending in over that many seconds): restart the switch timer.
fn preset_started(e: &mut LacteaEngine, soft: Option<f64>) {
    let now = e.engine.last_frame_time();
    match soft {
        Some(seconds) => {
            // PresetControl ends the smoothing state after its own soft-cut setting; borrow it for
            // this blend and give the setting back when the blend is over.
            e.restore_soft_cut
                .get_or_insert_with(|| e.control.soft_cut_duration());
            e.control.set_soft_cut_duration(seconds);
            e.control.start_smoothing(now);
        }
        None => {
            if let Some(d) = e.restore_soft_cut.take() {
                e.control.set_soft_cut_duration(d);
            }
            e.control.start_preset(now);
        }
    }
    e.pending_switch = None;
}

/// Loading `name` failed: record the message for `lactea_last_error` and tell the host.
fn load_failed(e: &mut LacteaEngine, name: &str, err: impl std::fmt::Display) -> LacteaStatus {
    let msg = err.to_string();
    let status = fail(LacteaStatus::Preset, &msg);
    e.events.switch_failed(name, &msg);
    status
}

/// Register (or, with NULL, remove) the callback for preset-switch requests; `user_data` is passed
/// back unchanged. See `LacteaSwitchRequestedFn`.
///
/// # Safety
/// `engine` valid; `callback` must stay callable until replaced or the engine is destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_switch_requested_callback(
    engine: *mut LacteaEngine,
    callback: LacteaSwitchRequestedFn,
    user_data: *mut c_void,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.events.requested = callback;
        e.events.requested_user = user_data;
        LacteaStatus::Ok
    })
}

/// Register (or, with NULL, remove) the callback for failed preset loads. See
/// `LacteaSwitchFailedFn`.
///
/// # Safety
/// `engine` valid; `callback` must stay callable until replaced or the engine is destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_switch_failed_callback(
    engine: *mut LacteaEngine,
    callback: LacteaSwitchFailedFn,
    user_data: *mut c_void,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.events.failed = callback;
        e.events.failed_user = user_data;
        LacteaStatus::Ok
    })
}

/// Use the images under `path` (recursive; jpg/jpeg/png/tga/bmp/dib) for preset `sampler_*`
/// textures. Takes effect for presets loaded afterwards.
///
/// # Safety
/// `engine` valid; `path` NUL-terminated UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_texture_dir(
    engine: *mut LacteaEngine,
    path: *const c_char,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        if path.is_null() {
            return fail(LacteaStatus::InvalidArgument, "path is NULL");
        }
        let Ok(p) = unsafe { CStr::from_ptr(path) }.to_str() else {
            return fail(LacteaStatus::InvalidArgument, "path is not UTF-8");
        };
        e.engine
            .set_texture_provider(Box::new(DirTextures::scan(Path::new(p))));
        LacteaStatus::Ok
    })
}

/// Push `frames` sample frames of interleaved float PCM (`-1..1`) with `channels` channels (only
/// the first two are analysed).
///
/// # Safety
/// `engine` valid; `samples` points to `frames * channels` floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_push_pcm_f32(
    engine: *mut LacteaEngine,
    samples: *const f32,
    frames: usize,
    channels: u32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        if channels == 0 || (samples.is_null() && frames != 0) {
            return fail(
                LacteaStatus::InvalidArgument,
                "samples is NULL or channels is 0",
            );
        }
        if frames == 0 {
            return LacteaStatus::Ok;
        }
        let Some(n) = frames.checked_mul(channels as usize) else {
            return fail(LacteaStatus::InvalidArgument, "frames * channels overflows");
        };
        let pcm = unsafe { std::slice::from_raw_parts(samples, n) };
        e.engine.push_pcm_f32(pcm, channels as usize);
        LacteaStatus::Ok
    })
}

/// Change the render size (clears the feedback image; the loaded preset keeps running).
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_resize(
    engine: *mut LacteaEngine,
    width: u32,
    height: u32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return fail(
                LacteaStatus::InvalidArgument,
                format!("size must be 1..={MAX_DIMENSION} in both dimensions"),
            );
        }
        e.engine.resize(width, height);
        if e.target.is_some() {
            let (texture, view) = create_target(&e.device, width, height);
            e.target = Some(InternalTarget { texture, view });
        }
        e.width = width;
        e.height = height;
        LacteaStatus::Ok
    })
}

/// Record the frame into `view` and advance the switch timer. Returns the encoder and the switch
/// request the frame raised (already stored for `lactea_poll_switch_request`); hand the request to
/// [`notify_switch`] once the work is submitted.
fn encode_frame(
    e: &mut LacteaEngine,
    view: &wgpu::TextureView,
    time: f64,
    frame: u32,
) -> (wgpu::CommandEncoder, Option<SwitchRequest>) {
    let mut enc = e.device.create_command_encoder(&Default::default());
    e.engine.render(&mut enc, view, FrameClock { time, frame });
    let request = e.control.update_with_auto_cuts(
        time,
        e.engine.audio_frame().vol,
        e.engine.take_auto_cuts(),
    );
    if !e.control.is_smoothing()
        && let Some(d) = e.restore_soft_cut.take()
    {
        e.control.set_soft_cut_duration(d);
    }
    if request.is_some() {
        e.pending_switch = request;
    }
    (enc, request)
}

fn notify_switch(e: &mut LacteaEngine, request: Option<SwitchRequest>) {
    if let Some(r) = request {
        e.events.switch_requested(r.hard_cut);
    }
}

fn render_inner(
    e: &mut LacteaEngine,
    time: f64,
    frame: u32,
    out: Option<&mut [u8]>,
) -> LacteaStatus {
    let Some(t) = e.target.as_ref() else {
        return fail(
            LacteaStatus::InvalidArgument,
            "this engine has no internal target (created with lactea_create_vulkan): use lactea_render_to_vk_image",
        );
    };
    let (view, target) = (t.view.clone(), t.texture.clone());
    let (mut enc, request) = encode_frame(e, &view, time, frame);
    let Some(out) = out else {
        e.queue.submit(Some(enc.finish()));
        notify_switch(e, request);
        return LacteaStatus::Ok;
    };

    let (w, h) = (e.width, e.height);
    let bpr = (w * 4).div_ceil(256) * 256;
    let size = u64::from(bpr) * u64::from(h);
    // Reused across frames; recreated only when the render size changes.
    let buf = match &e.readback {
        Some(b) if b.size() == size => b.clone(),
        _ => {
            let b = e.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lactea readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            e.readback = Some(b.clone());
            b
        }
    };
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    e.queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    if let Err(err) = e.device.poll(wgpu::PollType::wait_indefinitely()) {
        return fail(LacteaStatus::Gpu, format!("poll: {err:?}"));
    }
    match rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return fail(LacteaStatus::Gpu, format!("map: {err}")),
        Err(err) => return fail(LacteaStatus::Gpu, format!("map: {err}")),
    }
    let data = match slice.get_mapped_range() {
        Ok(d) => d,
        Err(err) => return fail(LacteaStatus::Gpu, format!("mapped range: {err}")),
    };
    let row = (w * 4) as usize;
    for y in 0..h as usize {
        out[y * row..(y + 1) * row]
            .copy_from_slice(&data[y * bpr as usize..y * bpr as usize + row]);
    }
    drop(data);
    buf.unmap();
    LacteaStatus::Ok
}

/// Render one frame into the internal target (GPU work is submitted, no read-back).
/// `time` is seconds since the start, `frame` the frame counter; the audio analysis advances by
/// the time difference to the previous call.
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_render(
    engine: *mut LacteaEngine,
    time: f64,
    frame: u32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        render_inner(e, time, frame, None)
    })
}

/// Render one frame and copy it to `out` as tightly packed RGBA8 (`width * height * 4` bytes,
/// row 0 = top). `out_len` must be at least that (`lactea_width` / `lactea_height`).
///
/// # Safety
/// `engine` valid; `out` points to `out_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_render_to_buffer(
    engine: *mut LacteaEngine,
    time: f64,
    frame: u32,
    out: *mut u8,
    out_len: usize,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        let need = e.width as usize * e.height as usize * 4;
        if out.is_null() || out_len < need {
            return fail(
                LacteaStatus::InvalidArgument,
                format!("output buffer must hold {need} bytes (got {out_len})"),
            );
        }
        let out = unsafe { std::slice::from_raw_parts_mut(out, need) };
        render_inner(e, time, frame, Some(out))
    })
}

/// Current render width in pixels (0 for a NULL engine).
///
/// # Safety
/// `engine` NULL or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_width(engine: *const LacteaEngine) -> u32 {
    unsafe { engine.as_ref() }.map_or(0, |e| e.width)
}

/// Current render height in pixels (0 for a NULL engine).
///
/// # Safety
/// `engine` NULL or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_height(engine: *const LacteaEngine) -> u32 {
    unsafe { engine.as_ref() }.map_or(0, |e| e.height)
}

// ---------------------------------------------------------------------------------------------
// projectM API equivalents
// ---------------------------------------------------------------------------------------------

/// Push `frames` sample frames of interleaved signed 16-bit PCM (`projectm_pcm_add_int16`).
///
/// # Safety
/// `engine` valid; `samples` points to `frames * channels` values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_push_pcm_i16(
    engine: *mut LacteaEngine,
    samples: *const i16,
    frames: usize,
    channels: u32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        if channels == 0 || (samples.is_null() && frames != 0) {
            return fail(
                LacteaStatus::InvalidArgument,
                "samples is NULL or channels is 0",
            );
        }
        let Some(n) = frames.checked_mul(channels as usize) else {
            return fail(LacteaStatus::InvalidArgument, "frames * channels overflows");
        };
        if n != 0 {
            e.engine.push_pcm_i16(
                unsafe { std::slice::from_raw_parts(samples, n) },
                channels as usize,
            );
        }
        LacteaStatus::Ok
    })
}

/// Push `frames` sample frames of interleaved unsigned 8-bit PCM, silence = 128
/// (`projectm_pcm_add_uint8`).
///
/// # Safety
/// `engine` valid; `samples` points to `frames * channels` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_push_pcm_u8(
    engine: *mut LacteaEngine,
    samples: *const u8,
    frames: usize,
    channels: u32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        if channels == 0 || (samples.is_null() && frames != 0) {
            return fail(
                LacteaStatus::InvalidArgument,
                "samples is NULL or channels is 0",
            );
        }
        let Some(n) = frames.checked_mul(channels as usize) else {
            return fail(LacteaStatus::InvalidArgument, "frames * channels overflows");
        };
        if n != 0 {
            e.engine.push_pcm_u8(
                unsafe { std::slice::from_raw_parts(samples, n) },
                channels as usize,
            );
        }
        LacteaStatus::Ok
    })
}

/// Sample frames per channel the engine keeps (`projectm_pcm_get_max_samples`).
#[unsafe(no_mangle)]
pub extern "C" fn lactea_pcm_max_samples() -> usize {
    Engine::pcm_max_samples()
}

/// Library version as numbers (`projectm_get_version_components`); any pointer may be NULL.
///
/// # Safety
/// Each pointer is NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_version_components(
    major: *mut i32,
    minor: *mut i32,
    patch: *mut i32,
) {
    let part = |s: &str| s.parse::<i32>().unwrap_or(0);
    unsafe {
        if let Some(m) = major.as_mut() {
            *m = part(env!("CARGO_PKG_VERSION_MAJOR"));
        }
        if let Some(m) = minor.as_mut() {
            *m = part(env!("CARGO_PKG_VERSION_MINOR"));
        }
        if let Some(p) = patch.as_mut() {
            *p = part(env!("CARGO_PKG_VERSION_PATCH"));
        }
    }
}

/// Time of the last rendered frame in seconds (`projectm_get_last_frame_time`; 0 for NULL).
///
/// # Safety
/// `engine` NULL or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_last_frame_time(engine: *const LacteaEngine) -> f64 {
    unsafe { engine.as_ref() }.map_or(0.0, |e| e.engine.last_frame_time())
}

/// Load the built-in idle preset (a hard cut; see `lactea::IDLE_PRESET`).
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_load_idle_preset(engine: *mut LacteaEngine) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        match e.engine.load_idle_preset() {
            Ok(_) => {
                preset_started(e, None);
                LacteaStatus::Ok
            }
            Err(err) => load_failed(e, lactea::IDLE_PRESET_NAME, err),
        }
    })
}

/// Frame rate the presets see as `fps` (`projectm_set_fps`).
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_fps(engine: *mut LacteaEngine, fps: u32) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.engine.set_fps(fps);
        LacteaStatus::Ok
    })
}

/// `projectm_get_fps` (0 for NULL).
///
/// # Safety
/// `engine` NULL or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_get_fps(engine: *const LacteaEngine) -> u32 {
    unsafe { engine.as_ref() }.map_or(0, |e| e.engine.fps())
}

/// MilkDrop 3 mouse (`LACTEA_COMPAT_MILKDROP3`; ignored by the other modes): the cursor as fractions of the
/// visualizer's size, `(x, y) = (px / width, py / height)` with the origin at the **top left**, and the left button.
/// `inside == false` says the cursor is outside the window (`x` and `y` are then ignored). Call it whenever the
/// state changes; the engine reads the latest values once per rendered frame and derives the one-frame "released"
/// edge of `mouse.w` itself, so a press and release between two rendered frames is not seen.
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_mouse(
    engine: *mut LacteaEngine,
    x: f32,
    y: f32,
    inside: bool,
    left_down: bool,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.engine.set_mouse(lactea::MouseState {
            position: inside.then_some([x, y]),
            left_down,
        });
        LacteaStatus::Ok
    })
}

/// MilkDrop 3 `VolumeSensitivity`: linear gain (default 1) on the 8-bit quantised audio input. Only
/// `LACTEA_COMPAT_MILKDROP3` uses it.
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_volume_sensitivity(
    engine: *mut LacteaEngine,
    sensitivity: f32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.engine.set_volume_sensitivity(sensitivity);
        LacteaStatus::Ok
    })
}

/// Warp-mesh resolution in cells (`projectm_set_mesh_size`): odd sizes round up to even, each axis
/// is limited to 8..=300. Read the value in use back with `lactea_get_mesh_size`.
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_mesh_size(
    engine: *mut LacteaEngine,
    width: u32,
    height: u32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.engine.set_mesh_size(width, height);
        LacteaStatus::Ok
    })
}

/// `projectm_get_mesh_size`; either pointer may be NULL.
///
/// # Safety
/// `engine` NULL or valid; the pointers NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_get_mesh_size(
    engine: *const LacteaEngine,
    width: *mut u32,
    height: *mut u32,
) {
    let Some(e) = (unsafe { engine.as_ref() }) else {
        return;
    };
    let m = e.engine.mesh_size();
    unsafe {
        if let Some(w) = width.as_mut() {
            *w = m[0];
        }
        if let Some(h) = height.as_mut() {
            *h = m[1];
        }
    }
}

/// Sub-texel offset added to warp UVs (`projectm_set_texel_offset`).
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_texel_offset(
    engine: *mut LacteaEngine,
    x: f32,
    y: f32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.engine.set_texel_offset([x, y]);
        LacteaStatus::Ok
    })
}

/// `projectm_get_texel_offset`; either pointer may be NULL.
///
/// # Safety
/// `engine` NULL or valid; the pointers NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_get_texel_offset(
    engine: *const LacteaEngine,
    x: *mut f32,
    y: *mut f32,
) {
    let Some(e) = (unsafe { engine.as_ref() }) else {
        return;
    };
    let o = e.engine.texel_offset();
    unsafe {
        if let Some(px) = x.as_mut() {
            *px = o[0];
        }
        if let Some(py) = y.as_mut() {
            *py = o[1];
        }
    }
}

/// MilkDrop 3 `AutoHardCutMode` (`LacteaConfig::auto_hard_cut_mode`): 0 = off, 1..=6 = beat-driven
/// instant preset changes (they raise a hard-cut request, see `lactea_poll_switch_request`), 7 = a
/// random effect is injected into the composite shader and the preset stays; larger values are
/// clamped to 7. Takes effect on the next frame. Only acts with `LACTEA_COMPAT_MILKDROP3`: with the
/// other compat modes the mode is stored and read back but never does anything.
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_auto_hard_cut_mode(
    engine: *mut LacteaEngine,
    mode: u8,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.engine.set_auto_hard_cut_mode(mode);
        LacteaStatus::Ok
    })
}

/// The auto hard cut mode in use (`lactea_set_auto_hard_cut_mode`, after the clamp to 7), whether or
/// not the compat mode lets it act.
///
/// # Safety
/// `engine` valid; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_get_auto_hard_cut_mode(
    engine: *const LacteaEngine,
    out: *mut u8,
) -> LacteaStatus {
    guard(|| {
        let (Some(e), false) = (unsafe { engine.as_ref() }, out.is_null()) else {
            return fail(LacteaStatus::InvalidArgument, "engine or out is NULL");
        };
        unsafe { out.write(e.engine.auto_hard_cut_mode()) };
        LacteaStatus::Ok
    })
}

/// projectM's preset-switching settings in one struct (`projectm_set_*` / `projectm_get_*` of beat
/// sensitivity, aspect correction, easter egg, preset lock, preset / soft-cut / hard-cut timing).
/// Defaults (what a fresh engine reports): 1.0, true, 1.0, false, `config.preset_duration`, 3.0, false,
/// 20.0, 2.0, 10.0. `beat_sensitivity` is clamped to 0..2 and, like `aspect_correction`, stored only
/// (projectM does not use either anywhere else); reading `hard_cut_duration` back truncates it to
/// whole seconds like projectM.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LacteaSwitchSettings {
    /// Stored only; clamped to 0..=2.
    pub beat_sensitivity: f32,
    /// Stored only (projectM keeps it without using it).
    pub aspect_correction: bool,
    /// Sigma of the random preset display time; below 0.001 the duration is exact.
    pub easter_egg: f32,
    /// `true` suppresses `lactea_poll_switch_request`.
    pub preset_locked: bool,
    /// Display time in seconds; also the `progress` preset variable's denominator.
    pub preset_duration: f64,
    /// Seconds a soft cut blends the old and the new preset.
    pub soft_cut_duration: f64,
    /// Whether a loud beat (see `hard_cut_sensitivity`) may request a hard cut.
    pub hard_cut_enabled: bool,
    /// Minimum seconds a preset runs before a beat may cut it.
    pub hard_cut_duration: f64,
    /// Beat loudness (as a multiple of the running average) that triggers a hard cut.
    pub hard_cut_sensitivity: f32,
    /// MilkDrop 3 `TimeBetweenPresetsRand` in seconds. With `LACTEA_COMPAT_MILKDROP3` the display
    /// time of a preset is `soft_cut_duration` (MD3's `BlendTimeAuto`) + `preset_duration`
    /// (`TimeBetweenPresets`) + a seeded random 0..`preset_duration_rand` seconds in 1 ms steps, and
    /// `easter_egg` is ignored; applies to presets started afterwards. Negative counts as 0. With
    /// the other compat modes it is stored and read back but inert. Default 10.0.
    pub preset_duration_rand: f64,
}

/// Read the preset-switching settings (see `LacteaSwitchSettings`).
///
/// # Safety
/// `engine` valid; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_get_switch_settings(
    engine: *const LacteaEngine,
    out: *mut LacteaSwitchSettings,
) -> LacteaStatus {
    guard(|| {
        let (Some(e), false) = (unsafe { engine.as_ref() }, out.is_null()) else {
            return fail(LacteaStatus::InvalidArgument, "engine or out is NULL");
        };
        let c = &e.control;
        unsafe {
            out.write(LacteaSwitchSettings {
                beat_sensitivity: c.beat_sensitivity(),
                aspect_correction: c.aspect_correction(),
                easter_egg: c.easter_egg(),
                preset_locked: c.preset_locked(),
                preset_duration: c.preset_duration(),
                soft_cut_duration: e.restore_soft_cut.unwrap_or_else(|| c.soft_cut_duration()),
                hard_cut_enabled: c.hard_cut_enabled(),
                hard_cut_duration: c.hard_cut_duration(),
                hard_cut_sensitivity: c.hard_cut_sensitivity(),
                preset_duration_rand: e.md3_time_rand,
            });
        }
        LacteaStatus::Ok
    })
}

/// Apply preset-switching settings (see `LacteaSwitchSettings`). Typical use: read, change a field,
/// write back. A write that does not change `preset_locked` leaves the pending request state alone.
///
/// # Safety
/// `engine` valid; `settings` readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_switch_settings(
    engine: *mut LacteaEngine,
    settings: *const LacteaSwitchSettings,
) -> LacteaStatus {
    guard(|| {
        let (Some(e), Some(s)) = (unsafe { engine.as_mut() }, unsafe { settings.as_ref() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine or settings is NULL");
        };
        e.restore_soft_cut = None; // an explicit setting wins over a running soft cut's override
        let c = &mut e.control;
        c.set_beat_sensitivity(s.beat_sensitivity);
        c.set_aspect_correction(s.aspect_correction);
        c.set_easter_egg(s.easter_egg);
        if c.preset_locked() != s.preset_locked {
            c.set_preset_locked(s.preset_locked);
        }
        c.set_preset_duration(s.preset_duration);
        c.set_soft_cut_duration(s.soft_cut_duration);
        c.set_hard_cut_enabled(s.hard_cut_enabled);
        c.set_hard_cut_duration(s.hard_cut_duration);
        c.set_hard_cut_sensitivity(s.hard_cut_sensitivity);
        e.md3_time_rand = s.preset_duration_rand.max(0.0);
        if e.engine.config().compat.rules().md3_preset_time_rand {
            c.set_md3_time_rand(Some(e.md3_time_rand));
        }
        e.engine.set_preset_duration(s.preset_duration);
        LacteaStatus::Ok
    })
}

/// Did the last rendered frames ask for a preset change (`projectm_preset_switch_requested_event`)?
/// Returns `true` once per request and stores `*is_hard_cut` (beat-driven cut vs display time up;
/// NULL is allowed). The host then loads the next preset itself. Requests only happen while a preset
/// loaded through this API is running, the preset is not locked, and (projectM beat hard cuts) hard
/// cuts are enabled. With `LACTEA_COMPAT_MILKDROP3` and a non-zero auto hard cut mode
/// (`LacteaConfig::auto_hard_cut_mode`, `lactea_set_auto_hard_cut_mode`) a beat-driven auto hard
/// cut also raises a request with `*is_hard_cut = true`, independent of the projectM hard-cut
/// settings: load the next preset with a cut. `preset_locked` suppresses it like any other request.
///
/// # Safety
/// `engine` valid; `is_hard_cut` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_poll_switch_request(
    engine: *mut LacteaEngine,
    is_hard_cut: *mut bool,
) -> bool {
    let Some(e) = (unsafe { engine.as_mut() }) else {
        return false;
    };
    match e.pending_switch.take() {
        Some(r) => {
            if let Some(h) = unsafe { is_hard_cut.as_mut() } {
                *h = r.hard_cut;
            }
            true
        }
        None => false,
    }
}

/// `projectm_touch`. A documented no-op, exactly like projectM 4.2 (whose touch functions are empty
/// stubs): hosts written against projectM link unchanged and get the same pixels. These symbols
/// (as well as `lactea_pcm_max_samples` and `lactea_version_components`) are part of the stable
/// ABI. `touch_type` is `projectm_touch_type` (0..=8); anything else is
/// `LACTEA_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_touch(
    engine: *mut LacteaEngine,
    x: f32,
    y: f32,
    pressure: i32,
    touch_type: i32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        let Some(kind) = lactea::TouchType::from_i32(touch_type) else {
            return fail(
                LacteaStatus::InvalidArgument,
                format!("touch type {touch_type} is not 0..=8"),
            );
        };
        e.engine.touch(x, y, pressure, kind);
        LacteaStatus::Ok
    })
}

/// `projectm_touch_drag` (no-op, see `lactea_touch`).
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_touch_drag(
    engine: *mut LacteaEngine,
    x: f32,
    y: f32,
    pressure: i32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.engine.touch_drag(x, y, pressure);
        LacteaStatus::Ok
    })
}

/// `projectm_touch_destroy` (no-op, see `lactea_touch`).
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_touch_destroy(
    engine: *mut LacteaEngine,
    x: f32,
    y: f32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.engine.touch_destroy(x, y);
        LacteaStatus::Ok
    })
}

/// `projectm_touch_destroy_all` (no-op, see `lactea_touch`).
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_touch_destroy_all(engine: *mut LacteaEngine) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        e.engine.touch_destroy_all();
        LacteaStatus::Ok
    })
}

/// Copy a pipeline stage of the last rendered frame (`projectm_write_debug_image_on_next_frame`,
/// but pull-style: the library writes no files). `stage`: 0 final, 1 main, 2 warped, 3..=5 blur1..3
/// (blur stages hold data only when the preset samples blur). Stores the image size in `*width` /
/// `*height` (each may be NULL) and, when `out` is not NULL, copies `width * height * 4` RGBA8 bytes
/// (row 0 = top) into it; `out_len` must be large enough, so call once with `out = NULL` to size it.
///
/// # Safety
/// `engine` valid; `out` NULL or `out_len` writable bytes; `width`/`height` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_read_stage(
    engine: *mut LacteaEngine,
    stage: u32,
    out: *mut u8,
    out_len: usize,
    width: *mut u32,
    height: *mut u32,
) -> LacteaStatus {
    guard(|| {
        let Some(e) = (unsafe { engine.as_mut() }) else {
            return fail(LacteaStatus::InvalidArgument, "engine is NULL");
        };
        // The documented ids 0..=5 are the positions in `Stage::ALL` (final, main, warped, blur1..3).
        let Some(&st) = lactea::Stage::ALL.get(stage as usize) else {
            return fail(
                LacteaStatus::InvalidArgument,
                format!("stage {stage} is not 0..=5"),
            );
        };
        let img = match e.engine.debug_image(st) {
            Ok(i) => i,
            Err(err) => return fail(LacteaStatus::Gpu, err),
        };
        if !out.is_null() {
            if out_len < img.rgba.len() {
                return fail(
                    LacteaStatus::InvalidArgument,
                    format!(
                        "output buffer must hold {} bytes (got {out_len})",
                        img.rgba.len()
                    ),
                );
            }
            unsafe { std::slice::from_raw_parts_mut(out, img.rgba.len()) }
                .copy_from_slice(&img.rgba);
        }
        if let Some(w) = unsafe { width.as_mut() } {
            *w = img.width;
        }
        if let Some(h) = unsafe { height.as_mut() } {
            *h = img.height;
        }
        LacteaStatus::Ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::{null, null_mut};

    fn engine() -> Option<*mut LacteaEngine> {
        engine_with(LacteaCompat::Projectm, 0)
    }

    fn engine_with(compat: LacteaCompat, auto_hard_cut_mode: u8) -> Option<*mut LacteaEngine> {
        let instance = wgpu::Instance::default();
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
        let mut cfg = std::mem::MaybeUninit::<LacteaConfig>::uninit();
        let e = unsafe {
            lactea_config_default(cfg.as_mut_ptr());
            let mut cfg = cfg.assume_init();
            (cfg.width, cfg.height, cfg.mesh_x, cfg.mesh_y) = (64, 36, 16, 12);
            (cfg.compat, cfg.auto_hard_cut_mode) = (compat, auto_hard_cut_mode);
            lactea_create(&cfg)
        };
        assert!(!e.is_null());
        Some(e)
    }

    #[test]
    fn version_and_pcm_limits() {
        let (mut a, mut b, mut c) = (-1, -1, -1);
        unsafe { lactea_version_components(&mut a, &mut b, &mut c) };
        let v = unsafe { CStr::from_ptr(lactea_version()) }
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(v, format!("{a}.{b}.{c}"));
        unsafe { lactea_version_components(null_mut(), null_mut(), null_mut()) };
        assert_eq!(lactea_pcm_max_samples(), 576);
    }

    #[test]
    fn projectm_parity_calls_round_trip() {
        let Some(e) = engine() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        unsafe {
            // PCM entry points validate their arguments
            let s16 = [1000i16; 64];
            assert_eq!(
                lactea_push_pcm_i16(e, s16.as_ptr(), 32, 2),
                LacteaStatus::Ok
            );
            assert_eq!(
                lactea_push_pcm_i16(e, null(), 3, 2),
                LacteaStatus::InvalidArgument
            );
            assert_eq!(
                lactea_push_pcm_i16(e, s16.as_ptr(), 3, 0),
                LacteaStatus::InvalidArgument
            );
            let s8 = [200u8; 32];
            assert_eq!(lactea_push_pcm_u8(e, s8.as_ptr(), 32, 1), LacteaStatus::Ok);
            assert_eq!(
                lactea_push_pcm_u8(null_mut(), s8.as_ptr(), 32, 1),
                LacteaStatus::InvalidArgument
            );

            // parameters
            assert_eq!(lactea_set_fps(e, 35), LacteaStatus::Ok);
            assert_eq!(lactea_get_fps(e), 35);
            assert_eq!(lactea_set_mesh_size(e, 33, 5), LacteaStatus::Ok);
            let (mut w, mut h) = (0, 0);
            lactea_get_mesh_size(e, &mut w, &mut h);
            assert_eq!((w, h), (34, 8));
            assert_eq!(lactea_set_texel_offset(e, 0.5, -0.25), LacteaStatus::Ok);
            let (mut x, mut y) = (0.0, 0.0);
            lactea_get_texel_offset(e, &mut x, &mut y);
            assert_eq!((x, y), (0.5, -0.25));
            assert_eq!(lactea_last_frame_time(e), 0.0);

            // switch settings: projectM's defaults, clamps and the truncated hard-cut getter
            let mut s = std::mem::MaybeUninit::<LacteaSwitchSettings>::uninit();
            assert_eq!(
                lactea_get_switch_settings(e, s.as_mut_ptr()),
                LacteaStatus::Ok
            );
            let mut s = s.assume_init();
            assert_eq!(
                (
                    s.beat_sensitivity,
                    s.aspect_correction,
                    s.easter_egg,
                    s.preset_locked
                ),
                (1.0, true, 1.0, false)
            );
            assert_eq!(
                (
                    s.soft_cut_duration,
                    s.hard_cut_enabled,
                    s.hard_cut_duration,
                    s.hard_cut_sensitivity
                ),
                (3.0, false, 20.0, 2.0)
            );
            s.beat_sensitivity = 9.0;
            s.hard_cut_duration = 12.75;
            s.aspect_correction = false;
            assert_eq!(lactea_set_switch_settings(e, &s), LacteaStatus::Ok);
            let mut back = std::mem::MaybeUninit::<LacteaSwitchSettings>::uninit();
            assert_eq!(
                lactea_get_switch_settings(e, back.as_mut_ptr()),
                LacteaStatus::Ok
            );
            let back = back.assume_init();
            assert_eq!(
                (
                    back.beat_sensitivity,
                    back.hard_cut_duration,
                    back.aspect_correction
                ),
                (2.0, 12.0, false)
            );
            assert_eq!(
                lactea_get_switch_settings(e, null_mut()),
                LacteaStatus::InvalidArgument
            );

            lactea_destroy(e);
        }
    }

    #[test]
    fn touch_stage_read_and_callbacks() {
        use std::ffi::c_void;
        let Some(e) = engine() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        unsafe extern "C" fn on_fail(name: *const c_char, msg: *const c_char, user: *mut c_void) {
            let log = unsafe { &mut *(user as *mut Vec<(String, String)>) };
            let s = |p| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
            log.push((s(name), s(msg)));
        }
        unsafe extern "C" fn on_request(hard: bool, user: *mut c_void) {
            unsafe { &mut *(user as *mut Vec<bool>) }.push(hard);
        }
        unsafe {
            assert_eq!(lactea_touch(e, 0.5, 0.5, 1, 1), LacteaStatus::Ok);
            assert_eq!(
                lactea_touch(e, 0.5, 0.5, 1, 9),
                LacteaStatus::InvalidArgument
            );
            assert_eq!(lactea_touch_drag(e, 0.4, 0.4, 1), LacteaStatus::Ok);
            assert_eq!(lactea_touch_destroy(e, 0.4, 0.4), LacteaStatus::Ok);
            assert_eq!(lactea_touch_destroy_all(e), LacteaStatus::Ok);
            assert_eq!(
                lactea_touch_destroy_all(null_mut()),
                LacteaStatus::InvalidArgument
            );

            let mut fails: Vec<(String, String)> = Vec::new();
            assert_eq!(
                lactea_set_switch_failed_callback(
                    e,
                    Some(on_fail),
                    &mut fails as *mut _ as *mut c_void
                ),
                LacteaStatus::Ok
            );
            let bad = b"[preset00]\nper_frame_1=x = (;\n";
            assert_ne!(
                lactea_load_preset(e, bad.as_ptr(), bad.len(), c"broken".as_ptr()),
                LacteaStatus::Ok
            );
            assert_eq!(fails.len(), 1);
            assert_eq!(fails[0].0, "broken");
            assert!(!fails[0].1.is_empty());

            let mut s = std::mem::MaybeUninit::<LacteaSwitchSettings>::uninit();
            lactea_get_switch_settings(e, s.as_mut_ptr());
            let mut s = s.assume_init();
            (s.preset_duration, s.easter_egg) = (1.0, 0.0);
            lactea_set_switch_settings(e, &s);
            assert_eq!(lactea_load_idle_preset(e), LacteaStatus::Ok);
            let mut reqs: Vec<bool> = Vec::new();
            assert_eq!(
                lactea_set_switch_requested_callback(
                    e,
                    Some(on_request),
                    &mut reqs as *mut _ as *mut c_void
                ),
                LacteaStatus::Ok
            );
            for frame in 0..90u32 {
                assert_eq!(
                    lactea_render(e, f64::from(frame) / 60.0, frame),
                    LacteaStatus::Ok
                );
            }
            assert_eq!(reqs, vec![false]);
            assert!(lactea_poll_switch_request(e, null_mut()));

            let (mut w, mut h) = (0, 0);
            assert_eq!(
                lactea_read_stage(e, 0, null_mut(), 0, &mut w, &mut h),
                LacteaStatus::Ok
            );
            assert_eq!((w, h), (64, 36));
            let mut buf = vec![0u8; (w * h * 4) as usize];
            assert_eq!(
                lactea_read_stage(e, 1, buf.as_mut_ptr(), buf.len(), null_mut(), null_mut()),
                LacteaStatus::Ok
            );
            assert_eq!(
                lactea_read_stage(e, 1, buf.as_mut_ptr(), 3, null_mut(), null_mut()),
                LacteaStatus::InvalidArgument
            );
            assert_eq!(
                lactea_read_stage(e, 6, null_mut(), 0, null_mut(), null_mut()),
                LacteaStatus::InvalidArgument
            );
            lactea_destroy(e);
        }
    }

    #[test]
    fn display_time_up_is_polled_once_and_the_idle_preset_renders() {
        let Some(e) = engine() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        unsafe {
            let mut s = std::mem::MaybeUninit::<LacteaSwitchSettings>::uninit();
            lactea_get_switch_settings(e, s.as_mut_ptr());
            let mut s = s.assume_init();
            (s.preset_duration, s.easter_egg) = (1.0, 0.0);
            assert_eq!(lactea_set_switch_settings(e, &s), LacteaStatus::Ok);
            assert_eq!(lactea_load_idle_preset(e), LacteaStatus::Ok);
            let mut hard = true;
            assert!(!lactea_poll_switch_request(e, &mut hard));
            for frame in 0..90u32 {
                assert_eq!(
                    lactea_render(e, f64::from(frame) / 60.0, frame),
                    LacteaStatus::Ok
                );
            }
            // 1 s of display time is up at frame 60: one soft-cut request, then silence
            assert!(lactea_poll_switch_request(e, &mut hard));
            assert!(!hard);
            assert!(!lactea_poll_switch_request(e, null_mut()));
            assert!((lactea_last_frame_time(e) - 89.0 / 60.0).abs() < 1e-12);

            // a locked engine never asks
            s.preset_locked = true;
            lactea_set_switch_settings(e, &s);
            assert_eq!(lactea_load_idle_preset(e), LacteaStatus::Ok);
            for frame in 90..300u32 {
                lactea_render(e, f64::from(frame) / 60.0, frame);
            }
            assert!(!lactea_poll_switch_request(e, null_mut()));
            lactea_destroy(e);
        }
    }

    #[derive(Default)]
    struct Seen {
        requested: Vec<bool>,
        failed: Vec<(String, String)>,
    }

    unsafe extern "C" fn on_requested(is_hard_cut: bool, user: *mut c_void) {
        unsafe { &mut *user.cast::<Seen>() }
            .requested
            .push(is_hard_cut);
    }

    unsafe extern "C" fn on_failed(name: *const c_char, message: *const c_char, user: *mut c_void) {
        let (n, m) = unsafe { (CStr::from_ptr(name), CStr::from_ptr(message)) };
        unsafe { &mut *user.cast::<Seen>() }.failed.push((
            n.to_string_lossy().into_owned(),
            m.to_string_lossy().into_owned(),
        ));
    }

    #[test]
    fn soft_cut_load_validates_blends_and_gives_the_setting_back() {
        let Some(e) = engine() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let milk = lactea::IDLE_PRESET.as_bytes();
        let name = c"soft".as_ptr();
        unsafe {
            for bad in [-1.0, f64::NAN, f64::INFINITY] {
                assert_eq!(
                    lactea_load_preset_ex(e, milk.as_ptr(), milk.len(), name, bad),
                    LacteaStatus::InvalidArgument
                );
            }
            assert_eq!(lactea_load_idle_preset(e), LacteaStatus::Ok);
            for frame in 0..30u32 {
                lactea_render(e, f64::from(frame) / 60.0, frame);
            }
            // 1.5 s soft cut starting at t = 29/60 s, while the setting stays at projectM's 3 s
            assert_eq!(
                lactea_load_preset_ex(e, milk.as_ptr(), milk.len(), name, 1.5),
                LacteaStatus::Ok
            );
            assert!(
                (*e).engine
                    .transition_info()
                    .is_some_and(|t| t.duration == 1.5)
            );
            assert!((*e).control.is_smoothing());
            let mut s = std::mem::MaybeUninit::<LacteaSwitchSettings>::uninit();
            lactea_get_switch_settings(e, s.as_mut_ptr());
            assert_eq!(s.assume_init().soft_cut_duration, 3.0);
            // half way through: still blending
            for frame in 30..90u32 {
                lactea_render(e, f64::from(frame) / 60.0, frame);
            }
            assert!((*e).control.is_smoothing());
            assert!((*e).engine.transition_info().is_some());
            // the blend is over after 1.5 s: the engine drops it, the control restores its setting
            for frame in 90..150u32 {
                lactea_render(e, f64::from(frame) / 60.0, frame);
            }
            assert!(!(*e).control.is_smoothing());
            assert!((*e).engine.transition_info().is_none());
            assert_eq!((*e).control.soft_cut_duration(), 3.0);
            assert!((*e).restore_soft_cut.is_none());
            // a zero duration is a hard cut
            assert_eq!(
                lactea_load_preset_ex(e, milk.as_ptr(), milk.len(), name, 0.0),
                LacteaStatus::Ok
            );
            assert!((*e).engine.transition_info().is_none());
            lactea_destroy(e);
        }
    }

    #[test]
    fn switch_callbacks_fire_for_requests_and_failed_loads() {
        let Some(e) = engine() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let mut seen = Seen::default();
        let user = (&raw mut seen).cast::<c_void>();
        unsafe {
            assert_eq!(
                lactea_set_switch_requested_callback(e, Some(on_requested), user),
                LacteaStatus::Ok
            );
            assert_eq!(
                lactea_set_switch_failed_callback(e, Some(on_failed), user),
                LacteaStatus::Ok
            );
            assert_eq!(
                lactea_set_switch_failed_callback(null_mut(), None, null_mut()),
                LacteaStatus::InvalidArgument
            );

            // a load that does not parse: status, last error and callback agree; the old preset stays
            assert_eq!(lactea_load_idle_preset(e), LacteaStatus::Ok);
            let junk = b"per_frame_1=x = ;;(\n[preset00]\nper_frame_1=q1 = (((;\n";
            let status = lactea_load_preset(e, junk.as_ptr(), junk.len(), c"broken".as_ptr());
            assert_eq!(status, LacteaStatus::Preset);
            assert_eq!(seen.failed.len(), 1);
            assert_eq!(seen.failed[0].0, "broken");
            assert_eq!(
                seen.failed[0].1,
                CStr::from_ptr(lactea_last_error()).to_str().unwrap()
            );
            assert!(!seen.failed[0].1.is_empty());
            assert_eq!(
                lactea_load_preset_ex(e, junk.as_ptr(), junk.len(), c"soft broken".as_ptr(), 2.0),
                LacteaStatus::Preset
            );
            assert_eq!(seen.failed.len(), 2);
            assert!(
                !(*e).control.is_smoothing(),
                "a failed soft load must not start smoothing"
            );

            // display time up: the callback fires once, the poll still reports it
            let mut s = std::mem::MaybeUninit::<LacteaSwitchSettings>::uninit();
            lactea_get_switch_settings(e, s.as_mut_ptr());
            let mut s = s.assume_init();
            (s.preset_duration, s.easter_egg) = (1.0, 0.0);
            lactea_set_switch_settings(e, &s);
            assert_eq!(lactea_load_idle_preset(e), LacteaStatus::Ok);
            for frame in 0..90u32 {
                lactea_render(e, f64::from(frame) / 60.0, frame);
            }
            assert_eq!(seen.requested, vec![false]);
            assert!(lactea_poll_switch_request(e, null_mut()));

            // removing the callbacks silences them
            lactea_set_switch_requested_callback(e, None, null_mut());
            lactea_set_switch_failed_callback(e, None, null_mut());
            lactea_load_preset(e, junk.as_ptr(), junk.len(), null());
            assert_eq!(seen.failed.len(), 2);
            lactea_destroy(e);
        }
    }

    /// `lactea_get_auto_hard_cut_mode`, which must succeed.
    unsafe fn auto_mode(e: *const LacteaEngine) -> u8 {
        let mut m = 255u8;
        assert_eq!(
            unsafe { lactea_get_auto_hard_cut_mode(e, &mut m) },
            LacteaStatus::Ok
        );
        m
    }

    /// The engine's switch settings.
    unsafe fn switch_settings(e: *const LacteaEngine) -> LacteaSwitchSettings {
        let mut s = std::mem::MaybeUninit::<LacteaSwitchSettings>::uninit();
        assert_eq!(
            unsafe { lactea_get_switch_settings(e, s.as_mut_ptr()) },
            LacteaStatus::Ok
        );
        unsafe { s.assume_init() }
    }

    /// Silence the display timer (a huge display time, no easter egg) so only auto cuts can ask.
    unsafe fn quiet_timer(e: *mut LacteaEngine) -> LacteaSwitchSettings {
        let mut s = unsafe { switch_settings(e) };
        (s.preset_duration, s.easter_egg) = (1.0e6, 0.0);
        assert_eq!(
            unsafe { lactea_set_switch_settings(e, &s) },
            LacteaStatus::Ok
        );
        s
    }

    /// A v100 preset whose per-frame code pins `bass` to 2 (a loud beat for the auto hard cut).
    const BASS2: &[u8] = b"MILKDROP_PRESET_VERSION=100\nper_frame_1=bass=2;\n";

    #[test]
    fn auto_hard_cut_mode_config_setter_and_getter() {
        // the config field needs no GPU: default 0, and it reaches the engine config
        let mut cfg = std::mem::MaybeUninit::<LacteaConfig>::uninit();
        let mut cfg = unsafe {
            lactea_config_default(cfg.as_mut_ptr());
            cfg.assume_init()
        };
        assert_eq!(cfg.auto_hard_cut_mode, 0);
        cfg.auto_hard_cut_mode = 5;
        let ec = unsafe { engine_config(&cfg) }.unwrap();
        assert_eq!(ec.auto_hard_cut_mode, 5);
        assert_eq!(
            unsafe { lactea_get_auto_hard_cut_mode(null(), &mut 0u8) },
            LacteaStatus::InvalidArgument
        );
        assert_eq!(
            unsafe { lactea_set_auto_hard_cut_mode(null_mut(), 1) },
            LacteaStatus::InvalidArgument
        );

        let Some(e) = engine() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        unsafe {
            assert_eq!(auto_mode(e), 0);
            assert_eq!(lactea_set_auto_hard_cut_mode(e, 3), LacteaStatus::Ok);
            assert_eq!(auto_mode(e), 3);
            // more than 7 clamps to 7 (MD3 has 7 modes)
            assert_eq!(lactea_set_auto_hard_cut_mode(e, 200), LacteaStatus::Ok);
            assert_eq!(auto_mode(e), 7);
            assert_eq!(lactea_set_auto_hard_cut_mode(e, 0), LacteaStatus::Ok);
            assert_eq!(auto_mode(e), 0);
            assert_eq!(
                lactea_get_auto_hard_cut_mode(e, null_mut()),
                LacteaStatus::InvalidArgument
            );
            lactea_destroy(e);
        }

        // the setting is stored under every compat; the config value is clamped too
        for compat in [LacteaCompat::Projectm, LacteaCompat::Milkdrop3] {
            let Some(e) = engine_with(compat, 9) else {
                return;
            };
            unsafe {
                assert_eq!(auto_mode(e), 7, "{compat:?}");
                lactea_destroy(e);
            }
        }
    }

    #[test]
    fn md3_auto_hard_cut_raises_a_hard_cut_request_only_under_milkdrop_3() {
        let Some(e) = engine_with(LacteaCompat::Milkdrop3, 6) else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let mut seen = Seen::default();
        let user = (&raw mut seen).cast::<c_void>();
        unsafe {
            let mut s = quiet_timer(e);
            assert_eq!(
                lactea_set_switch_requested_callback(e, Some(on_requested), user),
                LacteaStatus::Ok
            );
            assert_eq!(
                lactea_load_preset(e, BASS2.as_ptr(), BASS2.len(), c"loud".as_ptr()),
                LacteaStatus::Ok
            );
            let mut hard = false;
            // frame 0 reads no levels yet: the per-frame code of this frame sets them for the next
            assert_eq!(lactea_render(e, 0.0, 0), LacteaStatus::Ok);
            assert!(!lactea_poll_switch_request(e, &mut hard));
            assert!(seen.requested.is_empty());
            // frame 1 sees bass = 2: mode 6 asks for an instant change, a normal hard-cut request
            assert_eq!(lactea_render(e, 1.0 / 60.0, 1), LacteaStatus::Ok);
            assert!(lactea_poll_switch_request(e, &mut hard));
            assert!(hard);
            assert_eq!(seen.requested, vec![true]);
            // polled once: the request is gone until the next cut
            assert!(!lactea_poll_switch_request(e, null_mut()));

            // a locked preset ignores auto cuts
            s.preset_locked = true;
            assert_eq!(lactea_set_switch_settings(e, &s), LacteaStatus::Ok);
            for frame in 2..12u32 {
                lactea_render(e, f64::from(frame) / 60.0, frame);
            }
            assert!(!lactea_poll_switch_request(e, null_mut()));
            assert_eq!(seen.requested.len(), 1);

            // unlocked again, the next loud frame asks (the lock was what held it back)
            s.preset_locked = false;
            assert_eq!(lactea_set_switch_settings(e, &s), LacteaStatus::Ok);
            lactea_render(e, 12.0 / 60.0, 12);
            assert!(lactea_poll_switch_request(e, &mut hard));
            assert!(hard);
            assert_eq!(seen.requested, vec![true, true]);

            // mode 0 switches it off
            assert_eq!(lactea_set_auto_hard_cut_mode(e, 0), LacteaStatus::Ok);
            for frame in 13..20u32 {
                lactea_render(e, f64::from(frame) / 60.0, frame);
            }
            assert!(!lactea_poll_switch_request(e, null_mut()));
            assert_eq!(seen.requested.len(), 2);
            lactea_destroy(e);
        }
    }

    #[test]
    fn auto_hard_cut_mode_is_inert_under_projectm() {
        let Some(e) = engine_with(LacteaCompat::Projectm, 6) else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let mut seen = Seen::default();
        let user = (&raw mut seen).cast::<c_void>();
        unsafe {
            quiet_timer(e);
            assert_eq!(
                lactea_set_switch_requested_callback(e, Some(on_requested), user),
                LacteaStatus::Ok
            );
            assert_eq!(
                lactea_load_preset(e, BASS2.as_ptr(), BASS2.len(), c"loud".as_ptr()),
                LacteaStatus::Ok
            );
            for frame in 0..12u32 {
                assert_eq!(
                    lactea_render(e, f64::from(frame) / 60.0, frame),
                    LacteaStatus::Ok
                );
            }
            // the mode is stored but never acts
            assert_eq!(auto_mode(e), 6);
            assert!(!lactea_poll_switch_request(e, null_mut()));
            assert!(seen.requested.is_empty());
            lactea_destroy(e);
        }
    }

    #[test]
    fn preset_duration_rand_is_stored_and_reaches_the_control_only_under_milkdrop_3() {
        for (compat, md3) in [
            (LacteaCompat::Projectm, false),
            (LacteaCompat::Milkdrop2, false),
            (LacteaCompat::Milkdrop3, true),
        ] {
            let Some(e) = engine_with(compat, 0) else {
                eprintln!("skipping: no wgpu adapter");
                return;
            };
            unsafe {
                // MD3's default TimeBetweenPresetsRand under every compat; only MD3 feeds the control
                let mut s = switch_settings(e);
                assert_eq!(s.preset_duration_rand, 10.0, "{compat:?}");
                assert_eq!(
                    (*e).control.md3_time_rand(),
                    md3.then_some(10.0),
                    "{compat:?}"
                );
                s.preset_duration_rand = 2.5;
                assert_eq!(lactea_set_switch_settings(e, &s), LacteaStatus::Ok);
                assert_eq!(switch_settings(e).preset_duration_rand, 2.5);
                assert_eq!((*e).control.md3_time_rand(), md3.then_some(2.5));
                // negative counts as 0
                s.preset_duration_rand = -4.0;
                assert_eq!(lactea_set_switch_settings(e, &s), LacteaStatus::Ok);
                assert_eq!(switch_settings(e).preset_duration_rand, 0.0);
                assert_eq!((*e).control.md3_time_rand(), md3.then_some(0.0));
                lactea_destroy(e);
            }
        }
    }
}
