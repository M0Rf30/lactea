//! Preset-switch notifications for the host: `projectm_set_preset_switch_requested_event_callback`
//! and `projectm_set_preset_switch_failed_event_callback` (`callbacks.h`).
//!
//! The C side only talks to the engine through [`SwitchEvents`]; [`CCallbacks`] is the implementation
//! that forwards to the function pointers a host registered. A lactea call only *queues* events
//! ([`Outbox`]); `deliver_events` in `lib.rs` runs the callbacks on the calling thread once the call
//! has finished and holds no reference into the engine, so a callback may call back into the same
//! handle. Callbacks run inside the C call that caused them (`lactea_render*` for a request,
//! `lactea_load_*` for a failure), after the call's own work is done and its status is decided.

use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_void};

/// Called when a rendered frame asks for another preset (display time up or a beat-driven hard cut,
/// see `lactea_set_switch_settings`). With `LACTEA_COMPAT_MILKDROP3` an auto hard cut
/// (`lactea_set_auto_hard_cut_mode`) calls it too, with `is_hard_cut = true`; `preset_locked`
/// silences every request. The request is *also*
/// kept for `lactea_poll_switch_request`. The callback runs synchronously on the calling thread,
/// inside `lactea_render*`, after the frame is submitted and the call's work is done. It may call
/// any lactea function on the same engine, for example `lactea_load_preset_ex` to load the next
/// preset; events raised by those nested calls are delivered after this callback returns, never
/// recursively. `lactea_destroy` on the engine is allowed too: the engine is then freed when the
/// outer lactea call returns, and the callback must not use the handle afterwards.
pub type LacteaSwitchRequestedFn =
    Option<unsafe extern "C" fn(is_hard_cut: bool, user_data: *mut c_void)>;

/// Called when a preset load fails (`lactea_load_preset`, `lactea_load_preset_ex`,
/// `lactea_load_idle_preset`); the previous preset keeps running. `preset_name` and `message` are
/// NUL-terminated and valid only during the call. Like the switch-requested callback it runs
/// synchronously inside the lactea call, after the call's status and `lactea_last_error` are set,
/// and may call back into the same engine (see `LacteaSwitchRequestedFn`). `lactea_last_error`
/// reads the same message again after the callback returns.
pub type LacteaSwitchFailedFn = Option<
    unsafe extern "C" fn(
        preset_name: *const c_char,
        message: *const c_char,
        user_data: *mut c_void,
    ),
>;

/// What the C API reports to a host.
pub(crate) trait SwitchEvents {
    /// The running preset asked to be replaced.
    fn switch_requested(&mut self, hard_cut: bool);
    /// Loading `preset` failed with `message`.
    fn switch_failed(&mut self, preset: &str, message: &str);
}

/// [`SwitchEvents`] over C function pointers (each one optional).
#[derive(Clone, Copy)]
pub(crate) struct CCallbacks {
    pub requested: LacteaSwitchRequestedFn,
    pub requested_user: *mut c_void,
    pub failed: LacteaSwitchFailedFn,
    pub failed_user: *mut c_void,
}

impl Default for CCallbacks {
    fn default() -> Self {
        Self {
            requested: None,
            requested_user: std::ptr::null_mut(),
            failed: None,
            failed_user: std::ptr::null_mut(),
        }
    }
}

impl SwitchEvents for CCallbacks {
    fn switch_requested(&mut self, hard_cut: bool) {
        if let Some(f) = self.requested {
            unsafe { f(hard_cut, self.requested_user) };
        }
    }

    fn switch_failed(&mut self, preset: &str, message: &str) {
        if let Some(f) = self.failed {
            let clean = |s: &str| CString::new(s.replace('\0', " ")).unwrap_or_default();
            let (name, msg) = (clean(preset), clean(message));
            unsafe { f(name.as_ptr(), msg.as_ptr(), self.failed_user) };
        }
    }
}

/// One notification waiting for delivery.
pub(crate) enum Event {
    /// A frame asked for a preset change.
    SwitchRequested { hard_cut: bool },
    /// Loading `preset` failed with `message`.
    SwitchFailed { preset: String, message: String },
}

impl Event {
    /// Hand the event to `sink`.
    pub(crate) fn deliver(self, sink: &mut impl SwitchEvents) {
        match self {
            Event::SwitchRequested { hard_cut } => sink.switch_requested(hard_cut),
            Event::SwitchFailed { preset, message } => sink.switch_failed(&preset, &message),
        }
    }
}

/// Events a lactea call raised and the delivery state around them. Delivery happens after the call
/// released its borrow of the engine (`deliver_events` in `lib.rs`), so the engine is never
/// borrowed while host code runs.
#[derive(Default)]
pub(crate) struct Outbox {
    pub queue: VecDeque<Event>,
    /// A delivery loop is running on this handle: nested calls leave their events to it, and
    /// `lactea_destroy` waits for it to finish.
    pub delivering: bool,
    /// `lactea_destroy` was called from a callback; the delivery loop frees the engine when it ends.
    pub destroy_pending: bool,
}
