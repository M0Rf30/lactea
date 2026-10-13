//! projectM core API gaps: `projectm_burn_texture`, `projectm_reset_textures`, the VCS version string, the log
//! callback and the texture load callback.

use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex};

use lactea::{Engine, Image, Media, TextureProvider};
use lactea_host::DirTextures;

use crate::{LacteaEngine, LacteaStatus, fail, guard, with_engine_mut};

// ---- burn / reset -------------------------------------------------------------------------------------------

/// `projectm_burn_texture` (`ProjectM::BurnInTexture`): draw one image as a quad into the image the next frame's
/// warp samples, in the running preset and, during a soft cut, in the incoming one. Nothing happens before the
/// first preset is loaded.
///
/// `rgba` is `width * height` RGBA8 pixels, row 0 = **top** (projectM takes an OpenGL texture id of the host's
/// context, which does not exist here: the pixels are copied before the call returns and the buffer stays the
/// caller's). `left`, `top`, `rect_width`, `rect_height` are in output pixels and follow projectM's placement
/// arithmetic: the quad is scaled by `rect_width / W` x `rect_height / H` about the centre of the image, and a
/// non-zero `left` / `top` perspective-shifts it rather than translating it, exactly as projectM does. The copy is
/// opaque (no blending).
///
/// Returns `LACTEA_STATUS_INVALID_ARGUMENT` for NULL pixels, zero sizes or a size beyond the device's texture limit.
///
/// # Safety
/// `engine` valid; `rgba` points to `width * height * 4` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_burn_texture_rgba(
    engine: *mut LacteaEngine,
    rgba: *const u8,
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    rect_width: i32,
    rect_height: i32,
) -> LacteaStatus {
    with_engine_mut(engine, |e| {
        if rgba.is_null() || width == 0 || height == 0 {
            return fail(
                LacteaStatus::InvalidArgument,
                "rgba is NULL or the size is 0",
            );
        }
        let Some(len) = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .filter(|&n| n <= isize::MAX as usize)
        else {
            return fail(LacteaStatus::InvalidArgument, "image size overflows");
        };
        let pixels = unsafe { std::slice::from_raw_parts(rgba, len) };
        match e
            .engine
            .burn_texture(pixels, width, height, [left, top, rect_width, rect_height])
        {
            Ok(()) => LacteaStatus::Ok,
            Err(err) => fail(LacteaStatus::InvalidArgument, err),
        }
    })
}

/// `projectm_reset_textures`: forgets every decoded texture, rescans the `lactea_set_texture_dir` directory and
/// resolves the running preset's textures again (call after the files or the texture callback's answers changed).
///
/// # Safety
/// `engine` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_reset_textures(engine: *mut LacteaEngine) -> LacteaStatus {
    with_engine_mut(engine, |e| {
        e.textures.rescan();
        e.textures.install(&mut e.engine);
        e.engine.reload_textures();
        LacteaStatus::Ok
    })
}

// ---- version ------------------------------------------------------------------------------------------------

/// `projectm_get_vcs_version_string`: `git describe --always --dirty` of the source the library was built from, or
/// `"unknown"` for a build outside a git checkout (a published crate). A static NUL-terminated string: do not free.
#[unsafe(no_mangle)]
pub extern "C" fn lactea_vcs_version() -> *const c_char {
    static V: LazyLock<CString> =
        LazyLock::new(|| CString::new(lactea::VCS_VERSION).unwrap_or_default());
    V.as_ptr()
}

// ---- logging ------------------------------------------------------------------------------------------------

/// A log level (`projectm_log_level`): one of the `LACTEA_LOG_LEVEL_*` values. A plain `int32_t`; out-of-range
/// values are clamped (below 0: `NOTSET`, above 6: `FATAL`).
pub type LacteaLogLevel = i32;
/// Use the default level ([`LACTEA_LOG_LEVEL_INFO`]).
pub const LACTEA_LOG_LEVEL_NOTSET: LacteaLogLevel = 0;
/// Verbose trace output.
pub const LACTEA_LOG_LEVEL_TRACE: LacteaLogLevel = 1;
/// Debug output.
pub const LACTEA_LOG_LEVEL_DEBUG: LacteaLogLevel = 2;
/// Informational messages.
pub const LACTEA_LOG_LEVEL_INFO: LacteaLogLevel = 3;
/// Non-critical problems, e.g. a shader that fell back to the default one.
pub const LACTEA_LOG_LEVEL_WARN: LacteaLogLevel = 4;
/// Recoverable errors, e.g. a preset that failed to load.
pub const LACTEA_LOG_LEVEL_ERROR: LacteaLogLevel = 5;
/// Irrecoverable errors (lactea reports none; accepted as the level that silences everything below it).
pub const LACTEA_LOG_LEVEL_FATAL: LacteaLogLevel = 6;

/// Log callback: `level` is the record's `LACTEA_LOG_LEVEL_*`, `message` NUL-terminated and valid only during the
/// call (it may contain line breaks).
pub type LacteaLogFn = Option<
    unsafe extern "C" fn(level: LacteaLogLevel, message: *const c_char, user_data: *mut c_void),
>;

struct Sink {
    callback: unsafe extern "C" fn(LacteaLogLevel, *const c_char, *mut c_void),
    /// The host's pointer, stored as an address: the logger is shared between threads and lactea never dereferences it.
    user: usize,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);
static LEVEL: AtomicU8 = AtomicU8::new(LACTEA_LOG_LEVEL_INFO as u8);

thread_local! {
    /// A record logged by the callback's own lactea call is dropped instead of recursing.
    static IN_CALLBACK: Cell<bool> = const { Cell::new(false) };
}

fn filter(level: u8) -> log::LevelFilter {
    match level {
        1 => log::LevelFilter::Trace,
        2 => log::LevelFilter::Debug,
        4 => log::LevelFilter::Warn,
        5 | 6 => log::LevelFilter::Error,
        _ => log::LevelFilter::Info,
    }
}

fn level_of(l: log::Level) -> LacteaLogLevel {
    match l {
        log::Level::Trace => LACTEA_LOG_LEVEL_TRACE,
        log::Level::Debug => LACTEA_LOG_LEVEL_DEBUG,
        log::Level::Info => LACTEA_LOG_LEVEL_INFO,
        log::Level::Warn => LACTEA_LOG_LEVEL_WARN,
        log::Level::Error => LACTEA_LOG_LEVEL_ERROR,
    }
}

struct Bridge;

impl log::Log for Bridge {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= filter(LEVEL.load(Ordering::Relaxed))
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) || IN_CALLBACK.with(Cell::get) {
            return;
        }
        // copy the target out so the lock is not held while the host runs: its callback may (re)set the callback
        let Some((cb, user)) = SINK
            .lock()
            .ok()
            .and_then(|s| s.as_ref().map(|s| (s.callback, s.user)))
        else {
            return;
        };
        let msg = CString::new(record.args().to_string().replace('\0', " ")).unwrap_or_default();
        IN_CALLBACK.with(|f| f.set(true));
        unsafe { cb(level_of(record.level()), msg.as_ptr(), user as *mut c_void) };
        IN_CALLBACK.with(|f| f.set(false));
    }

    fn flush(&self) {}
}

static BRIDGE: Bridge = Bridge;

/// Make sure the process's `log` facade ends at [`Bridge`]; `false` when another logger already owns it.
fn install_bridge() -> bool {
    static INSTALLED: LazyLock<bool> = LazyLock::new(|| log::set_logger(&BRIDGE).is_ok());
    *INSTALLED
}

/// `projectm_set_log_callback`: receives every log record lactea (and the engine crates under it) emits. Process-global,
/// like projectM's global callback (there is no per-thread variant); `NULL` removes it. Thread-safe: records are
/// delivered on the thread whose lactea call produced them, possibly from several threads at once, so the callback
/// must be reentrant on its own data.
///
/// Unlike the event callbacks, a log callback runs *inside* the lactea call, while the engine is in use: it must
/// **not** call any lactea function on an engine (setting the callback or the level is fine). A record logged by a
/// lactea call made in the callback is dropped. After removing the callback, `user_data` may still be passed to a
/// callback running on another thread: free it only once no lactea call is running.
///
/// Records are only produced while a level at or below theirs is set (`lactea_set_log_level`, default `INFO`).
/// Fails with `LACTEA_STATUS_INVALID_ARGUMENT` when the process already has a different `log` logger installed
/// (e.g. a Rust host).
///
/// # Safety
/// `callback`, if not NULL, must be callable from any thread until removed; `user_data` is passed to it unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_log_callback(
    callback: LacteaLogFn,
    user_data: *mut c_void,
) -> LacteaStatus {
    guard(|| {
        if callback.is_some() && !install_bridge() {
            return fail(
                LacteaStatus::InvalidArgument,
                "another logger is already installed in this process",
            );
        }
        if let Ok(mut sink) = SINK.lock() {
            *sink = callback.map(|callback| Sink {
                callback,
                user: user_data as usize,
            });
        }
        log::set_max_level(filter(LEVEL.load(Ordering::Relaxed)));
        LacteaStatus::Ok
    })
}

/// `projectm_set_log_level`: the lowest level delivered to the log callback (process-global; `NOTSET` selects the
/// default, `INFO`). `LACTEA_LOG_LEVEL_FATAL` behaves like `ERROR`: lactea has no fatal records.
#[unsafe(no_mangle)]
pub extern "C" fn lactea_set_log_level(level: LacteaLogLevel) -> LacteaStatus {
    guard(|| {
        let l = level.clamp(LACTEA_LOG_LEVEL_NOTSET, LACTEA_LOG_LEVEL_FATAL) as u8;
        LEVEL.store(l, Ordering::Relaxed);
        if SINK.lock().is_ok_and(|s| s.is_some()) {
            log::set_max_level(filter(l));
        }
        LacteaStatus::Ok
    })
}

// ---- texture provider: host callback in front of the directory -----------------------------------------------

/// Pixels a texture load callback hands back (`projectm_texture_load_data` with the OpenGL parts removed).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LacteaImage {
    /// `width * height * 4` RGBA8 bytes, **row 0 = top** (projectM's callback uses OpenGL order, first row =
    /// bottom, and accepts 3 or 4 channels and GL texture ids; none of those exist here). NULL = "no such
    /// texture".
    pub rgba: *const u8,
    /// Width in pixels (> 0 with pixels).
    pub width: u32,
    /// Height in pixels (> 0 with pixels).
    pub height: u32,
}

/// Texture load callback (`projectm_set_texture_load_event_callback`): `name` is the texture as the preset's shader
/// names it, lowercase, without the `sampler_` and quality prefixes and without extension (`sampler_fw_clouds2` asks
/// for `"clouds2"`), NUL-terminated and valid only during the call. Fill `*out` and return `true` to supply it;
/// `false` leaves the lookup to the `lactea_set_texture_dir` directory. `*out` starts zeroed. The pixels are copied
/// before the callback's caller continues, so they only need to stay valid until the callback returns.
///
/// It runs synchronously on the thread of the lactea call that loads a preset (`lactea_load_*`) or resets textures,
/// while the engine is in use: it must **not** call any lactea function on that engine.
pub type LacteaTextureLoadFn = Option<
    unsafe extern "C" fn(
        name: *const c_char,
        user_data: *mut c_void,
        out: *mut LacteaImage,
    ) -> bool,
>;

/// What `lactea_set_texture_dir` and `lactea_set_texture_load_callback` configured; rebuilt into one provider each
/// time either changes.
#[derive(Default)]
pub(crate) struct HostTextures {
    callback: LacteaTextureLoadFn,
    user: usize,
    dir: Option<PathBuf>,
    scanned: Option<Rc<RefCell<DirTextures>>>,
}

impl HostTextures {
    pub(crate) fn set_dir(&mut self, dir: &Path) {
        self.dir = Some(dir.to_path_buf());
        self.rescan();
    }

    fn rescan(&mut self) {
        self.scanned = self
            .dir
            .as_deref()
            .map(|d| Rc::new(RefCell::new(DirTextures::scan(d))));
    }

    /// Hand the engine a provider asking the callback first, then the directory.
    pub(crate) fn install(&self, engine: &mut Engine) {
        engine.set_texture_provider(Box::new(Chain {
            callback: self.callback,
            user: self.user as *mut c_void,
            dir: self.scanned.clone(),
        }));
    }
}

struct Chain {
    callback: LacteaTextureLoadFn,
    user: *mut c_void,
    dir: Option<Rc<RefCell<DirTextures>>>,
}

impl Chain {
    fn ask(&self, name: &str) -> Option<Image> {
        let cb = self.callback?;
        let cname = CString::new(name).ok()?;
        let mut out = LacteaImage {
            rgba: std::ptr::null(),
            width: 0,
            height: 0,
        };
        // a panic cannot cross the host's C frame, so the callback is the host's to get right
        if !unsafe { cb(cname.as_ptr(), self.user, &mut out) } {
            return None;
        }
        let len = (out.width as usize)
            .checked_mul(out.height as usize)?
            .checked_mul(4)
            .filter(|&n| n > 0 && n <= isize::MAX as usize)?;
        if out.rgba.is_null() {
            return None;
        }
        Some(Image {
            width: out.width,
            height: out.height,
            rgba8: unsafe { std::slice::from_raw_parts(out.rgba, len) }.to_vec(),
        })
    }
}

impl TextureProvider for Chain {
    fn load(&mut self, name: &str) -> Option<Image> {
        self.ask(name)
            .or_else(|| self.dir.as_ref().and_then(|d| d.borrow_mut().load(name)))
    }

    fn random_pool(&self) -> Vec<String> {
        self.dir
            .as_ref()
            .map(|d| d.borrow().random_pool())
            .unwrap_or_default()
    }

    fn load_md3_texture(&mut self, name: &str) -> Option<Media> {
        match self.ask(name) {
            Some(img) => Some(Media::Still(img)),
            None => self
                .dir
                .as_ref()
                .and_then(|d| d.borrow_mut().load_md3_texture(name)),
        }
    }

    fn load_sprite(&mut self, path: &str) -> Option<Media> {
        self.dir
            .as_ref()
            .and_then(|d| d.borrow_mut().load_sprite(path))
    }
}

/// `projectm_set_texture_load_event_callback`: consult `callback` for every texture a preset's shaders (and, with
/// `LACTEA_COMPAT_MILKDROP3`, shape images) ask for, before the `lactea_set_texture_dir` directory; `NULL` removes it.
/// See [`LacteaTextureLoadFn`] for the contract. Takes effect for presets loaded afterwards (or
/// `lactea_reset_textures`). `randNN` samplers still pick from the directory's files.
///
/// # Safety
/// `engine` valid; `callback`, if not NULL, valid to call from the thread that loads presets; `user_data` is passed
/// to it unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_set_texture_load_callback(
    engine: *mut LacteaEngine,
    callback: LacteaTextureLoadFn,
    user_data: *mut c_void,
) -> LacteaStatus {
    with_engine_mut(engine, |e| {
        e.textures.callback = callback;
        e.textures.user = user_data as usize;
        e.textures.install(&mut e.engine);
        LacteaStatus::Ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LacteaConfig, lactea_config_default, lactea_create, lactea_destroy};
    use crate::{lactea_load_preset, lactea_render_to_buffer};
    use std::ffi::CStr;

    fn engine() -> Option<*mut LacteaEngine> {
        let instance = wgpu::Instance::default();
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
        let mut cfg = std::mem::MaybeUninit::<LacteaConfig>::uninit();
        let e = unsafe {
            lactea_config_default(cfg.as_mut_ptr());
            let mut cfg = cfg.assume_init();
            (cfg.width, cfg.height, cfg.mesh_x, cfg.mesh_y) = (32, 32, 16, 12);
            lactea_create(&cfg)
        };
        assert!(!e.is_null());
        Some(e)
    }

    #[test]
    fn vcs_version_is_a_non_empty_static_string() {
        let v = unsafe { CStr::from_ptr(lactea_vcs_version()) }
            .to_str()
            .unwrap();
        assert!(!v.is_empty());
        assert_eq!(v, lactea::VCS_VERSION);
        assert_eq!(lactea_vcs_version(), lactea_vcs_version());
    }

    #[test]
    fn burn_and_reset_validate_and_run() {
        let Some(e) = engine() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let px = [255u8, 0, 0, 255].repeat(4);
        unsafe {
            // before the first load there is no preset to burn into: accepted, no effect
            assert_eq!(
                lactea_burn_texture_rgba(e, px.as_ptr(), 2, 2, 0, 0, 32, 32),
                LacteaStatus::Ok
            );
            for (p, w, h) in [(std::ptr::null(), 2, 2), (px.as_ptr(), 0, 2)] {
                assert_eq!(
                    lactea_burn_texture_rgba(e, p, w, h, 0, 0, 32, 32),
                    LacteaStatus::InvalidArgument
                );
            }
            assert_eq!(
                lactea_burn_texture_rgba(std::ptr::null_mut(), px.as_ptr(), 2, 2, 0, 0, 1, 1),
                LacteaStatus::InvalidArgument
            );
            let preset = b"[preset00]\nfDecay=1\nfWaveAlpha=0\n";
            assert_eq!(
                lactea_load_preset(e, preset.as_ptr(), preset.len(), c"p".as_ptr()),
                LacteaStatus::Ok
            );
            // burning the whole target in red shows in the next frame
            assert_eq!(
                lactea_burn_texture_rgba(e, px.as_ptr(), 2, 2, 0, 0, 32, 32),
                LacteaStatus::Ok
            );
            assert_eq!(lactea_reset_textures(e), LacteaStatus::Ok);
            let mut out = vec![0u8; 32 * 32 * 4];
            assert_eq!(
                lactea_render_to_buffer(e, 0.0, 0, out.as_mut_ptr(), out.len()),
                LacteaStatus::Ok
            );
            let c = &out[(16 * 32 + 16) * 4..][..4];
            assert!(c[0] > 200 && c[1] < 40 && c[2] < 40, "{c:?}");
            lactea_destroy(e);
        }
    }

    #[test]
    fn texture_callback_is_asked_before_the_directory() {
        let Some(e) = engine() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        static ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());
        static PIXEL: [u8; 4] = [10, 20, 30, 255];
        unsafe extern "C" fn cb(
            name: *const c_char,
            _user: *mut c_void,
            out: *mut LacteaImage,
        ) -> bool {
            let name = unsafe { CStr::from_ptr(name) }.to_str().unwrap().to_owned();
            ASKED.lock().unwrap().push(name.clone());
            if name != "hosttex" {
                return false;
            }
            unsafe {
                *out = LacteaImage {
                    rgba: PIXEL.as_ptr(),
                    width: 1,
                    height: 1,
                };
            }
            true
        }
        let mut chain = Chain {
            callback: Some(cb),
            user: std::ptr::null_mut(),
            dir: None,
        };
        let got = chain.load("hosttex").expect("callback texture");
        assert_eq!((got.width, got.height, got.rgba8), (1, 1, PIXEL.to_vec()));
        assert!(chain.load("other").is_none());
        assert_eq!(*ASKED.lock().unwrap(), ["hosttex", "other"]);
        unsafe {
            assert_eq!(
                lactea_set_texture_load_callback(e, Some(cb), std::ptr::null_mut()),
                LacteaStatus::Ok
            );
            assert_eq!(
                lactea_set_texture_load_callback(std::ptr::null_mut(), None, std::ptr::null_mut()),
                LacteaStatus::InvalidArgument
            );
            lactea_destroy(e);
        }
    }

    #[test]
    fn log_callback_receives_records_at_or_above_the_level() {
        static GOT: Mutex<Vec<(i32, String)>> = Mutex::new(Vec::new());
        unsafe extern "C" fn cb(level: i32, msg: *const c_char, user: *mut c_void) {
            assert_eq!(user as usize, 7);
            let m = unsafe { CStr::from_ptr(msg) }.to_str().unwrap().to_owned();
            GOT.lock().unwrap().push((level, m));
        }
        let status = unsafe { lactea_set_log_callback(Some(cb), 7 as *mut c_void) };
        if status != LacteaStatus::Ok {
            // another test binary logger owns the process
            return;
        }
        assert_eq!(
            lactea_set_log_level(LACTEA_LOG_LEVEL_WARN),
            LacteaStatus::Ok
        );
        log::info!("quiet");
        log::warn!("loud");
        log::error!("louder");
        assert_eq!(
            lactea_set_log_level(LACTEA_LOG_LEVEL_ERROR),
            LacteaStatus::Ok
        );
        log::warn!("dropped");
        unsafe { lactea_set_log_callback(None, std::ptr::null_mut()) };
        log::error!("after removal");
        assert_eq!(
            *GOT.lock().unwrap(),
            [
                (LACTEA_LOG_LEVEL_WARN, "loud".to_string()),
                (LACTEA_LOG_LEVEL_ERROR, "louder".to_string())
            ]
        );
    }
}
