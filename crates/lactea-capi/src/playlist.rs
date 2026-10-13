//! `lactea_playlist_*`: the C surface of `lactea::Playlist`, one function per
//! `projectm_playlist_*` (`playlist_*.h`).
//!
//! A playlist handle is independent of an engine. [`lactea_playlist_connect`] ties it to one: the
//! `play_*` calls then read the preset file and load it into that engine (a soft cut of the engine's
//! `soft_cut_duration` unless `hard_cut`), and a switch request of the engine (display time up,
//! beat cut) plays the next preset itself, like projectM's playlist connector. While connected, the
//! playlist handles those requests instead of the host's `lactea_set_switch_requested_callback`
//! (`lactea_poll_switch_request` still sees them). A preset load callback
//! ([`lactea_playlist_set_preset_load_event_callback`]) takes over the loading, also without an
//! engine.
//!
//! Host callbacks (switched, switch failed, load) must not call functions on the same playlist
//! handle; they may use the engine. The switched/failed callbacks run after the playlist call has
//! finished its work; the load callback runs inside it.
//!
//! Not thread-safe, like the rest of the C API: one thread per handle.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::rc::Rc;

use lactea::{LoadRequest, Playlist, SortOrder, SortPredicate, Transition};
use lactea_host::PlaylistFs;

use crate::{LacteaEngine, deliver_events, guard_value, load_failed, preset_started, set_error};

/// `projectm_playlist_sort_predicate`: sort by the whole path.
pub const LACTEA_PLAYLIST_SORT_FULL_PATH: i32 = 0;
/// `projectm_playlist_sort_predicate`: sort by the file name only.
pub const LACTEA_PLAYLIST_SORT_FILENAME_ONLY: i32 = 1;
/// `projectm_playlist_sort_order`: ascending.
pub const LACTEA_PLAYLIST_SORT_ASCENDING: i32 = 0;
/// `projectm_playlist_sort_order`: descending.
pub const LACTEA_PLAYLIST_SORT_DESCENDING: i32 = 1;

/// A preset was switched to (`projectm_playlist_preset_switched_event`). Not raised when a load
/// callback handled the load.
pub type LacteaPlaylistSwitchedFn =
    Option<unsafe extern "C" fn(is_hard_cut: bool, index: u32, user_data: *mut c_void)>;

/// Switching failed after the retries (`projectm_playlist_preset_switch_failed_event`); the strings
/// are valid during the call only.
pub type LacteaPlaylistSwitchFailedFn = Option<
    unsafe extern "C" fn(
        preset_filename: *const c_char,
        message: *const c_char,
        user_data: *mut c_void,
    ),
>;

/// Loads the preset at `index` yourself (`projectm_playlist_preset_load_event`): return `true` when
/// handled (the playlist then does nothing else and raises no switched event), `false` to let it
/// load the file.
pub type LacteaPlaylistLoadFn = Option<
    unsafe extern "C" fn(
        index: u32,
        filename: *const c_char,
        hard_cut: bool,
        user_data: *mut c_void,
    ) -> bool,
>;

enum PlEvent {
    Switched { hard_cut: bool, index: u32 },
    Failed { filename: String, message: String },
}

/// The engine's back-reference to its playlist: dropped with the engine, it tells the playlist.
pub(crate) struct Link(*mut LacteaPlaylist);

impl Default for Link {
    fn default() -> Self {
        Link(std::ptr::null_mut())
    }
}

impl Link {
    pub(crate) fn is_connected(&self) -> bool {
        !self.0.is_null()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Some(p) = unsafe { self.0.as_mut() } {
            p.engine = std::ptr::null_mut();
        }
    }
}

/// Opaque playlist handle.
pub struct LacteaPlaylist {
    pl: Playlist,
    engine: *mut LacteaEngine,
    switched: LacteaPlaylistSwitchedFn,
    switched_user: *mut c_void,
    failed: LacteaPlaylistSwitchFailedFn,
    failed_user: *mut c_void,
    load: LacteaPlaylistLoadFn,
    load_user: *mut c_void,
    /// Filled by the core's callbacks during a `play_*`, delivered to the host afterwards.
    events: Rc<RefCell<Vec<PlEvent>>>,
}

fn seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', " ")).unwrap_or_default()
}

fn leak_string(s: &str) -> *mut c_char {
    cstring(s).into_raw()
}

/// A NULL-terminated `char**` of owned copies; free with `lactea_playlist_free_string_array`.
fn leak_array<S: AsRef<str>>(items: &[S]) -> *mut *mut c_char {
    let mut v: Vec<*mut c_char> = items.iter().map(|s| leak_string(s.as_ref())).collect();
    v.push(std::ptr::null_mut());
    Box::into_raw(v.into_boxed_slice()).cast()
}

/// The strings of a host `const char**` array; `None` when it is NULL with `count != 0` or an
/// element is NULL.
unsafe fn read_array(list: *const *const c_char, count: usize) -> Option<Vec<String>> {
    if count == 0 {
        return Some(Vec::new());
    }
    if list.is_null() || !crate::fits_isize::<*const c_char>(count) {
        return None;
    }
    unsafe { std::slice::from_raw_parts(list, count) }
        .iter()
        .map(|&p| {
            (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
        })
        .collect()
}

unsafe fn read_str(p: *const c_char) -> Option<String> {
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// Run `f` on a valid handle; NULL sets the error and gives `default` (also the result of a panic).
fn with_pl<T: Copy>(
    h: *mut LacteaPlaylist,
    default: T,
    f: impl FnOnce(&mut LacteaPlaylist) -> T,
) -> T {
    guard_value(default, || match unsafe { h.as_mut() } {
        Some(p) => f(p),
        None => {
            set_error("playlist is NULL");
            default
        }
    })
}

fn bad_arg(what: &str) {
    set_error(format!("{what} is NULL or invalid"));
}

// ---------------------------------------------------------------------------------------------
// lifecycle
// ---------------------------------------------------------------------------------------------

/// `projectm_playlist_create`: an empty playlist, connected to `engine` unless that is NULL. The
/// shuffle sequence is seeded from the clock. Free with `lactea_playlist_destroy`; the playlist and
/// the engine may be destroyed in either order.
///
/// # Safety
/// `engine` NULL or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_create(engine: *mut LacteaEngine) -> *mut LacteaPlaylist {
    guard_value(std::ptr::null_mut(), || {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut pl = Playlist::new(seed());
        let q = events.clone();
        pl.set_preset_switched_callback(Some(Box::new(move |hard_cut, index| {
            q.borrow_mut().push(PlEvent::Switched { hard_cut, index });
        })));
        let q = events.clone();
        pl.set_preset_switch_failed_callback(Some(Box::new(move |filename, message| {
            q.borrow_mut().push(PlEvent::Failed {
                filename: filename.to_owned(),
                message: message.to_owned(),
            });
        })));
        let h = Box::into_raw(Box::new(LacteaPlaylist {
            pl,
            engine: std::ptr::null_mut(),
            switched: None,
            switched_user: std::ptr::null_mut(),
            failed: None,
            failed_user: std::ptr::null_mut(),
            load: None,
            load_user: std::ptr::null_mut(),
            events,
        }));
        unsafe { lactea_playlist_connect(h, engine) };
        h
    })
}

/// `projectm_playlist_destroy`. NULL is ignored. Must not be called from one of the playlist's own
/// callbacks.
///
/// # Safety
/// `playlist` NULL or a handle from `lactea_playlist_create`, not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_destroy(playlist: *mut LacteaPlaylist) {
    if playlist.is_null() {
        return;
    }
    guard_value((), || {
        unsafe { lactea_playlist_connect(playlist, std::ptr::null_mut()) };
        drop(unsafe { Box::from_raw(playlist) });
    });
}

/// `projectm_playlist_connect`: (re)connects the playlist to `engine`, or disconnects it with NULL.
/// See the module notes for what connecting does. An engine has at most one playlist: connecting
/// it here disconnects the one it had.
///
/// # Safety
/// `playlist` valid; `engine` NULL or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_connect(
    playlist: *mut LacteaPlaylist,
    engine: *mut LacteaEngine,
) {
    with_pl(playlist, (), |p| {
        if let Some(old) = unsafe { p.engine.as_mut() } {
            old.playlist_link = Link::default();
        }
        p.engine = std::ptr::null_mut();
        if let Some(e) = unsafe { engine.as_mut() } {
            // dropping the previous link tells the playlist that was connected to this engine
            e.playlist_link = Link(playlist);
            p.engine = engine;
        }
    });
}

// ---------------------------------------------------------------------------------------------
// items
// ---------------------------------------------------------------------------------------------

/// `projectm_playlist_size`.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_size(playlist: *mut LacteaPlaylist) -> u32 {
    with_pl(playlist, 0, |p| p.pl.len())
}

/// `projectm_playlist_clear`: removes every item and the history.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_clear(playlist: *mut LacteaPlaylist) {
    with_pl(playlist, (), |p| p.pl.clear());
}

/// `projectm_playlist_items`: copies of the items `start..start+count` (clamped) as a
/// NULL-terminated array; free it with `lactea_playlist_free_string_array`. NULL for a NULL handle.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_items(
    playlist: *mut LacteaPlaylist,
    start: u32,
    count: u32,
) -> *mut *mut c_char {
    with_pl(playlist, std::ptr::null_mut(), |p| {
        leak_array(p.pl.items_range(start, count))
    })
}

/// `projectm_playlist_item`: a copy of the item at `index`, to free with
/// `lactea_playlist_free_string`; NULL when out of range.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_item(
    playlist: *mut LacteaPlaylist,
    index: u32,
) -> *mut c_char {
    with_pl(playlist, std::ptr::null_mut(), |p| {
        p.pl.item(index).map_or(std::ptr::null_mut(), leak_string)
    })
}

/// `projectm_playlist_add_path`: appends the `.milk`, `.prjm` and `.milk2` files of the directory
/// `path` (also below it with `recurse_subdirs`), sorted by path. Returns how many were added;
/// the filter and `allow_duplicates` apply.
///
/// # Safety
/// `playlist` valid; `path` NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_add_path(
    playlist: *mut LacteaPlaylist,
    path: *const c_char,
    recurse_subdirs: bool,
    allow_duplicates: bool,
) -> u32 {
    unsafe {
        lactea_playlist_insert_path(
            playlist,
            path,
            lactea::INSERT_AT_END,
            recurse_subdirs,
            allow_duplicates,
        )
    }
}

/// `projectm_playlist_insert_path`: like `lactea_playlist_add_path`, starting at `index`
/// (`UINT32_MAX` appends).
///
/// # Safety
/// `playlist` valid; `path` NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_insert_path(
    playlist: *mut LacteaPlaylist,
    path: *const c_char,
    index: u32,
    recurse_subdirs: bool,
    allow_duplicates: bool,
) -> u32 {
    with_pl(playlist, 0, |p| {
        let Some(path) = (unsafe { read_str(path) }) else {
            bad_arg("path");
            return 0;
        };
        p.pl.insert_path(Path::new(&path), index, recurse_subdirs, allow_duplicates)
    })
}

/// `projectm_playlist_add_preset`.
///
/// # Safety
/// `playlist` valid; `filename` NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_add_preset(
    playlist: *mut LacteaPlaylist,
    filename: *const c_char,
    allow_duplicates: bool,
) -> bool {
    unsafe {
        lactea_playlist_insert_preset(playlist, filename, lactea::INSERT_AT_END, allow_duplicates)
    }
}

/// `projectm_playlist_insert_preset` (`index == UINT32_MAX` appends). `false` when the filter or a
/// duplicate rejected it.
///
/// # Safety
/// `playlist` valid; `filename` NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_insert_preset(
    playlist: *mut LacteaPlaylist,
    filename: *const c_char,
    index: u32,
    allow_duplicates: bool,
) -> bool {
    with_pl(playlist, false, |p| {
        let Some(name) = (unsafe { read_str(filename) }) else {
            bad_arg("filename");
            return false;
        };
        p.pl.insert_preset(&name, index, allow_duplicates)
    })
}

/// `projectm_playlist_add_presets`: returns how many of the `count` names were added.
///
/// # Safety
/// `playlist` valid; `filenames` points to `count` NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_add_presets(
    playlist: *mut LacteaPlaylist,
    filenames: *const *const c_char,
    count: u32,
    allow_duplicates: bool,
) -> u32 {
    unsafe {
        lactea_playlist_insert_presets(
            playlist,
            filenames,
            count,
            lactea::INSERT_AT_END,
            allow_duplicates,
        )
    }
}

/// `projectm_playlist_insert_presets`: the accepted names keep their order from `index` on.
///
/// # Safety
/// `playlist` valid; `filenames` points to `count` NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_insert_presets(
    playlist: *mut LacteaPlaylist,
    filenames: *const *const c_char,
    count: u32,
    index: u32,
    allow_duplicates: bool,
) -> u32 {
    with_pl(playlist, 0, |p| {
        let Some(names) = (unsafe { read_array(filenames, count as usize) }) else {
            bad_arg("filenames");
            return 0;
        };
        p.pl.insert_presets(&names, index, allow_duplicates)
    })
}

/// `projectm_playlist_remove_preset`.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_remove_preset(
    playlist: *mut LacteaPlaylist,
    index: u32,
) -> bool {
    with_pl(playlist, false, |p| p.pl.remove_preset(index))
}

/// `projectm_playlist_remove_presets`: returns how many were removed.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_remove_presets(
    playlist: *mut LacteaPlaylist,
    index: u32,
    count: u32,
) -> u32 {
    with_pl(playlist, 0, |p| p.pl.remove_presets(index, count))
}

/// `projectm_playlist_sort` of `count` items from `start_index`. `predicate` is
/// `LACTEA_PLAYLIST_SORT_FULL_PATH` or `..._FILENAME_ONLY`, `order` `LACTEA_PLAYLIST_SORT_ASCENDING`
/// or `..._DESCENDING`; other values are ignored (see `lactea_last_error`). Clears the history.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_sort(
    playlist: *mut LacteaPlaylist,
    start_index: u32,
    count: u32,
    predicate: i32,
    order: i32,
) {
    with_pl(playlist, (), |p| {
        let predicate = match predicate {
            LACTEA_PLAYLIST_SORT_FULL_PATH => SortPredicate::FullPath,
            LACTEA_PLAYLIST_SORT_FILENAME_ONLY => SortPredicate::FilenameOnly,
            _ => return bad_arg("predicate"),
        };
        let order = match order {
            LACTEA_PLAYLIST_SORT_ASCENDING => SortOrder::Ascending,
            LACTEA_PLAYLIST_SORT_DESCENDING => SortOrder::Descending,
            _ => return bad_arg("order"),
        };
        p.pl.sort(start_index, count, predicate, order);
    });
}

// ---------------------------------------------------------------------------------------------
// filter
// ---------------------------------------------------------------------------------------------

/// `projectm_playlist_set_filter`: `count` expressions (`+glob` keeps, `-glob` drops). Only items
/// added afterwards are checked, see `lactea_playlist_apply_filter`.
///
/// # Safety
/// `playlist` valid; `filter_list` points to `count` NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_set_filter(
    playlist: *mut LacteaPlaylist,
    filter_list: *const *const c_char,
    count: usize,
) {
    with_pl(playlist, (), |p| {
        match unsafe { read_array(filter_list, count) } {
            Some(list) => p.pl.set_filter(list),
            None => bad_arg("filter_list"),
        }
    });
}

/// `projectm_playlist_get_filter`: the expressions as a NULL-terminated array (free with
/// `lactea_playlist_free_string_array`); their number goes to `*count` (may be NULL).
///
/// # Safety
/// `playlist` valid; `count` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_get_filter(
    playlist: *mut LacteaPlaylist,
    count: *mut usize,
) -> *mut *mut c_char {
    with_pl(playlist, std::ptr::null_mut(), |p| {
        if let Some(c) = unsafe { count.as_mut() } {
            *c = p.pl.filter().len();
        }
        leak_array(p.pl.filter())
    })
}

/// `projectm_playlist_apply_filter`: removes the items the filter rejects; returns how many.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_apply_filter(playlist: *mut LacteaPlaylist) -> usize {
    with_pl(playlist, 0, |p| p.pl.apply_filter() as usize)
}

// ---------------------------------------------------------------------------------------------
// playback
// ---------------------------------------------------------------------------------------------

/// `projectm_playlist_set_shuffle`.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_set_shuffle(playlist: *mut LacteaPlaylist, shuffle: bool) {
    with_pl(playlist, (), |p| p.pl.set_shuffle(shuffle));
}

/// `projectm_playlist_get_shuffle`.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_get_shuffle(playlist: *mut LacteaPlaylist) -> bool {
    with_pl(playlist, false, |p| p.pl.shuffle())
}

/// `projectm_playlist_set_retry_count`: failed loads in a row before the switch-failed event
/// (default 500).
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_set_retry_count(
    playlist: *mut LacteaPlaylist,
    retry_count: u32,
) {
    with_pl(playlist, (), |p| p.pl.set_retry_count(retry_count));
}

/// `projectm_playlist_get_retry_count`.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_get_retry_count(playlist: *mut LacteaPlaylist) -> u32 {
    with_pl(playlist, 0, |p| p.pl.retry_count())
}

/// `projectm_playlist_get_position` (0 for an empty playlist).
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_get_position(playlist: *mut LacteaPlaylist) -> u32 {
    with_pl(playlist, 0, |p| p.pl.position())
}

type Loader<'a> = &'a mut dyn FnMut(LoadRequest<'_>) -> Result<(), String>;

/// Run one `play_*` of the core with the loader of this handle, then deliver what it raised.
fn drive(h: *mut LacteaPlaylist, run: impl FnOnce(&mut Playlist, Loader<'_>) -> u32) -> u32 {
    with_pl(h, 0, |p| {
        let (engine, load, load_user, events) = (p.engine, p.load, p.load_user, p.events.clone());
        let handled = Cell::new(false);
        let mut loader = |req: LoadRequest<'_>| -> Result<(), String> {
            if let Some(cb) = load {
                let name = cstring(req.filename);
                if unsafe { cb(req.index, name.as_ptr(), req.hard_cut, load_user) } {
                    handled.set(true);
                    return Ok(());
                }
            }
            load_file(engine, req.filename, req.hard_cut)
        };
        let position = run(&mut p.pl, &mut loader);
        // engine-side events (failed loads) and then the playlist's own
        unsafe { deliver_events(p.engine) };
        let queued = std::mem::take(&mut *events.borrow_mut());
        for ev in queued {
            match ev {
                // the host loaded it and announces the switch itself
                PlEvent::Switched { .. } if handled.get() => {}
                PlEvent::Switched { hard_cut, index } => {
                    if let Some(f) = p.switched {
                        unsafe { f(hard_cut, index, p.switched_user) };
                    }
                }
                PlEvent::Failed { filename, message } => {
                    if let Some(f) = p.failed {
                        let (n, m) = (cstring(&filename), cstring(&message));
                        unsafe { f(n.as_ptr(), m.as_ptr(), p.failed_user) };
                    }
                }
            }
        }
        position
    })
}

/// Read `filename` and load it into `engine` (no engine: nothing to load, counts as loaded). A
/// soft cut uses the engine's soft cut duration; a duration of 0 is a hard cut.
fn load_file(engine: *mut LacteaEngine, filename: &str, hard_cut: bool) -> Result<(), String> {
    let Some(e) = (unsafe { engine.as_mut() }) else {
        return Ok(());
    };
    let bytes = match std::fs::read(filename) {
        Ok(b) => b,
        Err(err) => {
            let msg = format!("{filename}: {err}");
            load_failed(e, filename, &msg);
            return Err(msg);
        }
    };
    let soft = if hard_cut {
        None
    } else {
        let seconds = e
            .restore_soft_cut
            .unwrap_or_else(|| e.control.soft_cut_duration());
        (seconds > 0.0).then_some(seconds)
    };
    let transition = soft.map_or(Transition::Cut, |seconds| Transition::Soft { seconds });
    match e.engine.load_preset(&bytes, filename, transition) {
        Ok(_) => {
            preset_started(e, soft);
            Ok(())
        }
        Err(err) => {
            load_failed(e, filename, &err);
            Err(err.to_string())
        }
    }
}

/// `projectm_playlist_set_position`: plays the preset at `new_position` (a failing one is dropped
/// and replaced, see `lactea_playlist_set_retry_count`) and returns the position afterwards.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_set_position(
    playlist: *mut LacteaPlaylist,
    new_position: u32,
    hard_cut: bool,
) -> u32 {
    drive(playlist, |pl, load| {
        pl.set_position(new_position, hard_cut, load)
    })
}

/// `projectm_playlist_play_next` (random with shuffle); returns the new position.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_play_next(
    playlist: *mut LacteaPlaylist,
    hard_cut: bool,
) -> u32 {
    drive(playlist, |pl, load| pl.play_next(hard_cut, load))
}

/// `projectm_playlist_play_previous`; returns the new position.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_play_previous(
    playlist: *mut LacteaPlaylist,
    hard_cut: bool,
) -> u32 {
    drive(playlist, |pl, load| pl.play_previous(hard_cut, load))
}

/// `projectm_playlist_play_last`: the preset played before this one; returns the new position.
///
/// # Safety
/// `playlist` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_play_last(
    playlist: *mut LacteaPlaylist,
    hard_cut: bool,
) -> u32 {
    drive(playlist, |pl, load| pl.play_last(hard_cut, load))
}

/// The engine raised a switch request: with a playlist connected it plays the next preset
/// (`projectm`'s connector). Returns whether a playlist took the request.
pub(crate) fn handle_switch_request(engine: *mut LacteaEngine, hard_cut: bool) -> bool {
    let Some(e) = (unsafe { engine.as_ref() }) else {
        return false;
    };
    let h = e.playlist_link.0;
    if h.is_null() {
        return false;
    }
    drive(h, |pl, load| pl.play_next(hard_cut, load));
    true
}

// ---------------------------------------------------------------------------------------------
// callbacks and memory
// ---------------------------------------------------------------------------------------------

/// `projectm_playlist_set_preset_switched_event_callback` (NULL removes it).
///
/// # Safety
/// `playlist` valid; the callback stays callable until replaced or the playlist is destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_set_preset_switched_event_callback(
    playlist: *mut LacteaPlaylist,
    callback: LacteaPlaylistSwitchedFn,
    user_data: *mut c_void,
) {
    with_pl(playlist, (), |p| {
        p.switched = callback;
        p.switched_user = user_data;
    });
}

/// `projectm_playlist_set_preset_switch_failed_event_callback` (NULL removes it).
///
/// # Safety
/// `playlist` valid; the callback stays callable until replaced or the playlist is destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_set_preset_switch_failed_event_callback(
    playlist: *mut LacteaPlaylist,
    callback: LacteaPlaylistSwitchFailedFn,
    user_data: *mut c_void,
) {
    with_pl(playlist, (), |p| {
        p.failed = callback;
        p.failed_user = user_data;
    });
}

/// `projectm_playlist_set_preset_load_event_callback` (NULL removes it): the host loads the presets.
///
/// # Safety
/// `playlist` valid; the callback stays callable until replaced or the playlist is destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_set_preset_load_event_callback(
    playlist: *mut LacteaPlaylist,
    callback: LacteaPlaylistLoadFn,
    user_data: *mut c_void,
) {
    with_pl(playlist, (), |p| {
        p.load = callback;
        p.load_user = user_data;
    });
}

/// `projectm_playlist_free_string`: frees a string returned by this API. NULL is ignored.
///
/// # Safety
/// `string` NULL or returned by `lactea_playlist_item`, not freed before.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_free_string(string: *mut c_char) {
    if !string.is_null() {
        drop(unsafe { CString::from_raw(string) });
    }
}

/// `projectm_playlist_free_string_array`: frees an array and its strings returned by
/// `lactea_playlist_items` or `lactea_playlist_get_filter`. NULL is ignored.
///
/// # Safety
/// `array` NULL or returned by one of those, not freed before.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lactea_playlist_free_string_array(array: *mut *mut c_char) {
    if array.is_null() {
        return;
    }
    let mut n = 0;
    while !unsafe { *array.add(n) }.is_null() {
        drop(unsafe { CString::from_raw(*array.add(n)) });
        n += 1;
    }
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(array, n + 1)) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::engine;
    use crate::*;
    use std::ptr::null_mut;

    unsafe fn strings(array: *mut *mut c_char) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while !unsafe { *array.add(i) }.is_null() {
            out.push(
                unsafe { CStr::from_ptr(*array.add(i)) }
                    .to_string_lossy()
                    .into_owned(),
            );
            i += 1;
        }
        out
    }

    unsafe fn items(h: *mut LacteaPlaylist) -> Vec<String> {
        let a = unsafe { lactea_playlist_items(h, 0, u32::MAX) };
        let v = unsafe { strings(a) };
        unsafe { lactea_playlist_free_string_array(a) };
        v
    }

    #[derive(Default)]
    struct Seen {
        switched: Vec<(bool, u32)>,
        failed: Vec<(String, String)>,
        loads: Vec<(u32, String, bool)>,
        requested: u32,
    }

    unsafe extern "C" fn on_switched(hard: bool, index: u32, user: *mut c_void) {
        unsafe { &mut *user.cast::<Seen>() }
            .switched
            .push((hard, index));
    }

    unsafe extern "C" fn on_failed(f: *const c_char, m: *const c_char, user: *mut c_void) {
        let (f, m) = unsafe { (CStr::from_ptr(f), CStr::from_ptr(m)) };
        unsafe { &mut *user.cast::<Seen>() }
            .failed
            .push((f.to_string_lossy().into(), m.to_string_lossy().into()));
    }

    unsafe extern "C" fn on_load(
        index: u32,
        f: *const c_char,
        hard: bool,
        user: *mut c_void,
    ) -> bool {
        let f = unsafe { CStr::from_ptr(f) }.to_string_lossy().into_owned();
        unsafe { &mut *user.cast::<Seen>() }
            .loads
            .push((index, f, hard));
        true
    }

    unsafe extern "C" fn on_requested(_hard: bool, user: *mut c_void) {
        unsafe { &mut *user.cast::<Seen>() }.requested += 1;
    }

    #[test]
    fn c_round_trip_without_an_engine() {
        unsafe {
            let h = lactea_playlist_create(null_mut());
            assert!(!h.is_null());
            let names = [c"/p/b.milk", c"/p/a.milk", c"/q/c.milk", c"/p/a.milk"];
            let ptrs: Vec<*const c_char> = names.iter().map(|n| n.as_ptr()).collect();
            assert_eq!(lactea_playlist_add_presets(h, ptrs.as_ptr(), 4, false), 3);
            lactea_playlist_sort(
                h,
                0,
                u32::MAX,
                LACTEA_PLAYLIST_SORT_FILENAME_ONLY,
                LACTEA_PLAYLIST_SORT_ASCENDING,
            );
            assert_eq!(items(h), ["/p/a.milk", "/p/b.milk", "/q/c.milk"]);
            lactea_playlist_sort(h, 0, u32::MAX, 7, 0);
            assert!(!lactea_last_error().is_null());
            assert_eq!(items(h).len(), 3);

            let one = lactea_playlist_item(h, 1);
            assert_eq!(CStr::from_ptr(one).to_str().unwrap(), "/p/b.milk");
            lactea_playlist_free_string(one);
            assert!(lactea_playlist_item(h, 9).is_null());
            assert!(lactea_playlist_insert_preset(
                h,
                c"/z/first.milk".as_ptr(),
                0,
                false
            ));
            assert!(lactea_playlist_remove_preset(h, 0));
            assert_eq!(lactea_playlist_remove_presets(h, 1, 9), 2);
            assert_eq!(lactea_playlist_size(h), 1);
            lactea_playlist_clear(h);
            assert_eq!(lactea_playlist_size(h), 0);

            let f = [c"-*c.milk".as_ptr()];
            lactea_playlist_set_filter(h, f.as_ptr(), 1);
            assert!(!lactea_playlist_add_preset(h, c"/q/c.milk".as_ptr(), false));
            let mut n = 0;
            let got = lactea_playlist_get_filter(h, &mut n);
            assert_eq!((n, strings(got)), (1, vec!["-*c.milk".to_string()]));
            lactea_playlist_free_string_array(got);
            lactea_playlist_set_filter(h, null_mut(), 0);
            assert!(lactea_playlist_add_preset(h, c"/q/c.milk".as_ptr(), false));
            lactea_playlist_set_filter(h, f.as_ptr(), 1);
            assert_eq!(lactea_playlist_apply_filter(h), 1);

            lactea_playlist_set_shuffle(h, true);
            assert!(lactea_playlist_get_shuffle(h));
            assert_eq!(lactea_playlist_get_retry_count(h), 500);
            lactea_playlist_set_retry_count(h, 3);
            assert_eq!(lactea_playlist_get_retry_count(h), 3);
            lactea_playlist_set_shuffle(h, false);

            // without an engine the switch only moves the position; a load callback sees the file
            // and its handling suppresses the switched event
            let mut seen = Seen::default();
            let user = (&raw mut seen).cast();
            assert_eq!(lactea_playlist_add_presets(h, ptrs.as_ptr(), 2, false), 2);
            lactea_playlist_set_preset_switched_event_callback(h, Some(on_switched), user);
            assert_eq!(lactea_playlist_play_next(h, true), 1);
            assert_eq!(seen.switched, [(true, 1)]);
            lactea_playlist_set_preset_load_event_callback(h, Some(on_load), user);
            assert_eq!(lactea_playlist_play_next(h, false), 0);
            assert_eq!(lactea_playlist_play_last(h, true), 1);
            assert_eq!(lactea_playlist_get_position(h), 1);
            assert_eq!(
                seen.loads,
                [
                    (0, "/p/b.milk".to_string(), false),
                    (1, "/p/a.milk".to_string(), true)
                ]
            );
            assert_eq!(seen.switched.len(), 1);
            lactea_playlist_destroy(h);
            assert_eq!(lactea_playlist_size(null_mut()), 0);
            assert!(lactea_playlist_item(null_mut(), 0).is_null());
        }
    }

    #[test]
    fn connected_playlist_loads_files_skips_broken_ones_and_follows_the_engine() {
        let Some(e) = engine() else {
            eprintln!("skipping: no wgpu adapter");
            return;
        };
        let dir = std::env::temp_dir().join(format!("lactea-capi-playlist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["a.milk", "b.milk"] {
            std::fs::write(dir.join(f), lactea::IDLE_PRESET).unwrap();
        }
        let missing = dir.join("missing.milk");
        let path = |p: &Path| CString::new(p.to_str().unwrap()).unwrap();
        unsafe {
            let mut seen = Seen::default();
            let user = (&raw mut seen).cast();
            let h = lactea_playlist_create(e);
            lactea_playlist_set_preset_switched_event_callback(h, Some(on_switched), user);
            lactea_playlist_set_preset_switch_failed_event_callback(h, Some(on_failed), user);
            lactea_set_switch_requested_callback(e, Some(on_requested), user);
            assert_eq!(
                lactea_playlist_add_path(h, path(&dir).as_ptr(), false, false),
                2
            );
            assert!(lactea_playlist_add_preset(
                h,
                path(&missing).as_ptr(),
                false
            ));
            assert_eq!(lactea_playlist_size(h), 3);

            // the missing file is dropped and the next one plays
            assert_eq!(lactea_playlist_set_position(h, 2, true), 0);
            assert_eq!(lactea_playlist_size(h), 2);
            assert_eq!(seen.switched, [(true, 0)]);
            assert!(seen.failed.is_empty());
            // a soft cut blends the loaded file in
            assert_eq!(lactea_playlist_play_next(h, false), 1);
            assert!((*e).engine.transition_info().is_some());

            // an engine switch request plays the next preset, not the host callback
            let mut s = std::mem::MaybeUninit::<LacteaSwitchSettings>::uninit();
            lactea_get_switch_settings(e, s.as_mut_ptr());
            let mut s = s.assume_init();
            (s.preset_duration, s.easter_egg, s.soft_cut_duration) = (1.0, 0.0, 0.0);
            assert_eq!(lactea_set_switch_settings(e, &s), LacteaStatus::Ok);
            assert_eq!(lactea_playlist_set_position(h, 0, true), 0);
            for frame in 0..90u32 {
                assert_eq!(
                    lactea_render(e, f64::from(frame) / 60.0, frame),
                    LacteaStatus::Ok
                );
            }
            assert_eq!(lactea_playlist_get_position(h), 1);
            assert_eq!(seen.requested, 0);
            assert!(!lactea_poll_switch_request(e, null_mut()));

            // all retries used up: the failure is reported, the item stays
            lactea_playlist_clear(h);
            assert!(lactea_playlist_add_preset(
                h,
                path(&missing).as_ptr(),
                false
            ));
            lactea_playlist_set_retry_count(h, 1);
            lactea_playlist_set_position(h, 0, true);
            assert_eq!(seen.failed.len(), 1);
            assert!(
                seen.failed[0].0.ends_with("missing.milk"),
                "{:?}",
                seen.failed
            );
            assert_eq!(lactea_playlist_size(h), 1);

            // the engine can go first; the playlist then just has no engine
            lactea_destroy(e);
            assert_eq!(lactea_playlist_set_position(h, 0, true), 0);
            lactea_playlist_destroy(h);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
